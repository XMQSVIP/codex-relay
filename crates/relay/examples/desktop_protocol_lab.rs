//! Local protocol compatibility lab. All peers are in memory; no model, desktop
//! credentials, device-check module, or network endpoint is used.
use clap::Parser;
use codex_relay::{installation::NativeClientInfo, rpc::Rpc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::HashSet, error::Error, path::PathBuf, time::Duration};
use tokio::io::{DuplexStream, ReadHalf, WriteHalf};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

type LabResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
type Reader = FramedRead<ReadHalf<DuplexStream>, LinesCodec>;
type Writer = FramedWrite<WriteHalf<DuplexStream>, LinesCodec>;
const RESEARCH_NAME: &str = "codex_relay_research";
const LOCAL_PROOF: &str = "LOCAL-RESEARCH-ONLY-NOT-A-DEVICE-PROOF";
const THREAD: &str = "research-thread";
const TURN: &str = "research-turn";

#[derive(Parser)]
#[command(about = "Run a desktop protocol simulation entirely in memory")]
struct Args {
    /// Save the non-sensitive research report as JSON.
    #[arg(long)]
    report: Option<PathBuf>,
}

fn require(condition: bool, message: &str) -> LabResult<()> {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::other(message).into())
    }
}

async fn read(reader: &mut Reader) -> LabResult<Value> {
    let line = reader
        .next()
        .await
        .ok_or_else(|| std::io::Error::other("Fixture connection closed"))??;
    Ok(serde_json::from_str(&line)?)
}

async fn send(writer: &mut Writer, message: Value) -> LabResult<()> {
    writer.send(message.to_string()).await?;
    Ok(())
}

