use super::{ClientAccess, Lease, Operations, storage};
use crate::{
    error::{Error, Result},
    provider::Provider,
    streaming::StreamSummary,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use uuid::Uuid;

impl Operations {
    pub(crate) fn has_api_key_routes(&self, group: Uuid) -> bool {
        // This is transport detection, not admission: paused/deleting routes
        // must still reach the normal route validation and rejection path.
        self.inner
            .lock()
            .unwrap()
            .settings
            .accounts
            .iter()
            .any(|a| {
                a.belongs_to(group)
                    && a.route
                        .as_ref()
                        .is_some_and(|p| p.execution == crate::provider::Execution::ApiKey)
            })
    }

    pub fn configured_provider(&self, account: Uuid) -> Option<Provider> {
        self.inner
            .lock()
            .unwrap()
            .settings
            .accounts
            .iter()
            .find(|a| a.id == account)
            .and_then(|a| a.route.clone())
    }
    pub fn invalidate_credentials(&self, account: Uuid) -> Result<()> {
        self.invalidate_credentials_except(account, None)
    }
    fn invalidate_credentials_except(&self, account: Uuid, import: Option<Uuid>) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner
            .active
            .iter()
            .any(|(id, a)| a.account == Some(account) && Some(*id) != import)
        {
            return Err(Error::new(
                "BUSY",
                "实例正在执行请求，请先停止或等待完成，再修改凭据。",
            ));
        }
        let mut next = inner.settings.clone();
        let policy = next
            .accounts
            .iter_mut()
            .find(|a| a.id == account)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        if policy.deleting {
            return Err(Error::new("BUSY", "账户正在删除。"));
        }
        policy.route = None;
        let revision = next.revision;
        Self::save(&mut inner, next, revision)?;
        inner.runtime.quotas.remove(&account);
        inner
            .db
            .execute(
                "DELETE FROM account_quotas WHERE account_id=?1",
                [account.to_string()],
            )
            .map_err(super::storage)?;
        Ok(())
    }
    /// Desktop debugging is not a client API account. Keep historical request
    /// records, but remove the reserved runtime entry from active configuration.
    pub fn remove_desktop_route(&self) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        let before = next.accounts.len();
        next.accounts
            .retain(|account| account.id != super::DESKTOP_ACCOUNT);
        if next.accounts.len() != before {
            let revision = next.revision;
            Self::save(&mut inner, next, revision)?;
        }
        Ok(())
    }
    pub fn sync_desktop_route(&self, provider: Option<Provider>) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        let id = super::DESKTOP_ACCOUNT;
        if let Some(account) = next.accounts.iter_mut().find(|a| a.id == id) {
            if account.route == provider {
                return Ok(());
            }
            account.route = provider;
        } else if let Some(provider) = provider {
            next.accounts.push(super::AccountPolicy {
                id,
                name: "桌面共享连接".into(),
                group_ids: vec![super::DEFAULT_GROUP],
                enabled: true,
                max_concurrency: 1,
                proxy: Default::default(),
                location: Default::default(),
                deleting: false,
                disabled_features: vec![],
                route: Some(provider),
            });
        } else {
            return Ok(());
        }
        let revision = next.revision;
        Self::save(&mut inner, next, revision)
    }
    pub fn set_inference_route(
        &self,
        account: Uuid,
        revision: u64,
        provider: Option<Provider>,
    ) -> Result<()> {
        if account == super::DESKTOP_ACCOUNT {
            return Err(Error::new(
                "INVALID_INPUT",
                "桌面连接仅用于本机调试；请在“客户端接入”配置本机配置实例或独立 OAuth 账户。",
            ));
        }
        let mut inner = self.inner.lock().unwrap();
        if provider.is_some()
            && (inner.active.values().any(|a| a.account == Some(account))
                || inner.pending.iter().any(|p| p.account == Some(account)))
        {
            return Err(Error::new(
                "BUSY",
                "账户仍有请求，请结束后再启用或更新路由。",
            ));
        }
        let mut next = inner.settings.clone();
        let policy = next
            .accounts
            .iter_mut()
            .find(|a| a.id == account)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        if policy.deleting {
            return Err(Error::new("BUSY", "账户正在删除，不能修改路由。"));
        }
        policy.route = provider;
        Self::save(&mut inner, next, revision)
    }
    pub fn inference_route(&self, group: Uuid, model: &str) -> Result<(Uuid, Provider)> {
        self.select_inference_route(group, model, false)
    }
    pub fn native_inference_route(&self, group: Uuid, model: &str) -> Result<(Uuid, Provider)> {
        self.select_inference_route(group, model, true)
    }
    fn select_inference_route(
        &self,
        group: Uuid,
        model: &str,
        dedicated: bool,
    ) -> Result<(Uuid, Provider)> {
        let inner = self.inner.lock().unwrap();
        if inner.draining {
            return Err(Error::new("SERVICE_DRAINING", "服务正在退出。"));
        }
        if !inner
            .settings
            .groups
            .iter()
            .any(|g| g.id == group && g.enabled)
        {
            return Err(Error::new("API_KEY_GROUP_DISABLED", "授权分组不可用。"));
        }
        if model.is_empty()
            || model.len() > 128
            || !model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        {
            return Err(Error::new("INVALID_INPUT", "模型 ID 格式无效。"));
        }
        let candidates: Vec<_> = inner
            .settings
            .accounts
            .iter()
            .filter(|a| {
                a.belongs_to(group)
                    && a.enabled
                    && a.route.is_some()
                    && (!dedicated
                        || (a.id != super::DESKTOP_ACCOUNT
                            && a.route.as_ref().is_some_and(|p| {
                                p.execution == crate::provider::Execution::Native
                            })))
            })
            .collect();
        // Pin the selected account while queued; never replay a dispatched request.
        let selected = candidates.iter().min_by_key(|a| {
            (
                inner.proxy_changes.contains_key(&a.id),
                Self::active_count(&inner, Some(a.id), None)
                    + Self::queued_count(&inner, Some(a.id), None),
                a.id,
            )
        });
        selected
            .map(|a| (a.id, a.route.clone().unwrap()))
            .ok_or_else(|| {
                Error::new(
                    "INFERENCE_DISABLED",
                    "此分组没有可用的客户端模型路由，请在“客户端接入”启用同组的本机配置实例或独立 OAuth 账户。",
                )
            })
    }
    pub fn check_inference_route(
        &self,
        account: Uuid,
        access: &ClientAccess,
        provider: &Provider,
        model: &str,
    ) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        super::api_keys::validate_access(&inner, access)?;
        Self::check_route(&inner, account, access, provider, model)
    }
    pub(crate) fn check_retained_inference_route(
        &self,
        account: Uuid,
        owner: Uuid,
        provider: &Provider,
        model: &str,
    ) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        let access = super::api_keys::current_access(&inner, owner)?;
        Self::check_route(&inner, account, &access, provider, model)
    }
    fn check_route(
        inner: &super::Inner,
        account: Uuid,
        access: &ClientAccess,
        provider: &Provider,
        model: &str,
    ) -> Result<()> {
        if !access.allows(model)
            || !inner.settings.accounts.iter().any(|a| {
                a.id == account
                    && a.belongs_to(access.group_id)
                    && a.enabled
                    && a.route.as_ref() == Some(provider)
            })
        {
            return Err(Error::new(
                "ROUTE_CHANGED",
                "等待期间模型授权或账户路由已改变，未派发请求。",
            ));
        }
        Ok(())
    }
    pub fn has_inference_routes(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.settings.accounts.iter().any(|a| {
            a.id != super::DESKTOP_ACCOUNT
                && a.enabled
                && a.route.is_some()
                && inner
                    .settings
                    .groups
                    .iter()
                    .any(|g| a.belongs_to(g.id) && g.enabled)
        })
    }
    pub fn inference_models(&self, access: Option<&ClientAccess>) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let mut models: Vec<_> = inner
            .settings
            .accounts
            .iter()
            .filter(|a| {
                a.id != super::DESKTOP_ACCOUNT
                    && a.enabled
                    && inner
                        .settings
                        .groups
                        .iter()
                        .any(|g| a.belongs_to(g.id) && g.enabled)
            })
            .filter(|a| access.is_none_or(|k| a.belongs_to(k.group_id)))
            .flat_map(|a| {
                a.route
                    .as_ref()
                    .map(|p| {
                        let mut available = inner
                            .runtime
                            .catalogs
                            .get(&a.id)
                            .filter(|c| c.provider == super::runtime::fingerprint(p))
                            .map(|c| {
                                c.models
                                    .iter()
                                    .filter_map(|m| m["id"].as_str().map(str::to_owned))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        if !p.model.is_empty() {
                            available.push(p.model.clone());
                        }
                        if let Some(key) = access.filter(|k| !k.models.is_empty()) {
                            available = key.models.clone();
                        }
                        available
                    })
                    .unwrap_or_default()
            })
            .collect();
        models.sort();
        models.dedup();
        models
    }
}
impl Lease {
    /// Keep the exclusive import reservation while invalidating the old route.
    pub(crate) fn invalidate_import_credentials(&self) -> Result<()> {
        let account = self
            .account_id
            .ok_or_else(|| Error::new("INVALID_INPUT", "缺少导入账户。"))?;
        {
            let inner = self.owner.inner.lock().unwrap();
            if !inner
                .active
                .get(&self.id)
                .is_some_and(|a| a.exclusive && a.limited)
            {
                return Err(Error::new("BUSY", "凭据导入需要独占账户。"));
            }
        }
        self.owner
            .invalidate_credentials_except(account, Some(self.id))
    }
    pub fn request_id(&self) -> Uuid {
        self.id
    }
    pub fn bind_client(&self, key: Uuid) -> Result<()> {
        let mut inner = self.owner.inner.lock().unwrap();
        if let Err(error) = inner.db.execute(
            "UPDATE requests SET key_id=?1,key_name=(SELECT name FROM api_keys WHERE id=?1) WHERE id=?2",
            params![key.to_string(), self.id.to_string()],
        ) {
            inner.storage_healthy = false;
            return Err(storage(error));
        }
        Ok(())
    }
    pub fn finish_inference(mut self, result: &Result<StreamSummary>) -> Result<()> {
        {
            let mut inner = self.owner.inner.lock().unwrap();
            let summary = result.as_ref().ok();
            super::scheduler::finished(
                &mut inner,
                self.id,
                summary.is_some_and(|s| s.outcome == "completed"),
                result
                    .as_ref()
                    .err()
                    .map(|e| e.code.as_str())
                    .or_else(|| summary.and_then(|s| s.error_code.as_deref())),
                summary.and_then(|s| s.first_delta_at),
            );
        }
        if let Ok(summary) = result {
            let mut inner = self.owner.inner.lock().unwrap();
            let saved = inner.db.execute("UPDATE requests SET input_tokens=?1,output_tokens=?2,native_thread_id=?3,native_turn_id=?4,cached_input_tokens=?6,first_delta_ms=CASE WHEN ?7 IS NULL THEN NULL ELSE MAX(0,?7-started_at-COALESCE(wait_ms,0)) END,response_model=?8,response_service_tier=?9 WHERE id=?5",
                params![summary.usage.as_ref().map(|u|u.input_tokens as i64),summary.usage.as_ref().map(|u|u.output_tokens as i64),summary.thread_id,summary.turn_id,self.id.to_string(),summary.usage.as_ref().and_then(|u|u.cached_input_tokens).map(|n|n as i64),summary.first_delta_at.map(|at|at as i64),summary.response_model,summary.response_service_tier]);
            if let Err(error) = saved {
                inner.storage_healthy = false;
                return Err(storage(error));
            }
        }
        let (status, code) = match result {
            Ok(s) if s.outcome == "completed" => ("succeeded", None),
            Ok(s) if s.outcome == "cancelled" => ("cancelled", s.error_code.as_deref()),
            Ok(s) => ("failed", s.error_code.as_deref()),
            Err(e) if ["STREAM_CANCELLED", "STREAM_CONSUMER_CLOSED"].contains(&e.code.as_str()) => {
                ("cancelled", Some(e.code.as_str()))
            }
            Err(e) => ("failed", Some(e.code.as_str())),
        };
        self.settle(status, code)
    }
}

