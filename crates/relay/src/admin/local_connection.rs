//! Preview and commit the existing native-config connection without inference.
use super::AdminState;
use crate::{
    backend::Backend,
    error::{Error, Result},
    installation,
    operations::NativeCapabilities,
    profiles::Profile,
    provider::Provider,
};
use axum::{Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Clone)]
struct Detection {
    provider: Provider,
    installation: String,
    capabilities: NativeCapabilities,
}
#[derive(Default)]
pub(super) struct Inspections(HashMap<Uuid, (Instant, Detection)>);
impl Inspections {
    fn remember(&mut self, detection: Detection) -> Value {
        self.0
            .retain(|_, (at, _)| at.elapsed() < Duration::from_secs(600));
        if self.0.len() >= 128 {
            if let Some(id) = self
                .0
                .iter()
                .min_by_key(|(_, (at, _))| at)
                .map(|(id, _)| *id)
            {
                self.0.remove(&id);
            }
        }
        let id = Uuid::new_v4();
        let view = detection.summary(id);
        self.0.insert(id, (Instant::now(), detection));
        view
    }
    fn get(&self, id: Uuid) -> Result<Detection> {
        self.0
            .get(&id)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(600))
            .map(|(_, value)| value.clone())
            .ok_or_else(|| {
                Error::new(
                    "LOCAL_INSPECTION_EXPIRED",
                    "检测结果已过期或服务已重启，请重新检测；已填写内容会保留。",
                )
            })
    }
}
impl Detection {
    fn summary(&self, id: Uuid) -> Value {
        let auth = if self.provider.auth.starts_with(crate::oauth::AUTH) {
            "native-chatgpt"
        } else {
            &self.provider.auth
        };
        // Neither token material nor the ChatGPT identity fingerprint leaves this API.
        json!({"id":id,"provider":self.provider.id,"auth":auth,"model":self.provider.model,
            "endpoint":url::Url::parse(&self.provider.base_url).ok().map(|u|u.origin().ascii_serialization()),"inferenceDispatched":false})
    }
    fn same_connection(&self, other: &Self) -> bool {
        self.provider == other.provider && self.installation == other.installation
    }
}

async fn detect(
    state: &AdminState,
    proxy: &crate::app_server_proxy::ProxyConfig,
) -> Result<Detection> {
    // Waiting behind another inspection must not consume this inspection's own
    // native startup/query budget (e.g. two retries of the same connection).
    let _detection = tokio::time::timeout(Duration::from_secs(45), state.local_detection.lock())
        .await
        .map_err(|_| Error::new("BUSY", "已有本机检测正在进行，请稍后重试。"))?;
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut install = installation::read_with_refresh().await?;
        if let Err(error) = installation::validate(&install).await {
            if install.manually_configured
                || !["CLI_VERSION", "DESKTOP_VERSION", "NATIVE_CLIENT_INFO"]
                    .contains(&error.code.as_str())
            {
                return Err(error);
            }
            install = installation::prepare(None).await?;
            installation::validate(&install).await?;
        }
        let cwd = state.profiles.inspection_directory()?;
        let backend =
            Backend::start_direct_with_proxy(&install, cwd.path(), None, false, proxy).await?;
        let result: Result<Detection> = async {
            let provider = crate::provider::read(&backend.rpc, cwd.path()).await?;
            state.check_route_loop(&provider.base_url).await?;
            let capabilities = crate::operations::runtime::discover(
                &backend.rpc,
                &provider,
                &install.app_server_sha256,
            )
            .await;
            Ok(Detection {
                provider,
                installation: install.app_server_sha256,
                capabilities,
            })
        }
        .await;
        let stopped = backend.stop().await;
        let detection = result?;
        stopped?;
        Ok(detection)
    })
    .await
    .map_err(|_| {
        Error::new(
            "LOCAL_INSPECTION_TIMEOUT",
            "本机检测超时，请检查 Codex 和网络连接后重新检测。",
        )
    })?
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct InspectInput {
    account_id: Option<Uuid>,
}

