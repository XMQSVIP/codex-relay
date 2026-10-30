use super::{Operations, storage, valid_name};
use crate::error::{Error, Result};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod transfer;
pub use transfer::{ApiKeyBackup, ApiKeyImport, ApiKeyImportResult};

const PREFIX: &str = "cr_live_";
const DAY: u64 = 86_400_000;
// Permanent keys use a sentinel in storage and null in the public API.
const NEVER_EXPIRES: u64 = i64::MAX as u64;
const COLUMNS: &str = "id,name,prefix,group_id,models,state,created_at,expires_at,last_authenticated_at,revision,max_concurrency,fast_mode";

pub(super) fn references_group(db: &Connection, id: Uuid) -> Result<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM api_keys WHERE group_id=?1 AND state!='revoked' AND expires_at>?2)",
        params![id.to_string(), crate::now_ms() as i64], |r| r.get(0)).map_err(storage)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiKeyCreate {
    pub name: String,
    pub group_id: Uuid,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    #[serde(default)]
    pub max_concurrency: u32,
    #[serde(default)]
    pub fast_mode: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiKeyConcurrency {
    pub revision: u64,
    pub max_concurrency: u32,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiKeyGroup {
    pub revision: u64,
    pub group_id: Uuid,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiKeyUpdate {
    pub revision: u64,
    pub state: Option<String>,
    pub name: Option<String>,
    pub group_id: Option<Uuid>,
    pub max_concurrency: Option<u32>,
    pub fast_mode: Option<bool>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyView {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    pub group_id: Uuid,
    pub models: Vec<String>,
    pub state: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub last_authenticated_at: Option<u64>,
    pub revision: u64,
    pub max_concurrency: u32,
    pub fast_mode: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyCreated {
    pub key: ApiKeyView,
    pub secret: String,
}
#[derive(Debug, Clone)]
pub struct ClientAccess {
    pub key_id: Uuid,
    pub group_id: Uuid,
    pub models: Vec<String>,
    pub fast_mode: bool,
}
impl ClientAccess {
    /// Freeze the authenticated key policy for this request, not for a connection.
    /// Only the top-level Responses service tier is overridden.
    pub(crate) fn apply_fast_mode(&self, body: &mut serde_json::Value) -> bool {
        if self.fast_mode
            && let Some(object) = body.as_object_mut()
        {
            object.insert("service_tier".into(), serde_json::json!("priority"));
            return true;
        }
        false
    }

    pub fn allows(&self, model: &str) -> bool {
        self.models.is_empty() || self.models.iter().any(|allowed| allowed == model)
    }
}
fn decode(row: &Row<'_>) -> rusqlite::Result<ApiKeyView> {
    let invalid = |index| {
        rusqlite::Error::InvalidColumnType(index, "api_key".into(), rusqlite::types::Type::Text)
    };
    let number = |index| -> rusqlite::Result<u64> {
        u64::try_from(row.get::<_, i64>(index)?).map_err(|_| invalid(index))
    };
    let expires_at = number(7)?;
    let state: String = row.get(5)?;
    Ok(ApiKeyView {
        id: Uuid::parse_str(&row.get::<_, String>(0)?).map_err(|_| invalid(0))?,
        name: row.get(1)?,
        prefix: row.get(2)?,
        secret: None,
        group_id: Uuid::parse_str(&row.get::<_, String>(3)?).map_err(|_| invalid(3))?,
        models: serde_json::from_str(&row.get::<_, String>(4)?).map_err(|_| invalid(4))?,
        state: if state != "revoked" && expires_at <= crate::now_ms() {
            "expired".into()
        } else {
            state
        },
        created_at: number(6)?,
        expires_at: (expires_at != NEVER_EXPIRES).then_some(expires_at),
        last_authenticated_at: row
            .get::<_, Option<i64>>(8)?
            .map(|n| u64::try_from(n).map_err(|_| invalid(8)))
            .transpose()?,
        revision: number(9)?,
        max_concurrency: row.get(10)?,
        fast_mode: row.get(11)?,
    })
}
fn invalid_key() -> Error {
    Error::new("INVALID_API_KEY", "API Key 无效、已暂停、已过期或已撤销。")
}

fn valid_secret(secret: &str) -> bool {
    secret.len() == PREFIX.len() + 64
        && secret.starts_with(PREFIX)
        && secret[PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn model_name(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 128
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
}
pub(super) fn validate_access(inner: &super::Inner, access: &ClientAccess) -> Result<ApiKeyView> {
    let key = inner
        .db
        .query_row(
            &format!("SELECT {COLUMNS} FROM api_keys WHERE id=?1"),
            [access.key_id.to_string()],
            decode,
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(invalid_key)?;
    if key.state != "active"
        || key.group_id != access.group_id
        || key.models != access.models
        || key.expires_at.is_some_and(|at| at <= crate::now_ms())
    {
        return Err(invalid_key());
    }
    if inner.draining {
        return Err(Error::new("SERVICE_DRAINING", "服务正在退出。"));
    }
    if !inner
        .settings
        .groups
        .iter()
        .any(|g| g.id == key.group_id && g.enabled)
    {
        return Err(Error::new("API_KEY_GROUP_DISABLED", "授权分组不可用。"));
    }
    Ok(key)
}
// Only parked turns use a fresh authorization snapshot. Queued and running
// requests must still reject changes to the access under which they were admitted.
pub(super) fn current_access(inner: &super::Inner, id: Uuid) -> Result<ClientAccess> {
    let key = inner
        .db
        .query_row(
            &format!("SELECT {COLUMNS} FROM api_keys WHERE id=?1"),
            [id.to_string()],
            decode,
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(invalid_key)?;
    let access = ClientAccess {
        key_id: key.id,
        group_id: key.group_id,
        models: key.models,
        fast_mode: key.fast_mode,
    };
    validate_access(inner, &access)?;
    Ok(access)
}
impl Operations {
    pub fn revalidate_api_key(&self, access: &ClientAccess) -> Result<()> {
        validate_access(&self.inner.lock().unwrap(), access).map(|_| ())
    }
    pub fn create_api_key(&self, input: ApiKeyCreate) -> Result<ApiKeyCreated> {
        if !valid_name(&input.name)
            || input
                .expires_in_days
                .is_some_and(|days| !(1..=365).contains(&days))
            || input.models.len() > 32
            || input.models.iter().any(|s| !model_name(s))
            || input.max_concurrency > 256
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "名称应为 1–60 字符；模型可留空，最多 32 个确切 ID，每个最长 128 字符；默认长期有效，指定有效期时为 1–365 天；Key 并发上限为 0–256。",
            ));
        }
        let mut models = input.models;
        models.sort();
        models.dedup();
        let mut inner = self.inner.lock().unwrap();
        if !inner
            .settings
            .groups
            .iter()
            .any(|g| g.id == input.group_id && g.enabled)
        {
            return Err(Error::new("INVALID_INPUT", "请选择一个存在且启用的分组。"));
        }
        let count: i64 = inner
            .db
            .query_row("SELECT COUNT(*) FROM api_keys", [], |r| r.get(0))
            .map_err(storage)?;
        if count >= 1000 {
            return Err(Error::new(
                "INVALID_INPUT",
                "实验版本最多保留 1000 条 API Key 记录。",
            ));
        }
        let secret = format!("{PREFIX}{}", crate::secret());
        let key = ApiKeyView {
            id: Uuid::new_v4(),
            name: input.name.trim().into(),
            prefix: format!("{}…", &secret[..PREFIX.len() + 8]),
            secret: None,
            group_id: input.group_id,
            models,
            state: "active".into(),
            created_at: crate::now_ms(),
            expires_at: input
                .expires_in_days
                .map(|days| crate::now_ms() + u64::from(days) * DAY),
            last_authenticated_at: None,
            revision: 1,
            max_concurrency: input.max_concurrency,
            fast_mode: input.fast_mode,
        };
        let hash = Sha256::digest(secret.as_bytes());
        let saved = inner.db.execute("INSERT INTO api_keys(id,name,prefix,secret_hash,group_id,models,state,created_at,expires_at,revision,max_concurrency,secret,fast_mode) VALUES(?1,?2,?3,?4,?5,?6,'active',?7,?8,1,?9,?10,?11)",
            params![key.id.to_string(), key.name, key.prefix, hash.as_slice(), key.group_id.to_string(), serde_json::to_string(&key.models)?, key.created_at as i64, key.expires_at.unwrap_or(NEVER_EXPIRES) as i64, key.max_concurrency, secret, key.fast_mode]);
        if let Err(error) = saved {
            inner.storage_healthy = false;
            return Err(storage(error));
        }
        Ok(ApiKeyCreated { key, secret })
    }
    pub fn list_api_keys(&self) -> Result<Vec<ApiKeyView>> {
        let inner = self.inner.lock().unwrap();
        let mut statement = inner
            .db
            .prepare(&format!(
                "SELECT {COLUMNS},secret FROM api_keys WHERE state!='revoked' ORDER BY created_at DESC,id DESC"
            ))
            .map_err(storage)?;
        statement
            .query_map([], |row| {
                let mut key = decode(row)?;
                key.secret = Some(row.get::<_, String>(12)?);
                Ok(key)
            })
            .map_err(storage)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage)
    }
    pub fn update_api_key(&self, id: Uuid, input: ApiKeyUpdate) -> Result<ApiKeyView> {
        if input
            .state
            .as_deref()
            .is_some_and(|state| !["active", "paused", "revoked"].contains(&state))
        {
            return Err(Error::new("INVALID_INPUT", "密钥状态无效。"));
        }
        if input.name.as_ref().is_some_and(|name| !valid_name(name))
            || input.max_concurrency.is_some_and(|limit| limit > 256)
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "密钥名称应为 1–60 个可见字符，并发上限应为 0–256 的整数。",
            ));
        }
        if input.state.is_none()
            && input.name.is_none()
            && input.group_id.is_none()
            && input.max_concurrency.is_none()
            && input.fast_mode.is_none()
        {
            return Err(Error::new("INVALID_INPUT", "请提供需要修改的密钥设置。"));
        }
        let mut inner = self.inner.lock().unwrap();
        let mut key = inner
            .db
            .query_row(
                &format!("SELECT {COLUMNS} FROM api_keys WHERE id=?1"),
                [id.to_string()],
                decode,
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| Error::new("NOT_FOUND", "API Key 不存在。"))?;
        let settings_changed = input.name.is_some()
            || input.group_id.is_some()
            || input.max_concurrency.is_some()
            || input.fast_mode.is_some();
        if key.state == "revoked" && input.state.as_deref() == Some("revoked") && !settings_changed
        {
            return Ok(key);
        }
        if key.revision != input.revision {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "密钥设置已变化，请重新打开编辑窗口。",
            ));
        }
        let state = input.state.unwrap_or_else(|| key.state.clone());
        if key.state == "revoked"
            || (key.state == "expired" && (state != "revoked" || settings_changed))
        {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "已失效的密钥不能编辑或重新启用，请创建新密钥。",
            ));
        }
        let group_id = input.group_id.unwrap_or(key.group_id);
        if group_id != key.group_id
            && !inner
                .settings
                .groups
                .iter()
                .any(|g| g.id == group_id && g.enabled)
        {
            return Err(Error::new("INVALID_INPUT", "请选择存在且启用的分组。"));
        }
        let name = input
            .name
            .map(|name| name.trim().to_owned())
            .unwrap_or_else(|| key.name.clone());
        let max_concurrency = input.max_concurrency.unwrap_or(key.max_concurrency);
        let fast_mode = input.fast_mode.unwrap_or(key.fast_mode);
        if key.state == state
            && key.name == name
            && key.group_id == group_id
            && key.max_concurrency == max_concurrency
            && key.fast_mode == fast_mode
        {
            return Ok(key);
        }
        // All editor fields commit together; a stale or invalid change never
        // leaves a renamed key with the old group or concurrency settings.
        if let Err(error) = inner.db.execute("UPDATE api_keys SET name=?1,state=?2,group_id=?3,max_concurrency=?4,revision=revision+1,fast_mode=?7 WHERE id=?5 AND revision=?6",
            params![name,state,group_id.to_string(),max_concurrency,id.to_string(),input.revision as i64,fast_mode]) {
            inner.storage_healthy = false; inner.changed.notify_waiters(); return Err(storage(error));
        }
        if state == "revoked" {
            inner.runtime.scheduling.invalidate_key(id);
        }
        key.name = name;
        key.state = state;
        key.group_id = group_id;
        key.max_concurrency = max_concurrency;
        key.fast_mode = fast_mode;
        key.revision += 1;
        inner.changed.notify_waiters();
        Ok(key)
    }
    pub fn set_api_key_group(&self, id: Uuid, input: ApiKeyGroup) -> Result<ApiKeyView> {
        let mut inner = self.inner.lock().unwrap();
        let mut key = inner
            .db
            .query_row(
                &format!("SELECT {COLUMNS} FROM api_keys WHERE id=?1"),
                [id.to_string()],
                decode,
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| Error::new("NOT_FOUND", "API Key 不存在。"))?;
        if key.revision != input.revision || !["active", "paused"].contains(&key.state.as_str()) {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "密钥已变化、过期或撤销，请刷新后重试。",
            ));
        }
        if !inner
            .settings
            .groups
            .iter()
            .any(|g| g.id == input.group_id && g.enabled)
        {
            return Err(Error::new("INVALID_INPUT", "请选择存在且启用的分组。"));
        }
        if key.group_id == input.group_id {
            return Ok(key);
        }
        if let Err(error) = inner.db.execute(
            "UPDATE api_keys SET group_id=?1,revision=revision+1 WHERE id=?2 AND revision=?3",
            params![
                input.group_id.to_string(),
                id.to_string(),
                input.revision as i64
            ],
        ) {
            inner.storage_healthy = false;
            inner.changed.notify_waiters();
            return Err(storage(error));
        }
        key.group_id = input.group_id;
        key.revision += 1;
        inner.changed.notify_waiters();
        Ok(key)
    }
    pub fn delete_api_key(&self, id: Uuid, revision: u64) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let current: Option<i64> = inner
            .db
            .query_row(
                "SELECT revision FROM api_keys WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        let Some(current) = current else {
            return Ok(());
        };
        if u64::try_from(current).ok() != Some(revision) {
            return Err(Error::new("CONFIG_CONFLICT", "密钥已变化，请刷新后重试。"));
        }
        // Request records intentionally retain their key_id for historical statistics.
        if let Err(error) = inner.db.execute(
            "DELETE FROM api_keys WHERE id=?1 AND revision=?2",
            params![id.to_string(), revision as i64],
        ) {
            inner.storage_healthy = false;
            inner.changed.notify_waiters();
            return Err(storage(error));
        }
        inner.changed.notify_waiters();
        Ok(())
    }
    pub fn set_api_key_concurrency(
        &self,
        id: Uuid,
        input: ApiKeyConcurrency,
    ) -> Result<ApiKeyView> {
        if input.max_concurrency > 256 {
            return Err(Error::new(
                "INVALID_INPUT",
                "Key 并发上限应为 0–256 的整数，0 表示不单独限制调用方并发。",
            ));
        }
        let mut inner = self.inner.lock().unwrap();
        let mut key = inner
            .db
            .query_row(
                &format!("SELECT {COLUMNS} FROM api_keys WHERE id=?1"),
                [id.to_string()],
                decode,
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| Error::new("NOT_FOUND", "API Key 不存在。"))?;
        if key.revision != input.revision || !["active", "paused"].contains(&key.state.as_str()) {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "密钥已变化、过期或撤销，请刷新后重试。",
            ));
        }
        if key.max_concurrency == input.max_concurrency {
            return Ok(key);
        }
        if let Err(error) = inner.db.execute("UPDATE api_keys SET max_concurrency=?1,revision=revision+1 WHERE id=?2 AND revision=?3",
            params![input.max_concurrency, id.to_string(), input.revision as i64]) {
            inner.storage_healthy = false;
            inner.changed.notify_waiters();
            return Err(storage(error));
        }
        key.max_concurrency = input.max_concurrency;
        key.revision += 1;
        inner.changed.notify_waiters();
        Ok(key)
    }
    /// No credential cache: revocation, expiry and group pauses affect the next request.
    pub fn authenticate_api_key(&self, secret: &str) -> Result<ClientAccess> {
        if !valid_secret(secret) {
            return Err(invalid_key());
        }
        let hash = Sha256::digest(secret.as_bytes());
        let mut inner = self.inner.lock().unwrap();
        let key = inner
            .db
            .query_row(
                &format!("SELECT {COLUMNS} FROM api_keys WHERE secret_hash=?1"),
                [hash.as_slice()],
                decode,
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(invalid_key)?;
        if key.state != "active" {
            return Err(invalid_key());
        }
        if !inner
            .settings
            .groups
            .iter()
            .any(|g| g.id == key.group_id && g.enabled)
        {
            return Err(Error::new(
                "API_KEY_GROUP_DISABLED",
                "此 API Key 的分组不可用。",
            ));
        }
        if inner.draining {
            return Err(Error::new("SERVICE_DRAINING", "服务正在停止。"));
        }
        let now = crate::now_ms();
        // Coalesce metadata writes without caching permission decisions.
        if key
            .last_authenticated_at
            .is_none_or(|last| now.saturating_sub(last) >= 60_000)
        {
            if let Err(error) = inner.db.execute(
                "UPDATE api_keys SET last_authenticated_at=?1 WHERE id=?2",
                params![now as i64, key.id.to_string()],
            ) {
                inner.storage_healthy = false;
                return Err(storage(error));
            }
        }
        Ok(ClientAccess {
            key_id: key.id,
            group_id: key.group_id,
            models: key.models,
            fast_mode: key.fast_mode,
        })
    }
}

