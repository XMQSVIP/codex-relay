//! One ready inference host per enabled native account. No model turn is submitted.
use super::{Profiles, warm};
use crate::{
    backend::Backend,
    error::{Error, Result},
    installation,
    operations::Operations,
};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

impl Profiles {
    pub(super) fn pin_response(&self, id: Uuid, selected: &Arc<Backend>) {
        let clients = self.runtime_clients.lock().unwrap();
        for backend in clients
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(std::sync::Weak::upgrade)
        {
            if let Some(worker) = backend.response_worker.lock().unwrap().as_mut() {
                worker.standby = Arc::ptr_eq(&backend, selected);
            }
        }
    }

    pub(crate) async fn has_ready_response(&self, id: Uuid) -> bool {
        let clients: Vec<_> = self
            .runtime_clients
            .lock()
            .unwrap()
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        for backend in clients {
            let ready = backend
                .response_worker
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|worker| {
                    worker.standby
                        && if crate::native_cache::enabled() && worker.managed_contexts {
                            worker.matches(&worker.key) && !worker.resources.retire()
                        } else {
                            worker.accepts(&worker.key, 0)
                        }
                });
            if ready && backend.alive().await {
                return true;
            }
        }
        false
    }

    // Caller holds an account maintenance lease, excluding inference and stop.
    pub(crate) async fn prewarm_response(&self, id: Uuid, ops: &Operations) -> Result<()> {
        let _permit = self
            .standby_start_limit
            .acquire()
            .await
            .map_err(|_| Error::new("SERVICE_DRAINING", "实例预热已停止。"))?;
        if !ops.account_scheduling_enabled(id)? {
            return Ok(());
        }
        let provider = ops
            .configured_provider(id)
            .ok_or_else(|| Error::new("INFERENCE_DISABLED", "账户尚未配置模型路由。"))?;
        let _preparation = self.response_preparation(id).await;
        let installation = installation::read_with_refresh().await?;
        let oauth = self.is_oauth(id).await?;
        let proxy = self.account_proxy(id);
        let location = if let Some(settings) = &self.proxy_settings {
            settings.resolve_location(id, false).await
        } else {
            None
        };
        // Same startup identity as a normal text Responses request.
        let key = warm::digest(&json!({"installation":installation,"proxy":proxy,
            "images":false,"model":(!crate::native_cache::enabled()).then_some(&provider.model),"oauth":oauth,"location":location}))?;
        let (slot, mut worker) = self
            .shared_response_slot(id, ops.account_concurrency(id)?, &key, 0, |_| Ok(false))
            .await?;
        if let Some(backend) = worker.as_ref() {
            let compatible = backend
                .response_worker
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|state| state.accepts(&key, 0));
            if compatible && backend.alive().await {
                self.pin_response(id, backend);
                return Ok(());
            }
            backend.stop().await?;
            *worker = None;
        }
        let workspace = self.root.join(id.to_string()).join("workspace");
        let directory = tempfile::Builder::new()
            .prefix("worker-")
            .tempdir_in(&workspace)?;
        let home = self.home(id);
        let backend = Arc::new(
            Backend::start_response_with_proxy(
                &installation,
                directory.path(),
                if oauth { Some(&home) } else { None },
                false,
                &proxy,
                &provider.model,
            )
            .await?,
        );
        // Let Backend own its workspace even if authentication or cleanup fails.
        let mut state = warm::Worker::new(key, directory);
        state.shared = true;
        state.managed_contexts = true;
        state.idle();
        let cwd = state.directory.path().to_path_buf();
        *backend.response_worker.lock().unwrap() = Some(state);
        backend.rpc.set_location(location);
        let prepared = self
            .read_route_with_config(
                id,
                &backend,
                &cwd,
                if oauth { Some(&provider.model) } else { None },
            )
            .await;
        let config = match prepared {
            Ok((actual, _, config)) if actual == provider => config,
            result => {
                // Keep ownership on cleanup failure so the normal stop path can retry.
                if backend.stop().await.is_err() {
                    *worker = Some(backend);
                }
                return Err(result.err().unwrap_or_else(|| {
                    Error::new("ROUTE_CHANGED", "账户路由已变化，请重新检查账户。")
                }));
            }
        };
        backend
            .response_worker
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .config = Some(warm::digest(&config)?);
        self.track_client(id, &backend);
        self.pin_response(id, &backend);
        *worker = Some(backend.clone());
        warm::reap_when_idle(&slot, &backend);
        eprintln!(
            "{}",
            json!({"event":"native_standby_ready","accountId":id,"nativeProcessId":backend.pid})
        );
        Ok(())
    }
}