/// This fixture models a protocol peer, not an OpenAI server or device-proof
/// issuer. It deliberately also probes an unsolicited proof request when the
/// capability is off, to verify the host rejects it without leaking an event.
async fn fixture(
    stream: DuplexStream,
    proof_enabled: bool,
    wrong_thread: bool,
) -> LabResult<Value> {
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = FramedRead::new(reader, LinesCodec::new_with_max_length(64 * 1024));
    let mut writer = FramedWrite::new(writer, LinesCodec::new_with_max_length(64 * 1024));
    let fixture_directory = std::env::temp_dir().join("codex-relay-protocol-fixture");
    let initialize = read(&mut reader).await?;
    require(initialize["method"] == "initialize", "Missing initialize")?;
    require(
        initialize["params"]["clientInfo"]["name"] == RESEARCH_NAME,
        "Research client identity was not preserved",
    )?;
    require(
        initialize["params"]["capabilities"]["requestAttestation"] == proof_enabled,
        "Proof capability does not match the registered fixture provider",
    )?;
    send(
        &mut writer,
        json!({"id":initialize["id"],"result":{"userAgent":"research-fixture/1","codexHome":fixture_directory,"platformFamily":std::env::consts::FAMILY,"platformOs":std::env::consts::OS}}),
    )
    .await?;
    require(
        read(&mut reader).await?["method"] == "initialized",
        "Missing initialized notification",
    )?;

    let start = read(&mut reader).await?;
    require(start["method"] == "thread/start", "Missing thread/start")?;
    let now = codex_relay::now_ms() / 1000;
    send(
        &mut writer,
        json!({"id":start["id"],"result":{
            "thread":{"id":THREAD,"sessionId":"research-session","cliVersion":env!("CARGO_PKG_VERSION"),"createdAt":now,"updatedAt":now,"cwd":fixture_directory,"ephemeral":true,"modelProvider":"research-fixture","preview":"Local protocol fixture","projectId":null,"source":{"custom":"research-fixture"},"status":{"type":"idle"},"turns":[]},
            "model":"synthetic-model","modelProvider":"research-fixture","approvalPolicy":"untrusted","approvalsReviewer":"user","sandbox":{"type":"readOnly"},"cwd":fixture_directory
        }}),
    )
    .await?;
    let turn = read(&mut reader).await?;
    require(
        turn["method"] == "turn/start" && turn["params"]["threadId"] == THREAD,
        "Turn belongs to a different fixture thread",
    )?;
    send(
        &mut writer,
        json!({"id":turn["id"],"result":{"turn":{"id":TURN,"status":"inProgress","items":[]}}}),
    )
    .await?;
    let before = codex_relay::now_ms() / 1000;
    for callback in [
        json!({"id":"host-time","method":"currentTime/read","params":{"threadId":THREAD}}),
        json!({"id":"host-proof","method":"attestation/generate","params":{}}),
        json!({"id":"host-tool","method":"item/tool/call","params":{"threadId":THREAD,"turnId":TURN,"callId":"lab-call","namespace":"functions","tool":"research_echo","arguments":{"text":"lab"}}}),
    ] {
        send(&mut writer, callback).await?;
    }
    let mut replies = HashSet::new();
    while replies.len() < 3 {
        let reply = read(&mut reader).await?;
        let id = reply["id"].as_str().unwrap_or("");
        require(replies.insert(id.to_owned()), "Duplicate callback response")?;
        match id {
            "host-time" => {
                let seconds = reply["result"]["currentTimeAt"].as_u64();
                let after = codex_relay::now_ms() / 1000;
                require(
                    seconds.is_some_and(|time| (before..=after).contains(&time)),
                    "Host time is not current whole Unix seconds",
                )?;
            }
            "host-proof" if proof_enabled => require(
                reply["result"]["token"] == LOCAL_PROOF,
                "Local fixture proof was not returned to its private peer",
            )?,
            "host-proof" => require(
                reply["error"]["code"] == -32001 && reply.get("result").is_none(),
                "Unsolicited proof request was not rejected",
            )?,
            "host-tool" => require(
                reply["result"]
                    == json!({"success":true,"contentItems":[{"type":"inputText","text":"lab"}]}),
                "Tool result was not correlated with its callback",
            )?,
            _ => require(false, "Unexpected callback response ID")?,
        }
    }
    let usage = json!({"inputTokens":10,"outputTokens":2,"totalTokens":12,"cachedInputTokens":0,"reasoningOutputTokens":0});
    for event in [
        json!({"method":"thread/tokenUsage/updated","params":{"threadId":THREAD,"turnId":TURN,"tokenUsage":{"total":usage,"last":usage}}}),
        json!({"method":"item/agentMessage/delta","params":{"threadId":THREAD,"turnId":TURN,"itemId":"lab-message","delta":"Protocol lab completed"}}),
        json!({"method":"turn/completed","params":{"threadId":if wrong_thread {"other-thread"} else {THREAD},"turn":{"id":TURN,"status":"completed","items":[]}}}),
    ] {
        send(&mut writer, event).await?;
    }
    // Keep the peer open until the client has consumed all events.
    let unload = read(&mut reader).await?;
    require(
        unload["method"] == "thread/unload",
        "Client did not finish its event stream",
    )?;
    send(&mut writer, json!({"id":unload["id"],"result":{}})).await?;
    Ok(
        json!({"correlatedCallbackReplies":replies.len(),"hostTimeVerified":true,"proofResponseVerified":true,"toolResultVerified":true}),
    )
}

