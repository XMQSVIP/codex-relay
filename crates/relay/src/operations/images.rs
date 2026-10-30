//! Images use the same admission, key quotas and journal as text requests.
use super::{ClientAccess, Lease, Operations, storage};
use crate::{
    error::{Error, Result},
    execution::RequestContext,
    images::ImageResponse,
};
use rusqlite::params;
use serde_json::json;

impl Operations {
    pub(crate) fn record_client_rejection(
        &self,
        context: &RequestContext,
        access: &ClientAccess,
        action: &str,
        model: Option<&str>,
        error: &Error,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let now = crate::now_ms() as i64;
        let saved = inner.db.execute("INSERT OR IGNORE INTO requests(id,started_at,finished_at,kind,action,group_id,status,duration_ms,error_code,key_id,key_name,model,user_agent,streaming,subagent_kind,endpoint,client_transport) VALUES(?1,?2,?2,'inference',?3,?4,'rejected',0,?5,?6,(SELECT name FROM api_keys WHERE id=?6),?7,?8,0,?9,?10,'http')",
            params![context.id.to_string(),now,action,access.group_id.to_string(),error.code,access.key_id.to_string(),model,context.user_agent,context.subagent_kind,context.endpoint]);
        if saved.is_err() {
            inner.storage_healthy = false;
        }
        saved.map_err(storage)?;
        drop(inner);
        let (status, body) = crate::client_api::client_error_payload(error);
        self.record_failure_content(context.id, None, &body, "http", Some(status.as_u16()))
    }
}
impl Lease {
    pub(crate) fn finish_image(mut self, result: &Result<ImageResponse>) -> Result<()> {
        let error = match result {
            Ok(response) => response.error(),
            Err(error) => Some(error.clone()),
        };
        let usage = result
            .as_ref()
            .ok()
            .filter(|response| response.status.is_success())
            .map(|r| &r.value["usage"]);
        let count = |name: &str| {
            usage
                .and_then(|u| u[name].as_u64())
                .and_then(|n| i64::try_from(n).ok())
        };
        {
            let mut inner = self.owner.inner.lock().unwrap();
            let saved = inner.db.execute("UPDATE requests SET input_tokens=?2,output_tokens=?3,model_outcome=?4,failure_stage=?5,delivery_status='terminal_pending' WHERE id=?1",
                params![self.id.to_string(),count("input_tokens"),count("output_tokens"),if error.is_some(){"failed"}else{"completed"},error.as_ref().map(|_| "image_http")]);
            if saved.is_err() {
                inner.storage_healthy = false;
            }
            saved.map_err(storage)?;
        }
        let status = if error
            .as_ref()
            .is_some_and(|e| crate::execution::cancelled(&e.code))
        {
            "cancelled"
        } else if error.is_some() {
            "failed"
        } else {
            "succeeded"
        };
        self.settle(status, error.as_ref().map(|e| e.code.as_str()))?;
        if let Some(error) = error {
            let (status, body) = match result {
                Ok(response) => (response.status.as_u16(), response.value.clone()),
                Err(_) => {
                    let (status, body) = crate::client_api::client_error_payload(&error);
                    (status.as_u16(), body)
                }
            };
            self.owner.record_failure_content(self.id, Some(&json!({"body":{"input":"[已省略图片与提示词]"},"textOmitted":true,"truncated":false})), &body, "http", Some(status))?;
        }
        Ok(())
    }
}
