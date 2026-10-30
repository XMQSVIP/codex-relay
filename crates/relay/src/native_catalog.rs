//! Reuse bounded native metadata while refreshing it outside the inference path.
use crate::installation::Installation;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 4 * 1024 * 1024;
// Native discovery has a five-second deadline, including the response body.
// Allow a bounded stale snapshot during discovery outages, without changing its
// original timestamp. Refresh is scheduled after successful inference below.
const MAX_AGE_SECONDS: i64 = 24 * 3600;
const REFRESH_AGE_SECONDS: i64 = 60 * 60;

mod refresh;
pub(crate) use refresh::Refreshes;

pub(crate) struct ResponseCatalog {
    pub path: PathBuf,
    pub models: std::collections::HashSet<String>,
}

/// 为中转进程派生工具策略，不改写原生缓存、账号配置或模型能力参数。
/// 模型目录的 tool_mode 优先于 features.code_mode；仅关闭 feature 会泄露 exec。
fn client_tools_only(catalog: &mut Value) -> crate::error::Result<()> {
    let invalid = || {
        crate::error::Error::new(
            "NATIVE_TOOL_POLICY",
            "无法读取原生模型目录，未启动工具中转。请检查原生版本与模型配置。",
        )
    };
    let models = catalog["models"]
        .as_array_mut()
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .ok_or_else(invalid)?;
    for model in models {
        let object = model.as_object_mut().ok_or_else(invalid)?;
        object.insert("tool_mode".into(), json!("direct"));
        object.insert("multi_agent_version".into(), json!("disabled"));
        object.insert("apply_patch_tool_type".into(), Value::Null);
        object.insert("experimental_supported_tools".into(), json!([]));
    }
    Ok(())
}

pub(crate) async fn response_snapshot(
    installation: &Installation,
    home: Option<&Path>,
    cwd: &Path,
    model: &str,
    proxy: &crate::app_server_proxy::ProxyConfig,
) -> crate::error::Result<ResponseCatalog> {
    use std::{process::Stdio, time::Duration};
    use tokio::io::AsyncReadExt;
    let cached = match home {
        Some(home) => snapshot(installation, home, cwd, model).await,
        None => None,
    };
    let mut catalog: Value = if let Some(path) = &cached {
        serde_json::from_slice(&tokio::fs::read(path).await?)?
    } else {
        // 由同一个原生程序解析用户模型目录（包括自定义目录），避免内置默认值
        // 或目录缺失时悄悄回退到带宿主工具的会话。此命令只发现模型，不发起推理。
        let mut command = tokio::process::Command::new(&installation.app_server_executable);
        command
            .args(["debug", "models"])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        crate::backend::clean_env(&mut command, home.is_some());
        if let Some(home) = home {
            command.env("CODEX_HOME", home);
        }
        proxy.apply(&mut command).await?;
        crate::platform::hide(&mut command);
        let mut child = command.spawn()?;
        let mut stdout = child.stdout.take().unwrap().take(MAX_BYTES + 1);
        let mut bytes = vec![];
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            stdout.read_to_end(&mut bytes).await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>(status.success())
        })
        .await;
        if !matches!(result, Ok(Ok(true))) || bytes.len() as u64 > MAX_BYTES {
            let _ = child.kill().await;
            return Err(crate::error::Error::new(
                "NATIVE_TOOL_POLICY",
                "原生模型目录导出失败或超时，未启动工具中转。请检查原生版本与模型配置。",
            ));
        }
        serde_json::from_slice(&bytes)?
    };
    client_tools_only(&mut catalog)?;
    // 未知模型会触发原生的 fallback metadata；不能让 fallback 恢复宿主工具。
    let models: std::collections::HashSet<String> = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["slug"].as_str().map(str::to_owned))
        .collect();
    if !models.contains(model) {
        return Err(crate::error::Error::new(
            "NATIVE_TOOL_POLICY",
            "请求模型不在原生模型目录中，无法保证客户端工具隔离。请刷新模型目录。",
        ));
    }
    let path = cwd.join("client-tool-model-catalog.json");
    tokio::fs::write(&path, serde_json::to_vec(&catalog)?).await?;
    Ok(ResponseCatalog { path, models })
}

