use super::{Operations, storage};
use crate::error::Result;
use rusqlite::params;
use serde_json::{Value, json};
use uuid::Uuid;

impl Operations {
    pub(crate) fn debug_settings(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        json!({"enabled":inner.debug_enabled,"revision":inner.debug_epoch})
    }

    pub(crate) fn set_debug(&self, enabled: bool, revision: u64) -> Result<Value> {
        let mut inner = self.inner.lock().unwrap();
        if inner.debug_epoch != revision {
            return Err(crate::error::Error::new(
                "REVISION_CONFLICT",
                "调试设置已变化，请刷新后重试。",
            ));
        }
        if !enabled {
            // Serialized with every writer, including completion of old requests.
            inner
                .db
                .execute(
                    "UPDATE requests SET request_content=NULL,response_content=NULL",
                    [],
                )
                .map_err(storage)?;
        }
        inner.debug_epoch += 1;
        inner.debug_enabled = enabled;
        Ok(json!({"enabled":enabled,"revision":inner.debug_epoch}))
    }

    pub(crate) fn debug_epoch(&self) -> Option<u64> {
        let inner = self.inner.lock().unwrap();
        inner.debug_enabled.then_some(inner.debug_epoch)
    }

    pub(crate) fn save_debug_content(
        &self,
        id: Uuid,
        epoch: u64,
        request: &Value,
        response: &Value,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.debug_enabled || inner.debug_epoch != epoch {
            return;
        }
        if inner
            .db
            .execute(
                "UPDATE requests SET request_content=?1,response_content=?2 WHERE id=?3",
                params![request.to_string(), response.to_string(), id.to_string()],
            )
            .is_err()
        {
            inner.storage_healthy = false;
        }
    }
}
