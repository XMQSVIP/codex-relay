use super::Rpc;
use crate::error::{Error, Result};
use serde_json::json;
use std::{collections::HashSet, time::Duration};

impl Rpc {
    /// Unsubscribe is only an acknowledgement. Capacity belongs to the thread
    /// until its shutdown is confirmed, including after notification loss.
    pub(crate) async fn close_idle_thread(&self, thread: &str) -> Result<()> {
        let mut events = self.subscribe_thread(thread);
        let close = async {
            self.request("thread/unsubscribe", json!({"threadId":thread}))
                .await?;
            let mut checks = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    event=events.recv()=>match event {
                        Ok(event) if event["method"]=="thread/closed" && event["params"]["threadId"]==thread => return Ok(()),
                        Err(tokio::sync::broadcast::error::RecvError::Closed)=>return Err(cleanup()),
                        _=>{},
                    },
                    _=self.closed()=>return Err(cleanup()),
                    _=checks.tick()=>if self.thread_absent(thread).await? {return Ok(());},
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), close)
            .await
            .map_err(|_| cleanup())??;
        self.inner.thread_events.lock().unwrap().remove(thread);
        self.clear_thread_tool_callbacks(thread);
        Ok(())
    }
    async fn thread_absent(&self, thread: &str) -> Result<bool> {
        let mut cursor = None;
        let mut seen = HashSet::new();
        loop {
            let page = self
                .request("thread/loaded/list", json!({"cursor":cursor,"limit":100}))
                .await?;
            let entries = page["data"].as_array().ok_or_else(cleanup)?;
            if entries.iter().any(|entry| entry.as_str() == Some(thread)) {
                return Ok(false);
            }
            if entries.iter().any(|entry| !entry.is_string()) {
                return Err(cleanup());
            }
            let Some(next) = page["nextCursor"].as_str() else {
                if !page["nextCursor"].is_null() {
                    return Err(cleanup());
                }
                return Ok(true);
            };
            if next.is_empty() || !seen.insert(next.to_owned()) || seen.len() > 1024 {
                return Err(cleanup());
            }
            cursor = Some(next.to_owned());
        }
    }
}
fn cleanup() -> Error {
    Error::new(
        "NATIVE_THREAD_CLEANUP",
        "无法确认原生线程已关闭，执行器将安全退役。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    #[tokio::test]
    async fn repeated_closed_notifications_release_callbacks_and_event_channels() {
        let (client, server) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(client);
        let rpc = Rpc::from_io(r, w);
        let _sibling = rpc.subscribe_thread("sibling");
        rpc.inner
            .tool_calls
            .lock()
            .unwrap()
            .insert("sibling-call".into(), "sibling".into());
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server);
            let mut lines = BufReader::new(r).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let mut data = format!(
                    "{}\n",
                    json!({"id":request["id"],"result":{"data":[],"status":"unsubscribed"}})
                );
                if request["method"] == "thread/unsubscribe" {
                    data.push_str(&format!("{}\n",json!({"method":"thread/closed","params":{"threadId":request["params"]["threadId"]}})));
                }
                if w.write_all(data.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        for i in 0..70 {
            let thread = format!("old-{i}");
            rpc.inner
                .tool_calls
                .lock()
                .unwrap()
                .insert(i.to_string(), thread.clone());
            rpc.close_idle_thread(&thread).await.unwrap();
            assert_eq!(rpc.inner.thread_events.lock().unwrap().len(), 1);
            assert_eq!(rpc.inner.tool_calls.lock().unwrap().len(), 1);
        }
        rpc.close();
        server.abort();
    }
    #[tokio::test]
    async fn acknowledgement_without_shutdown_times_out_and_keeps_channel() {
        let (client, server) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(client);
        let rpc = Rpc::from_io(r, w);
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server);
            let mut lines = BufReader::new(r).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let result = if request["method"] == "thread/unsubscribe" {
                    json!({"status":"unsubscribed"})
                } else {
                    json!({"data":["old"],"nextCursor":null})
                };
                if w.write_all(
                    format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes(),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        });
        assert_eq!(
            rpc.close_idle_thread("old").await.unwrap_err().code,
            "NATIVE_THREAD_CLEANUP"
        );
        assert!(rpc.inner.thread_events.lock().unwrap().contains_key("old"));
        rpc.close();
        server.abort();
    }
    #[tokio::test]
    async fn confirmation_uses_all_pages_and_cleans_only_its_own_channel() {
        let (client, server) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(client);
        let rpc = Rpc::from_io(r, w);
        let _sibling = rpc.subscribe_thread("sibling");
        let server = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server);
            let mut lines = BufReader::new(r).lines();
            let mut queries = 0;
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let result = if request["method"] == "thread/unsubscribe" {
                    json!({"status":"unsubscribed"})
                } else {
                    queries += 1;
                    match queries {
                        1 => json!({"data":["sibling"],"nextCursor":"second"}),
                        2 => json!({"data":["old"],"nextCursor":null}),
                        _ => json!({"data":["sibling"],"nextCursor":null}),
                    }
                };
                w.write_all(
                    format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes(),
                )
                .await
                .unwrap();
                if queries == 3 {
                    break;
                }
            }
            queries
        });
        rpc.close_idle_thread("old").await.unwrap();
        assert_eq!(server.await.unwrap(), 3);
        assert_eq!(rpc.inner.thread_events.lock().unwrap().len(), 1);
        rpc.close();
    }
}