async fn read_cache(home: &Path) -> Option<Value> {
    let source = home.join("models_cache.json");
    if tokio::fs::metadata(&source).await.ok()?.len() > MAX_BYTES {
        return None;
    }
    let bytes = tokio::fs::read(&source).await.ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn age(cache: &Value, now: i64) -> Option<i64> {
    Some(
        now - chrono::DateTime::parse_from_rfc3339(cache["fetched_at"].as_str()?)
            .ok()?
            .timestamp(),
    )
}

pub(crate) async fn invalidate(home: &Path) -> crate::error::Result<()> {
    match tokio::fs::remove_file(home.join("models_cache.json")).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn catalog(cache: Value, installation: &Installation, model: &str, now: i64) -> Option<Value> {
    let version = installation
        .app_server_version
        .as_deref()?
        .strip_prefix("codex-cli ")?
        .split('-')
        .next()?;
    if cache["client_version"].as_str()? != version {
        return None;
    }
    if !(-300..=MAX_AGE_SECONDS).contains(&age(&cache, now)?) {
        return None;
    }
    let models = cache["models"].as_array()?;
    if models.len() > 512 || !models.iter().any(|m| m["slug"] == model) {
        return None;
    }
    // Keep the native model definitions unchanged. Auth and cache identity are
    // not part of a catalog, and never become configuration overrides here.
    Some(json!({"models":models}))
}

pub(crate) async fn snapshot(
    installation: &Installation,
    home: &Path,
    cwd: &Path,
    model: &str,
) -> Option<PathBuf> {
    // Preserve a deliberately configured native catalog. Even a commented key
    // makes this conservative optimization opt out, without parsing TOML here.
    if tokio::fs::read_to_string(home.join("config.toml"))
        .await
        .is_ok_and(|config| config.contains("model_catalog_json"))
    {
        return None;
    }
    let cache = read_cache(home).await?;
    let catalog = catalog(cache, installation, model, chrono::Utc::now().timestamp())?;
    let path = cwd.join("native-model-catalog.json");
    tokio::fs::write(&path, serde_json::to_vec(&catalog).ok()?)
        .await
        .ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_catalog_removes_host_tool_selectors_without_changing_model_capabilities() {
        let original = json!({"models":[{"slug":"gpt-6.1-sol","tool_mode":"code_mode_only",
            "multi_agent_version":"v2","apply_patch_tool_type":"freeform",
            "experimental_supported_tools":["clock","request_user_input_async"],
            "context_window":123456,"input_modalities":["text","image"],
            "supported_reasoning_levels":[{"effort":"high","description":"High"}],
            "service_tiers":["priority"],"future_model_field":{"keep":true}}]});
        let mut adapted = original.clone();
        client_tools_only(&mut adapted).unwrap();
        assert_eq!(adapted["models"][0]["tool_mode"], "direct");
        assert_eq!(adapted["models"][0]["multi_agent_version"], "disabled");
        assert!(adapted["models"][0]["apply_patch_tool_type"].is_null());
        assert_eq!(
            adapted["models"][0]["experimental_supported_tools"],
            json!([])
        );
        for field in [
            "slug",
            "context_window",
            "input_modalities",
            "supported_reasoning_levels",
            "service_tiers",
            "future_model_field",
        ] {
            assert_eq!(adapted["models"][0][field], original["models"][0][field]);
        }
        for mut invalid in [
            Value::Null,
            json!({}),
            json!({"models":[]}),
            json!({"models":[null]}),
        ] {
            assert!(client_tools_only(&mut invalid).is_err());
        }
    }

    fn installation() -> Installation {
        serde_json::from_value(json!({
            "format":1,"preparedAt":0,"desktopExecutable":"desktop",
            "appServerExecutable":"native","appServerSha256":"hash",
            "archive":"app.asar","sourceFiles":[],
            "appServerVersion":"codex-cli 0.163.0-alpha.11.2"
        }))
        .unwrap()
    }
    fn cache() -> Value {
        json!({"client_version":"0.163.0","fetched_at":"2026-10-24T14:00:00Z",
            "identity":"private-account-cache-tag","etag":"native-etag",
            "models":[{"slug":"test-model","default_reasoning_level":"high"}]})
    }
    #[test]
    fn cache_is_bounded_by_version_age_and_requested_model() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-24T14:30:00Z")
            .unwrap()
            .timestamp();
        let expected = json!({"models":cache()["models"]});
        assert_eq!(
            catalog(cache(), &installation(), "test-model", now),
            Some(expected)
        );
        for (field, value) in [
            ("client_version", json!("0.162.0")),
            ("fetched_at", json!("2026-10-23T13:00:00Z")),
            ("fetched_at", json!("2026-10-24T15:00:00Z")),
            ("fetched_at", json!("invalid")),
            ("models", json!([])),
        ] {
            let mut cache = cache();
            cache[field] = value;
            assert!(catalog(cache, &installation(), "test-model", now).is_none());
        }
        assert!(catalog(cache(), &installation(), "unknown-model", now).is_none());
        let mut stale = cache();
        stale["fetched_at"] = json!("2026-10-24T12:00:00Z");
        assert!(catalog(stale, &installation(), "test-model", now).is_some());
        let mut unknown = installation();
        unknown.app_server_version = None;
        assert!(catalog(cache(), &unknown, "test-model", now).is_none());
    }
    #[tokio::test]
    async fn response_catalog_covers_every_switchable_model_with_client_only_tools() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let cwd = root.path().join("request");
        tokio::fs::create_dir(&home).await.unwrap();
        tokio::fs::create_dir(&cwd).await.unwrap();
        let mut cache = cache();
        cache["fetched_at"] = json!(chrono::Utc::now().to_rfc3339());
        cache["models"].as_array_mut().unwrap().push(json!({
            "slug":"second-model","tool_mode":"code_mode_only",
            "multi_agent_version":"v2","experimental_supported_tools":["shell"]
        }));
        let bytes = serde_json::to_vec(&cache).unwrap();
        tokio::fs::write(home.join("models_cache.json"), &bytes)
            .await
            .unwrap();
        let snapshot = response_snapshot(
            &installation(),
            Some(&home),
            &cwd,
            "test-model",
            &Default::default(),
        )
        .await
        .unwrap();
        assert!(snapshot.models.contains("test-model"));
        assert!(snapshot.models.contains("second-model"));
        assert!(!snapshot.models.contains("unknown-model"));
        let saved: Value =
            serde_json::from_slice(&tokio::fs::read(snapshot.path).await.unwrap()).unwrap();
        for model in saved["models"].as_array().unwrap() {
            assert_eq!(model["tool_mode"], "direct");
            assert_eq!(model["multi_agent_version"], "disabled");
            assert_eq!(model["experimental_supported_tools"], json!([]));
        }
        assert_eq!(
            tokio::fs::read(home.join("models_cache.json"))
                .await
                .unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn snapshot_is_request_local_and_never_rewrites_native_cache() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let cwd = root.path().join("request");
        tokio::fs::create_dir(&home).await.unwrap();
        tokio::fs::create_dir(&cwd).await.unwrap();
        assert!(
            snapshot(&installation(), &home, &cwd, "test-model")
                .await
                .is_none()
        );
        let source = home.join("models_cache.json");
        tokio::fs::write(&source, b"invalid").await.unwrap();
        assert!(
            snapshot(&installation(), &home, &cwd, "test-model")
                .await
                .is_none()
        );
        let mut cache = cache();
        cache["fetched_at"] = json!(chrono::Utc::now().to_rfc3339());
        let bytes = serde_json::to_vec(&cache).unwrap();
        tokio::fs::write(&source, &bytes).await.unwrap();
        let path = snapshot(&installation(), &home, &cwd, "test-model")
            .await
            .unwrap();
        assert_eq!(path.parent(), Some(cwd.as_path()));
        let saved: Value = serde_json::from_slice(&tokio::fs::read(path).await.unwrap()).unwrap();
        assert_eq!(saved, json!({"models":cache["models"]}));
        assert_eq!(tokio::fs::read(source).await.unwrap(), bytes);
        tokio::fs::write(
            home.join("config.toml"),
            "model_catalog_json = 'custom.json'\n",
        )
        .await
        .unwrap();
        assert!(
            snapshot(&installation(), &home, &cwd, "test-model")
                .await
                .is_none()
        );
    }
}
