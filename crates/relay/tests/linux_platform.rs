//! Headless Linux must never fall back to Windows desktop discovery.
#![cfg(target_os = "linux")]

use codex_relay::{desktop, installation, platform};

#[tokio::test]
async fn desktop_discovery_explains_standalone_configuration() {
    let error = platform::discover().await.unwrap_err();
    assert_eq!(error.code, "DESKTOP_UNAVAILABLE");
    assert!(error.message.contains("--codex-path"));
    assert_eq!(
        installation::prepare(None).await.unwrap_err().code,
        "DESKTOP_UNAVAILABLE"
    );
    assert!(
        !platform::verify_peer(1, 2, std::process::id(), std::path::Path::new("/bin/sh")).await
    );
}

#[tokio::test]
async fn headless_diagnostics_do_not_require_powershell_or_a_desktop() {
    let report = desktop::doctor().await.unwrap();
    assert_eq!(report["desktops"], serde_json::json!([]));
    assert_eq!(report["sharedDesktopVerified"], false);
}
