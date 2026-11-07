//! Per-account proxy settings and admission barrier during an immediate restart.
use super::{DESKTOP_ACCOUNT, Operations};
use crate::{
    app_server_proxy::ProxyConfig,
    error::{Error, Result},
};
use serde_json::{Value, json};
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub(super) struct Change {
    generation: u64,
    state: &'static str,
    error: Option<Error>,
    pub(super) restart_allowed: bool,
    was_running: bool,
}

impl Change {
    pub(super) fn view(&self) -> Value {
        json!({"state": self.state, "error": self.error})
    }
}

impl Operations {
    pub(crate) fn location_config(
        &self,
        id: Uuid,
    ) -> Result<(crate::account_location::Config, ProxyConfig, u64)> {
        let inner = self.inner.lock().unwrap();
        let account = inner
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id && !a.deleting)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在或正在删除。"))?;
        Ok((
            account.location.clone(),
            account.proxy.clone(),
            self.locations.token(id),
        ))
    }

    pub(crate) async fn resolve_location(
        self: &std::sync::Arc<Self>,
        id: Uuid,
        force: bool,
    ) -> Option<crate::account_location::Location> {
        let (config, proxy, token) = self.location_config(id).ok()?;
        self.locations
            .resolve(id, token, config, proxy, force)
            .await
    }

    pub fn account_proxy(&self, id: Uuid) -> ProxyConfig {
        self.inner
            .lock()
            .unwrap()
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.proxy.clone())
            .unwrap_or_default()
    }

    pub(crate) fn proxy_blocked(&self, id: Uuid) -> bool {
        self.inner.lock().unwrap().proxy_changes.contains_key(&id)
    }
    pub(crate) fn proxy_failed(&self, id: Uuid) -> bool {
        self.inner
            .lock()
            .unwrap()
            .proxy_changes
            .get(&id)
            .is_some_and(|change| change.state == "failed")
    }

    pub fn set_account_proxy(
        &self,
        id: Uuid,
        revision: u64,
        config: ProxyConfig,
    ) -> Result<Option<u64>> {
        self.set_account_environment(id, revision, Some(config.validated()?), None, false)
    }

    pub fn set_account_location(
        &self,
        id: Uuid,
        revision: u64,
        config: crate::account_location::Config,
        force: bool,
    ) -> Result<Option<u64>> {
        self.set_account_environment(id, revision, None, Some(config.validated()?), force)
    }

    fn set_account_environment(
        &self,
        id: Uuid,
        revision: u64,
        proxy: Option<ProxyConfig>,
        location: Option<crate::account_location::Config>,
        force: bool,
    ) -> Result<Option<u64>> {
        let mut inner = self.inner.lock().unwrap();
        if inner.draining {
            return Err(Error::new("SERVICE_DRAINING", "服务正在停止。"));
        }
        if revision != inner.settings.revision {
            return Err(Error::new("CONFIG_CONFLICT", "配置已更新，请刷新后重试。"));
        }
        if !inner.storage_healthy {
            return Err(Error::new("OPERATIONS_STORAGE", "运行数据读写失败。"));
        }
        if id == DESKTOP_ACCOUNT {
            return Err(Error::new(
                "INVALID_INPUT",
                "桌面共享调试连接不支持代理或位置设置。",
            ));
        }
        let account = inner
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        if account.deleting {
            return Err(Error::new("BUSY", "账户正在删除。"));
        }
        let proxy_changed = proxy
            .as_ref()
            .is_some_and(|config| account.proxy != *config);
        let location_changed = location
            .as_ref()
            .is_some_and(|config| account.location != *config);
        if !proxy_changed && !location_changed && !force && !inner.proxy_changes.contains_key(&id) {
            return Ok(None);
        }
        let restart_allowed = inner
            .proxy_changes
            .get(&id)
            .is_none_or(|c| c.restart_allowed);
        let was_running = inner.proxy_changes.get(&id).is_some_and(|c| c.was_running);
        let mut next = inner.settings.clone();
        let account = next.accounts.iter_mut().find(|a| a.id == id).unwrap();
        if let Some(config) = proxy {
            account.proxy = config;
        }
        if let Some(config) = location {
            account.location = config;
        }
        Self::save(&mut inner, next, revision)?;
        self.locations.invalidate(id, proxy_changed);
        let generation = inner.settings.revision;
        inner.proxy_changes.insert(
            id,
            Change {
                generation,
                state: "switching",
                error: None,
                restart_allowed,
                was_running,
            },
        );
        inner.changed.notify_waiters();
        Ok(Some(generation))
    }

    pub(crate) fn proxy_current(&self, id: Uuid, generation: u64) -> bool {
        let inner = self.inner.lock().unwrap();
        !inner.draining
            && inner
                .settings
                .accounts
                .iter()
                .any(|a| a.id == id && !a.deleting)
            && inner
                .proxy_changes
                .get(&id)
                .is_some_and(|c| c.generation == generation)
    }

    pub(crate) fn begin_proxy_switch(
        &self,
        id: Uuid,
        generation: u64,
        running: bool,
    ) -> Option<bool> {
        let mut inner = self.inner.lock().unwrap();
        if inner.draining {
            return None;
        }
        inner
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id && !a.deleting)?;
        let change = inner
            .proxy_changes
            .get_mut(&id)
            .filter(|c| c.generation == generation)?;
        change.state = "switching";
        change.was_running |= running;
        Some(change.restart_allowed && change.was_running)
    }

    pub(crate) fn cancel_proxy_restart(&self, id: Uuid) {
        if let Some(change) = self.inner.lock().unwrap().proxy_changes.get_mut(&id) {
            change.restart_allowed = false;
        }
    }

    pub(crate) fn finish_proxy_switch(&self, id: Uuid, generation: u64, result: Result<()>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(change) = inner
            .proxy_changes
            .get_mut(&id)
            .filter(|c| c.generation == generation)
        {
            if let Err(error) = result {
                change.state = "failed";
                change.error = Some(error);
            } else {
                inner.proxy_changes.remove(&id);
            }
            inner.changed.notify_waiters();
        }
    }
}
