//! Account/group policies, revisioned updates and the management snapshot.

use super::{Feature, Inner, Operations, api_keys, runtime, storage};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

fn default_queue_limit() -> u32 {
    64
}
fn default_queue_timeout() -> u32 {
    30
}
fn default_retention_days() -> u32 {
    30
}
fn default_native_affinity_ttl_hours() -> u32 {
    24
}

pub const DEFAULT_ACCOUNT_CONCURRENCY: u32 = 5;
pub const DEFAULT_GROUP: Uuid = Uuid::nil();
pub const DESKTOP_ACCOUNT: Uuid = Uuid::from_u128(1);
pub(super) fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 60 && !name.chars().any(char::is_control)
}
fn valid_limit(limit: u32) -> bool {
    (1..=256).contains(&limit)
}

fn account_groups<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<Uuid>, D::Error> {
    let groups = Vec::<Uuid>::deserialize(deserializer)?;
    if groups.is_empty() || groups.len() > 64 {
        return Err(serde::de::Error::custom("账户必须属于 1–64 个分组。"));
    }
    Ok(groups)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountPolicy {
    pub id: Uuid,
    pub name: String,
    #[serde(deserialize_with = "account_groups")]
    pub group_ids: Vec<Uuid>,
    pub enabled: bool,
    pub max_concurrency: u32,
    pub proxy: crate::app_server_proxy::ProxyConfig,
    pub location: crate::account_location::Config,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<crate::provider::Provider>,
    pub deleting: bool,
    pub disabled_features: Vec<Feature>,
}
impl AccountPolicy {
    pub fn groups(&self) -> &[Uuid] {
        &self.group_ids
    }
    pub fn belongs_to(&self, group: Uuid) -> bool {
        self.groups().contains(&group)
    }
    pub(crate) fn allowed_features(&self) -> Vec<Feature> {
        std::iter::once(Feature::NamespacedTools)
            .chain(
                Feature::OPTIONAL
                    .into_iter()
                    .filter(|f| !self.disabled_features.contains(f)),
            )
            .collect()
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub revision: u64,
    /// Remember the manual limit even while automatic capacity is selected.
    pub global_limit: u32,
    pub global_limit_auto: bool,
    pub queue_limit: u32,
    pub queue_timeout_seconds: u32,
    pub native_affinity_ttl_hours: u32,
    pub retention_days: u32,
    pub groups: Vec<Group>,
    pub accounts: Vec<AccountPolicy>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            revision: 1,
            global_limit: DEFAULT_ACCOUNT_CONCURRENCY,
            global_limit_auto: true,
            queue_limit: default_queue_limit(),
            queue_timeout_seconds: default_queue_timeout(),
            native_affinity_ttl_hours: default_native_affinity_ttl_hours(),
            retention_days: default_retention_days(),
            groups: vec![Group {
                id: DEFAULT_GROUP,
                name: "默认分组".into(),
                enabled: true,
            }],
            accounts: vec![],
        }
    }
}
impl Settings {
    pub(super) fn account_capacity(&self) -> u32 {
        self.accounts
            .iter()
            .filter(|account| !account.deleting)
            .fold(0u32, |total, account| {
                total.saturating_add(account.max_concurrency)
            })
    }

