//! Queue inspection, cancellation, shutdown and waiter claim/drop lifecycle.

use super::{Active, Inner, Lease, Operations, Waiter, storage};
use crate::error::{Error, Result};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueQuery {
    pub group_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
    pub page: Option<u32>,
}

impl Operations {
    pub fn queue(&self, query: QueueQuery) -> Result<Value> {
        let page = query.page.unwrap_or(1);
        if page == 0 || page > 10000 {
            return Err(Error::new("INVALID_INPUT", "队列页码无效。"));
        }
        let inner = self.inner.lock().unwrap();
        let matching: Vec<_> = inner
            .pending
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                query.group_id.is_none_or(|id| p.group == Some(id))
                    && query.account_id.is_none_or(|id| p.account == Some(id))
            })
            .collect();
        let mut records = Vec::new();
        let mut statement = inner
            .db
            .prepare(
                "SELECT started_at,kind,action,account_name,group_name FROM requests WHERE id=?1",
            )
            .map_err(storage)?;
        for (position, pending) in matching.iter().skip((page as usize - 1) * 50).take(50) {
            let record = statement.query_row([pending.id.to_string()], |r| Ok(json!({"id":pending.id,"position":position+1,"startedAt":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"action":r.get::<_,String>(2)?,"accountId":pending.account,"accountName":r.get::<_,Option<String>>(3)?,"groupId":pending.group,"groupName":r.get::<_,Option<String>>(4)?,"waitMs":pending.started.elapsed().as_millis().min(i64::MAX as u128) as i64,"remainingMs":pending.deadline.saturating_duration_since(tokio::time::Instant::now()).as_millis().min(i64::MAX as u128) as i64}))).map_err(storage)?;
            records.push(record);
        }
        Ok(
            json!({"total":matching.len(),"totalQueued":inner.pending.len(),"page":page,"pageSize":50,"records":records}),
        )
    }
    pub fn cancel_queued(&self, id: Uuid) -> Result<Value> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(pending) = inner.pending.iter().find(|p| p.id == id).cloned() {
            let result = inner.db.execute("UPDATE requests SET status='cancelled',finished_at=?1,wait_ms=?2,error_code='REQUEST_CANCELLED' WHERE id=?3 AND status='queued'", params![crate::now_ms() as i64,pending.started.elapsed().as_millis().min(i64::MAX as u128) as i64,id.to_string()]);
            if result.is_err() {
                inner.storage_healthy = false;
                inner.changed.notify_waiters();
            }
            result.map_err(storage)?;
            pending
                .cancellation
                .set(Error::new(
                    "REQUEST_CANCELLED",
                    "排队请求已由管理员取消，未派发原生操作。",
                ))
                .ok();
            inner.pending.retain(|p| p.id != id);
            inner.changed.notify_waiters();
            return Ok(json!({"id":id,"status":"cancelled","alreadyCancelled":false}));
        }
        let status: Option<String> = inner
            .db
            .query_row(
                "SELECT status FROM requests WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        match status.as_deref() {
            Some("cancelled") => Ok(json!({"id":id,"status":"cancelled","alreadyCancelled":true})),
            Some(_) => Err(Error::new(
                "REQUEST_NOT_QUEUED",
                "请求已开始执行或已结束，无法取消排队；请刷新队列。",
            )),
            None => Err(Error::new("NOT_FOUND", "请求不存在。")),
        }
    }
    pub fn start_shutdown(&self) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.draining = true;
        let pending: Vec<_> = inner.pending.iter().cloned().collect();
        let persisted = (|| -> rusqlite::Result<()> {
            let transaction = inner.db.transaction()?;
            for p in &pending {
                transaction.execute("UPDATE requests SET status='cancelled',finished_at=?1,wait_ms=?2,error_code='SERVICE_SHUTDOWN' WHERE id=?3 AND status='queued'",params![crate::now_ms() as i64,p.started.elapsed().as_millis().min(i64::MAX as u128) as i64,p.id.to_string()])?;
            }
            transaction.commit()
        })();
        // Shutdown must release waiters even if the journal is unavailable.
        for p in pending {
            p.cancellation
                .set(Error::new(
                    "SERVICE_SHUTDOWN",
                    "服务正在退出，排队请求未派发。",
                ))
                .ok();
        }
        inner.pending.clear();
        if persisted.is_err() {
            inner.storage_healthy = false;
        }
        inner.changed.notify_waiters();
        persisted.map_err(storage)
    }
}

