//! Match the client transcript to an owned native context, even after compaction.
use crate::error::Result;
use serde_json::Value;

pub(in crate::responses) struct Context {
    checkpoint: Option<[u8; 32]>,
    pending: Option<[u8; 32]>,
    valid: bool,
}

impl Context {
    pub fn new(checkpoint: Option<[u8; 32]>) -> Self {
        Self {
            checkpoint,
            pending: None,
            valid: true,
        }
    }

    // The caller has already verified thread and turn ownership. No native
    // replacement history is read, copied, or injected back into this thread.
    pub fn observe(&mut self, method: &str, params: &Value, pending_calls: bool) -> Result<()> {
        if method == "thread/compacted" {
            self.valid &=
                !pending_calls && params["turnId"].as_str().is_some_and(|id| !id.is_empty());
            // A deprecated notification cannot finish a canonical item that is
            // still running. Wait for its matching item/completed notification.
            if self.pending.is_none() {
                self.checkpoint = Some(super::digest(&(
                    self.checkpoint,
                    params["turnId"].as_str(),
                ))?);
            }
        } else if params["item"]["type"] == "contextCompaction"
            && matches!(method, "item/started" | "item/completed")
        {
            self.valid &=
                !pending_calls && params["turnId"].as_str().is_some_and(|id| !id.is_empty());
            let Some(id) = params["item"]["id"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 256)
            else {
                self.valid = false;
                return Ok(());
            };
            let id = super::digest(&(params["turnId"].as_str(), id))?;
            if method == "item/started" {
                self.valid &= self.pending.is_none();
                self.pending = Some(id);
            } else if self.pending == Some(id) {
                self.pending = None;
                self.checkpoint = Some(super::digest(&(self.checkpoint, id))?);
            } else {
                self.valid = false;
            }
        }
        Ok(())
    }

    pub fn reusable(&self) -> bool {
        self.valid
            && self.pending.is_none()
            && (crate::native_cache::enabled() || self.checkpoint.is_none())
    }

    pub fn checkpoint(&self) -> Option<[u8; 32]> {
        self.checkpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str) -> Value {
        json!({"threadId":"thread","turnId":"turn","item":{"type":"contextCompaction","id":id}})
    }

    #[test]
    fn only_confirmed_compaction_can_checkpoint_the_client_ledger() {
        let mut context = Context::new(None);
        assert!(context.reusable());
        context
            .observe("item/started", &item("one"), false)
            .unwrap();
        assert!(!context.reusable());
        context
            .observe("thread/compacted", &json!({"turnId":"turn"}), false)
            .unwrap();
        assert!(
            !context.reusable(),
            "deprecated notification cannot finish a pending item"
        );
        context
            .observe("item/completed", &item("one"), false)
            .unwrap();
        assert_eq!(context.reusable(), crate::native_cache::enabled());
        let first = context.checkpoint().unwrap();
        let mut next = Context::new(Some(first));
        next.observe("item/started", &item("two"), false).unwrap();
        next.observe("item/completed", &item("two"), false).unwrap();
        assert_ne!(next.checkpoint(), Some(first));
        assert_eq!(next.reusable(), crate::native_cache::enabled());
    }

    #[test]
    fn mismatched_missing_or_overlapping_compactions_cannot_authorize_reuse() {
        for (started, ended, pending_calls) in [
            (Some("one"), "other", false),
            (None, "one", false),
            (Some(""), "", false),
            (Some("one"), "one", true),
        ] {
            let mut context = Context::new(None);
            if let Some(id) = started {
                context
                    .observe("item/started", &item(id), pending_calls)
                    .unwrap();
            }
            context
                .observe("item/completed", &item(ended), pending_calls)
                .unwrap();
            assert!(!context.reusable());
        }
        let mut context = Context::new(None);
        context
            .observe("item/started", &item("one"), false)
            .unwrap();
        context
            .observe("item/started", &item("two"), false)
            .unwrap();
        context
            .observe("item/completed", &item("two"), false)
            .unwrap();
        assert!(!context.reusable());
        let mut context = Context::new(None);
        let mut missing_turn = item("no-turn");
        missing_turn.as_object_mut().unwrap().remove("turnId");
        context
            .observe("item/started", &missing_turn, false)
            .unwrap();
        context
            .observe("item/completed", &missing_turn, false)
            .unwrap();
        assert!(!context.reusable());
    }
}
