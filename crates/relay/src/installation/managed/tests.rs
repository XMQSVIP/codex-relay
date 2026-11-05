use super::*;
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha512};

fn manager(directory: &Path) -> Arc<Manager> {
    let mut manager = Manager::open(
        directory.join("native-codex"),
        directory.join("launch.json"),
    );
    manager.architecture = Some("x64");
    Arc::new(manager)
}

fn metadata(origin: &url::Url, bytes: &[u8]) -> Value {
    json!({"name":"@openai/codex","version":"0.166.0-linux-x64","os":["linux"],"cpu":["x64"],
        "dist":{"tarball":origin.join("@openai/codex/-/codex-0.166.0-linux-x64.tgz").unwrap().as_str(),
        "integrity":format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))),"unpackedSize":1024}})
}

fn release(manager: &Manager, selected: &str) -> Installation {
    let directory = manager.root.join("versions").join(selected);
    std::fs::create_dir_all(directory.join("bin")).unwrap();
    let executable = directory.join("bin/codex");
    std::fs::write(&executable, "test binary").unwrap();
    let hash = super::super::hash_file(&executable).unwrap();
    let release = Release {
        version: selected.into(),
        installed_at: 1,
        executable: "bin/codex".into(),
        sha256: hash.clone(),
        integrity: "test".into(),
    };
    write_json(&directory.join("relay-release.json"), &release).unwrap();
    manager.runtime.lock().unwrap().releases.push(release);
    serde_json::from_value(json!({"format":1,"manuallyConfigured":true,"preparedAt":1,
        "desktopExecutable":null,"archive":null,"sourceFiles":[],"nativeClientInfo":{"name":"codex_desktop","title":"Codex Desktop","version":selected},
        "appServerExecutable":executable,"appServerVersion":format!("codex-cli {selected}"),"appServerSha256":hash})).unwrap()
}

#[test]
fn rejects_unstable_versions_and_untrusted_packages() {
    for value in [
        "../0.1.0",
        "0.1.0/../../",
        "0.1.0-beta",
        "01.2.3",
        "1.2",
        "1.2.3?x",
        "1.2.3\n",
    ] {
        assert!(version(value).is_none(), "{value}");
    }
    let registry = download::Registry::official();
    let origin = url::Url::parse("https://registry.npmjs.org/").unwrap();
    let valid = metadata(&origin, b"fixture");
    assert!(download::test_support::parse(&registry, &valid, "0.166.0-linux-x64", "x64").is_ok());
    for (field, value) in [
        ("tarball", json!("https://evil.example/codex.tgz")),
        (
            "tarball",
            json!(
                "https://registry.npmjs.org/@openai/codex/-/codex-0.166.0-linux-x64.tgz?redirect=1"
            ),
        ),
        ("integrity", json!("sha512-aGVsbG8=")),
        ("unpackedSize", json!(download::MAX_UNPACKED_BYTES + 1)),
    ] {
        let mut invalid = valid.clone();
        invalid["dist"][field] = value;
        assert!(
            download::test_support::parse(&registry, &invalid, "0.166.0-linux-x64", "x64").is_err()
        );
    }
    let mut wrong_arch = valid;
    wrong_arch["cpu"] = json!(["arm64"]);
    assert!(
        download::test_support::parse(&registry, &wrong_arch, "0.166.0-linux-x64", "x64").is_err()
    );
}

#[tokio::test]
async fn publication_rollback_deletion_and_restart_preserve_versions() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    let old = release(&manager, "0.165.0");
    let new = release(&manager, "0.166.0");
    save_installation(&new, &manager.config).unwrap();
    let shutdown = CancellationToken::new();
    let run = manager.begin(Action::Activate, &shutdown).await.unwrap();
    manager.activate(&run, &old).unwrap();
    run.finish(Ok("old version activated".into()));
    let state = manager.snapshot(Some(&old), |_| false);
    assert_eq!(state.active_version.as_deref(), Some("0.165.0"));
    assert_eq!(state.versions.len(), 2);
    let run = manager.begin(Action::Remove, &shutdown).await.unwrap();
    assert_eq!(
        manager.detach(&run, "0.165.0").unwrap_err().code,
        "CODEX_CURRENT_VERSION"
    );
    let trash = manager.detach(&run, "0.166.0").unwrap();
    std::fs::remove_dir_all(trash).unwrap();
    run.finish(Ok("removed".into()));
    let reopened = Manager::open(manager.root.clone(), manager.config.clone());
    assert_eq!(reopened.snapshot(Some(&old), |_| false).versions.len(), 1);
    let run = manager.begin(Action::Activate, &shutdown).await.unwrap();
    std::fs::write(&manager.config, b"external change").unwrap();
    assert_eq!(
        manager.activate(&run, &old).unwrap_err().code,
        "CONFIG_CONFLICT"
    );
    assert_eq!(std::fs::read(&manager.config).unwrap(), b"external change");
}