impl Waiter {
    fn finish_locked(&mut self, inner: &mut Inner, status: &str, error: &str) -> Result<()> {
        inner.pending.retain(|p| p.id != self.id);
        self.finished = true;
        let result = inner.db.execute("UPDATE requests SET status=?1,finished_at=?2,wait_ms=?3,error_code=?4 WHERE id=?5 AND status='queued'", params![status,crate::now_ms() as i64,self.started.elapsed().as_millis().min(i64::MAX as u128) as i64,error,self.id.to_string()]);
        if result.is_err() {
            inner.storage_healthy = false;
        }
        inner.changed.notify_waiters();
        result.map_err(storage)?;
        Ok(())
    }
    fn try_claim(&mut self) -> Result<Option<Lease>> {
        let owner = self.owner.clone();
        let mut inner = owner.inner.lock().unwrap();
        if let Some(error) = self.cancellation.get() {
            self.finished = true;
            return Err(error.clone());
        }
        let pending = inner
            .pending
            .iter()
            .find(|p| p.id == self.id)
            .cloned()
            .ok_or_else(|| Error::new("QUEUE_STATE", "等待请求已结束。"))?;
        let selected = Operations::select(
            &inner,
            pending.account,
            pending.group,
            true,
            pending.client.as_ref(),
            pending.routing.as_ref(),
            false,
        );
        let error = if !inner.storage_healthy {
            Some(Error::new(
                "OPERATIONS_STORAGE",
                "统计写入失败，请检查磁盘并重启服务。",
            ))
        } else if tokio::time::Instant::now() >= self.deadline {
            Some(Error::new(
                "QUEUE_TIMEOUT",
                "等待并发额度超时，请稍后重试。",
            ))
        } else {
            match &selected {
                Err(e) => Some(e.clone()),
                Ok(s) => s
                    .rejection
                    .as_ref()
                    .filter(|e| e.code != "CONCURRENCY_LIMIT")
                    .cloned(),
            }
        };
        if let Some(error) = error {
            self.finish_locked(
                &mut inner,
                if error.code == "QUEUE_TIMEOUT" {
                    "timed_out"
                } else {
                    "rejected"
                },
                &error.code,
            )?;
            return Err(error);
        }
        let selected = selected?;
        // A capacity-blocked waiter cannot claim anything, regardless of queue
        // order. Avoid revalidating/routing the entire queue on every wakeup.
        // Errors and deadlines above must still be processed while saturated.
        if selected.rejection.is_some() || Operations::first_eligible(&inner) != Some(self.id) {
            return Ok(None);
        }
        let provider = selected.account.as_ref().and_then(|a| a.route.clone());
        let account_id = selected.account.as_ref().map(|a| a.id);
        let group_id = selected.group.as_ref().map(|g| g.id);
        let result = inner.db.execute("UPDATE requests SET status='running',wait_ms=?1,account_id=?2,account_name=?3,scheduling=?5 WHERE id=?4 AND status='queued'", params![self.started.elapsed().as_millis().min(i64::MAX as u128) as i64,account_id.map(|id|id.to_string()),selected.account.map(|a|a.name),self.id.to_string(),selected.scheduling.as_ref().map(|d|serde_json::to_string(d).expect("finite scheduling scores"))]);
        if let Err(e) = result {
            inner.storage_healthy = false;
            return Err(storage(e));
        }
        inner.pending.retain(|p| p.id != self.id);
        if let (Some(a), Some(g)) = (account_id, group_id) {
            inner.cursor.insert(g, a);
        }
        if pending.client.is_some() {
            super::quota::claim(&mut inner, account_id, self.id);
            super::rate_limit::claim(&mut inner, account_id, self.id);
        }
        if let (Some(access), Some(request), Some(account), Some(decision)) = (
            &pending.client,
            &pending.routing,
            account_id,
            &selected.scheduling,
        ) {
            super::scheduler::admitted(&mut inner, self.id, access, request, account, decision);
        }
        inner.active.insert(
            self.id,
            Active {
                account: account_id,
                group: group_id,
                limited: true,
                key_id: pending.client.as_ref().map(|access| access.key_id),
                exclusive: pending.client.is_none(),
            },
        );
        self.finished = true;
        inner.changed.notify_waiters();
        Ok(Some(Lease {
            provider,
            owner: owner.clone(),
            id: self.id,
            started: Instant::now(),
            account_id,
            finished: false,
        }))
    }
    pub(super) async fn wait(mut self) -> Result<Lease> {
        loop {
            let changed = self.owner.changed.clone();
            let notified = changed.notified();
            tokio::pin!(notified);
            // Register before inspecting capacity so completion cannot be lost between check and await.
            notified.as_mut().enable();
            if let Some(lease) = self.try_claim()? {
                return Ok(lease);
            }
            tokio::select! { _ = notified => {}, _ = tokio::time::sleep_until(self.deadline.min(tokio::time::Instant::now()+Duration::from_millis(250))) => {} }
        }
    }
}
impl Drop for Waiter {
    fn drop(&mut self) {
        if !self.finished && self.cancellation.get().is_none() {
            let owner = self.owner.clone();
            let mut inner = owner.inner.lock().unwrap();
            let _ = self.finish_locked(&mut inner, "cancelled", "REQUEST_CANCELLED");
        }
    }
}
