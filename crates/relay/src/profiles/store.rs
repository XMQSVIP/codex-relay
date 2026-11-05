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
        let mut records: Vec<Profile> = match tokio::fs::read(root.join("profiles.json")).await {
            Ok(data) => serde_json::from_slice(&data)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        for profile in &mut records {
            profile.resolve_name();
        }
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
            records: Arc::new(Mutex::new(records)),
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
        self.create_configured(name, source, None, "", false).await
    }
    pub(crate) async fn create_local_once(
        &self,
        name: &str,
        request: uuid::Uuid,
        note: &str,
    ) -> Result<Profile> {
        if request.is_nil() || request == crate::operations::DESKTOP_ACCOUNT {
            return Err(Error::new("INVALID_INPUT", "连接请求标识无效。"));
        }
        self.create_configured(name, "native-config", Some(request), note, false)
            .await
    }
    pub(crate) async fn create_configured(
        &self,
        name: &str,
        source: &str,
        request: Option<uuid::Uuid>,
        note: &str,
        auto_name: bool,
    ) -> Result<Profile> {
        let name = if !note.trim().is_empty() {
            note.trim()
        } else if auto_name && name.trim().is_empty() {
            "未命名账户"
        } else {
            name.trim()
        };
        validate_labels(name, note, auto_name, source)?;
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
        let mut profile = Profile {
            id: request.unwrap_or_else(uuid::Uuid::new_v4),
            name: name.into(),
            note: if note.trim().is_empty() && !auto_name {
                name.into()
            } else {
                note.trim().into()
            },
            email: None,
            auto_name,
            metadata_revision: 0,
            created_at: crate::now_ms(),
            source: source.into(),
            connection_request_id: request,
        };
        profile.resolve_name();
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
        next[index].resolve_name();
        if next[index].auto_name || next[index].name != records[index].name {
            next[index].auto_name = false;
            next[index].metadata_revision += 1;
        }
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

impl Profile {
    fn resolve_name(&mut self) {
        self.name = if !self.note.is_empty() {
            self.note.clone()
        } else if let Some(prefix) = self.email.as_deref().and_then(email_prefix) {
            prefix.into()
        } else {
            match self.source.as_str() {
                "native-config" => "本机 Codex",
                "api-key" => "API Key 账户",
                _ => "未命名账户",
            }
            .into()
        };
    }
}

#[derive(Clone)]
pub(super) struct Metadata {
    root: PathBuf,
    records: Arc<Mutex<Vec<Profile>>>,
    operations: Option<Arc<crate::operations::Operations>>,
}

pub(crate) fn validate_labels(name: &str, note: &str, auto_name: bool, source: &str) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > 500 || name.chars().any(char::is_control) {
        return Err(Error::new(
            "INVALID_INPUT",
            "账户名称应为 1–500 个可见字符。",
        ));
    }
    if note.chars().count() > 500 || note.chars().any(char::is_control) {
        return Err(Error::new(
            "INVALID_INPUT",
            "账户备注最多 500 个字符，不能包含控制字符。",
        ));
    }
    if auto_name && source != "isolated" {
        return Err(Error::new(
            "INVALID_INPUT",
            "仅独立 ChatGPT 账户支持自动命名。",
        ));
    }
    Ok(())
}

impl Profiles {
    pub(super) fn metadata_store(&self) -> Metadata {
        Metadata {
            root: self.root.clone(),
            records: self.records.clone(),
            operations: self.proxy_settings.clone(),
        }
    }
    pub(crate) async fn update_metadata(
        &self,
        id: uuid::Uuid,
        revision: u64,
        note: &str,
    ) -> Result<Profile> {
        self.metadata_store()
            .update(id, |profile| {
                if profile.metadata_revision != revision {
                    return Err(Error::new(
                        "CONFIG_CONFLICT",
                        "账户备注已更新，请刷新后重试。",
                    ));
                }
                validate_labels("备注", note, false, &profile.source)?;
                profile.auto_name = note.trim().is_empty() && profile.source == "isolated";
                profile.note = if note.trim().is_empty() {
                    profile
                        .email
                        .as_deref()
                        .and_then(email_prefix)
                        .unwrap_or("")
                        .into()
                } else {
                    note.trim().into()
                };
                Ok(())
            })
            .await
    }
    pub(crate) async fn restore_email(&self, id: uuid::Uuid, email: Option<&str>) -> Result<()> {
        self.metadata_store()
            .update(id, |profile| {
                if let Some(email) = email {
                    if profile.source != "isolated" || email_prefix(email).is_none() {
                        return Err(Error::new("INVALID_INPUT", "账户邮箱无效。"));
                    }
                    profile.email = Some(email.into());
                }
                Ok(())
            })
            .await?;
        Ok(())
    }
}
impl Metadata {
    async fn update(
        &self,
        id: uuid::Uuid,
        edit: impl FnOnce(&mut Profile) -> Result<()>,
    ) -> Result<Profile> {
        let mut records = self.records.lock().await;
        let mut next = records.clone();
        let profile = next
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        edit(profile)?;
        profile.resolve_name();
        let previous = records.iter().find(|p| p.id == id).unwrap();
        let changed = profile.name != previous.name
            || profile.note != previous.note
            || profile.auto_name != previous.auto_name
            || profile.email != previous.email;
        if changed {
            profile.metadata_revision += 1;
        }
        let result = profile.clone();
        if changed {
            crate::oauth::save(
                &self.root.join("profiles.json"),
                &serde_json::to_value(&next)?,
            )
            .await?;
            *records = next;
        }
        if let Some(operations) = &self.operations {
            operations.rename_account(id, &result.name)?;
        }
        Ok(result)
    }
    pub(super) async fn name_from_account(
        &self,
        id: uuid::Uuid,
        account: &serde_json::Value,
    ) -> Result<()> {
        let Some(email) = account["email"].as_str().map(str::trim) else {
            return Ok(());
        };
        let Some(prefix) = email_prefix(email) else {
            return Ok(());
        };
        if account["type"] != "chatgpt" {
            return Ok(());
        }
        self.update(id, |profile| {
            if profile.auto_name && profile.source == "isolated" {
                profile.note = prefix.into();
                profile.name = prefix.into();
            }
            profile.email = Some(email.into());
            Ok(())
        })
        .await?;
        Ok(())
    }
}