async fn run_case(proof_enabled: bool, wrong_thread: bool) -> LabResult<Value> {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client);
    let rpc = Rpc::from_io(reader, writer);
    rpc.enable_client_tools();
    if proof_enabled {
        // Only this in-memory fixture can receive this conspicuously synthetic
        // value. There is no option for a network endpoint or a desktop token.
        rpc.set_attestation_provider(|| Box::pin(async { Ok(LOCAL_PROOF.into()) }));
    }
    let mut events = rpc.subscribe();
    let mut peer = tokio::spawn(fixture(server, proof_enabled, wrong_thread));
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let client_info = NativeClientInfo {
            name: RESEARCH_NAME.into(),
            title: "Codex Relay Research Lab".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        };
        let initialized = rpc.initialize(&client_info).await?;
        require(
            initialized["userAgent"] == "research-fixture/1",
            "Initialize response was lost",
        )?;
        let thread = rpc
            .request(
                "thread/start",
                json!({"model":"synthetic-model","ephemeral":true}),
            )
            .await?;
        require(thread["thread"]["id"] == THREAD, "Thread response was lost")?;
        let turn = rpc
            .request(
                "turn/start",
                json!({"threadId":THREAD,"input":[{"type":"text","text":"Run the local fixture"}]}),
            )
            .await?;
        require(turn["turn"]["id"] == TURN, "Turn response was lost")?;
        let mut event_methods = Vec::new();
        let mut saw_text = false;
        let mut saw_usage = false;
        let mut saw_tool = false;
        loop {
            let event = events.recv().await?;
            require(
                !event.to_string().contains(LOCAL_PROOF),
                "Proof leaked to the client event stream",
            )?;
            require(
                event["params"]["threadId"] == THREAD,
                "Event belongs to a different thread",
            )?;
            let method = event["method"].as_str().unwrap_or("");
            event_methods.push(method.to_owned());
            match method {
                "item/tool/call" => {
                    require(
                        event["params"]["turnId"] == TURN
                            && event["params"]["tool"] == "research_echo",
                        "Unexpected tool callback",
                    )?;
                    require(
                        event["params"]["arguments"] == json!({"text":"lab"}),
                        "Unexpected fixture tool input",
                    )?;
                    rpc.answer_tool(
                        event["id"].clone(),
                        json!({"success":true,"contentItems":[{"type":"inputText","text":"lab"}]}),
                    )
                    .await?;
                    saw_tool = true;
                }
                "thread/tokenUsage/updated" => {
                    require(
                        event["params"]["tokenUsage"]["total"]["outputTokens"] == 2,
                        "Usage event changed",
                    )?;
                    saw_usage = true;
                }
                "item/agentMessage/delta" => {
                    require(
                        event["params"]["turnId"] == TURN
                            && event["params"]["delta"] == "Protocol lab completed",
                        "Text event changed",
                    )?;
                    saw_text = true;
                }
                "turn/completed" => {
                    require(
                        event["params"]["turn"]["id"] == TURN
                            && event["params"]["turn"]["status"] == "completed",
                        "Wrong completion event",
                    )?;
                    break;
                }
                _ => require(
                    false,
                    "Private or unexpected callback reached the client event stream",
                )?,
            }
        }
        require(
            saw_tool && saw_text && saw_usage,
            "Required turn events are missing",
        )?;
        rpc.request("thread/unload", json!({"threadId":THREAD}))
            .await?;
        let verified = (&mut peer).await??;
        Ok::<_, Box<dyn Error + Send + Sync>>(json!({
            "case":if proof_enabled {"synthetic-proof-provider"} else {"proof-unavailable"},
            "status":"passed",
            "clientInfo":client_info,
            "requestAttestation":proof_enabled,
            "proofKind":if proof_enabled {"local-test-value-only"} else {"unavailable"},
            "proofAbsentFromClientEvents":true,
            "eventMethods":event_methods,
            "fixture":verified,
        }))
    })
    .await;
    rpc.close();
    if !peer.is_finished() {
        peer.abort();
        let _ = peer.await;
    }
    result?
}

#[tokio::main]
async fn main() -> LabResult<()> {
    let args = Args::parse();
    let mut cases = Vec::new();
    for enabled in [false, true] {
        cases.push(run_case(enabled, false).await?);
    }
    let report = json!({
        "purpose":"desktop-protocol-compatibility-research",
        "checkedAt":codex_relay::now_ms(),
        "transport":"in-memory duplex",
        "realModelRequests":0,
        "networkConnections":0,
        "deviceAttestationVerified":false,
        "officialDesktopEquivalenceVerified":false,
        "cases":cases,
        "limits":["Protocol fixtures exercise the real bridge RPC client, not the official desktop application.","Synthetic proof responses are test data and establish no device or application authenticity.","No upstream HTTP/TLS/WebSocket fingerprint or service-side acceptance was tested."],
    });
    let output = serde_json::to_string_pretty(&report)? + "\n";
    if let Some(path) = args.report {
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, &output).await?;
    }
    print!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn complete_protocol_with_and_without_a_mock_proof() {
        for enabled in [false, true] {
            let report = run_case(enabled, false).await.unwrap();
            assert_eq!(report["status"], "passed");
            assert_eq!(report["fixture"]["correlatedCallbackReplies"], 3);
        }
    }

    #[tokio::test]
    async fn simulator_rejects_completion_from_another_thread() {
        let error = run_case(true, true).await.unwrap_err();
        assert!(error.to_string().contains("different thread"));
    }
}
