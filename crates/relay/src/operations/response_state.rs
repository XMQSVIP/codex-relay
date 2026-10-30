//! Response routing fingerprints share the operations database and authorization lock.
use super::{Operations, storage};
use crate::error::Result;
use rusqlite::{Connection, params};
use serde_json::Value;

impl Operations {
    pub(crate) fn with_response_db<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        work(&mut self.inner.lock().unwrap().db)
    }
    pub(crate) fn record_parameter_handling(&self, id: uuid::Uuid, notes: &[Value]) -> Result<()> {
        self.with_response_db(|db| {
            db.execute(
                "UPDATE requests SET parameter_handling=?1 WHERE id=?2",
                params![serde_json::to_string(notes)?, id.to_string()],
            )
            .map_err(storage)?;
            Ok(())
        })
    }
}
