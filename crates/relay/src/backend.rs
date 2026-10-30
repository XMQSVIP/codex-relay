use crate::{
    error::{Error, Result},
    installation::Installation,
    platform,
    rpc::Rpc,
};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    process::{Child, Command},
    sync::Mutex,
};

pub struct Backend {
    pub rpc: Rpc,
    pub pid: u32,
    pub endpoint: Option<String>,
    pub token: Option<String>,
    pub user_agent: Option<String>,
    pub client_info: crate::installation::NativeClientInfo,
    child: Mutex<Child>,
    tree: crate::process_tree::ProcessTree,
    cached_catalog: bool,
    response_models: std::collections::HashSet<String>,
    pub(crate) response_worker: std::sync::Mutex<Option<crate::profiles::warm::Worker>>,
    // Shared Responses hold a read lease until their thread has stopped. Process
    // teardown takes the write lease, so retiring a worker never kills a sibling.
    pub(crate) response_activity: std::sync::Arc<tokio::sync::RwLock<()>>,
    external_credential: Mutex<Option<[u8; 32]>>,
    pub(crate) auth_cache: crate::oauth::cache::AuthCache,
}

pub fn clean_env(command: &mut Command, isolated: bool) {
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_uppercase();
        if (upper.starts_with("CODEX_")
            && upper != "CODEX_HOME"
            && upper != "CODEX_CA_CERTIFICATE"
            && !(!isolated && upper == "CODEX_API_KEY"))
            || (isolated
                && ["OPENAI_", "CHATGPT_", "AZURE_OPENAI_", "ANTHROPIC_"]
                    .iter()
                    .any(|p| upper.starts_with(p)))
        {
            command.env_remove(&key);
        }
    }
    let no_proxy = std::env::vars()
        .filter(|(k, _)| k.eq_ignore_ascii_case("NO_PROXY"))
        .map(|(_, v)| v)
        .collect::<Vec<_>>()
        .join(",");
    command.env_remove("no_proxy");
    command.env("NO_PROXY", format!("{no_proxy},127.0.0.1,localhost,::1"));
}