pub(super) fn usage(db: &Connection, filter: &str, bind: &[&dyn rusqlite::ToSql]) -> Result<Value> {
    db.query_row(&format!("SELECT SUM(input_tokens),SUM(output_tokens),COUNT(input_tokens),SUM(cached_input_tokens),SUM(CASE WHEN cached_input_tokens IS NOT NULL THEN input_tokens END),COUNT(cached_input_tokens) FROM requests WHERE {filter}"),bind,|row| {
        let cached = row.get::<_,Option<i64>>(3)?;
        let known_input = row.get::<_,Option<i64>>(4)?;
        let rate = cached.zip(known_input).filter(|(_,input)|*input>0).map(|(cache,input)|cache as f64/input as f64);
        Ok(json!({"inputTokens":row.get::<_,Option<i64>>(0)?,"outputTokens":row.get::<_,Option<i64>>(1)?,"usageKnownRequests":row.get::<_,i64>(2)?,"cachedInputTokens":cached,"cacheKnownInputTokens":known_input,"cacheKnownRequests":row.get::<_,i64>(5)?,"cacheHitRate":rate}))
    }).map_err(storage)
}

#[cfg(test)]
mod retained_route_tests {
    use super::*;
    use crate::operations::{AccountInput, ApiKeyGroup, DEFAULT_GROUP, GroupInput};