async fn detect_account(state: &AdminState, account_id: Option<Uuid>) -> Result<Detection> {
    let lease = account_id
        .map(|id| {
            state
                .operations
                .begin_account_maintenance(id, "native_check", "local_connection")
        })
        .transpose()?;
    let proxy = account_id
        .map(|id| state.operations.account_proxy(id))
        .unwrap_or_default();
    let detected = detect(state, &proxy).await;
    match lease {
        Some(lease) => lease.complete(detected),
        None => detected,
    }
}

pub(super) async fn inspect(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<InspectInput>,
) -> Result<Json<Value>> {
    let detection = detect_account(&state, input.account_id).await?;
    Ok(Json(
        state.local_inspections.lock().await.remember(detection),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ConnectInput {
    request_id: Uuid,
    inspection_id: Uuid,
    account_id: Option<Uuid>,
    name: String,
    group_id: Uuid,
    revision: u64,
}

pub(super) async fn connect(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<ConnectInput>,
) -> Result<Json<Value>> {
    // Serialize account setup with the login/import/start/delete handlers. Client
    // inference is still admitted independently and checked atomically at commit.
    let expected = state
        .local_inspections
        .lock()
        .await
        .get(input.inspection_id)?;
    let account_id = input.account_id.or_else(|| {
        state.operations.snapshot()["accounts"]
            .as_array()
            .filter(|accounts| {
                accounts
                    .iter()
                    .any(|a| a["id"] == input.request_id.to_string())
            })
            .map(|_| input.request_id)
    });
    let current = detect_account(&state, account_id).await?;
    if !expected.same_connection(&current) {
        return Ok(Json(
            json!({"status":"changed","inspection":state.local_inspections.lock().await.remember(current),
            "message":"本机配置或账户已变化，请确认新的检测结果后再次连接。"}),
        ));
    }
    let _setup = state.account_setup.lock().await;
    let result = commit(&state, &input, current).await?;
    if result["status"] == "enabled" {
        super::activation::schedule_start(&state, input.account_id.unwrap_or(input.request_id));
    }
    Ok(Json(result))
}

async fn commit(state: &AdminState, input: &ConnectInput, detection: Detection) -> Result<Value> {
    let id = input.account_id.unwrap_or(input.request_id);
    let snapshot = state.operations.snapshot();
    if !snapshot["groups"].as_array().is_some_and(|groups| {
        groups
            .iter()
            .any(|group| group["id"] == input.group_id.to_string() && group["enabled"] == true)
    }) {
        return Err(Error::new(
            "LOCAL_GROUP_UNAVAILABLE",
            "所选分组已暂停或不存在，请选择启用的分组。",
        ));
    }
    let policy = snapshot["accounts"]
        .as_array()
        .and_then(|items| items.iter().find(|p| p["id"] == id.to_string()));
    let converting = input.account_id.is_some() && state.profiles.is_oauth(id).await?;
    // A response lost after a successful commit is safe to replay, including
    // across restarts. Never overwrite subsequent policy edits on replay.
    if input.account_id.is_none()
        && policy.is_some_and(|p| {
            p["enabled"] == true
                && p["groupIds"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().any(|id| id == &input.group_id.to_string()))
        })
        && state.operations.configured_provider(id).as_ref() == Some(&detection.provider)
    {
        let profile = state
            .profiles
            .create_local_once(&input.name, input.request_id)
            .await?;
        return Ok(json!({"status":"enabled","account":profile}));
    }
    state.operations.check_local_connection(
        policy.map(|_| id),
        input.group_id,
        input.revision,
        converting,
    )?;
    let profile: Profile = if let Some(id) = input.account_id {
        state.profiles.local_target(id).await?
    } else {
        state
            .profiles
            .create_local_once(&input.name, input.request_id)
            .await?
    };
    let mut enabled = false;
    let result = (|| {
        state
            .operations
            .register_accounts(&[(id, profile.name.clone())])?;
        let revision = input.revision + u64::from(policy.is_none());
        state
            .operations
            .connect_local_route(id, input.group_id, revision, detection.provider)?;
        enabled = true;
        state
            .operations
            .publish_capabilities(id, detection.capabilities)
    })();
    match result {
        Ok(()) => Ok(json!({"status":"enabled","account":profile})),
        Err(error) => Ok(json!({"status":if enabled {"enabled"}else{"pending"},
            "account":profile,"error":error,"message":if enabled {"接入已启用，但能力信息未完成更新。"}else if state.operations.configured_provider(id).is_some() {"更新未完成，原接入保持不变。请重试。"}else{"账户已添加，尚未启用。重试会继续使用这个账户。"}})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{DEFAULT_GROUP, GroupInput};
    fn detection(auth: &str) -> Detection {
        let mut detection = Detection {
            provider: Provider {
                execution: Default::default(),
                id: "OpenAI".into(),
                model: "synthetic-model".into(),
                base_url: "https://example.invalid/v1".into(),
                auth: auth.into(),
                forward_headers: vec![],
                required_headers: vec![],
                mcp_servers: vec![],
            },
            installation: "synthetic-install".into(),
            capabilities: Default::default(),
        };
        detection.capabilities.provider =
            crate::operations::runtime::fingerprint(&detection.provider);
        detection.capabilities.installation = detection.installation.clone();
        detection
    }
    fn input(state: &AdminState) -> ConnectInput {
        ConnectInput {
            request_id: Uuid::new_v4(),
            inspection_id: Uuid::new_v4(),
            account_id: None,
            name: "local".into(),
            group_id: DEFAULT_GROUP,
            revision: state.operations.snapshot()["revision"].as_u64().unwrap(),
        }
    }
    async fn state(root: &std::path::Path) -> Arc<AdminState> {
        AdminState::new_with_network(4317, root.join("accounts"), None, vec![])
            .await
            .unwrap()
    }
    #[test]
    fn preview_masks_identity_and_detects_provider_identity_and_install_changes() {
        let first = detection("native-chatgpt:PRIVATE_IDENTITY:local-config");
        let summary = first.summary(Uuid::new_v4()).to_string();
        assert!(!summary.contains("PRIVATE_IDENTITY"));
        assert!(summary.contains("native-chatgpt"));
        let mut second = first.clone();
        second.provider.auth = "native-chatgpt:OTHER_IDENTITY:local-config".into();
        assert!(!first.same_connection(&second));
        second = first.clone();
        second.provider.base_url = "https://other.invalid/v1".into();
        assert!(!first.same_connection(&second));
        second = first.clone();
        second.installation = "updated".into();
        assert!(!first.same_connection(&second));
        for auth in [
            "native-api-key",
            "native-bearer",
            "native-provider-auth",
            "native-headers",
            "none",
        ] {
            assert_eq!(detection(auth).summary(Uuid::new_v4())["auth"], auth);
        }
    }
    #[test]
    fn preview_expiry_and_bounded_storage() {
        let mut previews = Inspections::default();
        let view = previews.remember(detection("native-bearer"));
        let id = Uuid::parse_str(view["id"].as_str().unwrap()).unwrap();
        previews.0.get_mut(&id).unwrap().0 = Instant::now() - Duration::from_secs(601);
        assert_eq!(
            previews.get(id).err().unwrap().code,
            "LOCAL_INSPECTION_EXPIRED"
        );
        for _ in 0..130 {
            previews.remember(detection("native-bearer"));
        }
        assert_eq!(previews.0.len(), 128);
    }
    #[tokio::test]
    async fn connection_is_idempotent_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let state = state(temp.path()).await;
        let request = input(&state);
        let first = commit(&state, &request, detection("native-bearer"))
            .await
            .unwrap();
        let second = commit(&state, &request, detection("native-bearer"))
            .await
            .unwrap();
        assert_eq!(first["status"], "enabled");
        assert_eq!(first["account"]["id"], second["account"]["id"]);
        assert_eq!(state.profiles.metadata().await.len(), 1);
        drop(state);
        let restarted = self::state(temp.path()).await;
        let third = commit(&restarted, &request, detection("native-bearer"))
            .await
            .unwrap();
        assert_eq!(third["account"]["id"], first["account"]["id"]);
        assert_eq!(restarted.profiles.metadata().await.len(), 1);
    }
    #[tokio::test]
    async fn pending_creation_resumes_without_another_account() {
        let temp = tempfile::tempdir().unwrap();
        let state = state(temp.path()).await;
        let mut request = input(&state);
        // Simulate interruption after profile persistence, before route activation.
        let created = state
            .profiles
            .create_local_once(&request.name, request.request_id)
            .await
            .unwrap();
        state
            .operations
            .register_accounts(&[(created.id, created.name.clone())])
            .unwrap();
        assert!(state.operations.configured_provider(created.id).is_none());
        request.revision = state.operations.snapshot()["revision"].as_u64().unwrap();
        let response = commit(&state, &request, detection("native-api-key"))
            .await
            .unwrap();
        assert_eq!(response["status"], "enabled");
        assert_eq!(response["account"]["id"], created.id.to_string());
        assert_eq!(state.profiles.metadata().await.len(), 1);
    }
    #[tokio::test]
    async fn stale_revision_and_paused_group_do_not_create_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let state = state(temp.path()).await;
        let mut request = input(&state);
        request.revision += 1;
        assert_eq!(
            commit(&state, &request, detection("none"))
                .await
                .unwrap_err()
                .code,
            "CONFIG_CONFLICT"
        );
        request.revision -= 1;
        state
            .operations
            .save_group(
                Some(DEFAULT_GROUP),
                GroupInput {
                    revision: request.revision,
                    name: "default".into(),
                    enabled: false,
                },
            )
            .unwrap();
        request.revision += 1;
        assert_eq!(
            commit(&state, &request, detection("none"))
                .await
                .unwrap_err()
                .code,
            "LOCAL_GROUP_UNAVAILABLE"
        );
        assert!(state.profiles.metadata().await.is_empty());
    }
    #[tokio::test]
    async fn empty_conversion_preserves_identity_and_rejects_credentials_routes_and_work() {
        let temp = tempfile::tempdir().unwrap();
        let state = state(temp.path()).await;
        let account = state.profiles.create("original-name").await.unwrap();
        state
            .operations
            .register_accounts(&[(account.id, account.name.clone())])
            .unwrap();
        let mut request = input(&state);
        request.account_id = Some(account.id);
        request.name = "ignored-name".into();
        let auth = temp
            .path()
            .join("accounts")
            .join(account.id.to_string())
            .join("home/auth.json");
        std::fs::write(&auth, "SYNTHETIC_CREDENTIAL").unwrap();
        assert_eq!(
            commit(&state, &request, detection("native-bearer"))
                .await
                .unwrap_err()
                .code,
            "LOCAL_ACCOUNT_NOT_EMPTY"
        );
        assert_eq!(
            std::fs::read_to_string(&auth).unwrap(),
            "SYNTHETIC_CREDENTIAL"
        );
        assert!(state.profiles.is_oauth(account.id).await.unwrap());
        std::fs::remove_file(auth).unwrap();
        let lease = state
            .operations
            .begin(Some(account.id), None, "management", "test", false)
            .unwrap();
        assert_eq!(
            commit(&state, &request, detection("native-bearer"))
                .await
                .unwrap_err()
                .code,
            "BUSY"
        );
        lease.complete(Ok(())).unwrap();
        state
            .operations
            .set_inference_route(
                account.id,
                request.revision,
                Some(detection("native-bearer").provider),
            )
            .unwrap();
        request.revision += 1;
        assert_eq!(
            commit(&state, &request, detection("native-bearer"))
                .await
                .unwrap_err()
                .code,
            "LOCAL_ACCOUNT_NOT_EMPTY"
        );
        state
            .operations
            .set_inference_route(account.id, request.revision, None)
            .unwrap();
        request.revision += 1;
        let result = commit(&state, &request, detection("native-bearer"))
            .await
            .unwrap();
        assert_eq!(result["account"]["id"], account.id.to_string());
        assert_eq!(result["account"]["name"], "original-name");
        assert_eq!(result["account"]["source"], "native-config");
        assert_eq!(
            state.operations.snapshot()["accounts"][0]["groupIds"],
            json!([DEFAULT_GROUP])
        );
    }
}
