use super::{AccountPolicy, Inner, Operations, storage};
use crate::{
    error::{Error, Result},
    provider::Provider,
    rpc::Rpc,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Feature {
    ImageInput,
    NamespacedTools,
    WebSearch,
    ImageGeneration,
}
impl Feature {
    pub const OPTIONAL: [Self; 3] = [Self::ImageInput, Self::WebSearch, Self::ImageGeneration];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ImageInput => "imageInput（图片输入）",
            Self::NamespacedTools => "namespacedTools（客户端工具）",
            Self::WebSearch => "webSearch（搜索）",
            Self::ImageGeneration => "imageGeneration（生图）",
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeCapabilities {
    pub installation: String,
    pub provider: String,
    pub checked_at: u64,
    pub stale: bool,
    pub models: Vec<Value>,
    pub web_search: Option<bool>,
    pub image_generation: Option<bool>,
    pub error_code: Option<String>,
    #[serde(default)]
    pub verified_features: Vec<Feature>,
    #[serde(default)]
    pub verified_model: Option<String>,
    #[serde(default)]
    pub verification_errors: HashMap<String, String>,
}
impl NativeCapabilities {
    fn is_stale(&self) -> bool {
        self.stale || crate::now_ms().saturating_sub(self.checked_at) > 3_600_000
    }
    pub fn supports(&self, feature: Feature, model: &str) -> Option<bool> {
        if feature == Feature::NamespacedTools {
            return Some(true);
        }
        if self.is_stale() {
            return None;
        }
        match feature {
            Feature::ImageInput => self
                .models
                .iter()
                .find(|v| v["model"] == model || v["id"] == model)
                .and_then(|v| v["inputModalities"].as_array())
                .map(|a| a.iter().any(|v| v == "image")),
            Feature::WebSearch => self.web_search,
            Feature::ImageGeneration => self.image_generation,
            // Namespace support is validated when the native thread is opened.
            Feature::NamespacedTools => Some(true),
        }
    }
}

pub async fn discover(rpc: &Rpc, provider: &Provider, installation: &str) -> NativeCapabilities {
    let mut result = NativeCapabilities {
        installation: installation.into(),
        provider: fingerprint(provider),
        checked_at: crate::now_ms(),
        ..Default::default()
    };
    let read = async {
        let mut cursor = Value::Null;
        for _ in 0..16 {
            let page = rpc
                .request(
                    "model/list",
                    json!({"limit":100,"includeHidden":true,"cursor":cursor}),
                )
                .await?;
            let models = page["data"]
                .as_array()
                .ok_or_else(|| Error::new("CAPABILITY_PROTOCOL", "模型目录格式无效。"))?;
            for model in models {
                if let Some(id) = model["model"]
                    .as_str()
                    .or_else(|| model["id"].as_str())
                    .filter(|id| id.len() <= 128)
                {
                    result.models.push(json!({"id":id,"model":id,"inputModalities":model["inputModalities"],"supportedReasoningEfforts":model["supportedReasoningEfforts"],
                        "serviceTiers":model["serviceTiers"],"defaultServiceTier":model["defaultServiceTier"]}));
                }
            }
            cursor = page["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
        }
        let caps = rpc
            .request("modelProvider/capabilities/read", json!({}))
            .await?;
        result.web_search = caps["webSearch"].as_bool();
        result.image_generation = caps["imageGeneration"].as_bool();
        Ok::<_, Error>(())
    };
    match tokio::time::timeout(Duration::from_secs(5), read).await {
        Ok(Ok(())) => {}
        outcome => {
            result.stale = true;
            result.error_code = Some(match outcome {
                Ok(Err(e)) => e.code,
                _ => "CAPABILITY_TIMEOUT".into(),
            });
        }
    }
    result
}

pub fn fingerprint(provider: &Provider) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        serde_json::to_vec(provider).unwrap_or_default(),
    ))
}

