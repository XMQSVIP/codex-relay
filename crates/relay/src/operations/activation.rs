//! Account service activation, independent of the lifetime of its worker.

use super::{AccountPolicy, DESKTOP_ACCOUNT, Inner, Operations};
use crate::{
    error::{Error, Result},
    provider::Provider,
};
use uuid::Uuid;

impl Operations {
    fn activation_policy(inner: &Inner, id: Uuid, revision: u64) -> Result<&AccountPolicy> {
        if id == DESKTOP_ACCOUNT {
            return Err(Error::new("INVALID_INPUT", "桌面连接仅用于本机调试。"));
        }
        if inner.draining {
            return Err(Error::new("SERVICE_DRAINING", "服务正在停止，请稍后重试。"));
        }
        if !inner.storage_healthy {
            return Err(Error::new(
                "OPERATIONS_STORAGE",
                "统计存储异常，请检查磁盘并重启服务。",
            ));
        }
        if revision != inner.settings.revision {
            return Err(Error::new(
                "CONFIG_CONFLICT",
                "配置已被其他操作更新，请刷新后重试。",
            ));
        }
        let account = inner
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))?;
        if account.deleting {
            return Err(Error::new("BUSY", "账户正在删除，不能修改启用状态。"));
        }
        Ok(account)
    }

    pub(crate) fn activation_provider(&self, id: Uuid, revision: u64) -> Result<Option<Provider>> {
        let inner = self.inner.lock().unwrap();
        Ok(Self::activation_policy(&inner, id, revision)?.route.clone())
    }

    pub fn set_account_activation(
        &self,
        id: Uuid,
        revision: u64,
        enabled: bool,
        provider: Option<Provider>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        Self::activation_policy(&inner, id, revision)?;
        if provider.is_some() && Self::account_busy(&inner, id) {
            return Err(Error::new("BUSY", "账户仍有请求，请结束后再更新接入配置。"));
        }
        let mut next = inner.settings.clone();
        let account = next.accounts.iter_mut().find(|a| a.id == id).unwrap();
        if let Some(provider) = provider {
            account.route = Some(provider);
        }
        if enabled && account.route.is_none() {
            return Err(Error::new("INFERENCE_DISABLED", "请先读取账户接入配置。"));
        }
        account.enabled = enabled;
        Self::save(&mut inner, next, revision)
    }

    pub(crate) fn account_scheduling_enabled(&self, id: Uuid) -> Result<bool> {
        let inner = self.inner.lock().unwrap();
        inner
            .settings
            .accounts
            .iter()
            .find(|a| a.id == id)
            .map(|a| {
                a.enabled
                    && inner
                        .settings
                        .groups
                        .iter()
                        .any(|g| a.belongs_to(g.id) && g.enabled)
            })
            .ok_or_else(|| Error::new("NOT_FOUND", "账户不存在。"))
    }

    fn account_busy(inner: &Inner, id: Uuid) -> bool {
        inner.active.values().any(|a| a.account == Some(id))
            || inner.pending.iter().any(|p| p.account == Some(id))
    }

    pub(crate) fn require_account_idle(&self, id: Uuid) -> Result<()> {
        if Self::account_busy(&self.inner.lock().unwrap(), id) {
            return Err(Error::new(
                "BUSY",
                "账户仍有执行或排队中的操作，请完成后重试。",
            ));
        }
        Ok(())
    }

    pub(crate) fn disabled_account_idle(&self, id: Uuid) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .settings
            .accounts
            .iter()
            .any(|a| a.id == id && !a.enabled && !a.deleting)
            && !Self::account_busy(&inner, id)
    }

    pub(crate) async fn wait_for_disabled_account_idle(&self, id: Uuid) -> bool {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let inner = self.inner.lock().unwrap();
                if !inner
                    .settings
                    .accounts
                    .iter()
                    .any(|a| a.id == id && !a.enabled && !a.deleting)
                {
                    return false;
                }
                if !Self::account_busy(&inner, id) {
                    return true;
                }
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_keeps_its_capacity_when_the_account_is_resumed() {
        let temp = tempfile::tempdir().unwrap();
        let ops = Operations::open(temp.path()).unwrap();
        let id = Uuid::new_v4();
        ops.register_accounts(&[(id, "maintenance".into())])
            .unwrap();
        let revision = |ops: &Operations| ops.snapshot()["revision"].as_u64().unwrap();
        let provider = Provider {
            execution: Default::default(),
            id: "test".into(),
            model: "test".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            auth: "none".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        };
        ops.set_inference_route(id, revision(&ops), Some(provider))
            .unwrap();
        ops.set_account_activation(id, revision(&ops), false, None)
            .unwrap();
        let maintenance = ops
            .begin_account_maintenance(id, "diagnostic", "test")
            .unwrap();
        ops.set_account_activation(id, revision(&ops), true, None)
            .unwrap();
        assert_eq!(ops.snapshot()["accounts"][0]["active"], 1);
        assert_eq!(
            ops.begin(Some(id), None, "inference", "responses", true)
                .err()
                .unwrap()
                .code,
            "CONCURRENCY_LIMIT"
        );
        maintenance.complete(Ok(())).unwrap();
        ops.begin(Some(id), None, "inference", "responses", true)
            .unwrap()
            .complete(Ok(()))
            .unwrap();
    }
}