#[cfg(test)]
mod fast_mode_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forced_fast_tier_overrides_only_the_top_level_and_reaches_native_turns() {
        let mut access = ClientAccess {
            key_id: Uuid::nil(),
            group_id: Uuid::nil(),
            models: vec![],
            fast_mode: true,
        };
        for tier in [
            None,
            Some(json!(null)),
            Some(json!("default")),
            Some(json!("auto")),
            Some(json!("flex")),
            Some(json!("priority")),
        ] {
            let mut body = json!({"model":"m","input":"hello","metadata":{"service_tier":"flex"},"reasoning":{"effort":"high"}});
            if let Some(tier) = tier {
                body["service_tier"] = tier;
            }
            let original = body.clone();
            access.fast_mode = false;
            assert!(!access.apply_fast_mode(&mut body));
            assert_eq!(body, original);
            access.fast_mode = true;
            assert!(access.apply_fast_mode(&mut body));
            assert_eq!(body["service_tier"], "priority");
            assert_eq!(body["metadata"], original["metadata"]);
            let request = crate::responses::Request::parse(body).unwrap();
            let mut turn = json!({});
            request.apply_turn(&mut turn);
            assert_eq!(
                turn[if crate::native_cache::enabled() {
                    "serviceTierForTurn"
                } else {
                    "serviceTier"
                }],
                "priority"
            );
            assert_eq!(turn["effort"], "high");
        }
    }
}