    pub(super) fn effective_global_limit(&self) -> u32 {
        if self.global_limit_auto {
            self.account_capacity()
        } else {
            self.global_limit
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupInput {
    pub revision: u64,
    pub name: String,
    pub enabled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountInput {
    pub revision: u64,
    pub group_id: Uuid,
    pub enabled: bool,
    pub max_concurrency: u32,
}

impl Operations {
    pub(crate) fn account_policy(&self, id: Uuid) -> Result<AccountPolicy> {
        self.inner
            .lock()
            .unwrap()
            .settings
            .accounts
            .iter()
            .find(|account| account.id == id)
            .cloned()
            .ok_or_else(|| Error::new("NOT_FOUND", "账号不存在。"))
    }

    pub(crate) fn account_concurrency(&self, id: Uuid) -> Result<usize> {
        self.inner
            .lock()
            .unwrap()
            .settings
            .accounts
            .iter()
            .find(|account| account.id == id)
            .map(|account| account.max_concurrency as usize)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))
    }
    pub(crate) fn check_local_connection(
        &self,
        id: Option<Uuid>,
        group: Uuid,
        revision: u64,
        converting: bool,
    ) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        Self::validate_local_connection(&inner, id, group, revision, converting)
    }
    fn validate_local_connection(
        inner: &Inner,
        id: Option<Uuid>,
        group: Uuid,
        revision: u64,
        converting: bool,
    ) -> Result<()> {
        if inner.draining {
            return Err(Error::new("SERVICE_DRAINING", "服务正在停止，请稍后重试。"));
        }
        if !inner.storage_healthy {
            return Err(Error::new(
                "OPERATIONS_STORAGE",
                "统计存储异常，请检查磁盘并重启服务。",
            ));
        }
        if inner.settings.revision != revision {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "账户或分组设置已更新，请刷新状态后重试。",
            ));
        }
        if !inner
            .settings
            .groups
            .iter()
            .any(|g| g.id == group && g.enabled)
        {
            return Err(Error::new(
                "LOCAL_GROUP_UNAVAILABLE",
                "所选分组已暂停或不存在，请选择启用的分组。",
            ));
        }
        if let Some(id) = id {
            let account = inner
                .settings
                .accounts
                .iter()
                .find(|a| a.id == id)
                .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
            if account.deleting
                || inner.active.values().any(|a| a.account == Some(id))
                || inner.pending.iter().any(|p| p.account == Some(id))
            {
                return Err(Error::new(
                    "BUSY",
                    "账户有运行或排队中的操作，请等待完成后重试。",
                ));
            }
            if converting && (account.route.is_some() || !account.belongs_to(group)) {
                return Err(Error::new(
                    "LOCAL_ACCOUNT_NOT_EMPTY",
                    "只能在原分组中连接尚未配置接入的空账户。",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn connect_local_route(
        &self,
        id: Uuid,
        group: Uuid,
        revision: u64,
        provider: crate::provider::Provider,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        Self::validate_local_connection(&inner, Some(id), group, revision, false)?;
        let mut next = inner.settings.clone();
        let account = next.accounts.iter_mut().find(|a| a.id == id).unwrap();
        if !account.belongs_to(group) {
            account.group_ids = vec![group];
        }
        account.enabled = true;
        account.route = Some(provider);
        Self::save(&mut inner, next, revision)
    }
    pub(super) fn save(inner: &mut Inner, mut next: Settings, revision: u64) -> Result<()> {
        if revision != inner.settings.revision {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "配置已被其他操作更新，请刷新后重试。",
            ));
        }
        next.revision = revision + 1;
        inner
            .db
            .execute(
                "UPDATE settings SET document=?1 WHERE id=1",
                [serde_json::to_string(&next)?],
            )
            .map_err(storage)?;
        for (id, caps) in &mut inner.runtime.catalogs {
            if !next.accounts.iter().any(|a| {
                a.id == *id
                    && a.route
                        .as_ref()
                        .is_some_and(|p| runtime::fingerprint(p) == caps.provider)
            }) {
                caps.stale = true;
                caps.verified_features.clear();
                caps.verified_model = None;
            }
        }
        // Explicit disable/delete wins over an already scheduled restart.
        let disabled: Vec<_> = next
            .accounts
            .iter()
            .filter(|a| {
                a.deleting
                    || (!a.enabled
                        && inner
                            .settings
                            .accounts
                            .iter()
                            .any(|old| old.id == a.id && old.enabled))
            })
            .map(|a| a.id)
            .collect();
        for id in disabled {
            if let Some(change) = inner.proxy_changes.get_mut(&id) {
                change.restart_allowed = false;
            }
        }
        inner
            .proxy_changes
            .retain(|id, _| next.accounts.iter().any(|a| a.id == *id && !a.deleting));
        inner.runtime.scheduling.invalidate(&inner.settings, &next);
        super::subscription::retain_routes(inner, &next)?;
        inner.settings = next;
        inner.changed.notify_waiters();
        Ok(())
    }
    pub fn register_accounts(&self, accounts: &[(Uuid, String)]) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        for (id, name) in accounts {
            if !next.accounts.iter().any(|a| a.id == *id) {
                next.accounts.push(AccountPolicy {
                    id: *id,
                    name: name.clone(),
                    group_ids: vec![DEFAULT_GROUP],
                    enabled: true,
                    max_concurrency: if *id == DESKTOP_ACCOUNT {
                        1
                    } else {
                        DEFAULT_ACCOUNT_CONCURRENCY
                    },
                    proxy: Default::default(),
                    location: Default::default(),
                    route: None,
                    deleting: false,
                    disabled_features: vec![],
                });
            }
        }
        if next.accounts.len() != inner.settings.accounts.len() {
            let revision = next.revision;
            Self::save(&mut inner, next, revision)?;
        }
        Ok(())
    }
    pub fn set_global(&self, revision: u64, limit: u32) -> Result<()> {
        if !valid_limit(limit) {
            return Err(Error::new("INVALID_INPUT", "并发上限应为 1–256。"));
        }
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        next.global_limit = limit;
        next.global_limit_auto = false;
        Self::save(&mut inner, next, revision)
    }
    pub fn set_global_auto(&self, revision: u64) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        next.global_limit_auto = true;
        Self::save(&mut inner, next, revision)
    }
    pub fn pending_account_deletions(&self) -> Vec<Uuid> {
        self.inner
            .lock()
            .unwrap()
            .settings
            .accounts
            .iter()
            .filter(|a| a.deleting)
            .map(|a| a.id)
            .collect()
    }
    // Persist the intent before touching profile files. Admission and this check
    // share a lock, so no new work can enter between checking and disabling.
    pub fn begin_account_delete(&self, id: Uuid, revision: u64) -> Result<()> {
        if id == DESKTOP_ACCOUNT {
            return Err(Error::new(
                "INVALID_INPUT",
                "桌面共享路由由共享连接管理，请使用停止共享后端。",
            ));
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.active.values().any(|a| a.account == Some(id))
            || inner.pending.iter().any(|p| p.account == Some(id))
        {
            return Err(Error::new(
                "BUSY",
                "账户仍有执行或排队中的请求，请结束或取消后再删除。",
            ));
        }
        let mut next = inner.settings.clone();
        let account = next
            .accounts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        account.deleting = true;
        account.enabled = false;
        account.route = None;
        Self::save(&mut inner, next, revision)?;
        self.locations.invalidate(id, true);
        Ok(())
    }
    pub fn finish_account_delete(&self, id: Uuid) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        if next.accounts.iter().any(|a| a.id == id && !a.deleting) {
            return Err(Error::new("BUSY", "账户尚未进入删除状态。"));
        }
        next.accounts.retain(|a| a.id != id);
        let revision = next.revision;
        Self::save(&mut inner, next, revision)?;
        let tx = inner.db.transaction().map_err(storage)?;
        tx.execute(
            "DELETE FROM response_affinities WHERE account=?1",
            [id.to_string()],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        super::subscription::clear(&mut inner, id)?;
        Ok(())
    }
    pub fn set_queue(&self, revision: u64, limit: u32, timeout_seconds: u32) -> Result<()> {
        if !(1..=4096).contains(&limit) || !(1..=300).contains(&timeout_seconds) {
            return Err(Error::new(
                "INVALID_INPUT",
                "队列长度应为 1–4096，等待超时应为 1–300 秒。",
            ));
        }
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        next.queue_limit = limit;
        next.queue_timeout_seconds = timeout_seconds;
        Self::save(&mut inner, next, revision)
    }
    pub fn set_native_affinity_ttl(&self, revision: u64, hours: u32) -> Result<()> {
        if !(1..=720).contains(&hours) {
            return Err(Error::new(
                "INVALID_INPUT",
                "账户亲和时间应为 1–720 个整数小时。",
            ));
        }
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        next.native_affinity_ttl_hours = hours;
        Self::save(&mut inner, next, revision)
    }
    pub fn save_group(&self, id: Option<Uuid>, input: GroupInput) -> Result<Uuid> {
        if !valid_name(&input.name) {
            return Err(Error::new("INVALID_INPUT", "分组名称应为 1–60 个字符。"));
        }
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        if next
            .groups
            .iter()
            .any(|g| Some(g.id) != id && g.name.to_lowercase() == input.name.trim().to_lowercase())
        {
            return Err(Error::new("INVALID_INPUT", "分组名称已存在。"));
        }
        let group = Group {
            id: id.unwrap_or_else(Uuid::new_v4),
            name: input.name.trim().into(),
            enabled: input.enabled,
        };
        let result = group.id;
        if let Some(id) = id {
            let existing = next
                .groups
                .iter_mut()
                .find(|g| g.id == id)
                .ok_or_else(|| Error::new("NOT_FOUND", "分组不存在。"))?;
            *existing = group;
        } else {
            if next.groups.len() >= 64 {
                return Err(Error::new("INVALID_INPUT", "最多建立 64 个分组。"));
            }
            next.groups.push(group);
        }
        Self::save(&mut inner, next, input.revision)?;
        Ok(result)
    }
    pub fn delete_group(&self, id: Uuid, revision: u64) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        if id == DEFAULT_GROUP
            || next.accounts.iter().any(|a| a.belongs_to(id))
            || inner.active.values().any(|a| a.group == Some(id))
            || inner.pending.iter().any(|p| p.group == Some(id))
        {
            return Err(Error::new(
                "BUSY",
                "默认分组、非空分组或仍有请求的分组不能删除。",
            ));
        }
        if !next.groups.iter().any(|g| g.id == id) {
            return Err(Error::new("NOT_FOUND", "分组不存在。"));
        }
        if api_keys::references_group(&inner.db, id)? {
            return Err(Error::new(
                "BUSY",
                "分组仍绑定未过期的 API Key，请先撤销这些密钥。",
            ));
        }
        next.groups.retain(|g| g.id != id);
        Self::save(&mut inner, next, revision)
    }
    pub fn set_account(&self, id: Uuid, input: AccountInput) -> Result<()> {
        let groups = vec![input.group_id];
        self.set_account_groups(id, input, groups)
    }
    pub fn set_account_groups(
        &self,
        id: Uuid,
        input: AccountInput,
        mut groups: Vec<Uuid>,
    ) -> Result<()> {
        if !valid_limit(input.max_concurrency) {
            return Err(Error::new("INVALID_INPUT", "并发上限应为 1–256。"));
        }
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        if groups.is_empty() || groups.len() > 64 {
            return Err(Error::new(
                "INVALID_INPUT",
                "请选择至少一个分组，最多 64 个。",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        groups.retain(|id| seen.insert(*id));
        if groups
            .iter()
            .any(|id| !next.groups.iter().any(|g| g.id == *id))
        {
            return Err(Error::new("NOT_FOUND", "分组不存在。"));
        }
        let account = next
            .accounts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        if account.deleting {
            return Err(Error::new("BUSY", "账户正在删除，请重试删除以完成清理。"));
        }
        if id == DESKTOP_ACCOUNT && input.max_concurrency != 1 {
            return Err(Error::new(
                "INVALID_INPUT",
                "桌面共享调试连接的并发固定为 1；客户端账户可单独设置。",
            ));
        }
        if (account.groups().len() != groups.len()
            || groups.iter().any(|id| !account.belongs_to(*id)))
            && (inner.active.values().any(|a| a.account == Some(id))
                || inner.pending.iter().any(|p| p.account == Some(id)))
        {
            return Err(Error::new(
                "BUSY",
                "账户仍有执行或排队中的请求，请完成后再移动分组。",
            ));
        }
        account.group_ids = groups;
        account.enabled = input.enabled;
        account.max_concurrency = input.max_concurrency;
        Self::save(&mut inner, next, input.revision)
    }
    pub fn snapshot(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        json!({"revision":inner.settings.revision,"globalLimit":inner.settings.effective_global_limit(),"globalLimitAuto":inner.settings.global_limit_auto,"manualGlobalLimit":inner.settings.global_limit,"accountCapacity":inner.settings.account_capacity(),"queueLimit":inner.settings.queue_limit,"queueTimeoutSeconds":inner.settings.queue_timeout_seconds,"nativeAffinityTtlHours":inner.settings.native_affinity_ttl_hours,"queued":inner.pending.len(),"active":Self::active_count(&inner,None,None),"storageHealthy":inner.storage_healthy,"draining":inner.draining,"retentionDays":inner.settings.retention_days,
            "groups":inner.settings.groups.iter().map(|g|{let mut value=serde_json::to_value(g).unwrap();value["active"]=Self::active_count(&inner,None,Some(g.id)).into();value["queued"]=Self::queued_count(&inner,None,Some(g.id)).into();value["memberCount"]=inner.settings.accounts.iter().filter(|a|a.belongs_to(g.id)).count().into();value}).collect::<Vec<_>>(),
            "accounts":inner.settings.accounts.iter().map(|a|{let mut value=serde_json::to_value(a).unwrap();value["features"]=json!(a.allowed_features());value["locationStatus"]=self.locations.view(a.id,&a.location);value["proxyStatus"]=inner.proxy_changes.get(&a.id).map(|c|c.view()).unwrap_or_else(||json!({"state":"applied"}));value["active"]=Self::active_count(&inner,Some(a.id),None).into();value["queued"]=Self::queued_count(&inner,Some(a.id),None).into();value["health"]=runtime::health_view(&inner,a);value["quota"]=super::quota::view(&inner,a);value["capabilities"]=runtime::catalog_view(&inner,a.id);value}).collect::<Vec<_>>()})
    }
}
