//! Explicit administrator-triggered native capability acceptance checks.
use crate::{
    backend::Backend,
    error::{Error, Result},
    operations::Feature,
    provider::Provider,
    responses::Request,
};
use serde_json::{Value, json};
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub async fn text_roundtrip(
    backend: &Backend,
    provider: &Provider,
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<crate::streaming::StreamSummary> {
    let request =
        Request::parse(json!({"model":provider.model,"input":"Reply with OK.","store":false}))?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let work = crate::responses::run(
        &backend.rpc,
        provider,
        cwd,
        &request,
        &request.input,
        tx,
        cancel.clone(),
    );
    let combined = async { tokio::join!(work, async { while rx.recv().await.is_some() {} }) };
    tokio::pin!(combined);
    let (result, ()) = tokio::select! {result=&mut combined=>result,_=tokio::time::sleep(std::time::Duration::from_secs(90))=>{cancel.cancel();combined.await}};
    let mut result = result?;
    if result.outcome != "completed" || result.text_bytes == 0 {
        result.outcome = "failed".into();
        if result.error_code.is_none() {
            result.error_code = Some("OAUTH_INFERENCE_UNVERIFIED".into());
        }
    }
    Ok(result)
}

pub async fn run(
    backend: &Backend,
    provider: &Provider,
    cwd: &Path,
    feature: Feature,
    cancel: CancellationToken,
) -> Result<()> {
    let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFUlEQVR4nGNgYPhPIhrVMKph2GoAAJLb/wFh5Z4RAAAAAElFTkSuQmCC";
    let mut value = json!({"model":provider.model,"input":"Reply with OK.","store":false});
    match feature {
        Feature::ImageInput => {
            value["input"] = json!([{"role":"user","content":[{"type":"input_text","text":"Describe the attached image briefly."},{"type":"input_image","image_url":image}]}])
        }
        Feature::NamespacedTools => {
            value["input"] = json!(
                "Call client.bridge_check once with no arguments. Do not call any other tool."
            );
            value["tools"] = json!([{"type":"namespace","name":"client","description":"Client validation","tools":[{"type":"function","name":"bridge_check","parameters":{"type":"object","properties":{}}}]}]);
        }
        Feature::WebSearch => {
            value["input"] =
                json!("Use web search to find the official Rust language website and cite it.");
            value["tools"] = json!([{"type":"web_search"}]);
        }
        Feature::ImageGeneration => {
            value["input"] =
                json!("Generate a small plain blue square image. Use image generation.");
            value["tools"] = json!([{"type":"image_generation"}]);
        }
    }
    let request = Request::parse(value)?;
    let deadline_cancel = cancel.clone();
    let deadline_task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        deadline_cancel.cancel();
    });
    struct Timer(tokio::task::JoinHandle<()>);
    impl Drop for Timer {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _timer = Timer(deadline_task);
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let work = crate::responses::run(
        &backend.rpc,
        provider,
        cwd,
        &request,
        &request.input,
        tx,
        cancel.clone(),
    );
    let collect = async {
        let mut terminal = None;
        while let Some(event) = rx.recv().await {
            if event["type"] == "response.completed" {
                terminal = Some(event["response"].clone());
            }
        }
        terminal
    };
    let deadline = async {
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        cancel.cancel();
    };
    let combined = async { tokio::join!(work, collect) };
    tokio::pin!(combined);
    tokio::pin!(deadline);
    let (result, response) = tokio::select! {r=&mut combined=>r,_=&mut deadline=>combined.await};
    let summary = result?;
    if summary.outcome != "completed" {
        return Err(Error::new(
            summary
                .error_code
                .as_deref()
                .unwrap_or("CAPABILITY_PROBE_FAILED"),
            "原生能力验证未完成。",
        ));
    }
    let response =
        response.ok_or_else(|| Error::new("CAPABILITY_PROBE_FAILED", "验证没有完整终态。"))?;
    let items = response["output"]
        .as_array()
        .ok_or_else(|| Error::new("CAPABILITY_PROBE_FAILED", "验证没有输出。"))?;
    let valid = items.iter().any(|v| match feature {
        Feature::ImageInput => v["type"] == "message",
        Feature::NamespacedTools => {
            v["type"] == "function_call"
                && v["namespace"] == "client"
                && v["name"] == "bridge_check"
        }
        Feature::WebSearch => v["type"] == "web_search_call" && v["status"] == "completed",
        Feature::ImageGeneration => {
            v["type"] == "image_generation_call" && v["status"] == "completed"
        }
    });
    if !valid {
        return Err(Error::new(
            "CAPABILITY_PROBE_INCONCLUSIVE",
            "模型未产生所需的原生能力条目，保持关闭。",
        ));
    }
    if feature == Feature::NamespacedTools {
        let mut history = request.input.clone();
        history.extend(items.clone());
        for item in items.iter().filter(|v| v["type"] == "function_call") {
            history.push(json!({"type":"function_call_output","call_id":item["call_id"],"output":"BRIDGE_CLIENT_TOOL_OK"}));
        }
        let follow = Request::parse(
            json!({"model":provider.model,"input":history,"tools":request.tools,"tool_choice":"none","store":false}),
        )?;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(16);
        let work = crate::responses::run(
            &backend.rpc,
            provider,
            cwd,
            &follow,
            &follow.input,
            tx,
            cancel.clone(),
        );
        let drain = async { while rx.recv().await.is_some() {} };
        let (result, ()) = tokio::join!(work, drain);
        let summary = result?;
        if summary.outcome != "completed" {
            return Err(Error::new(
                "CAPABILITY_PROBE_FAILED",
                "namespace 工具结果续接未通过。",
            ));
        }
    }
    Ok(())
}
