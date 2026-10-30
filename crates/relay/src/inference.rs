use crate::{
    admin::AdminState,
    error::{Error, Result},
    operations::ClientAccess,
    provider::Provider,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TextRequest {
    pub model: String,
    pub input: Value,
    #[serde(default)]
    pub stream: bool,
    pub store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}
impl TextRequest {
    pub fn client_payload(&self) -> Result<Option<Value>> {
        if self.tools.is_none()
            && self.input.is_string()
            && self.instructions.is_none()
            && self.tool_choice.is_none()
            && self.parallel_tool_calls.is_none()
        {
            return Ok(None);
        }
        let mut value = serde_json::to_value(self)?;
        value["stream"] = json!(true);
        value["store"] = json!(false);
        crate::client_tools::validate_request(&value)?;
        if self.store == Some(true) {
            return Err(Error::new(
                "INVALID_INPUT",
                "仅支持 store=false；请随 input 回传完整上下文。",
            ));
        }
        Ok(Some(value))
    }
    pub fn prompt(&self) -> Result<String> {
        if self.client_payload()?.is_some() {
            // Client content and tools never enter the server agent's context.
            return Ok("Start the client-owned inference transport.".into());
        }
        if self.store == Some(true) {
            return Err(Error::new(
                "INVALID_INPUT",
                "此入口仅支持 store=false 的单轮文本请求。",
            ));
        }
        let text = self.input.as_str().ok_or_else(|| {
            Error::new(
                "INVALID_INPUT",
                "当前 input 只支持文本字符串；尚未开放多轮、工具或多模态。",
            )
        })?;
        if text.trim().is_empty() || text.len() > 16 * 1024 {
            return Err(Error::new(
                "INVALID_INPUT",
                "input 应为 1–16384 字节的非空文本。",
            ));
        }
        Ok(text.into())
    }
}
pub struct RequestJob {
    pub account: Uuid,
    pub access: ClientAccess,
    pub provider: Provider,
    pub model: String,
    pub prompt: String,
    pub client_payload: Option<Value>,
}
pub struct CancelOnDrop(pub CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub async fn start(
    state: Arc<AdminState>,
    access: ClientAccess,
    request: &TextRequest,
) -> Result<(mpsc::Receiver<Value>, CancelOnDrop, Uuid)> {
    let prompt = request.prompt()?;
    if !access.allows(&request.model) {
        return Err(Error::new("MODEL_NOT_ALLOWED", "此 API Key 未授权该模型。"));
    }
    let client_payload = request.client_payload()?;
    let lease = state
        .operations
        .acquire_inference(
            &access,
            crate::operations::RoutingRequest {
                id: Uuid::new_v4(),
                model: request.model.clone(),
                streaming: request.stream,
                class: crate::operations::RequestClass::LegacyText,
                features: if request.tools.as_ref().is_some_and(|t| !t.is_empty()) {
                    vec![crate::operations::Feature::NamespacedTools]
                } else {
                    vec![]
                },
                ..Default::default()
            },
        )
        .await?;
    let account = lease
        .account_id
        .ok_or_else(|| Error::new("INFERENCE_DISABLED", "调度未选择账户。"))?;
    let provider = lease
        .provider
        .clone()
        .ok_or_else(|| Error::new("INFERENCE_DISABLED", "账户路由缺失。"))?;
    if let Err(error) =
        state
            .operations
            .check_inference_route(account, &access, &provider, &request.model)
    {
        return lease.complete(Err(error));
    }
    let id = lease.request_id();
    let cancel = state.shutdown.child_token();
    let drop_guard = CancelOnDrop(cancel.clone());
    // Register before scheduling so a concurrent stop cannot miss a task
    // whose backend has not been spawned yet.
    let registration = if account != crate::operations::DESKTOP_ACCOUNT {
        Some(state.profiles.register_inference(account, cancel.clone())?)
    } else {
        None
    };
    let (tx, rx) = mpsc::channel::<Value>(16);
    let job = RequestJob {
        account,
        access,
        provider,
        model: request.model.clone(),
        prompt,
        client_payload,
    };
    state.tasks.clone().spawn(async move {
        let _registration = registration;
        let (native_tx,mut native_rx) = mpsc::channel::<Value>(16);
        let pump = async {
            let mut terminal = None;
            let mut sequence = 0;
            while let Some(event) = native_rx.recv().await {
                sequence = event["sequence_number"].as_u64().unwrap_or(sequence)+1;
                if ["response.completed","response.failed"].contains(&event["type"].as_str().unwrap_or("")) {
                    terminal = Some(event); continue;
                }
                let sent = tokio::select! {
                    _=cancel.cancelled()=>false,
                    result=tokio::time::timeout(Duration::from_secs(5),tx.send(event))=>result.is_ok_and(|r|r.is_ok()),
                };
                if !sent { cancel.cancel(); break; }
            }
            drop(native_rx);
            (terminal,sequence)
        };
        // Always join cleanup. A disconnected response cancels the token, never
        // aborts the owned native task while a lease is still held.
        let native = async {
            if account == crate::operations::DESKTOP_ACCOUNT {
                state.desktop_control.stream(&job,native_tx,cancel.clone()).await
            } else { state.profiles.stream_lan(&job,&state.operations,native_tx,cancel.clone()).await }
        };
        let (result,(terminal,sequence)) = tokio::join!(native,pump);
        let settled = lease.finish_inference(&result);
        // StreamSummary contains only identifiers, counts, usage and our own
        // codes. Never log prompts, content, credential values or native errors.
        eprintln!("{}", json!({"event":"inference_finished","requestId":id,"summary":result.as_ref().ok(),"errorCode":result.as_ref().err().map(|e|&e.code),"settlementError":settled.as_ref().err().map(|e|&e.code)}));
        let failure = settled.err().or_else(||result.as_ref().err().cloned()).or_else(|| {
            result.as_ref().ok().filter(|s| s.outcome != "completed").map(|s|
                crate::streaming::classify_failure(s.error_code.as_deref().unwrap_or("STREAM_NATIVE_FAILED"), &s.guard))
        });
        let output = if let Some(error) = failure {
            json!({"type":"error","sequence_number":sequence,"code":error.code,"message":error.message,"param":Value::Null})
        } else if let Some(terminal) = terminal {
            terminal
        } else {
            json!({"type":"error","sequence_number":sequence,"code":"STREAM_INCOMPLETE","message":"原生流未生成完整终态。","param":Value::Null})
        };
        let _ = tokio::time::timeout(Duration::from_secs(5),tx.send(output)).await;
    });
    Ok((rx, drop_guard, id))
}
