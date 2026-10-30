use super::{
    AdminState,
    activation::{ActivationInput, activate_locked},
};
use crate::{
    error::{Error, Result},
    operations::{AccountInput, AccountPolicy, DEFAULT_GROUP},
};
use axum::{Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ImportItem {
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub document: Option<Value>,
    #[serde(default)]
    pub upstream: Option<crate::upstream::Backup>,
    pub group_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub max_concurrency: Option<u32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BatchInput {
    revision: u64,
    action: String,
    #[serde(default)]
    account_ids: Vec<Uuid>,
    #[serde(default)]
    items: Vec<ImportItem>,
    enabled: Option<bool>,
    group_ids: Option<Vec<Uuid>>,
    max_concurrency: Option<u32>,
}
fn invalid() -> Error {
    Error::new("INVALID_INPUT", "批量请求无效；最多选择 32 项。")
}
fn validate(input: &BatchInput) -> Result<()> {
    let valid_groups = |multiple: &Option<Vec<Uuid>>| {
        multiple
            .as_ref()
            .is_none_or(|ids| !ids.is_empty() && ids.len() <= 64)
    };
    if !valid_groups(&input.group_ids)
        || input
            .items
            .iter()
            .any(|item| !valid_groups(&item.group_ids))
    {
        return Err(invalid());
    }
    if !["configure", "delete", "import"].contains(&input.action.as_str())
        || input
            .max_concurrency
            .is_some_and(|v| !(1..=256).contains(&v))
    {
        return Err(invalid());
    }
    if input.action == "import" {
        if input.items.is_empty() || input.items.len() > 32 || !input.account_ids.is_empty() {
            return Err(invalid());
        }
        for item in &input.items {
            if item.name.trim().is_empty()
                || item.name.chars().count() > 60
                || item.name.chars().any(char::is_control)
                || item
                    .max_concurrency
                    .is_some_and(|v| !(1..=256).contains(&v))
                || item
                    .document
                    .as_ref()
                    .is_some_and(|v| serde_json::to_vec(v).map_or(true, |v| v.len() > 256 * 1024))
                || item
                    .source
                    .as_deref()
                    .is_some_and(|s| !matches!(s, "isolated" | "native-config" | "api-key"))
                || (item.source.as_deref() == Some("api-key")
                    && (item.document.is_some() || item.upstream.is_none()))
                || (item.source.as_deref() != Some("api-key") && item.upstream.is_some())
            {
                return Err(invalid());
            }
        }
    } else if input.account_ids.is_empty()
        || input.account_ids.len() > 32
        || !input.items.is_empty()
        || input.account_ids.iter().collect::<HashSet<_>>().len() != input.account_ids.len()
        || input
            .account_ids
            .contains(&crate::operations::DESKTOP_ACCOUNT)
        || input.action == "configure"
            && input.enabled.is_none()
            && input.group_ids.is_none()
            && input.max_concurrency.is_none()
    {
        return Err(invalid());
    }
    Ok(())
}
pub(super) async fn submit(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<BatchInput>,
) -> Result<Json<Value>> {
    validate(&input)?;
    let operation_state = state.clone();
    // Finish accepted mutations even if the HTTP client disconnects.
    state
        .tasks
        .spawn(async move {
            let state = operation_state;
            let _batch = state
                .batch_lock
                .try_lock()
                .map_err(|_| Error::new("BUSY", "已有批量操作正在执行。"))?;
            let snapshot = state.operations.snapshot();
            if snapshot["draining"] == true {
                return Err(Error::new("SERVICE_DRAINING", "服务正在退出。"));
            }
            if snapshot["storageHealthy"] == false {
                return Err(Error::new(
                    "OPERATIONS_STORAGE",
                    "运行存储异常，暂不能执行批量操作。",
                ));
            }
            if revision(&state) != input.revision {
                return Err(conflict());
            }
            Ok(Json(run(&state, input).await))
        })
        .await
        .map_err(|_| Error::new("INTERNAL", "批量操作未完成，请刷新账户状态。"))?
}
fn revision(state: &AdminState) -> u64 {
    state.operations.snapshot()["revision"].as_u64().unwrap()
}
fn conflict() -> Error {
    Error::new(
        "CONFIG_CONFLICT",
        "配置已被其他操作修改，请刷新后重试剩余项目。",
    )
}
fn policy(state: &AdminState, id: Uuid) -> Result<AccountPolicy> {
    state.operations.account_policy(id)
}
async fn run(state: &Arc<AdminState>, input: BatchInput) -> Value {
    let count = if input.action == "import" {
        input.items.len()
    } else {
        input.account_ids.len()
    };
    let mut expected = input.revision;
    let mut halt = false;
    let mut items = Vec::with_capacity(count);
    for index in 0..count {
        let mut item = json!({});
        if input.action != "import" {
            item["accountId"] = input.account_ids[index].to_string().into();
        }
        if halt || state.shutdown.is_cancelled() {
            item["status"] = "skipped".into();
            item["code"] = if halt {
                "CONFIG_CONFLICT"
            } else {
                "SERVICE_DRAINING"
            }
            .into();
        } else {
            let result = {
                let _setup = state.account_setup.lock().await;
                if revision(state) != expected {
                    Err(conflict())
                } else if state.shutdown.is_cancelled() {
                    Err(Error::new("SERVICE_DRAINING", "服务正在退出。"))
                } else if input.action == "import" {
                    import_one(state, &input.items[index], &input, &mut expected).await
                } else {
                    operate(state, input.account_ids[index], &input, &mut expected).await
                }
            };
            halt = result.as_ref().is_err_and(|e| e.code == "CONFIG_CONFLICT")
                || revision(state) != expected;
            match result {
                Ok(id) => {
                    item["status"] = "succeeded".into();
                    item["accountId"] = id.to_string().into();
                }
                Err(error) => {
                    item["status"] = if matches!(error.code.as_str(), "BUSY" | "DUPLICATE_ACCOUNT")
                    {
                        "skipped"
                    } else {
                        "failed"
                    }
                    .into();
                    item["code"] = error.code.into();
                }
            }
        }
        items.push(item);
    }
    let status = if halt {
        "conflict"
    } else if items.iter().any(|i| i["status"] != "succeeded") {
        "partial"
    } else {
        "completed"
    };
    json!({"action": input.action, "status": status, "items": items})
}
async fn operate(
    state: &Arc<AdminState>,
    id: Uuid,
    input: &BatchInput,
    expected: &mut u64,
) -> Result<Uuid> {
    if input.action == "delete" {
        state.operations.require_account_idle(id)?;
        state.operations.begin_account_delete(id, *expected)?;
        *expected += 1;
        state.profiles.delete(id).await?;
        state.operations.finish_account_delete(id)?;
        *expected += 1;
        return Ok(id);
    }
    let p = policy(state, id)?;
    if input.group_ids.is_some() || input.max_concurrency.is_some() {
        let groups = input
            .group_ids
            .clone()
            .unwrap_or_else(|| p.groups().to_vec());
        state.operations.set_account_groups(
            id,
            AccountInput {
                revision: *expected,
                group_id: groups[0],
                enabled: p.enabled,
                max_concurrency: input.max_concurrency.unwrap_or(p.max_concurrency),
            },
            groups,
        )?;
        *expected += 1;
    }
    if let Some(enabled) = input.enabled {
        let _ = activate_locked(
            state,
            id,
            ActivationInput {
                revision: *expected,
                enabled,
            },
        )
        .await?;
        *expected += 1;
    }
    Ok(id)
}
async fn import_one(
    state: &Arc<AdminState>,
    item: &ImportItem,
    input: &BatchInput,
    expected: &mut u64,
) -> Result<Uuid> {
    let mut groups = input
        .group_ids
        .clone()
        .or_else(|| item.group_ids.clone())
        .unwrap_or_else(|| vec![DEFAULT_GROUP]);
    let mut seen = HashSet::new();
    groups.retain(|id| seen.insert(*id));
    if state.profiles.metadata().await.len() >= 32
        || groups.iter().any(|id| {
            !state.operations.snapshot()["groups"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["id"] == id.to_string())
        })
    {
        return Err(invalid());
    }
    let api_config = item
        .upstream
        .as_ref()
        .map(|upstream| upstream.restore())
        .transpose()?;
    if let Some(config) = &api_config {
        state.check_route_loop(&config.base_url).await?;
    }
    let prepared = if let Some(document) = &item.document {
        let tokens = if document["tokens"].is_object() {
            &document["tokens"]
        } else {
            document
        };
        if let Some(token) = tokens["refreshToken"]
            .as_str()
            .or_else(|| tokens["refresh_token"].as_str())
        {
            for (id, _) in state.profiles.metadata().await {
                if state.profiles.owns_refresh_token(id, token).await? {
                    return Err(Error::new(
                        "DUPLICATE_ACCOUNT",
                        "该续期凭据已由现有账号管理。",
                    ));
                }
            }
        }
        let credential = crate::oauth::prepare_import(document, &Default::default()).await?;
        let fingerprint = crate::oauth::fingerprint(&credential.identity);
        for (id, _) in state.profiles.metadata().await {
            if state.profiles.credential_identity(id).await?.as_deref() == Some(&fingerprint) {
                return Err(Error::new("DUPLICATE_ACCOUNT", "账号身份已存在。"));
            }
        }
        Some(credential.native_document())
    } else {
        None
    };
    if revision(state) != *expected {
        return Err(conflict());
    }
    if groups.iter().any(|id| {
        !state.operations.snapshot()["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["id"] == id.to_string())
    }) {
        return Err(invalid());
    }
    let account = if let Some(config) = api_config {
        state.profiles.create_upstream(&item.name, config).await?
    } else if item.source.as_deref() == Some("native-config") && prepared.is_none() {
        state.profiles.create_native_config(&item.name).await?
    } else {
        state.profiles.create(&item.name).await?
    };
    let id = account.id;
    if let Err(error) = state.operations.register_import(
        id,
        account.name,
        AccountInput {
            revision: *expected,
            group_id: groups[0],
            enabled: false,
            max_concurrency: input
                .max_concurrency
                .or(item.max_concurrency)
                .unwrap_or(crate::operations::DEFAULT_ACCOUNT_CONCURRENCY),
        },
        groups,
    ) {
        state.profiles.delete(id).await?;
        return Err(error);
    }
    *expected += 1;
    let result = async {
        if let Some(document) = prepared {
            state.profiles.import_credentials(id, document).await?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        // Only the new, unrouted account is eligible for cleanup.
        if revision(state) == *expected {
            state.operations.begin_account_delete(id, *expected)?;
            *expected += 1;
            state.profiles.delete(id).await?;
            state.operations.finish_account_delete(id)?;
            *expected += 1;
        }
        return Err(error);
    }
    if input.enabled == Some(true) && (item.document.is_some() || item.upstream.is_some()) {
        let _ = activate_locked(
            state,
            id,
            ActivationInput {
                revision: *expected,
                enabled: true,
            },
        )
        .await?;
        *expected += 1;
    }
    Ok(id)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ExportInput {
    account_ids: Vec<Uuid>,
}
pub(super) async fn export(
    State(state): State<Arc<AdminState>>,
    Json(input): Json<ExportInput>,
) -> Result<Json<Value>> {
    if input.account_ids.is_empty() || input.account_ids.len() > 32 {
        return Err(invalid());
    }
    let _setup = state.account_setup.lock().await;
    let snapshot = state.operations.snapshot();
    let mut items = vec![];
    let metadata = state.profiles.list().await;
    for id in &input.account_ids {
        let lease =
            state
                .operations
                .begin_account_maintenance(*id, "management", "export_credentials")?;
        let account = metadata
            .iter()
            .find(|a| a.profile.id == *id)
            .ok_or_else(|| Error::new("NOT_FOUND", "账号不存在。"))?;
        let p = policy(&state, *id)?;
        let group_names: Vec<_> = snapshot["groups"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|g| p.groups().iter().any(|id| g["id"] == id.to_string()))
            .map(|g| g["name"].clone())
            .collect();
        let document = state.profiles.export_credentials(*id).await?;
        let mut item = json!({"name":p.name,"source":account.profile.source,"groupIds":p.groups(),"groupNames":group_names,"maxConcurrency":p.max_concurrency,"requiresReconnect":document.is_none(),"document":document});
        if account.profile.source == "api-key" {
            item["upstream"] = serde_json::to_value(state.profiles.upstream(*id).await?.backup()?)?;
            item["requiresReconnect"] = json!(false);
        }
        items.push(item);
        lease.complete(Ok(()))?;
    }
    Ok(Json(
        json!({"format":"codex-relay-accounts","version":1,"items":items}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn setup() -> (tempfile::TempDir, Arc<AdminState>, Vec<Uuid>) {
        let dir = tempfile::tempdir().unwrap();
        let state = AdminState::new_with_network(4317, dir.path().join("accounts"), None, vec![])
            .await
            .unwrap();
        let mut ids = vec![];
        for name in ["one", "two"] {
            let p = state.profiles.create(name).await.unwrap();
            state
                .operations
                .register_accounts(&[(p.id, p.name)])
                .unwrap();
            ids.push(p.id);
        }
        (dir, state, ids)
    }
    #[tokio::test]
    async fn external_revision_conflict_stops_remaining_items() {
        let (_dir, state, ids) = setup().await;
        let lock = state.account_setup.lock().await;
        let input = serde_json::from_value(json!({"revision":revision(&state),"action":"configure","accountIds":ids,"maxConcurrency":8})).unwrap();
        let running_state = state.clone();
        let operation = tokio::spawn(async move { run(&running_state, input).await });
        state.operations.set_global(revision(&state), 5).unwrap();
        drop(lock);
        let result = operation.await.unwrap();
        assert_eq!(result["status"], "conflict");
        assert_eq!(result["items"][1]["status"], "skipped");
        assert_eq!(policy(&state, ids[0]).unwrap().max_concurrency, 5);
    }
    #[tokio::test]
    async fn concurrent_batch_is_rejected_without_mutation() {
        let (_dir, state, ids) = setup().await;
        let _lock = state.batch_lock.lock().await;
        let input = serde_json::from_value(json!({"revision":revision(&state),"action":"configure","accountIds":ids,"maxConcurrency":8})).unwrap();
        let error = submit(State(state.clone()), Json(input)).await.unwrap_err();
        assert_eq!(error.code, "BUSY");
        assert_eq!(policy(&state, ids[0]).unwrap().max_concurrency, 5);
    }
}
