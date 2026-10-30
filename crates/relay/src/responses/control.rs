use crate::{
    error::{Error, Result},
    rpc::Rpc,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
pub(super) struct Control {
    active: Mutex<Option<(Rpc, String, String)>>,
    history: Mutex<Vec<Value>>,
    base: Mutex<Vec<Value>>,
    interrupt_requested: AtomicBool,
    interrupt_accepted: AtomicBool,
    operation: tokio::sync::Mutex<()>,
    tracker: Mutex<Option<crate::profiles::resources::Tracker>>,
    authorization: Mutex<
        Option<(
            Arc<crate::operations::Operations>,
            crate::operations::ClientAccess,
        )>,
    >,
}

impl Control {
    pub(super) fn set_authorization(
        &self,
        ops: Arc<crate::operations::Operations>,
        access: crate::operations::ClientAccess,
    ) {
        *self.authorization.lock().unwrap() = Some((ops, access));
    }
    pub(super) fn check_authorization(&self) -> Result<()> {
        if let Some((ops, access)) = self.authorization.lock().unwrap().as_ref() {
            ops.revalidate_api_key(access)?;
        }
        Ok(())
    }
    pub(super) fn set_tracker(&self, tracker: Option<crate::profiles::resources::Tracker>) {
        *self.tracker.lock().unwrap() = tracker;
    }
    pub fn prepare(&self, history: &[Value]) {
        *self.base.lock().unwrap() = history.to_vec();
    }
    pub fn activate(&self, rpc: &Rpc, thread: &str, turn: &str) {
        *self.active.lock().unwrap() = Some((rpc.clone(), thread.into(), turn.into()));
    }
    pub fn finish(&self) {
        self.active.lock().unwrap().take();
    }
    pub fn additions(&self) -> Vec<Value> {
        self.history.lock().unwrap().clone()
    }
    pub fn cache_reusable(&self) -> bool {
        // Called after finish(): no new steering can acquire an active turn.
        // An outstanding steer may already have reached native before its
        // history acknowledgement, so it also prevents prefix reuse.
        self.operation.try_lock().is_ok() && self.history.lock().unwrap().is_empty()
    }
    pub fn interrupt_requested(&self) -> bool {
        self.interrupt_requested.load(Ordering::SeqCst)
    }
    pub async fn interrupt(&self) -> Result<()> {
        let _operation = self.operation.lock().await;
        if self.interrupt_accepted.load(Ordering::SeqCst) {
            return Ok(());
        }
        let (rpc, thread, turn) = self
            .active
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::new("RESPONSE_NOT_ACTIVE", "响应已经结束或尚未开始。"))?;
        self.interrupt_requested.store(true, Ordering::SeqCst);
        let result = rpc
            .request_timeout(
                "turn/interrupt",
                json!({"threadId":thread,"turnId":turn}),
                std::time::Duration::from_secs(10),
            )
            .await;
        if result.is_ok() {
            self.interrupt_accepted.store(true, Ordering::SeqCst);
        } else {
            self.interrupt_requested.store(false, Ordering::SeqCst);
        }
        result.map(|_| ())
    }
    pub async fn steer(&self, input: Value) -> Result<()> {
        let _operation = self.operation.lock().await;
        self.check_authorization()?;
        if self.interrupt_requested() {
            return Err(Error::new(
                "RESPONSE_NOT_ACTIVE",
                "响应正在中断，请在下一轮提交输入。",
            ));
        }
        let items = super::request::normalize_input(&input)?;
        let mut native = vec![];
        for item in &items {
            if item["type"] != "message" || item["role"] != "user" {
                return Err(super::invalid("追加输入仅接受 user 消息。"));
            }
            for part in item["content"].as_array().unwrap() {
                match part["type"].as_str() {
                    Some("input_text") => {
                        native.push(json!({"type":"text","text":part["text"],"text_elements":[]}))
                    }
                    // Media requires the route capability check done at admission; a
                    // mid-turn request cannot silently broaden that route's permissions.
                    _ => return Err(super::invalid("在途追加图片请在下一轮提交。")),
                }
            }
        }
        let (rpc, thread, turn) = self
            .active
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::new("RESPONSE_NOT_ACTIVE", "响应尚未开始或已经结束。"))?;
        let mut all = self.base.lock().unwrap().clone();
        all.extend(self.additions());
        all.extend(items.clone());
        super::request::validate_history(&all)?;
        crate::media::budget(&json!(all), crate::media::BODY_LIMIT)?;
        if let Some(tracker) = self.tracker.lock().unwrap().as_ref() {
            tracker.reserve_input(serde_json::to_vec(&native)?.len())?;
        }
        rpc.request(
            "turn/steer",
            json!({"threadId":thread,"expectedTurnId":turn,"input":native}),
        )
        .await?;
        self.history.lock().unwrap().extend(items);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{ApiKeyGroup, DEFAULT_GROUP};
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn steering_waiting_for_control_lock_rechecks_the_admitted_group() {
        let root = tempfile::tempdir().unwrap();
        let ops = crate::operations::Operations::open(root.path()).unwrap();
        let key = ops
            .create_api_key(
                serde_json::from_value(json!({"name":"steer owner","groupId":DEFAULT_GROUP}))
                    .unwrap(),
            )
            .unwrap();
        let access = ops.authenticate_api_key(&key.secret).unwrap();
        let group = ops
            .save_group(
                None,
                serde_json::from_value(json!({
                    "revision":ops.snapshot()["revision"],"name":"new group","enabled":true
                }))
                .unwrap(),
            )
            .unwrap();
        let (client, peer) = tokio::io::duplex(1024);
        let (reader, writer) = tokio::io::split(client);
        let rpc = Rpc::from_io(reader, writer);
        let control = Arc::new(Control::default());
        control.activate(&rpc, "thread", "turn");
        control.set_authorization(ops.clone(), access.clone());
        let held = control.operation.lock().await;
        let pending = {
            let control = control.clone();
            tokio::spawn(async move { control.steer(json!("late input")).await })
        };
        ops.set_api_key_group(
            access.key_id,
            ApiKeyGroup {
                revision: 1,
                group_id: group,
            },
        )
        .unwrap();
        drop(held);
        assert_eq!(pending.await.unwrap().unwrap_err().code, "INVALID_API_KEY");
        let mut lines = BufReader::new(peer).lines();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), lines.next_line())
                .await
                .is_err()
        );
        rpc.close();
    }
}
