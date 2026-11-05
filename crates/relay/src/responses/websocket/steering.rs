//! Ordered, bounded control work must never stall socket event forwarding.
use super::{Control, LaneId, Signal, emit, error_event};
use crate::error::{Error, Result};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

const MAX_BYTES: usize = 64 * 1024 * 1024;
struct Command {
    control: Arc<Control>,
    lane: LaneId,
    input: Value,
    _bytes: OwnedSemaphorePermit,
}
pub(super) struct SteeringQueue {
    sender: mpsc::Sender<Command>,
    budget: Arc<Semaphore>,
}
impl SteeringQueue {
    pub fn new(
        events: mpsc::Sender<Signal>,
        cancel: CancellationToken,
    ) -> (Self, impl std::future::Future<Output = ()>) {
        let (sender, mut incoming) = mpsc::channel::<Command>(8);
        let queue = Self {
            sender,
            budget: Arc::new(Semaphore::new(MAX_BYTES)),
        };
        let worker = async move {
            loop {
                let command = tokio::select! {
                    _ = cancel.cancelled() => break,
                    command = incoming.recv() => match command { Some(value) => value, None => break },
                };
                let result = tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = command.control.steer(command.input) => result,
                };
                if let Err(error) = result {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = emit(&events, &command.lane, error_event(error)) => {},
                    }
                }
            }
        };
        (queue, worker)
    }
    pub fn enqueue(
        &self,
        control: Arc<Control>,
        lane: LaneId,
        input: Value,
        bytes: usize,
    ) -> Result<()> {
        control.check_authorization()?;
        let full = || Error::new("QUEUE_FULL", "WebSocket 追加输入队列已满，请稍后重试。");
        if bytes > MAX_BYTES {
            return Err(full());
        }
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(bytes.max(1) as u32)
            .map_err(|_| full())?;
        self.sender
            .try_send(Command {
                control,
                lane,
                input,
                _bytes: permit,
            })
            .map_err(|_| full())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    #[tokio::test]
    async fn delayed_steering_preserves_fifo_without_blocking_model_events() {
        let (client, server) = tokio::io::duplex(65536);
        let (reader, writer) = tokio::io::split(client);
        let rpc = crate::rpc::Rpc::from_io(reader, writer);
        let control = Arc::new(Control::default());
        control.activate(&rpc, "thread", "turn");
        let (events, mut receiver) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let (queue, work) = SteeringQueue::new(events.clone(), cancel.clone());
        let worker = tokio::spawn(work);
        queue
            .enqueue(control.clone(), None, json!("first"), 20)
            .unwrap();
        queue
            .enqueue(control.clone(), None, json!("second"), 20)
            .unwrap();
        let (reader, mut writer) = tokio::io::split(server);
        let mut lines = BufReader::new(reader).lines();
        let first: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["method"], "turn/steer");
        assert_eq!(first["params"]["input"][0]["text"], "first");
        emit(
            &events,
            &Some("another".into()),
            json!({"type":"response.output_text.delta","delta":"still flowing"}),
        )
        .await
        .unwrap();
        assert!(
            matches!(receiver.recv().await, Some(Signal::Event(Some(lane), _)) if lane == "another")
        );
        writer
            .write_all(format!("{}\n", json!({"id":first["id"],"result":{}})).as_bytes())
            .await
            .unwrap();
        let second: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(second["params"]["input"][0]["text"], "second");
        cancel.cancel();
        worker.await.unwrap();
        rpc.close();
    }
    #[test]
    fn steering_memory_is_bounded_and_released_on_shutdown() {
        let (events, _) = mpsc::channel(1);
        let (queue, worker) = SteeringQueue::new(events, CancellationToken::new());
        let control = Arc::new(Control::default());
        queue
            .enqueue(control.clone(), None, Value::Null, MAX_BYTES)
            .unwrap();
        assert!(queue.enqueue(control, None, Value::Null, 1).is_err());
        let budget = queue.budget.clone();
        drop(queue);
        drop(worker);
        assert_eq!(budget.available_permits(), MAX_BYTES);
    }
}