#[tokio::test]
async fn cancellation_drop_and_exclusion_release_the_install_lock() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    let shutdown = CancellationToken::new();
    let run = manager.begin(Action::Install, &shutdown).await.unwrap();
    assert_eq!(
        manager
            .begin(Action::Check, &shutdown)
            .await
            .err()
            .unwrap()
            .code,
        "BUSY"
    );
    let mut other = Manager::open(manager.root.clone(), manager.config.clone());
    other.architecture = Some("x64");
    assert_eq!(
        Arc::new(other)
            .begin(Action::Check, &shutdown)
            .await
            .err()
            .unwrap()
            .code,
        "BUSY"
    );
    manager.cancel(run.id()).unwrap();
    assert!(run.cancel.is_cancelled());
    drop(run);
    assert_eq!(
        manager.snapshot(None, |_| false).operation.unwrap().phase,
        "cancelled"
    );
    let run = manager.begin(Action::Check, &shutdown).await.unwrap();
    shutdown.cancel();
    assert_eq!(run.check_cancelled().unwrap_err().code, "CODEX_CANCELLED");
    drop(run);
    assert_eq!(
        manager
            .begin(Action::Check, &shutdown)
            .await
            .err()
            .unwrap()
            .code,
        "CODEX_CANCELLED"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn manually_selected_symlink_still_protects_the_current_version() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    let mut selected = release(&manager, "0.166.0");
    let link = directory.path().join("manual-codex");
    std::os::unix::fs::symlink(&selected.app_server_executable, &link).unwrap();
    selected.app_server_executable = link;
    save_installation(&selected, &manager.config).unwrap();
    assert_eq!(
        manager
            .snapshot(Some(&selected), |_| false)
            .active_version
            .as_deref(),
        Some("0.166.0")
    );
    let run = manager
        .begin(Action::Remove, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        manager.detach(&run, "0.166.0").unwrap_err().code,
        "CODEX_CURRENT_VERSION"
    );
}

#[tokio::test]
async fn daily_check_is_persisted_and_never_installs() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    assert!(manager.due(1));
    let run = manager
        .begin(Action::Check, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!manager.due(u64::MAX));
    {
        let mut state = manager.runtime.lock().unwrap();
        state.cache = Cache {
            latest_version: Some("0.166.0".into()),
            checked_at: Some(100),
            attempted_at: Some(100),
            failed: false,
        };
    }
    manager.save_cache().unwrap();
    run.finish(Ok("checked".into()));
    assert!(!manager.due(100 + DAY_MS - 1));
    assert!(manager.due(100 + DAY_MS));
    assert!(manager.snapshot(None, |_| false).versions.is_empty());
    assert!(!manager.config.exists());
    let mut reopened = Manager::open(manager.root.clone(), manager.config.clone());
    reopened.architecture = Some("x64");
    assert!(!reopened.due(100 + DAY_MS - 1));
    assert!(
        reopened
            .snapshot(None, |_| false)
            .download_url
            .unwrap()
            .ends_with("codex-0.166.0-linux-x64.tgz")
    );
    reopened.runtime.lock().unwrap().cache.failed = true;
    assert!(!reopened.due(100 + RETRY_MS - 1));
    assert!(reopened.due(100 + RETRY_MS));
}

async fn server(body: Vec<u8>) -> (url::Url, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = url::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let app = axum::Router::new().fallback(move || {
        let body = body.clone();
        async move { body }
    });
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (origin, task)
}