    #[test]
    fn parked_turns_follow_current_permissions_without_relaxing_admitted_requests() {
        let root = tempfile::tempdir().unwrap();
        let ops = Operations::open(root.path()).unwrap();
        let revision = || ops.snapshot()["revision"].as_u64().unwrap();
        let target = ops
            .save_group(
                None,
                GroupInput {
                    revision: revision(),
                    name: "target".into(),
                    enabled: true,
                },
            )
            .unwrap();
        let account = Uuid::new_v4();
        ops.register_accounts(&[(account, "synthetic".into())])
            .unwrap();
        let groups = |groups: Vec<Uuid>| {
            ops.set_account_groups(
                account,
                AccountInput {
                    revision: revision(),
                    group_id: DEFAULT_GROUP,
                    enabled: true,
                    max_concurrency: 2,
                },
                groups,
            )
            .unwrap()
        };
        groups(vec![DEFAULT_GROUP, target]);
        let provider: Provider = serde_json::from_value(json!({"id":"synthetic","model":"m","baseUrl":"http://127.0.0.1:1/v1","auth":"none","requiredHeaders":[],"forwardHeaders":[],"mcpServers":[]})).unwrap();
        ops.set_inference_route(account, revision(), Some(provider.clone()))
            .unwrap();
        let key = ops
            .create_api_key(
                serde_json::from_value(
                    json!({"name":"owner","groupId":DEFAULT_GROUP,"models":["m"]}),
                )
                .unwrap(),
            )
            .unwrap();
        let old = ops.authenticate_api_key(&key.secret).unwrap();
        ops.set_api_key_group(
            old.key_id,
            ApiKeyGroup {
                revision: 1,
                group_id: target,
            },
        )
        .unwrap();
        assert!(
            ops.check_retained_inference_route(account, old.key_id, &provider, "m")
                .is_ok()
        );
        assert_eq!(
            ops.check_inference_route(account, &old, &provider, "m")
                .unwrap_err()
                .code,
            "INVALID_API_KEY"
        );
        assert!(
            ops.check_retained_inference_route(account, old.key_id, &provider, "forbidden")
                .is_err()
        );
        let mut changed = provider.clone();
        changed.base_url.push_str("/changed");
        assert!(
            ops.check_retained_inference_route(account, old.key_id, &changed, "m")
                .is_err()
        );
        groups(vec![DEFAULT_GROUP]);
        assert!(
            ops.check_retained_inference_route(account, old.key_id, &provider, "m")
                .is_err()
        );
        groups(vec![DEFAULT_GROUP, target]);
        for enabled in [false, true] {
            ops.save_group(
                Some(target),
                GroupInput {
                    revision: revision(),
                    name: "target".into(),
                    enabled,
                },
            )
            .unwrap();
            assert_eq!(
                ops.check_retained_inference_route(account, old.key_id, &provider, "m")
                    .is_ok(),
                enabled
            );
        }
        for (sql, undo) in [
            (
                "UPDATE api_keys SET state='paused'",
                "UPDATE api_keys SET state='active'",
            ),
            (
                "UPDATE api_keys SET expires_at=1",
                "UPDATE api_keys SET expires_at=9223372036854775807",
            ),
            (
                "UPDATE api_keys SET models='[\"other\"]'",
                "UPDATE api_keys SET models='[\"m\"]'",
            ),
        ] {
            ops.inner.lock().unwrap().db.execute(sql, []).unwrap();
            assert!(
                ops.check_retained_inference_route(account, old.key_id, &provider, "m")
                    .is_err()
            );
            ops.inner.lock().unwrap().db.execute(undo, []).unwrap();
        }
        ops.delete_api_key(old.key_id, 2).unwrap();
        assert!(
            ops.check_retained_inference_route(account, old.key_id, &provider, "m")
                .is_err()
        );
    }
}
