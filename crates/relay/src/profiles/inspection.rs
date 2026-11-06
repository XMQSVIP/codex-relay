//! Native route inspection and isolated capability checks.

use super::Profiles;
use crate::{
    backend::Backend,
    error::{Error, Result},
    installation,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) enum RouteCheck {
    Access,
    Connection { model: Option<String> },
}

fn require_idle(backend: &Backend) -> Result<()> {
    // The caller holds the primary slot, preventing new activity leases. Parked
    // tools still own existing leases even though their HTTP request has ended.
    backend
        .response_activity
        .try_write()
        .map(|_| ())
        .map_err(|_| {
            Error::new(
                "BUSY",
                "实例仍有运行中的请求或待答工具回调，请完成后再执行接入检查。",
            )
        })
}

impl Profiles {
    pub async fn verify_feature(
        &self,
        id: uuid::Uuid,
        ops: &crate::operations::Operations,
        feature: crate::operations::Feature,
        cancel: CancellationToken,
    ) -> Result<()> {
        if self.is_api_key(id).await? {
            return Err(Error::new(
                "INVALID_INPUT",
                "API Key 账户的能力由上游决定，请使用连接测试。",
            ));
        }
        let oauth = self.is_oauth(id).await?;
        let slot = self.slot(id).await?;
        let mut worker = slot.lock().await;
        if let Some(backend) = worker.as_ref() {
            require_idle(backend)?;
        }
        super::warm::stop_owned(&mut worker).await?;
        let installation = installation::read_with_refresh().await?;
        installation::validate(&installation).await?;
        let workspace = self.root.join(id.to_string()).join("workspace");
        let directory = tempfile::Builder::new()
            .prefix("capability-check-")
            .tempdir_in(workspace)?;
        let cwd = directory.path().to_path_buf();
        let home = self.home(id);
        let backend = Arc::new(
            Backend::start_direct_with_proxy(
                &installation,
                directory.path(),
                if oauth { Some(&home) } else { None },
                feature == crate::operations::Feature::ImageGeneration,
                &self.account_proxy(id),
            )
            .await?,
        );
        self.track_client(id, &backend);
        *backend.response_worker.lock().unwrap() =
            Some(super::warm::Worker::new([0; 32], directory));
        *worker = Some(backend.clone());
        let mut checked_provider = None;
        let result = async {
            let configured = ops.configured_provider(id);
            let provider = self
                .read_route_provider(
                    id,
                    &backend,
                    &cwd,
                    if oauth {
                        configured.as_ref().map(|p| p.model.as_str())
                    } else {
                        None
                    },
                )
                .await?;
            let caps = crate::operations::runtime::discover(
                &backend.rpc,
                &provider,
                &installation.app_server_sha256,
            )
            .await;
            let supported = caps.supports(feature, &provider.model);
            ops.publish_capabilities(id, caps)?;
            let result = if supported == Some(true) {
                crate::capability_probe::run(&backend, &provider, &cwd, feature, cancel).await
            } else {
                Err(Error::new(
                    "CAPABILITY_UNAVAILABLE",
                    "当前提供方没有确认该能力。",
                ))
            };
            checked_provider = Some(provider);
            result
        }
        .await;
        let stopped = super::warm::stop_owned(&mut worker).await;
        let result = result.and(stopped);
        if let Some(provider) = checked_provider {
            ops.record_feature_verification(
                id,
                &provider,
                &installation.app_server_sha256,
                feature,
                result.as_ref().err(),
            )?;
        }
        result
    }
    pub async fn inspect_native_config(&self, id: uuid::Uuid) -> Result<crate::provider::Provider> {
        self.inspect_native_config_capabilities(id)
            .await
            .map(|(provider, _)| provider)
    }
    pub async fn inspect_native_config_capabilities(
        &self,
        id: uuid::Uuid,
    ) -> Result<(
        crate::provider::Provider,
        crate::operations::NativeCapabilities,
    )> {
        self.require_source(id, "native-config").await?;
        self.start(id).await?;
        let slot = self.slot(id).await?;
        let mut worker = slot.lock().await;
        let backend = worker
            .as_ref()
            .ok_or_else(|| Error::new("WORKER_STOPPED", "实例已停止。"))?;
        require_idle(backend)?;
        let result = async {
            let provider = self
                .read_route_provider(
                    id,
                    backend,
                    &self.root.join(id.to_string()).join("workspace"),
                    None,
                )
                .await?;
            let installation = installation::read_with_refresh().await?;
            let caps = crate::operations::runtime::discover(
                &backend.rpc,
                &provider,
                &installation.app_server_sha256,
            )
            .await;
            Ok((provider, caps))
        }
        .await;
        let stopped = super::warm::stop_owned(&mut worker).await;
        stopped?;
        result
    }
    pub(crate) async fn inspect_route(
        &self,
        id: uuid::Uuid,
        check: RouteCheck,
        cancel: CancellationToken,
    ) -> Result<(
        crate::provider::Provider,
        crate::operations::NativeCapabilities,
        Option<crate::execution::ExecutionOutcome>,
    )> {
        if self.is_api_key(id).await? {
            return self.inspect_upstream(id, check, cancel).await;
        }
        let isolated = self.is_oauth(id).await?;
        let slot = self.slot(id).await?;
        let mut worker = slot.lock().await;
        if let Some(backend) = worker.as_ref() {
            require_idle(backend)?;
        }
        super::warm::stop_owned(&mut worker).await?;
        let installation = installation::read_with_refresh().await?;
        installation::validate(&installation).await?;
        let directory = tempfile::Builder::new()
            .prefix("native-check-")
            .tempdir_in(self.root.join(id.to_string()).join("workspace"))?;
        let cwd = directory.path().to_path_buf();
        let home = self.home(id);
        let backend = Arc::new(
            Backend::start_direct_with_proxy(
                &installation,
                directory.path(),
                if isolated { Some(&home) } else { None },
                false,
                &self.account_proxy(id),
            )
            .await?,
        );
        self.track_client(id, &backend);
        *backend.response_worker.lock().unwrap() =
            Some(super::warm::Worker::new([0; 32], directory));
        *worker = Some(backend.clone());
        let result: Result<_> = async {
            let model = match &check {
                RouteCheck::Access => None,
                RouteCheck::Connection { model } => model.as_deref(),
            };
            let provider = self.read_route_provider(id, &backend, &cwd, model).await?;
            let caps = crate::operations::runtime::discover(
                &backend.rpc,
                &provider,
                &installation.app_server_sha256,
            )
            .await;
            if cancel.is_cancelled() {
                return Err(Error::new("REQUEST_CANCELLED", "接入检查已取消。"));
            }
            let result = match check {
                RouteCheck::Access => None,
                RouteCheck::Connection { .. } => Some(
                    crate::capability_probe::text_roundtrip(&backend, &provider, &cwd, cancel)
                        .await,
                ),
            };
            Ok((provider, caps, result))
        }
        .await;
        let stopped = super::warm::stop_owned(&mut worker).await;
        let (provider, caps, model) = result?;
        let outcome = if let Some(model) = model {
            Some(crate::execution::ExecutionOutcome {
                model,
                cleanup_error: stopped.err(),
            })
        } else {
            stopped?;
            None
        };
        Ok((provider, caps, outcome))
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn busy_or_failed_inspection_cleanup_keeps_the_owned_worker_for_retry() {
        let root = tempfile::tempdir().unwrap();
        let profiles = Profiles::open(root.path().join("accounts")).await.unwrap();
        let account = profiles
            .create_native_config("cleanup fixture")
            .await
            .unwrap();
        let backend = Arc::new(Backend::test_backend());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        std::fs::write(path.join("owned"), "fixture").unwrap();
        *backend.response_worker.lock().unwrap() =
            Some(super::super::warm::Worker::new([0; 32], directory));
        let slot = profiles.slot(account.id).await.unwrap();
        *slot.lock().await = Some(backend.clone());

        let active = backend.response_activity.clone().read_owned().await;
        assert_eq!(
            profiles
                .inspect_route(account.id, RouteCheck::Access, CancellationToken::new())
                .await
                .err()
                .unwrap()
                .code,
            "BUSY"
        );
        drop(active);
        backend.stop_failure.store(true, Ordering::SeqCst);
        let ops = crate::operations::Operations::open(&root.path().join("operations")).unwrap();
        assert_eq!(
            profiles
                .inspect_route(account.id, RouteCheck::Access, CancellationToken::new())
                .await
                .err()
                .unwrap()
                .code,
            "BACKEND_STOP"
        );
        assert_eq!(
            profiles
                .verify_feature(
                    account.id,
                    &ops,
                    crate::operations::Feature::ImageGeneration,
                    CancellationToken::new()
                )
                .await
                .unwrap_err()
                .code,
            "BACKEND_STOP"
        );
        assert!(
            slot.lock()
                .await
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &backend))
        );
        assert!(backend.alive().await);
        assert!(path.join("owned").is_file());
        backend.stop_failure.store(false, Ordering::SeqCst);
        profiles.stop(account.id).await.unwrap();
        assert!(slot.lock().await.is_none());
        assert!(!backend.alive().await);
        assert!(!path.exists());
    }
}
