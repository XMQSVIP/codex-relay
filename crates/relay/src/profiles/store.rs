//! Account metadata, directory creation and ordered deletion.

use super::{Profile, Profiles};
use crate::{
    error::{Error, Result},
    platform,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

impl Profiles {
    pub async fn metadata(&self) -> Vec<(uuid::Uuid, String)> {
        self.records
            .lock()
            .await
            .iter()
            .map(|p| (p.id, p.name.clone()))
            .collect()
    }
    pub async fn open(root: PathBuf) -> Result<Self> {
        crate::native_cache::limits()?;
        tokio::fs::create_dir_all(&root).await?;
        platform::protect_directory(&root).await?;
        let records: Vec<Profile> = match tokio::fs::read(root.join("profiles.json")).await {
            Ok(data) => serde_json::from_slice(&data)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        #[cfg(unix)]
        for profile in &records {
            let directory = root.join(profile.id.to_string());
            for path in [
                &directory,
                &directory.join("home"),
                &directory.join("workspace"),
            ] {
                if tokio::fs::try_exists(path).await? {
                    platform::protect_directory(path).await?;
                }
            }
        }
        Ok(Self {
            proxy_settings: None,
            root,
            records: Mutex::new(records),
            slots: Mutex::new(HashMap::new()),
            response_slots: Mutex::new(HashMap::new()),
            response_preparations: Mutex::new(HashMap::new()),
            native_tool_gates: Default::default(),
            auth_binding: Mutex::new(()),
            standby_start_limit: tokio::sync::Semaphore::new(2),
            deletion: Mutex::new(()),
            inference: Default::default(),
            continuations: Default::default(),
            catalog_refresh: Default::default(),
            runtime_clients: Default::default(),
            logins: Default::default(),
        })
    }
    pub async fn create(&self, name: &str) -> Result<Profile> {
        self.create_with_source(name, "isolated").await
    }
    pub async fn create_native_config(&self, name: &str) -> Result<Profile> {
        self.create_with_source(name, "native-config").await
    }
    pub(super) async fn create_with_source(&self, name: &str, source: &str) -> Result<Profile> {
        self.create_with_request(name, source, None).await
    }
    pub(crate) async fn create_local_once(
        &self,
        name: &str,
        request: uuid::Uuid,
    ) -> Result<Profile> {
        if request.is_nil() || request == crate::operations::DESKTOP_ACCOUNT {
            return Err(Error::new("INVALID_INPUT", "连接请求标识无效。"));
        }
        self.create_with_request(name, "native-config", Some(request))
            .await
    }
    async fn create_with_request(
        &self,
        name: &str,
        source: &str,
        request: Option<uuid::Uuid>,
    ) -> Result<Profile> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
            return Err(Error::new(
                "INVALID_INPUT",
                "账户名称应为 1–60 个可见字符。",
            ));
        }
        let mut records = self.records.lock().await;
        if let Some(existing) = request.and_then(|id| records.iter().find(|p| p.id == id)) {
            if existing.connection_request_id != request || existing.source != "native-config" {
                return Err(Error::new(
                    "CONFIG_CONFLICT",
                    "连接请求标识已被使用，请重新添加。",
                ));
            }
            return Ok(existing.clone());
        }
        if records.len() >= 32 {
            return Err(Error::new("INVALID_INPUT", "实验版本最多配置 32 个账户。"));
        }
        let profile = Profile {
            id: request.unwrap_or_else(uuid::Uuid::new_v4),
            name: name.into(),
            created_at: crate::now_ms(),
            source: source.into(),
            connection_request_id: request,
        };
        let directory = self.root.join(profile.id.to_string());
        tokio::fs::create_dir_all(directory.join("home")).await?;
        tokio::fs::create_dir_all(directory.join("workspace")).await?;
        for path in [
            &directory,
            &directory.join("home"),
            &directory.join("workspace"),
        ] {
            platform::protect_directory(path).await?;
        }
        // The native backend owns OAuth persistence. The gateway stores no token in its metadata database.
        tokio::fs::write(directory.join("home/config.toml"),"cli_auth_credentials_store = \"file\"\n[features]\nsecret_auth_storage = false\napps = false\nplugins = false\nhooks = false\nshell_tool = false\nmulti_agent = false\nbrowser_use = false\ncomputer_use = false\n").await?;
        let mut updated = records.clone();
        updated.push(profile.clone());
        tokio::fs::write(
            self.root.join("profiles.json.tmp"),
            serde_json::to_vec_pretty(&updated)?,
        )
        .await?;
        tokio::fs::rename(
            self.root.join("profiles.json.tmp"),
            self.root.join("profiles.json"),
        )
        .await?;
        *records = updated;
        Ok(profile)
    }
    pub(crate) async fn local_target(&self, id: uuid::Uuid) -> Result<Profile> {
        let slot = self.slot(id).await?;
        let worker = slot.lock().await;
        if worker.is_some() {
            return Err(Error::new(
                "BUSY",
                "账户实例或登录任务仍在运行，请先停止后再连接。",
            ));
        }
        let mut records = self.records.lock().await;
        let index = records
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        if records[index].source == "native-config" {
            return Ok(records[index].clone());
        }
        if records[index].source != "isolated" {
            return Err(Error::new("INVALID_INPUT", "此账户不能切换连接方式。"));
        }
        for name in ["auth.json", "bridge-auth.json", "bridge-token.json"] {
            if tokio::fs::try_exists(self.home(id).join(name)).await? {
                return Err(Error::new(
                    "LOCAL_ACCOUNT_NOT_EMPTY",
                    "账户已有凭据，请添加新的本机账户；现有凭据不会被覆盖。",
                ));
            }
        }
        let mut next = records.clone();
        next[index].source = "native-config".into();
        crate::oauth::save(
            &self.root.join("profiles.json"),
            &serde_json::to_value(&next)?,
        )
        .await?;
        *records = next;
        Ok(records[index].clone())
    }
    /// Called only after operations has durably blocked admission for this ID.
    pub async fn delete(&self, id: uuid::Uuid) -> Result<()> {
        let _deletion = self.deletion.lock().await;
        // 删除恢复可能已移除profiles记录但尚未完成operations结算；该阶段无可启动实例。
        // 保留幂等恢复，其他停止/进程清理错误仍必须阻止删除。
        match self.stop(id).await {
            Err(error) if error.code == "NOT_FOUND" => {}
            result => result?,
        }
        // Retain the slot until completion so an already acquired reference cannot
        // bypass serialization. Lock order matches start: worker, then records.
        let slot = self
            .slots
            .lock()
            .await
            .entry(id)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone();
        let mut worker = slot.lock().await;
        self.catalog_refresh.stop(id).await;
        if let Some(backend) = worker.as_ref() {
            backend.stop().await?;
        }
        *worker = None;
        let mut records = self.records.lock().await;
        let directory = self.root.join(id.to_string());
        match tokio::fs::symlink_metadata(&directory).await {
            Ok(metadata) => {
                let root = tokio::fs::canonicalize(&self.root).await?;
                let target = tokio::fs::canonicalize(&directory).await?;
                if metadata.file_type().is_symlink()
                    || target.parent() != Some(root.as_path())
                    || target.file_name() != Some(std::ffi::OsStr::new(&id.to_string()))
                {
                    return Err(Error::new(
                        "PROFILE_PATH",
                        "实例目录不在预期位置，已停止删除。",
                    ));
                }
                tokio::fs::remove_dir_all(&directory).await.map_err(|_| {
                    Error::new(
                        "PROFILE_DELETE_FAILED",
                        "实例文件清理失败，请检查目录占用或权限后重试删除。",
                    )
                })?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut updated = records.clone();
        updated.retain(|p| p.id != id);
        tokio::fs::write(
            self.root.join("profiles.json.tmp"),
            serde_json::to_vec_pretty(&updated)?,
        )
        .await?;
        tokio::fs::rename(
            self.root.join("profiles.json.tmp"),
            self.root.join("profiles.json"),
        )
        .await?;
        *records = updated;
        self.slots.lock().await.remove(&id);
        self.response_slots.lock().await.remove(&id);
        self.response_preparations.lock().await.remove(&id);
        self.invalidate_subscription(id);
        Ok(())
    }
    pub(super) async fn source(&self, id: uuid::Uuid) -> Result<String> {
        self.records
            .lock()
            .await
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.source.clone())
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))
    }
    pub(super) async fn require_source(&self, id: uuid::Uuid, source: &str) -> Result<()> {
        if self.source(id).await? != source {
            return Err(Error::new(
                "INVALID_INPUT",
                "此操作不适用于该实例的配置来源。",
            ));
        }
        Ok(())
    }
}