fn email_prefix(email: &str) -> Option<&str> {
    if email.len() > 254
        || email.chars().any(char::is_whitespace)
        || email.chars().any(char::is_control)
    {
        return None;
    }
    email
        .split_once('@')
        .filter(|(prefix, domain)| !prefix.is_empty() && !domain.is_empty())
        .map(|(prefix, _)| prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn empty_notes_resolve_names_from_source_and_saved_names_do_not_override_notes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("accounts");
        let profiles = Profiles::open(root.clone()).await.unwrap();
        let local = profiles.create_native_config("备注").await.unwrap();
        let saved = profiles.update_metadata(local.id, 0, "").await.unwrap();
        assert_eq!(saved.name, "本机 Codex");
        assert!(saved.note.is_empty());
        let custom = profiles.create("用户备注").await.unwrap();
        drop(profiles);
        let file = root.join("profiles.json");
        let mut records: Vec<Profile> =
            serde_json::from_slice(&tokio::fs::read(&file).await.unwrap()).unwrap();
        for record in &mut records {
            record.name = "不应作为名称的状态文字".into();
        }
        crate::oauth::save(&file, &serde_json::to_value(records).unwrap())
            .await
            .unwrap();
        let reopened = Profiles::open(root).await.unwrap();
        let names = reopened.metadata().await;
        assert!(names.contains(&(local.id, "本机 Codex".into())));
        assert!(names.contains(&(custom.id, "用户备注".into())));
    }
    #[tokio::test]
    async fn default_notes_use_email_prefix_custom_notes_win_and_email_persists() {
        let temp = tempfile::tempdir().unwrap();
        let mut profiles = Profiles::open(temp.path().join("accounts")).await.unwrap();
        let operations =
            crate::operations::Operations::open(&temp.path().join("operations")).unwrap();
        profiles.use_proxy_settings(operations.clone());
        let profile = profiles
            .create_configured("", "isolated", None, "", true)
            .await
            .unwrap();
        operations
            .register_accounts(&profiles.metadata().await)
            .unwrap();
        let metadata = profiles.metadata_store();
        for account in [
            json!({"type":"chatgpt"}),
            json!({"type":"chatgpt","email":""}),
            json!({"type":"chatgpt","email":"a\nb"}),
            json!({"type":"apiKey","email":"ignored@example.test"}),
        ] {
            metadata
                .name_from_account(profile.id, &account)
                .await
                .unwrap();
        }
        assert_eq!(profiles.metadata().await[0].1, "未命名账户");
        metadata
            .name_from_account(profile.id, &json!({"type":"chatgpt","email":"123@xx.com"}))
            .await
            .unwrap();
        let saved = profiles.list().await.remove(0).profile;
        assert_eq!(saved.name, "123");
        assert_eq!(saved.note, "123");
        assert_eq!(saved.email.as_deref(), Some("123@xx.com"));
        assert_eq!(operations.snapshot()["accounts"][0]["name"], "123");
        let edited = profiles
            .update_metadata(profile.id, saved.metadata_revision, "生产备用")
            .await
            .unwrap();
        metadata
            .name_from_account(
                profile.id,
                &json!({"type":"chatgpt","email":"next@example.test"}),
            )
            .await
            .unwrap();
        let updated = profiles.list().await.remove(0).profile;
        assert_eq!(updated.name, "生产备用");
        assert_eq!(updated.note, "生产备用");
        assert_eq!(updated.email.as_deref(), Some("next@example.test"));
        assert!(
            profiles
                .update_metadata(profile.id, edited.metadata_revision, "旧表单")
                .await
                .is_err()
        );
        profiles
            .update_metadata(profile.id, updated.metadata_revision, "")
            .await
            .unwrap();
        let reopened = Profiles::open(temp.path().join("accounts")).await.unwrap();
        let saved = reopened.list().await.remove(0).profile;
        assert_eq!(saved.name, "next");
        assert_eq!(saved.note, "next");
        assert_eq!(saved.email.as_deref(), Some("next@example.test"));
        assert!(saved.auto_name);
        assert_eq!(reopened.list().await[0].state, "stopped");
    }
}
