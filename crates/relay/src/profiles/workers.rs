//! Per-account worker slots, status, cancellation and service shutdown.

use super::{InferenceRegistration, ProfileView, Profiles, Slot};
use crate::{
    backend::Backend,
    error::{Error, Result},
    installation,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

impl Profiles {
    // A weak registry keeps identity readable while inference owns the worker slot.
    pub(super) fn track_client(&self, id: uuid::Uuid, backend: &Arc<Backend>) {
        let mut registry = self.runtime_clients.lock().unwrap();
        registry.retain(|_, clients| {
            clients.retain(|b| b.strong_count() > 0);
            !clients.is_empty()
        });
        let clients = registry.entry(id).or_default();
        if !clients.iter().any(|b| b.ptr_eq(&Arc::downgrade(backend))) {
            clients.push(Arc::downgrade(backend));
        }
    }
    async fn current_clients(&self, id: uuid::Uuid) -> Vec<super::RuntimeClient> {
        let backends: Vec<_> = self
            .runtime_clients
            .lock()
            .unwrap()
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(std::sync::Weak::upgrade)
            .collect();
        let mut clients = Vec::new();
        for backend in backends {
            if backend.alive().await {
                clients.push(super::RuntimeClient {
                    pid: backend.pid,
                    client_info: backend.client_info.clone(),
                    user_agent: backend.user_agent.clone(),
                });
            }
        }
        clients.sort_by_key(|client| client.pid);
        clients
    }
    pub(crate) async fn has_worker(&self, id: uuid::Uuid) -> bool {
        let mut slots = self
            .response_slots
            .lock()
            .await
            .get(&id)
            .cloned()
            .unwrap_or_default();
        if let Some(slot) = self.slots.lock().await.get(&id).cloned() {
            slots.push(slot);
        }
        for slot in slots {
            match slot.try_lock() {
                Ok(worker) if worker.is_none() => {}
                _ => return true,
            }
        }
        false
    }
    pub(super) async fn slot(&self, id: uuid::Uuid) -> Result<Slot> {
        if !self.records.lock().await.iter().any(|p| p.id == id) {
            return Err(Error::new("NOT_FOUND", "账户不存在。"));
        }
        Ok(self
            .slots
            .lock()
            .await
            .entry(id)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone())
    }
    pub async fn start(&self, id: uuid::Uuid) -> Result<()> {
        if self.is_api_key(id).await? {
            return Err(Error::new(
                "INVALID_INPUT",
                "API Key 账户无需原生进程，请使用账户启用开关。",
            ));
        }
        let slot = self.slot(id).await?;
        let mut worker = slot.lock().await;
        if let Some(backend) = worker.as_ref() {
            if backend.response_worker.lock().unwrap().is_none() && backend.alive().await {
                return Ok(());
            }
            backend.stop().await?;
            *worker = None;
        }
        let installation = installation::read_with_refresh().await?;
        installation::validate(&installation).await?;
        let directory = self.root.join(id.to_string());
        let native_config = self.source(id).await? == "native-config";
        let home = directory.join("home");
        let backend = Backend::start_with_proxy(
            &installation,
            &directory.join("workspace"),
            if native_config { None } else { Some(&home) },
            !native_config,
            &self.account_proxy(id),
        )
        .await?;
        if !native_config && self.has_binding(id).await? {
            if let Err(error) = self
                .prepare_auth(id, &backend, &directory.join("workspace"))
                .await
            {
                let _ = backend.stop().await;
                return Err(error);
            }
        }
        let backend = Arc::new(backend);
        self.track_client(id, &backend);
        *worker = Some(backend);
        Ok(())
    }
    pub async fn stop(&self, id: uuid::Uuid) -> Result<()> {
        self.cancel_active_login(id).await;
        let primary = self.slot(id).await?;
        let _stopping = self.begin_stop(id)?;
        self.catalog_refresh.stop(id).await;
        let mut slots = self
            .response_slots
            .lock()
            .await
            .get(&id)
            .cloned()
            .unwrap_or_default();
        if !slots.iter().any(|slot| Arc::ptr_eq(slot, &primary)) {
            slots.push(primary);
        }
        let mut failure = None;
        // 账户停止要等待全部执行器清理；一个执行器失败也不能跳过其余进程。
        for slot in slots {
            let mut worker = slot.lock().await;
            if let Some(backend) = worker.as_ref() {
                if let Err(error) = backend.stop().await {
                    failure.get_or_insert(error);
                    continue;
                }
            }
            *worker = None;
        }
        failure.map_or(Ok(()), Err)
    }
    pub async fn check_native_account(&self, id: uuid::Uuid) -> Result<Value> {
        self.start(id).await?;
        let slot = self.slot(id).await?;
        let worker = slot.lock().await;
        let backend = worker
            .as_ref()
            .ok_or_else(|| Error::new("WORKER_STOPPED", "账户实例未运行。"))?;
        let result = backend
            .rpc
            .request("account/read", json!({"refreshToken":false}))
            .await?;
        Ok(
            json!({"accountId":id,"pid":backend.pid,"credentialsLoaded":!result["account"].is_null(),"inferenceDispatched":false}),
        )
    }
    pub async fn list(&self) -> Vec<ProfileView> {
        let records = self.records.lock().await.clone();
        let mut slots = self.slots.lock().await.clone();
        for (id, pool) in self.response_slots.lock().await.iter() {
            if let Some(slot) = pool
                .iter()
                .find(|slot| slot.try_lock().is_ok_and(|worker| worker.is_some()))
            {
                slots.insert(*id, slot.clone());
            }
        }
        let generating: Vec<_> = self
            .inference
            .lock()
            .unwrap()
            .active
            .keys()
            .copied()
            .collect();
        futures_util::future::join_all(records.into_iter().map(|profile|{
            let slot=slots.get(&profile.id).cloned();let active=generating.contains(&profile.id);async move{
                let clients = self.current_clients(profile.id).await;
                let mut view=ProfileView{profile,state:"stopped".into(),pid:None,account:None,credential_source:None,plan_type:None,subscription:None,clients,upstream:None};
                if view.profile.source == "api-key" {
                    view.upstream=self.upstream(view.profile.id).await.ok().map(|c|c.view());
                    view.credential_source=Some("api-key".into());
                    view.state=if active {"generating"} else {"ready"}.into();
                    return view;
                }
                view.credential_source=self.credential_source(view.profile.id).await.ok().flatten();
                view.subscription=self.subscription_view(view.profile.id);
                view.plan_type=view.subscription.as_ref().and_then(|s|s["planType"].as_str()).map(str::to_owned).or(self.credential_plan_type(view.profile.id).await.ok().flatten());
                if active {view.state="generating".into();return view;}
                if let Some(slot)=slot{
                    let Ok(worker)=slot.try_lock() else{view.state="starting".into();return view;};
                    if let Some(backend)=worker.as_ref(){view.pid=Some(backend.pid);if !backend.alive().await{view.state="disconnected".into();return view;}
                        if view.profile.source=="native-config" || backend.response_worker.lock().unwrap().is_some() {view.state="ready".into();return view;}
                        match backend.rpc.request_timeout("account/read",json!({"refreshToken":false}),std::time::Duration::from_secs(3)).await{
                            Ok(value)=>{view.state=if value["account"].is_null(){"signed-out"}else{"credentials-loaded"}.into();if !value["account"].is_null(){view.account=Some(json!({"type":value["account"]["type"],"email":value["account"]["email"],"planType":value["account"]["planType"]}));}else{view.plan_type=None;view.subscription=None;self.invalidate_subscription(view.profile.id);}},
                            Err(_)=>view.state="unavailable".into(),
                        }
                    }
                }view
            }
        })).await
    }
    pub async fn shutdown(&self) {
        self.cancel_inference();
        let ids: Vec<_> = self.records.lock().await.iter().map(|p| p.id).collect();
        for id in ids {
            let _ = self.stop(id).await;
        }
        self.catalog_refresh.shutdown().await;
    }
    pub fn cancel_inference(&self) {
        let registry = self.inference.lock().unwrap();
        for cancel in registry
            .active
            .values()
            .chain(registry.retained.values())
            .flat_map(|requests| requests.values())
        {
            cancel.cancel();
        }
    }
    pub(crate) fn register_inference(
        &self,
        id: uuid::Uuid,
        cancel: CancellationToken,
    ) -> Result<InferenceRegistration> {
        self.register_execution(id, cancel, false)
    }
    pub(super) fn register_retained(
        &self,
        id: uuid::Uuid,
        cancel: CancellationToken,
    ) -> Result<InferenceRegistration> {
        self.register_execution(id, cancel, true)
    }
    fn register_execution(
        &self,
        id: uuid::Uuid,
        cancel: CancellationToken,
        retained: bool,
    ) -> Result<InferenceRegistration> {
        let mut registry = self.inference.lock().unwrap();
        if registry.stopping.contains(&id) {
            return Err(Error::new("BUSY", "账户实例正在停止。"));
        }
        let request = uuid::Uuid::new_v4();
        let registrations = if retained {
            &mut registry.retained
        } else {
            &mut registry.active
        };
        registrations.entry(id).or_default().insert(request, cancel);
        Ok(InferenceRegistration {
            owner: self.inference.clone(),
            account: id,
            id: request,
            retained,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restart_detects_busy_workers_without_waiting_for_the_slot_lock() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = Profiles::open(dir.path().join("accounts")).await.unwrap();
        let id = profiles.create("busy worker").await.unwrap().id;
        let slot = profiles.slot(id).await.unwrap();
        let guard = slot.lock().await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                profiles.has_worker(id)
            )
            .await
            .unwrap()
        );
        drop(guard);
        assert!(!profiles.has_worker(id).await);
    }
}
