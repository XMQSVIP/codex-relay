//! Route selection and atomic admission into active or queued work.

use super::{
    AccountPolicy, Active, ClientAccess, DESKTOP_ACCOUNT, Entry, Feature, Group, Inner, Lease,
    Operations, Pending, Selection, Waiter, api_keys, runtime, storage,
};
use crate::error::{Error, Result};
use rusqlite::params;
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[cfg(test)]
mod tests;

#[derive(Clone, Default)]
pub struct RoutingRequest {
    pub id: Uuid,
    pub model: String,
    pub streaming: bool,
    pub endpoint: Option<String>,
    pub client_transport: Option<String>,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub user_agent: Option<String>,
    pub subagent_kind: Option<String>,
    pub features: Vec<Feature>,
    pub class: super::RequestClass,
    pub soft_affinity: Option<[u8; 32]>,
    pub parent_affinity: Option<[u8; 32]>,
}

impl Operations {
    pub(super) fn active_count(inner: &Inner, account: Option<Uuid>, group: Option<Uuid>) -> usize {
        inner
            .active
            .values()
            .filter(|a| {
                a.limited
                    && account.is_none_or(|id| a.account == Some(id))
                    && group.is_none_or(|id| a.group == Some(id))
            })
            .count()
    }
    fn denial(
        inner: &Inner,
        account: Option<&AccountPolicy>,
        group: Option<&Group>,
        maintenance: bool,
        exclusive: bool,
    ) -> Option<Error> {
        if account.is_some_and(|a| inner.proxy_changes.contains_key(&a.id)) {
            return Some(Error::new(
                "CONCURRENCY_LIMIT",
                "账户代理正在切换，等待恢复。",
            ));
        }
        if !maintenance
            && (account.is_some_and(|a| !a.enabled) || group.is_some_and(|g| !g.enabled))
        {
            return Some(Error::new("ROUTE_DISABLED", "账户或分组已暂停。"));
        }
        // Keep one administrative slot before any accounts exist. Client requests
        // still require an eligible account and its own capacity.
        if Self::active_count(inner, None, None)
            >= inner.settings.effective_global_limit().max(1) as usize
        {
            return Some(Error::new("CONCURRENCY_LIMIT", "已达到全局并发上限。"));
        }
        // 登录、凭据导入和实例维护改变整个账户；它们与客户端推理互斥，不能按普通并发叠加。
        if account.is_some_and(|a| {
            inner.active.values().any(|active| {
                active.limited && active.account == Some(a.id) && (exclusive || active.exclusive)
            })
        }) {
            return Some(Error::new(
                "CONCURRENCY_LIMIT",
                "等待账户维护或现有请求完成。",
            ));
        }
        if account.is_some_and(|a| {
            Self::active_count(inner, Some(a.id), None) >= a.max_concurrency as usize
        }) {
            return Some(Error::new("CONCURRENCY_LIMIT", "已达到账户并发上限。"));
        }
        None
    }
    pub(super) fn select(
        inner: &Inner,
        account_id: Option<Uuid>,
        group_id: Option<Uuid>,
        limited: bool,
        client: Option<&ClientAccess>,
        routing: Option<&RoutingRequest>,
        maintenance: bool,
    ) -> Result<Selection> {
        let key = client
            .map(|access| api_keys::validate_access(inner, access))
            .transpose()?;
        if key.as_ref().is_some_and(|k| Some(k.group_id) != group_id) {
            return Err(Error::new("ROUTE_CHANGED", "请求不属于密钥授权分组。"));
        }
        if let Some(request) = routing {
            if !client.is_some_and(|c| c.allows(&request.model)) {
                return Err(Error::new("MODEL_NOT_ALLOWED", "此密钥未授权该模型。"));
            }
        }
        let mut account = account_id
            .map(|id| {
                inner
                    .settings
                    .accounts
                    .iter()
                    .find(|a| a.id == id)
                    .cloned()
                    .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))
            })
            .transpose()?;
        if account.as_ref().is_some_and(|a| a.deleting) {
            return Err(Error::new("BUSY", "账户正在删除，不能启动新操作。"));
        }
        let group = if let Some(id) = group_id.or_else(|| {
            account.as_ref().map(|a| {
                inner
                    .settings
                    .groups
                    .iter()
                    .find(|g| a.belongs_to(g.id) && g.enabled)
                    .map_or(a.groups()[0], |g| g.id)
            })
        }) {
            Some(
                inner
                    .settings
                    .groups
                    .iter()
                    .find(|g| g.id == id)
                    .cloned()
                    .ok_or_else(|| Error::new("NOT_FOUND", "分组不存在。"))?,
            )
        } else {
            None
        };
        if let (Some(account), Some(group_id)) = (&account, group_id)
            && !account.belongs_to(group_id)
        {
            return Err(Error::new("ROUTE_CHANGED", "账户已不属于请求授权的分组。"));
        }
        let mut rejection = None;
        let mut scheduling = account
            .as_ref()
            .and(routing)
            .map(|_| super::scheduler::Decision::bound());
        if let (Some(account), Some(request)) = (&account, routing) {
            let disabled = runtime::disabled_requirements(account, request);
            if !disabled.is_empty() {
                return Err(Error::new(
                    "CAPABILITY_UNAVAILABLE",
                    &format!(
                        "响应链绑定账户已手动关闭请求能力：{}。请在账户的高级能力中重新允许，或从请求中移除对应能力。",
                        disabled.join("、")
                    ),
                ));
            }
        }
        if account.is_none() && group_id.is_some() {
            let g = group.as_ref().unwrap();
            let candidates: Vec<_> = inner
                .settings
                .accounts
                .iter()
                .filter(|a| a.belongs_to(g.id) && a.enabled && !a.deleting)
                .filter(|a| {
                    routing.is_none_or(|r| {
                        a.id != DESKTOP_ACCOUNT
                            && a.route.is_some()
                            && runtime::requirements(inner, a, r)
                    })
                })
                .cloned()
                .collect();
            let start = inner
                .cursor
                .get(&g.id)
                .and_then(|last| candidates.iter().position(|a| a.id == *last))
                .map_or(0, |i| i + 1);
            let available: Vec<_> = (0..candidates.len())
                .map(|offset| &candidates[(start + offset) % candidates.len()])
                .filter(|a| {
                    Self::denial(inner, Some(a), Some(g), maintenance, client.is_none()).is_none()
                        && (routing.is_none() || runtime::available(inner, a))
                })
                .collect();
            if let (Some(request), Some(access)) = (routing, client) {
                if let Some((chosen, decision)) =
                    super::scheduler::choose(inner, &available, access, request)
                {
                    account = Some(chosen);
                    scheduling = Some(decision);
                }
            } else {
                account = available.first().map(|a| (*a).clone());
            }
            if account.is_none() {
                rejection = if !g.enabled {
                    Some(Error::new("ROUTE_DISABLED", "账户或分组已暂停。"))
                } else if candidates.is_empty() {
                    Some(
                        if let Some(request) = routing.filter(|_| {
                            inner.settings.accounts.iter().any(|a| {
                                a.belongs_to(g.id)
                                    && a.id != DESKTOP_ACCOUNT
                                    && a.enabled
                                    && !a.deleting
                                    && a.route.is_some()
                            })
                        }) {
                            let disabled = inner
                                .settings
                                .accounts
                                .iter()
                                .filter(|a| {
                                    a.belongs_to(g.id)
                                        && a.id != DESKTOP_ACCOUNT
                                        && a.enabled
                                        && !a.deleting
                                        && a.route.is_some()
                                })
                                .flat_map(|a| runtime::disabled_requirements(a, request))
                                .collect::<std::collections::BTreeSet<_>>()
                                .into_iter()
                                .collect::<Vec<_>>();
                            Error::new(
                                "CAPABILITY_UNAVAILABLE",
                                &format!(
                                    "分组内可用账户的手动策略阻止了请求能力：{}。请在账户的高级能力中重新允许，或从请求中移除对应能力；无需运行能力验证。",
                                    disabled.join("、")
                                ),
                            )
                        } else if routing.is_some() {
                            Error::new("INFERENCE_DISABLED", "此分组没有可用的客户端模型路由。")
                        } else {
                            Error::new("NO_ELIGIBLE_ACCOUNT", "分组没有启用的账户。")
                        },
                    )
                } else {
                    Self::denial(inner, None, Some(g), maintenance, client.is_none())
                        .or_else(|| Some(Error::new("CONCURRENCY_LIMIT", "组内账户并发已满。")))
                };
            }
        }
        if limited && rejection.is_none() {
            rejection = Self::denial(
                inner,
                account.as_ref(),
                group.as_ref(),
                maintenance,
                client.is_none(),
            );
        }
        if client.is_some()
            && account
                .as_ref()
                .is_some_and(|a| !runtime::available(inner, a))
            && rejection.is_none()
        {
            rejection = Some(Error::new("CONCURRENCY_LIMIT", "账户暂时受限，等待恢复。"));
        }
        if limited
            && rejection.is_none()
            && key.as_ref().is_some_and(|key| {
                key.max_concurrency > 0
                    && inner
                        .active
                        .values()
                        .filter(|active| active.key_id == Some(key.id))
                        .count()
                        >= key.max_concurrency as usize
            })
        {
            rejection = Some(Error::new(
                "CONCURRENCY_LIMIT",
                "已达到此 API Key 的并发上限。",
            ));
        }
        Ok(Selection {
            account,
            group,
            rejection,
            scheduling,
        })
    }
    pub(super) fn first_eligible(inner: &Inner) -> Option<Uuid> {
        let now = tokio::time::Instant::now();
        inner
            .pending
            .iter()
            .find(|p| {
                p.deadline > now
                    && Self::select(
                        inner,
                        p.account,
                        p.group,
                        true,
                        p.client.as_ref(),
                        p.routing.as_ref(),
                        false,
                    )
                    .is_ok_and(|s| s.rejection.is_none())
            })
            .map(|p| p.id)
    }
    pub(super) fn queued_count(inner: &Inner, account: Option<Uuid>, group: Option<Uuid>) -> usize {
        inner
            .pending
            .iter()
            .filter(|p| {
                account.is_none_or(|id| p.account == Some(id))
                    && group.is_none_or(|id| p.group == Some(id))
            })
            .count()
    }
    // Non-waiting admission is reserved for maintenance and synchronous callers.
    pub fn begin(
        self: &Arc<Self>,
        account_id: Option<Uuid>,
        group_id: Option<Uuid>,
        kind: &str,
        action: &str,
        limited: bool,
    ) -> Result<Lease> {
        match self.enter(
            account_id,
            group_id,
            kind,
            action,
            (limited, false, false),
            None,
            None,
        )? {
            Entry::Ready(lease) => Ok(lease),
            Entry::Waiting(_) => unreachable!("waiting was disabled"),
        }
    }
    // Explicit maintenance of a paused account still reserves execution capacity,
    // so resuming it cannot admit a client request on top of that operation.
    pub(crate) fn begin_account_maintenance(
        self: &Arc<Self>,
        account: Uuid,
        kind: &str,
        action: &str,
    ) -> Result<Lease> {
        match self.enter(
            Some(account),
            None,
            kind,
            action,
            (true, false, true),
            None,
            None,
        )? {
            Entry::Ready(lease) => Ok(lease),
            Entry::Waiting(_) => unreachable!("maintenance never queues"),
        }
    }
    pub async fn acquire(
        self: &Arc<Self>,
        account_id: Option<Uuid>,
        group_id: Option<Uuid>,
        kind: &str,
        action: &str,
    ) -> Result<Lease> {
        match self.enter(
            account_id,
            group_id,
            kind,
            action,
            (true, true, false),
            None,
            None,
        )? {
            Entry::Ready(lease) => Ok(lease),
            Entry::Waiting(waiter) => waiter.wait().await,
        }
    }
    pub async fn acquire_client(
        self: &Arc<Self>,
        account: Uuid,
        access: &ClientAccess,
    ) -> Result<Lease> {
        match self.enter(
            Some(account),
            Some(access.group_id),
            "inference",
            "responses",
            (true, true, false),
            Some(access.clone()),
            None,
        )? {
            Entry::Ready(lease) => Ok(lease),
            Entry::Waiting(waiter) => waiter.wait().await,
        }
    }
    pub async fn acquire_inference(
        self: &Arc<Self>,
        access: &ClientAccess,
        request: RoutingRequest,
    ) -> Result<Lease> {
        self.acquire_inference_on(access, request, None).await
    }
    pub(crate) async fn acquire_inference_on(
        self: &Arc<Self>,
        access: &ClientAccess,
        request: RoutingRequest,
        account: Option<Uuid>,
    ) -> Result<Lease> {
        if request.reasoning_effort.as_deref().is_some_and(|effort| {
            ![
                "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
            ]
            .contains(&effort)
        }) {
            return Err(Error::new("INVALID_INPUT", "推理强度无效。"));
        }
        if request.model.is_empty()
            || request.model.len() > 128
            || !request
                .model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        {
            return Err(Error::new("INVALID_INPUT", "模型 ID 格式无效。"));
        }
        let action = match request.class {
            super::RequestClass::ImageGeneration => "images.generations",
            super::RequestClass::ImageEdit => "images.edits",
            super::RequestClass::Search => "web.search",
            _ => "responses",
        };
        match self.enter(
            account,
            Some(access.group_id),
            "inference",
            action,
            (true, true, false),
            Some(access.clone()),
            Some(request),
        )? {
            Entry::Ready(lease) => Ok(lease),
            Entry::Waiting(waiter) => waiter.wait().await,
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn enter(
        self: &Arc<Self>,
        account_id: Option<Uuid>,
        group_id: Option<Uuid>,
        kind: &str,
        action: &str,
        mode: (bool, bool, bool),
        client: Option<ClientAccess>,
        routing: Option<RoutingRequest>,
    ) -> Result<Entry> {
        let (limited, wait, maintenance) = mode;
        let key_id = client.as_ref().map(|access| access.key_id);
        let mut inner = self.inner.lock().unwrap();
        if (limited
            || matches!(
                action,
                "quota_reset" | "quota_reset_auto" | "subscription_refresh"
            ))
            && inner.draining
        {
            return Err(Error::new(
                "SERVICE_DRAINING",
                "服务正在退出，不再接收新的执行请求。",
            ));
        }
        if !inner.storage_healthy {
            return Err(Error::new(
                "OPERATIONS_STORAGE",
                "统计写入曾失败，请检查磁盘并重启服务。",
            ));
        }
        let mut selected = Self::select(
            &inner,
            account_id,
            group_id,
            limited,
            client.as_ref(),
            routing.as_ref(),
            maintenance,
        )?;
        if limited && selected.rejection.is_none() && Self::first_eligible(&inner).is_some() {
            selected.rejection = Some(Error::new(
                "CONCURRENCY_LIMIT",
                "等待先进入队列的可执行请求。",
            ));
        }
        let should_wait = wait
            && selected
                .rejection
                .as_ref()
                .is_some_and(|e| e.code == "CONCURRENCY_LIMIT");
        let queued = should_wait && inner.pending.len() < inner.settings.queue_limit as usize;
        if should_wait && !queued {
            selected.rejection = Some(Error::new("QUEUE_FULL", "等待队列已满，请稍后重试。"));
        }
        if queued {
            selected.rejection = None;
            selected.scheduling = None;
            // A group request chooses its account when capacity actually becomes available.
            if account_id.is_none() {
                selected.account = None;
            }
        }
        let rejection = selected.rejection;
        let scheduling = if rejection.is_none() {
            selected.scheduling
        } else {
            None
        };
        let account = selected.account;
        let group = selected.group;
        let id = routing.as_ref().map_or_else(Uuid::new_v4, |r| r.id);
        let now = crate::now_ms() as i64;
        // Background quota refreshes and startup use non-waiting maintenance
        // admission. Reserve capacity without journaling routine retries.
        // Waiting callers still need a journal row for queue inspection.
        if wait || kind != "management" || !matches!(action, "quota_refresh" | "auto_start") {
            inner.db.execute("INSERT INTO requests(id,started_at,finished_at,kind,action,account_id,account_name,group_id,group_name,status,duration_ms,error_code,wait_ms,key_id,key_name) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,(SELECT name FROM api_keys WHERE id=?14))",params![id.to_string(),now,rejection.as_ref().map(|_|now),kind,action,account.as_ref().map(|a|a.id.to_string()),account.as_ref().map(|a|a.name.clone()),group.as_ref().map(|g|g.id.to_string()),group.as_ref().map(|g|g.name.clone()),if queued {"queued"} else if rejection.is_some(){"rejected"}else{"running"},rejection.as_ref().map(|_|0),rejection.as_ref().map(|e|e.code.clone()),if queued {None} else {Some(0)},key_id.map(|id|id.to_string())]).map_err(storage)?;
        }
        if let Some(request) = &routing {
            inner
                .db
                .execute(
                    "UPDATE requests SET model=?1,reasoning_effort=?3,user_agent=?4,streaming=?5,scheduling=?6,subagent_kind=?7,service_tier=?8,endpoint=?9,client_transport=?10 WHERE id=?2",
                    params![
                        request.model,
                        id.to_string(),
                        request.reasoning_effort.as_deref().unwrap_or("default"),
                        request.user_agent,
                        request.streaming,
                        scheduling.as_ref().map(|d| serde_json::to_string(d).expect("finite scheduling scores")),
                        request.subagent_kind,
                        request.service_tier,
                        request.endpoint,
                        request.client_transport
                    ],
                )
                .map_err(storage)?;
        }
        if let Some(error) = rejection {
            return Err(error);
        }
        let group_id = group.map(|g| g.id);
        if queued {
            let started = Instant::now();
            let cancellation = Arc::new(OnceLock::new());
            let deadline = tokio::time::Instant::now()
                + Duration::from_secs(u64::from(inner.settings.queue_timeout_seconds));
            inner.pending.push_back(Pending {
                routing,
                id,
                account: account_id,
                group: group_id,
                client,
                deadline,
                started,
                cancellation: cancellation.clone(),
            });
            inner.changed.notify_waiters();
            return Ok(Entry::Waiting(Waiter {
                owner: self.clone(),
                id,
                started,
                deadline,
                finished: false,
                cancellation,
            }));
        }
        let provider = account.as_ref().and_then(|a| a.route.clone());
        let account_id = account.map(|a| a.id);
        if let (Some(a), Some(g)) = (account_id, group_id) {
            inner.cursor.insert(g, a);
        }
        if client.is_some() {
            super::quota::claim(&mut inner, account_id, id);
            super::rate_limit::claim(&mut inner, account_id, id);
        }
        if let (Some(access), Some(request), Some(account), Some(decision)) =
            (&client, &routing, account_id, &scheduling)
        {
            super::scheduler::admitted(&mut inner, id, access, request, account, decision);
        }
        inner.active.insert(
            id,
            Active {
                account: account_id,
                group: group_id,
                limited,
                key_id,
                exclusive: limited && client.is_none(),
            },
        );
        Ok(Entry::Ready(Lease {
            provider,
            owner: self.clone(),
            id,
            started: Instant::now(),
            account_id,
            finished: false,
        }))
    }
}
