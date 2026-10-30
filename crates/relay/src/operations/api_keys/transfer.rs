//! Portable client keys: preserve credentials and scopes without overwriting existing keys.

use super::{
    COLUMNS, NEVER_EXPIRES, Operations, PREFIX, decode, model_name, storage, valid_name,
    valid_secret,
};
use crate::error::{Error, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupKey {
    name: String,
    secret: String,
    group_id: Uuid,
    models: Vec<String>,
    state: String,
    created_at: u64,
    expires_at: Option<u64>,
    max_concurrency: u32,
    fast_mode: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyBackup {
    version: u32,
    keys: Vec<BackupKey>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiKeyImport {
    backup: ApiKeyBackup,
    /// Explicitly selected destination for imports from another installation.
    group_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyImportResult {
    pub imported: usize,
    pub skipped: usize,
}

impl Operations {
    pub fn export_api_keys(&self) -> Result<ApiKeyBackup> {
        let inner = self.inner.lock().unwrap();
        let mut statement = inner.db.prepare(&format!(
            "SELECT {COLUMNS},secret FROM api_keys WHERE state!='revoked' ORDER BY created_at DESC,id DESC"
        )).map_err(storage)?;
        let keys = statement
            .query_map([], |row| {
                let key = decode(row)?;
                Ok(BackupKey {
                    name: key.name,
                    secret: row.get(12)?,
                    group_id: key.group_id,
                    models: key.models,
                    // Keep the stored state even if the key is currently expired.
                    state: row.get(5)?,
                    created_at: key.created_at,
                    expires_at: key.expires_at,
                    max_concurrency: key.max_concurrency,
                    fast_mode: key.fast_mode,
                })
            })
            .map_err(storage)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage)?;
        Ok(ApiKeyBackup { version: 1, keys })
    }

    pub fn import_api_keys(&self, input: ApiKeyImport) -> Result<ApiKeyImportResult> {
        if input.backup.version != 1
            || input.backup.keys.is_empty()
            || input.backup.keys.len() > 1000
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "请选择版本为 1 的密钥备份，每批支持 1–1000 个密钥。",
            ));
        }
        let now = crate::now_ms();
        // Validate before writing. Errors identify the row, never its credential.
        for (index, key) in input.backup.keys.iter().enumerate() {
            if !valid_name(&key.name)
                || !valid_secret(&key.secret)
                || key.models.len() > 32
                || key.models.iter().any(|s| !model_name(s))
                || !["active", "paused"].contains(&key.state.as_str())
                || key.max_concurrency > 256
                || key.created_at > now
                || key.expires_at.is_some_and(|at| at >= NEVER_EXPIRES)
            {
                return Err(Error::new(
                    "INVALID_INPUT",
                    &format!(
                        "第 {} 项密钥格式或配置无效，请检查名称、完整密钥、模型、状态、时间和并发上限。",
                        index + 1
                    ),
                ));
            }
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.draining {
            return Err(Error::new("SERVICE_DRAINING", "服务正在退出，请稍后导入。"));
        }
        let groups = inner.settings.groups.clone();
        if input
            .group_id
            .is_some_and(|id| !groups.iter().any(|g| g.id == id && g.enabled))
        {
            return Err(Error::new("INVALID_INPUT", "请选择存在且启用的目标分组。"));
        }
        let tx = inner.db.transaction().map_err(storage)?;
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM api_keys", [], |row| row.get(0))
            .map_err(storage)?;
        let mut result = ApiKeyImportResult {
            imported: 0,
            skipped: 0,
        };
        for (index, mut key) in input.backup.keys.into_iter().enumerate() {
            let hash = Sha256::digest(key.secret.as_bytes());
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM api_keys WHERE secret_hash=?1)",
                    [hash.as_slice()],
                    |row| row.get(0),
                )
                .map_err(storage)?;
            if exists {
                result.skipped += 1;
                continue;
            }
            let group_id = input.group_id.unwrap_or(key.group_id);
            if !groups.iter().any(|g| g.id == group_id) {
                return Err(Error::new(
                    "INVALID_INPUT",
                    &format!(
                        "第 {} 项的原分组不存在，请选择目标分组后重新导入。",
                        index + 1
                    ),
                ));
            }
            if count + result.imported as i64 >= 1000 {
                return Err(Error::new(
                    "INVALID_INPUT",
                    "导入后将超过 1000 条 API Key 记录上限，本次未导入。",
                ));
            }
            key.models.sort();
            key.models.dedup();
            let prefix = format!("{}…", &key.secret[..PREFIX.len() + 8]);
            tx.execute("INSERT INTO api_keys(id,name,prefix,secret_hash,group_id,models,state,created_at,expires_at,revision,max_concurrency,secret,fast_mode) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,?11,?12)",
                params![Uuid::new_v4().to_string(),key.name.trim(),prefix,hash.as_slice(),group_id.to_string(),serde_json::to_string(&key.models)?,key.state,key.created_at as i64,key.expires_at.unwrap_or(NEVER_EXPIRES) as i64,key.max_concurrency,key.secret,key.fast_mode]).map_err(storage)?;
            result.imported += 1;
        }
        tx.commit().map_err(storage)?;
        inner.changed.notify_waiters();
        Ok(result)
    }
}
