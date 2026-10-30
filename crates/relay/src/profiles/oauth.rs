//! Durable credential binding, desktop reuse and authenticated provider lookup.
use super::Profiles;
use crate::oauth::{self, Identity};
use crate::{
    backend::Backend,
    error::{Error, Result},
    installation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Binding {
    mode: String,
    identity: Identity,
    #[serde(default)]
    generation: String,
}

impl Profiles {
    pub(crate) async fn owns_refresh_token(&self, id: uuid::Uuid, token: &str) -> Result<bool> {
        if !self.is_oauth(id).await? || self.binding(id).await?.is_some_and(|b| b.mode == "desktop")
        {
            return Ok(false);
        }
        let path = self.home(id).join("auth.json");
        if !tokio::fs::try_exists(&path).await? {
            return Ok(false);
        }
        let value = oauth::load(&path).await?;
        Ok(value["tokens"]["refresh_token"]
            .as_str()
            .or_else(|| value["refresh_token"].as_str())
            == Some(token))
    }
    pub(crate) async fn export_credentials(&self, id: uuid::Uuid) -> Result<Option<Value>> {
        if !self.is_oauth(id).await? {
            return Ok(None);
        }
        if let Some(binding) = self.binding(id).await? {
            if binding.mode == "desktop" {
                return Ok(None);
            }
            let file = if binding.mode == "external" {
                "bridge-token.json"
            } else {
                "auth.json"
            };
            let credential = oauth::parse(&oauth::load(&self.home(id).join(file)).await?)?;
            oauth::check_identity(&binding.identity, &credential.identity)?;
            return Ok(Some(credential.native_document()));
        }
        match oauth::load(&self.home(id).join("auth.json")).await {
            Ok(value) => Ok(Some(oauth::parse(&value)?.native_document())),
            Err(e) if e.code == "OAUTH_NOT_SIGNED_IN" => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub(crate) async fn credential_identity(&self, id: uuid::Uuid) -> Result<Option<String>> {
        if !self.is_oauth(id).await? {
            return Ok(None);
        }
        if let Some(binding) = self.binding(id).await? {
            return Ok(Some(oauth::fingerprint(&binding.identity)));
        }
        self.export_credentials(id)
            .await?
            .map(|v| oauth::parse(&v).map(|c| oauth::fingerprint(&c.identity)))
            .transpose()
    }
    pub(super) async fn credential_plan_type(&self, id: uuid::Uuid) -> Result<Option<String>> {
        if self.source(id).await? == "native-config" {
            return Ok(self
                .subscription_view(id)
                .and_then(|v| v["planType"].as_str().map(str::to_owned)));
        }
        // Read saved credentials without starting a worker or refreshing tokens.
        self.export_credentials(id)
            .await?
            .map(|value| oauth::parse(&value).map(|c| c.plan_type().map(str::to_owned)))
            .transpose()
            .map(Option::flatten)
    }
    pub async fn logout_credentials(&self, id: uuid::Uuid) -> Result<()> {
        self.require_source(id, "isolated").await?;
        self.stop(id).await?;
        let slot = self.slot(id).await?;
        let _worker = slot.lock().await;
        self.reset_binding_for_login(id).await?;
        match tokio::fs::remove_file(self.home(id).join("auth.json")).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
    pub async fn is_oauth(&self, id: uuid::Uuid) -> Result<bool> {
        Ok(self.source(id).await? == "isolated")
    }
    pub(super) fn home(&self, id: uuid::Uuid) -> PathBuf {
        self.root.join(id.to_string()).join("home")
    }
    async fn binding(&self, id: uuid::Uuid) -> Result<Option<Binding>> {
        let path = self.home(id).join("bridge-auth.json");
        if !tokio::fs::try_exists(&path).await? {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(oauth::load(&path).await?)?))
    }
    pub(super) async fn has_binding(&self, id: uuid::Uuid) -> Result<bool> {
        Ok(self.binding(id).await?.is_some())
    }
    pub(super) async fn credential_source(&self, id: uuid::Uuid) -> Result<Option<String>> {
        if !self.is_oauth(id).await? {
            return Ok(None);
        }
        if let Some(binding) = self.binding(id).await? {
            return Ok(Some(binding.mode));
        }
        Ok(tokio::fs::try_exists(self.home(id).join("auth.json"))
            .await?
            .then(|| "native-login".into()))
    }
    async fn remove_auth_file(&self, id: uuid::Uuid, name: &str) -> Result<()> {
        match tokio::fs::remove_file(self.home(id).join(name)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
    pub async fn import_credentials(&self, id: uuid::Uuid, document: Value) -> Result<Value> {
        self.require_source(id, "isolated").await?;
        self.require_no_login(id)?;
        let credential = oauth::prepare_import(&document, &self.account_proxy(id)).await?;
        self.invalidate_subscription(id);
        self.stop(id).await?;
        let slot = self.slot(id).await?;
        let _worker = slot.lock().await;
        let home = self.home(id);
        crate::platform::protect_directory(&home).await?;
        crate::native_catalog::invalidate(&home).await?;
        let managed = credential.refresh_token.is_some();
        let binding = Binding {
            mode: if managed { "managed" } else { "external" }.into(),
            identity: credential.identity.clone(),
            generation: uuid::Uuid::new_v4().to_string(),
        };
        oauth::save(
            &home.join("bridge-auth.json"),
            &serde_json::to_value(binding)?,
        )
        .await?;
        if managed {
            oauth::save(&home.join("auth.json"), &credential.native_document()).await?;
            self.remove_auth_file(id, "bridge-token.json").await?;
        } else {
            oauth::save(&home.join("bridge-token.json"), &credential.login_params()).await?;
            self.remove_auth_file(id, "auth.json").await?;
        }
        Ok(
            json!({"ok":true,"storage":"isolated-account-directory","refresh":if managed{"native-managed"}else{"manual"},"inferenceVerified":false}),
        )
    }
    pub async fn link_desktop(&self, id: uuid::Uuid) -> Result<Value> {
        self.require_source(id, "isolated").await?;
        self.require_no_login(id)?;
        self.stop(id).await?;
        let slot = self.slot(id).await?;
        let _worker = slot.lock().await;
        let installation = installation::read_with_refresh().await?;
        installation::validate(&installation).await?;
        let credential = oauth::desktop_credential_with_proxy(
            &installation,
            &self.root.join(id.to_string()).join("workspace"),
            &self.account_proxy(id),
        )
        .await?;
        self.invalidate_subscription(id);
        crate::native_catalog::invalidate(&self.home(id)).await?;
        oauth::save(
            &self.home(id).join("bridge-auth.json"),
            &serde_json::to_value(Binding {
                mode: "desktop".into(),
                identity: credential.identity,
                generation: uuid::Uuid::new_v4().to_string(),
            })?,
        )
        .await?;
        self.remove_auth_file(id, "auth.json").await?;
        self.remove_auth_file(id, "bridge-token.json").await?;
        Ok(json!({"ok":true,"storage":"desktop-native","inferenceVerified":false}))
    }
    pub(super) async fn reset_binding_for_login(&self, id: uuid::Uuid) -> Result<()> {
        self.invalidate_subscription(id);
        crate::native_catalog::invalidate(&self.home(id)).await?;
        for name in ["bridge-auth.json", "bridge-token.json"] {
            match tokio::fs::remove_file(self.home(id).join(name)).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    pub(super) async fn prepare_auth(
        &self,
        id: uuid::Uuid,
        backend: &Backend,
        cwd: &std::path::Path,
    ) -> Result<oauth::Credential> {
        let binding = self.binding(id).await?;
        let document = match binding.as_ref().map(|value| value.mode.as_str()) {
            Some("managed") => oauth::load(&self.home(id).join("auth.json")).await?,
            Some("external") => oauth::load(&self.home(id).join("bridge-token.json")).await?,
            _ => Value::Null,
        };
        let bounded_age = binding.as_ref().is_none_or(|value| value.mode == "desktop");
        let key = super::warm::digest(&json!({"binding":binding,"credential":document}))?;
        backend
            .auth_cache
            .get_or_refresh(
                key,
                bounded_age,
                self.prepare_auth_uncached(id, backend, cwd),
            )
            .await
    }

    async fn prepare_auth_uncached(
        &self,
        id: uuid::Uuid,
        backend: &Backend,
        cwd: &std::path::Path,
    ) -> Result<oauth::Credential> {
        // Bootstrap a durable identity once even when separate generations start together.
        let _bootstrap = if self.binding(id).await?.is_none() {
            Some(self.auth_binding.lock().await)
        } else {
            None
        };
        if let Some(binding) = self.binding(id).await? {
            match binding.mode.as_str() {
                "external" => {
                    let value = oauth::load(&self.home(id).join("bridge-token.json")).await?;
                    let credential = oauth::parse(&value)?;
                    oauth::check_identity(&binding.identity, &credential.identity)?;
                    backend.install_external_credential(&credential).await?;
                    backend.rpc.set_auth_refresh(|_| {
                        Box::pin(async {
                            Err(Error::new("OAUTH_TOKEN_EXPIRED", "外部令牌需要重新导入。"))
                        })
                    });
                    return Ok(credential);
                }
                "desktop" => {
                    let installation = installation::read_with_refresh().await?;
                    let credential = oauth::desktop_credential_with_proxy(
                        &installation,
                        cwd,
                        &self.account_proxy(id),
                    )
                    .await?;
                    oauth::check_identity(&binding.identity, &credential.identity)?;
                    backend.install_external_credential(&credential).await?;
                    let expected = binding.identity.clone();
                    let cwd = cwd.to_path_buf();
                    let proxy_settings = self.proxy_settings.clone();
                    backend.rpc.set_auth_refresh(move |previous| {
                        let installation = installation.clone();
                        let cwd = cwd.clone();
                        let expected = expected.clone();
                        let proxy = proxy_settings
                            .as_ref()
                            .map(|ops| ops.account_proxy(id))
                            .unwrap_or_default();
                        Box::pin(async move {
                            if previous
                                .as_deref()
                                .is_some_and(|p| p != expected.account_id)
                            {
                                return Err(Error::new(
                                    "OAUTH_IDENTITY_CHANGED",
                                    "原生刷新请求的账户不匹配。",
                                ));
                            }
                            let credential =
                                oauth::desktop_credential_with_proxy(&installation, &cwd, &proxy)
                                    .await?;
                            oauth::check_identity(&expected, &credential.identity)?;
                            let mut result = credential.login_params();
                            result.as_object_mut().unwrap().remove("type");
                            result["chatgptPlanType"] = Value::Null;
                            Ok(result)
                        })
                    });
                    return Ok(credential);
                }
                "managed" => {}
                _ => return Err(Error::new("OAUTH_BINDING", "账户凭据模式无效。")),
            }
            // Native auth() handles proactive refresh; account/read would also discover remote workspaces.
            if binding.mode == "managed" {
                let cached = oauth::parse(&oauth::load(&self.home(id).join("auth.json")).await?)?;
                oauth::check_identity(&binding.identity, &cached.identity)?;
            }
            let credential = oauth::native_credential(backend).await?;
            oauth::check_identity(&binding.identity, &credential.identity)?;
            Ok(credential)
        } else {
            let credential = oauth::native_credential(backend).await?;
            oauth::save(
                &self.home(id).join("bridge-auth.json"),
                &serde_json::to_value(Binding {
                    mode: "managed".into(),
                    identity: credential.identity.clone(),
                    generation: uuid::Uuid::new_v4().to_string(),
                })?,
            )
            .await?;
            Ok(credential)
        }
    }
    pub(super) async fn read_route_provider(
        &self,
        id: uuid::Uuid,
        backend: &Backend,
        cwd: &std::path::Path,
        model: Option<&str>,
    ) -> Result<crate::provider::Provider> {
        Ok(self.read_route(id, backend, cwd, model).await?.0)
    }
    pub(super) async fn read_route(
        &self,
        id: uuid::Uuid,
        backend: &Backend,
        cwd: &std::path::Path,
        model: Option<&str>,
    ) -> Result<(crate::provider::Provider, Option<oauth::Credential>)> {
        let (provider, credential, _) =
            self.read_route_with_config(id, backend, cwd, model).await?;
        Ok((provider, credential))
    }
    pub(super) async fn read_route_with_config(
        &self,
        id: uuid::Uuid,
        backend: &Backend,
        cwd: &std::path::Path,
        model: Option<&str>,
    ) -> Result<(crate::provider::Provider, Option<oauth::Credential>, Value)> {
        if self.is_oauth(id).await? {
            let credential = self.prepare_auth(id, backend, cwd).await?;
            let (mut provider, config) =
                oauth::provider_with_config(backend, cwd, model, &credential.identity).await?;
            let binding = self
                .binding(id)
                .await?
                .ok_or_else(|| Error::new("OAUTH_BINDING", "账户绑定不存在。"))?;
            provider.auth.push(':');
            provider.auth.push_str(&binding.mode);
            provider.auth.push(':');
            provider.auth.push_str(&binding.generation);
            self.observe_subscription(id, &provider, Some(&credential));
            Ok((provider, Some(credential), config))
        } else {
            let reading = crate::provider::read_with_credential(&backend.rpc, cwd).await;
            if reading.as_ref().err().is_some_and(|e| {
                matches!(
                    e.code.as_str(),
                    "PROVIDER_AUTH_MISSING"
                        | "OAUTH_NOT_SIGNED_IN"
                        | "OAUTH_IDENTITY_MISMATCH"
                        | "OAUTH_IDENTITY_CHANGED"
                )
            }) {
                self.invalidate_subscription(id);
            }
            let (mut provider, config, credential) = reading?;
            if let Some(model) = model {
                provider.model = model.into();
            }
            self.observe_subscription(id, &provider, credential.as_ref());
            Ok((provider, credential, config))
        }
    }
}