#[derive(Default)]
pub(super) struct Health {
    failures: u32,
    until: Option<tokio::time::Instant>,
    reason: Option<String>,
}
#[derive(Default)]
pub(super) struct Runtime {
    pub subscriptions: HashMap<Uuid, super::subscription::Snapshot>,
    pub scheduling: super::scheduler::State,
    pub quotas: HashMap<Uuid, super::quota::QuotaSnapshot>,
    pub catalogs: HashMap<Uuid, NativeCapabilities>,
    health: HashMap<Uuid, Health>,
    pub rate_limits: HashMap<Uuid, super::rate_limit::Cooldown>,
    pub verified: HashMap<Uuid, u64>,
}
pub(super) fn catalog_view(inner: &Inner, account: Uuid) -> Value {
    inner
        .runtime
        .catalogs
        .get(&account)
        .map_or(Value::Null, |caps| {
            let mut value = serde_json::to_value(caps).unwrap();
            value["stale"] = caps.is_stale().into();
            value
        })
}
pub(super) fn available(inner: &Inner, account: &AccountPolicy) -> bool {
    let now = tokio::time::Instant::now();
    let healthy = inner
        .runtime
        .health
        .get(&account.id)
        .is_none_or(|h| h.until.is_none_or(|t| t <= now));
    // 冷却期过后只允许一个恢复探针；配置多个并发也不能同时冲击故障实例。
    let recovering = inner
        .runtime
        .health
        .get(&account.id)
        .is_some_and(|h| h.failures >= 3);
    healthy
        && super::rate_limit::available(inner, account)
        && super::quota::available(inner, account)
        && (!recovering || Operations::active_count(inner, Some(account.id), None) == 0)
}
pub(super) fn health_view(inner: &Inner, account: &AccountPolicy) -> Value {
    let now = tokio::time::Instant::now();
    let local = inner.runtime.health.get(&account.id);
    let rate_limit = super::rate_limit::current(inner, account);
    let until = rate_limit
        .map(|state| state.until)
        .max(local.and_then(|h| h.until));
    let remaining = until
        .map(|t| t.saturating_duration_since(now).as_millis() as u64)
        .unwrap_or(0);
    json!({"state":if remaining > 0 {"cooling"} else if rate_limit.is_some() || local.is_some_and(|h| h.failures >= 3) {"halfOpen"} else {"available"},
        "consecutiveFailures":local.map(|h|h.failures).unwrap_or(0),"retryAt":if remaining>0 {Some(crate::now_ms()+remaining)}else{None},
        "reason":if rate_limit.is_some_and(|r| Some(r.until) == until) {Some("UPSTREAM_RATE_LIMITED")} else {local.and_then(|h|h.reason.as_deref())},"sharedUpstream":false,
        "rateLimitFailures":rate_limit.map(|r|r.failures).unwrap_or(0),
        "lastVerifiedAt":inner.runtime.verified.get(&account.id)})
}
pub(super) fn requirements(
    _inner: &Inner,
    account: &AccountPolicy,
    request: &super::RoutingRequest,
) -> bool {
    if request.class == super::RequestClass::NativeSession
        && account
            .route
            .as_ref()
            .is_some_and(|p| p.execution == crate::provider::Execution::ApiKey)
    {
        return false;
    }
    request
        .features
        .iter()
        .all(|f| *f == Feature::NamespacedTools || !account.disabled_features.contains(f))
}
/// Catalog evidence helps choose a route, but unknown/expired data never denies
/// a real request. The unchanged native agent/provider remains authoritative.
pub(super) fn capability_rank(
    inner: &Inner,
    account: &AccountPolicy,
    request: &super::RoutingRequest,
) -> u8 {
    let catalog = inner.runtime.catalogs.get(&account.id).filter(|c| {
        account
            .route
            .as_ref()
            .is_some_and(|p| fingerprint(p) == c.provider)
    });
    Feature::OPTIONAL
        .iter()
        .filter(|f| request.features.contains(f))
        .map(
            |f| match catalog.and_then(|c| c.supports(*f, &request.model)) {
                Some(true) => 0,
                None => 1,
                Some(false) => 2,
            },
        )
        .sum()
}
pub(super) fn disabled_requirements(
    account: &AccountPolicy,
    request: &super::RoutingRequest,
) -> Vec<&'static str> {
    Feature::OPTIONAL
        .iter()
        .filter(|f| request.features.contains(f) && account.disabled_features.contains(f))
        .map(|f| f.label())
        .collect()
}