impl Backend {
    pub(crate) async fn start_test_chat(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
        proxy: &crate::app_server_proxy::ProxyConfig,
    ) -> Result<Self> {
        Self::start_inner_with_catalog(
            installation,
            cwd,
            home,
            None,
            home.is_some(),
            true,
            false,
            false,
            proxy,
            None,
            true,
        )
        .await
    }
    pub async fn start(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
        ws_port: Option<u16>,
        isolated: bool,
    ) -> Result<Self> {
        Self::start_inner(
            installation,
            cwd,
            home,
            ws_port,
            isolated,
            false,
            false,
            false,
            &Default::default(),
        )
        .await
    }
    pub async fn start_direct(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
    ) -> Result<Self> {
        Self::start_inner(
            installation,
            cwd,
            home,
            None,
            home.is_some(),
            true,
            false,
            false,
            &Default::default(),
        )
        .await
    }
    pub async fn start_direct_features(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
        image_generation: bool,
    ) -> Result<Self> {
        Self::start_inner(
            installation,
            cwd,
            home,
            None,
            home.is_some(),
            true,
            image_generation,
            false,
            &Default::default(),
        )
        .await
    }
    pub async fn start_with_proxy(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
        isolated: bool,
        proxy: &crate::app_server_proxy::ProxyConfig,
    ) -> Result<Self> {
        Self::start_inner(
            installation,
            cwd,
            home,
            None,
            isolated,
            false,
            false,
            false,
            proxy,
        )
        .await
    }
    pub async fn start_direct_with_proxy(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
        image_generation: bool,
        proxy: &crate::app_server_proxy::ProxyConfig,
    ) -> Result<Self> {
        Self::start_inner(
            installation,
            cwd,
            home,
            None,
            home.is_some(),
            true,
            image_generation,
            false,
            proxy,
        )
        .await
    }
    pub async fn start_response_with_proxy(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
        image_generation: bool,
        proxy: &crate::app_server_proxy::ProxyConfig,
        model: &str,
    ) -> Result<Self> {
        let catalog =
            crate::native_catalog::response_snapshot(installation, home, cwd, model, proxy).await?;
        // 工具策略是会话边界。目录被拒绝时不能降级到原生默认宿主工具。
        let mut backend = Self::start_inner_with_catalog(
            installation,
            cwd,
            home,
            None,
            home.is_some(),
            true,
            image_generation,
            false,
            proxy,
            Some(&catalog.path),
            false,
        )
        .await?;
        backend.response_models = catalog.models;
        Ok(backend)
    }
    pub(crate) fn supports_response_model(&self, model: &str) -> bool {
        self.response_models.contains(model)
    }
    pub fn uses_cached_catalog(&self) -> bool {
        self.cached_catalog
    }
    pub async fn start_auth_reader(installation: &Installation, cwd: &Path) -> Result<Self> {
        Self::start_inner(
            installation,
            cwd,
            None,
            None,
            false,
            false,
            false,
            true,
            &Default::default(),
        )
        .await
    }
    pub async fn start_auth_reader_with_proxy(
        installation: &Installation,
        cwd: &Path,
        proxy: &crate::app_server_proxy::ProxyConfig,
    ) -> Result<Self> {
        Self::start_inner(
            installation,
            cwd,
            None,
            None,
            false,
            false,
            false,
            true,
            proxy,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn start_inner(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
        ws_port: Option<u16>,
        isolated: bool,
        direct: bool,
        image_generation: bool,
        auth_reader: bool,
        proxy: &crate::app_server_proxy::ProxyConfig,
    ) -> Result<Self> {
        Self::start_inner_with_catalog(
            installation,
            cwd,
            home,
            ws_port,
            isolated,
            direct,
            image_generation,
            auth_reader,
            proxy,
            None,
            false,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn start_inner_with_catalog(
        installation: &Installation,
        cwd: &Path,
        home: Option<&Path>,
        ws_port: Option<u16>,
        isolated: bool,
        direct: bool,
        image_generation: bool,
        auth_reader: bool,
        proxy: &crate::app_server_proxy::ProxyConfig,
        catalog: Option<&Path>,
        private_session: bool,
    ) -> Result<Self> {
        let client_info = installation.client_info()?;
        let mut command = Command::new(&installation.app_server_executable);
        command
            .arg("app-server")
            .current_dir(cwd)
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if private_session {
            for override_value in [
                "history.persistence=\"none\"",
                "otel.log_user_prompt=false",
                "otel.exporter=\"none\"",
                "otel.trace_exporter=\"none\"",
            ] {
                command.args(["-c", override_value]);
            }
            command.args([
                "-c",
                &format!("log_dir={}", serde_json::to_string(&cwd.join("logs"))?),
            ]);
            command.env("RUST_LOG", "off");
        }
        if auth_reader {
            command.args(["-c", "model_provider=\"openai\""]);
        }
        if let Some(catalog) = catalog {
            command.args([
                "-c",
                &format!("model_catalog_json={}", serde_json::to_string(catalog)?),
            ]);
        }
        if direct || auth_reader {
            for feature in [
                "apps",
                "plugins",
                "hooks",
                "shell_tool",
                "unified_exec",
                "multi_agent",
                "browser_use",
                "computer_use",
                "image_generation",
                "goals",
                "memories",
                "skill_search",
                "workspace_dependencies",
                "view_image",
                "sleep_tool",
                "tool_suggest",
                "browser_use_external",
                "in_app_browser",
                "code_mode_host",
                "request_permissions_tool",
            ] {
                let enabled = feature == "image_generation" && image_generation;
                command.args(["-c", &format!("features.{feature}={enabled}")]);
            }
        }
        let token = ws_port.map(|_| crate::secret());
        let endpoint = ws_port.map(|port| format!("ws://127.0.0.1:{port}"));
        if let (Some(endpoint), Some(token)) = (&endpoint, &token) {
            command
                .args([
                    "--listen",
                    endpoint,
                    "--ws-auth",
                    "capability-token",
                    "--ws-token-sha256",
                    &hex::encode(Sha256::digest(token.as_bytes())),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null());
        } else {
            command
                .args(["--listen", "stdio://"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
        }
        if isolated {
            command.args([
                "-c",
                "cli_auth_credentials_store=\"file\"",
                "-c",
                "features.secret_auth_storage=false",
            ]);
        }
        clean_env(&mut command, isolated);
        proxy.apply(&mut command).await?;
        if let Some(home) = home {
            command.env("CODEX_HOME", home);
        }
        platform::hide(&mut command);
        crate::process_tree::ProcessTree::configure(&mut command);
        let mut child = command.spawn().map_err(|e| {
            crate::desktop_process::io_error("BACKEND_SPAWN", "启动原生 app-server", e)
        })?;
        let tree = match crate::process_tree::ProcessTree::attach(&child) {
            Ok(tree) => tree,
            Err(error) => {
                let _ = child.kill().await;
                return Err(error);
            }
        };
        let pid = child
            .id()
            .ok_or_else(|| Error::new("BACKEND_START", "原生进程未启动。"))?;
        let rpc = if let (Some(endpoint), Some(token)) = (&endpoint, &token) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            loop {
                if child.try_wait()?.is_some() || tokio::time::Instant::now() > deadline {
                    return Err(Error::new("BACKEND_START", "原生后端未完成启动。"));
                }
                if let Ok(rpc) = Rpc::connect(endpoint, token).await {
                    break rpc;
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        } else {
            Rpc::from_io(child.stdout.take().unwrap(), child.stdin.take().unwrap())
        };
        let initialized = match rpc.initialize(client_info).await {
            Ok(value) => value,
            Err(error) => {
                rpc.close();
                let _ = child.kill().await;
                return Err(error);
            }
        };
        Ok(Self {
            rpc,
            pid,
            endpoint,
            token,
            user_agent: initialized["userAgent"].as_str().map(str::to_owned),
            client_info: client_info.clone(),
            child: Mutex::new(child),
            tree,
            cached_catalog: catalog.is_some(),
            response_models: Default::default(),
            response_worker: std::sync::Mutex::new(None),
            response_activity: Default::default(),
            external_credential: Mutex::new(None),
            auth_cache: Default::default(),
        })
    }
    pub(crate) async fn install_external_credential(
        &self,
        credential: &crate::oauth::Credential,
    ) -> Result<crate::oauth::Identity> {
        let digest: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&credential.login_params())?).into();
        let mut installed = self.external_credential.lock().await;
        if installed.as_ref() != Some(&digest) {
            crate::oauth::install_external_credential(&self.rpc, credential).await?;
            *installed = Some(digest);
        }
        Ok(credential.identity.clone())
    }
    pub async fn alive(&self) -> bool {
        self.child
            .lock()
            .await
            .try_wait()
            .is_ok_and(|status| status.is_none())
            && !self.rpc.is_closed()
    }
    pub async fn stop(&self) -> Result<()> {
        let _idle = self.response_activity.write().await;
        let mut child = self.child.lock().await;
        self.tree.terminate()?;
        self.rpc.close();
        if child.try_wait()?.is_none() {
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| Error::new("BACKEND_STOP", "拥有的后端进程未退出。"))??;
        }
        self.tree.wait().await?;
        if let Some(worker) = self.response_worker.lock().unwrap().take() {
            worker.close()?;
        }
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Capture descendants before Child's kill_on_drop can orphan them.
        let _ = self.tree.terminate();
        self.rpc.close();
    }
}

pub async fn unused_port() -> Result<u16> {
    Ok(tokio::net::TcpListener::bind("127.0.0.1:0")
        .await?
        .local_addr()?
        .port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_proxy_bypass_survives_case_insensitive_windows_environment() {
        let mut command = Command::new("unused");
        clean_env(&mut command, true);
        let bypass = command
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                key.to_string_lossy()
                    .eq_ignore_ascii_case("NO_PROXY")
                    .then_some(value)
            })
            .flatten()
            .expect("NO_PROXY must be set, not removed by its lowercase alias");
        for host in ["127.0.0.1", "localhost", "::1"] {
            assert!(
                bypass
                    .to_string_lossy()
                    .split(',')
                    .any(|entry| entry == host)
            );
        }
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            assert!(!command.as_std().get_envs().any(|(key, _)| key == name));
        }
    }
}