#[tokio::test]
async fn downloads_direct_or_through_proxy_and_discards_bad_checksums() {
    let directory = tempfile::tempdir().unwrap();
    let manager = manager(directory.path());
    let run = manager
        .begin(Action::Install, &CancellationToken::new())
        .await
        .unwrap();
    let bytes = b"fixture archive";
    let (origin, task) = server(bytes.to_vec()).await;
    let registry = download::test_support::registry(origin.clone());
    let package = download::test_support::parse(
        &registry,
        &metadata(&origin, bytes),
        "0.166.0-linux-x64",
        "x64",
    )
    .unwrap();
    let downloaded = registry
        .download(&package, &manager.root, &run)
        .await
        .unwrap();
    assert_eq!(std::fs::read(downloaded.path()).unwrap(), bytes);
    drop(downloaded);
    let bad = download::test_support::parse(
        &registry,
        &metadata(&origin, b"different"),
        "0.166.0-linux-x64",
        "x64",
    )
    .unwrap();
    assert_eq!(
        registry
            .download(&bad, &manager.root, &run)
            .await
            .err()
            .unwrap()
            .code,
        "CODEX_INTEGRITY"
    );
    // The origin cannot resolve: success requires the explicitly configured proxy.
    let unreachable = url::Url::parse("http://codex-download.invalid/").unwrap();
    let proxy = download::test_support::proxied(unreachable.clone(), origin.to_string());
    let package = download::test_support::parse(
        &proxy,
        &metadata(&unreachable, bytes),
        "0.166.0-linux-x64",
        "x64",
    )
    .unwrap();
    let downloaded = proxy.download(&package, &manager.root, &run).await.unwrap();
    assert_eq!(std::fs::read(downloaded.path()).unwrap(), bytes);
    drop(downloaded);
    task.abort();
    assert!(proxy.download(&package, &manager.root, &run).await.is_err());
    manager.cancel(run.id()).unwrap();
    assert_eq!(
        registry
            .download(&bad, &manager.root, &run)
            .await
            .err()
            .unwrap()
            .code,
        "CODEX_CANCELLED"
    );
    assert!(!std::fs::read_dir(&manager.root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".download-")
    }));
}

fn archive_fixture(path: &Path, name: &str, kind: tar::EntryType, payload: &[u8]) {
    let encoder =
        flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(0o755);
    header.set_size(payload.len() as u64);
    if kind.is_symlink() {
        header.set_link_name("/tmp/escape").unwrap();
    }
    header.set_cksum();
    archive.append_data(&mut header, name, payload).unwrap();
    archive.into_inner().unwrap().finish().unwrap();
}

#[test]
fn archive_rejects_links_wrong_architecture_and_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let archive_path = directory.path().join("native.tgz");
    let target = download::target("x64").unwrap();
    let name = format!("package/vendor/{target}/bin/codex");
    archive_fixture(&archive_path, &name, tar::EntryType::Symlink, b"");
    assert!(
        archive::extract(
            &archive_path,
            &directory.path().join("bad"),
            target,
            &CancellationToken::new()
        )
        .is_err()
    );
    let mut elf = [0u8; 64];
    elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    elf[18] = 62;
    archive_fixture(&archive_path, &name, tar::EntryType::Regular, &elf);
    let output = directory.path().join("good");
    archive::extract(&archive_path, &output, target, &CancellationToken::new()).unwrap();
    assert_eq!(archive::executable(&output, "x64").unwrap(), "bin/codex");
    assert!(archive::executable(&output, "arm64").is_err());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&archive_path)
        .unwrap();
    file.set_len(file.metadata().unwrap().len() - 8).unwrap();
    assert!(
        archive::extract(
            &archive_path,
            &directory.path().join("truncated"),
            target,
            &CancellationToken::new()
        )
        .is_err()
    );
}

#[tokio::test]
#[cfg(target_os = "linux")]
#[ignore = "Downloads official Codex and starts isolated Linux app-server; no account login or model request"]
async fn official_linux_download_install_activate_and_reload() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(Manager::open(
        directory.path().join("native-codex"),
        directory.path().join("launch.json"),
    ));
    let run = manager
        .begin(Action::Install, &CancellationToken::new())
        .await
        .unwrap();
    let installed = manager.install(&run, None).await.unwrap();
    manager.activate(&run, &installed).unwrap();
    run.finish(Ok("installed".into()));
    let selected = manager
        .snapshot(Some(&installed), |_| false)
        .active_version
        .unwrap();
    let run = manager
        .begin(Action::Activate, &CancellationToken::new())
        .await
        .unwrap();
    manager.validate_installed(&run, &selected).await.unwrap();
    manager.activate(&run, &installed).unwrap();
    assert_eq!(
        manager.detach(&run, &selected).unwrap_err().code,
        "CODEX_CURRENT_VERSION"
    );
    run.finish(Ok("revalidated".into()));
    let reopened = Manager::open(manager.root.clone(), manager.config.clone());
    assert_eq!(
        reopened
            .snapshot(Some(&installed), |_| false)
            .active_version,
        Some(selected)
    );
}