impl Operations {
    pub fn record_native_health(
        &self,
        account: Uuid,
        result: &Result<crate::streaming::StreamSummary>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let h = inner.runtime.health.entry(account).or_default();
        let code = result
            .as_ref()
            .err()
            .map(|e| e.code.as_str())
            .or_else(|| result.as_ref().ok().and_then(|s| s.error_code.as_deref()));
        match result {
            Ok(s) if s.outcome == "completed" => {
                *h = Health::default();
                inner.runtime.verified.insert(account, crate::now_ms());
            }
            _ if code.is_some_and(|code| {
                matches!(
                    code,
                    "RPC_CLOSED"
                        | "RPC_TIMEOUT"
                        | "RPC_PROTOCOL"
                        | "BACKEND_START"
                        | "BACKEND_SPAWN"
                        | "IO_ERROR"
                        | "STREAM_EVENTS_LOST"
                )
            }) =>
            {
                h.failures = h.failures.saturating_add(1);
                h.reason = code.map(str::to_owned);
                if h.failures >= 3 {
                    h.until = Some(tokio::time::Instant::now() + Duration::from_secs(30));
                }
            }
            _ => {}
        }
        inner.changed.notify_waiters();
    }
    pub fn publish_capabilities(
        &self,
        account: Uuid,
        mut capabilities: NativeCapabilities,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.settings.accounts.iter().any(|a| {
            a.id == account
                && a.route
                    .as_ref()
                    .is_some_and(|p| fingerprint(p) == capabilities.provider)
        }) {
            return Err(Error::new("ROUTE_CHANGED", "能力读取期间路由已变化。"));
        }
        if let Some(old) = inner.runtime.catalogs.get(&account).filter(|c| {
            c.provider == capabilities.provider && c.installation == capabilities.installation
        }) {
            capabilities.verified_features = old.verified_features.clone();
            capabilities.verified_model = old.verified_model.clone();
            capabilities.verification_errors = old.verification_errors.clone();
        }
        if capabilities.stale {
            if let Some(old) = inner.runtime.catalogs.get(&account).filter(|c| {
                c.provider == capabilities.provider && c.installation == capabilities.installation
            }) {
                let error = capabilities.error_code;
                capabilities = old.clone();
                capabilities.stale = true;
                capabilities.error_code = error;
            }
        }
        inner.db.execute("INSERT INTO native_capabilities(account_id,document) VALUES(?1,?2) ON CONFLICT(account_id) DO UPDATE SET document=excluded.document",params![account.to_string(),serde_json::to_string(&capabilities)?]).map_err(storage)?;
        inner.runtime.catalogs.insert(account, capabilities);
        inner.changed.notify_waiters();
        Ok(())
    }
    pub fn capabilities(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        json!({"automaticCapabilities":true,"accounts":inner.settings.accounts.iter().filter(|a| a.id!=super::DESKTOP_ACCOUNT).map(|a|json!({"id":a.id,"name":a.name,"enabledFeatures":a.allowed_features(),"disabledFeatures":a.disabled_features,"health":health_view(&inner,a),"capabilities":catalog_view(&inner,a.id),"oauthInference":{"enabled":a.enabled&&a.route.as_ref().is_some_and(|p|p.auth.starts_with(crate::oauth::AUTH)),"requiresNativeVerification":false}})).collect::<Vec<_>>(),
            "unsupported":["fileInput","audioInput","videoInput","standaloneImages","standaloneSearch","hostedMcp","fileSearch","codeInterpreter"]})
    }
    pub fn record_feature_verification(
        &self,
        account: Uuid,
        provider: &Provider,
        installation: &str,
        feature: Feature,
        error: Option<&Error>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.settings.accounts.iter().any(|a| {
            a.id == account && !a.deleting && a.enabled && a.route.as_ref() == Some(provider)
        }) {
            return Err(Error::new(
                "ROUTE_CHANGED",
                "验证期间路由已变化，结果未发布。",
            ));
        }
        let caps = inner
            .runtime
            .catalogs
            .get(&account)
            .filter(|c| c.provider == fingerprint(provider) && c.installation == installation)
            .ok_or_else(|| Error::new("ROUTE_CHANGED", "请先刷新当前原生能力目录。"))?;
        let mut next = caps.clone();
        next.verified_model = Some(provider.model.clone());
        let name = serde_json::to_value(feature)?.as_str().unwrap().to_owned();
        if let Some(e) = error {
            next.verified_features.retain(|f| *f != feature);
            next.verification_errors.insert(name, e.code.clone());
        } else {
            if !next.verified_features.contains(&feature) {
                next.verified_features.push(feature);
            }
            next.verification_errors.remove(&name);
        }
        inner
            .db
            .execute(
                "UPDATE native_capabilities SET document=?1 WHERE account_id=?2",
                params![serde_json::to_string(&next)?, account.to_string()],
            )
            .map_err(storage)?;
        inner.runtime.catalogs.insert(account, next);
        inner.changed.notify_waiters();
        Ok(())
    }
    pub fn check_capability_generation(
        &self,
        account: Uuid,
        installation: &str,
        features: &[Feature],
    ) -> Result<()> {
        if features.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.lock().unwrap();
        let changed = inner
            .runtime
            .catalogs
            .get(&account)
            .is_some_and(|c| c.installation != installation);
        if changed {
            if let Some(c) = inner.runtime.catalogs.get_mut(&account) {
                c.stale = true;
                c.verified_features.clear();
                c.verified_model = None;
            }
            // Invalidate diagnostic evidence, not permission to use the native API.
        }
        Ok(())
    }
    pub fn set_disabled_features(
        &self,
        account: Uuid,
        revision: u64,
        disabled: Vec<Feature>,
    ) -> Result<()> {
        if disabled.iter().any(|f| !Feature::OPTIONAL.contains(f))
            || disabled.len() > Feature::OPTIONAL.len()
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "仅图片、搜索和生图可以通过能力策略关闭；客户端工具默认可用。",
            ));
        }
        let mut inner = self.inner.lock().unwrap();
        let mut next = inner.settings.clone();
        let policy = next
            .accounts
            .iter_mut()
            .find(|a| a.id == account)
            .ok_or_else(|| Error::new("NOT_FOUND", "实例不存在。"))?;
        if policy.route.is_none() || policy.deleting {
            return Err(Error::new("ROUTE_DISABLED", "请先启用路由。"));
        }
        if inner.active.values().any(|a| a.account == Some(account)) {
            return Err(Error::new(
                "BUSY",
                "实例仍有执行中的请求，请完成后调整能力。",
            ));
        }
        policy.disabled_features = Feature::OPTIONAL
            .into_iter()
            .filter(|f| disabled.contains(f))
            .collect();
        Self::save(&mut inner, next, revision)
    }
}

pub(super) fn load(db: &Connection) -> Result<Runtime> {
    let mut result = Runtime {
        subscriptions: super::subscription::load(db)?,
        quotas: super::quota::load(db)?,
        ..Default::default()
    };
    let mut stmt = db
        .prepare("SELECT account_id,document FROM native_capabilities")
        .map_err(storage)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(storage)?;
    for row in rows {
        let (id, doc) = row.map_err(storage)?;
        if let (Ok(id), Ok(mut caps)) = (
            Uuid::parse_str(&id),
            serde_json::from_str::<NativeCapabilities>(&doc),
        ) {
            caps.stale = true;
            result.catalogs.insert(id, caps);
        }
    }
    Ok(result)
}
