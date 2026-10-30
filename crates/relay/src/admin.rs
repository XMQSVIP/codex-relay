//! Shared management state and route composition.
//!
//! All protected routes pass through the same authorization/body-limit layers.
//! Domain handlers remain private; public entry points keep their original paths.

use crate::{desktop, error::Result, operations::Operations, profiles::Profiles};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue},
    middleware,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::Mutex;
use tokio_util::task::TaskTracker;
use tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer};

mod access;
mod accounts;
mod activation;
mod api_keys;
mod batch;
mod chat_test;
mod diagnostics;
mod local_connection;
mod location;
mod operations;
mod proxy;
mod quota;
mod route_loop;
mod routing;
mod server;
mod subscription;

pub use access::authorize;
pub use diagnostics::{Job, run_job};
pub use server::{serve, serve_with_network};

use access::session;
use accounts::{
    cancel_login, create_account, delete_account, import_token, link_desktop_credentials,
    login_account, login_callback, login_status, logout_credentials, restart_account,
    start_account, stop_account,
};
use activation::set_activation;
use api_keys::{
    api_keys, create_api_key, delete_api_key, export_api_keys, import_api_keys,
    set_api_key_concurrency, set_api_key_group, update_api_key,
};
use diagnostics::{overview, start_desktop, start_job, stop_desktop};
use operations::{
    account_policy, cancel_request, clear_request_history, create_group, delete_group, operations,
    queue, request_content, set_concurrency, set_native_affinity, set_queue, set_retention,
    statistics, update_group,
};
use routing::{
    capabilities, check_account, check_group, set_features, set_route, test_connection,
    verify_feature,
};
use server::stop_service;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionInput {
    revision: u64,
}

pub struct AdminState {
    pub responses: crate::responses::Store,
    pub(crate) upstream_connections: crate::upstream::Connections,
    pub profiles: Profiles,
    pub(crate) desktop_control: desktop::Controller,
    pub operations: Arc<Operations>,
    pub(crate) access: Arc<crate::admin_access::Access>,
    token: Option<String>,
    origin: String,
    network_endpoints: Vec<crate::network::Endpoint>,
    local_policy: crate::network::AccessPolicy,
    pub jobs: Mutex<Vec<Job>>,
    pub(crate) account_setup: Mutex<()>,
    account_stops: std::sync::Mutex<activation::PendingStops>,
    local_inspections: Mutex<local_connection::Inspections>,
    local_detection: Mutex<()>,
    quota_refreshes: quota::Refreshes,
    subscription_refreshes: subscription::Refreshes,
    batch_lock: Mutex<()>,
    busy: AtomicBool,
    pub(crate) tasks: TaskTracker,
    stop_requested: tokio_util::sync::CancellationToken,
    pub(crate) shutdown: tokio_util::sync::CancellationToken,
}
impl AdminState {
    pub(crate) fn management_allows(&self, headers: &HeaderMap) -> bool {
        self.local_policy
            .allows_with_public(headers, self.access.public())
    }
    fn operations_snapshot(&self) -> Value {
        let mut snapshot = self.operations.snapshot();
        snapshot["responseBindings"] = self.responses.snapshot();
        snapshot
    }

    pub(crate) fn local_access_policy(&self) -> crate::network::AccessPolicy {
        self.local_policy.clone()
    }
    pub async fn new(port: u16, root: PathBuf, token: String) -> Result<Arc<Self>> {
        Self::new_with_network(port, root, Some(token), vec![]).await
    }
    pub async fn new_with_network(
        port: u16,
        root: PathBuf,
        token: Option<String>,
        network_endpoints: Vec<crate::network::Endpoint>,
    ) -> Result<Arc<Self>> {
        let mut profiles = Profiles::open(root.clone()).await?;
        let operations = Operations::open(&root.join("operations"))?;
        profiles.use_proxy_settings(operations.clone());
        operations.remove_desktop_route()?;
        for id in operations.pending_account_deletions() {
            match profiles.delete(id).await {
                Ok(()) => operations.finish_account_delete(id)?,
                Err(error) => eprintln!("account_delete_pending: {id} {}", error.code),
            }
        }
        operations.register_accounts(&profiles.metadata().await)?;
        let access = crate::admin_access::Access::open(root.join("admin-access.json")).await?;
        let state = Arc::new(Self {
            responses: crate::responses::Store::with_persistent_bindings(operations.clone())?,
            upstream_connections: Default::default(),
            profiles,
            desktop_control: desktop::Controller::default(),
            operations,
            access,
            token,
            origin: format!("http://127.0.0.1:{port}"),
            local_policy: crate::network::AccessPolicy::local(port),
            network_endpoints,
            jobs: Mutex::new(Vec::new()),
            account_setup: Mutex::new(()),
            account_stops: std::sync::Mutex::new(Default::default()),
            local_inspections: Mutex::new(Default::default()),
            local_detection: Mutex::new(()),
            quota_refreshes: quota::Refreshes::default(),
            subscription_refreshes: subscription::Refreshes::default(),
            batch_lock: Mutex::new(()),
            busy: AtomicBool::new(false),
            tasks: TaskTracker::new(),
            stop_requested: tokio_util::sync::CancellationToken::new(),
            shutdown: tokio_util::sync::CancellationToken::new(),
        });
        Ok(state)
    }
}
pub fn router(state: Arc<AdminState>, web: PathBuf) -> Router {
    let api = Router::new()
        .route(
            "/settings/access",
            get(crate::admin_access::settings).post(crate::admin_access::update_settings),
        )
        .route("/session/logout", post(crate::admin_access::logout))
        .route("/overview", get(overview))
        .route("/service/stop", post(stop_service))
        .route("/desktop/start", post(start_desktop))
        .route("/desktop/stop", post(stop_desktop))
        .route("/accounts", post(create_account))
        .route("/accounts/{id}/upstream", post(accounts::update_upstream))
        .route("/accounts/metrics", get(operations::account_metrics))
        .route(
            "/accounts/batch",
            post(batch::submit).layer(DefaultBodyLimit::max(4 * 1024 * 1024)),
        )
        .route("/accounts/export", post(batch::export))
        .route("/accounts/{id}/quota", get(quota::read))
        .route("/accounts/{id}/subscription", get(subscription::read))
        .route(
            "/accounts/{id}/subscription/refresh",
            post(subscription::refresh),
        )
        .route("/accounts/{id}/quota/refresh", post(quota::refresh))
        .route("/accounts/{id}/quota/reset", post(quota::consume_reset))
        .route(
            "/accounts/{id}/quota/reset/automation",
            get(quota::read_automation).post(quota::update_automation),
        )
        .route("/local-connection/inspect", post(local_connection::inspect))
        .route("/local-connection/connect", post(local_connection::connect))
        .route("/accounts/{id}", axum::routing::delete(delete_account))
        .route("/accounts/{id}/start", post(start_account))
        .route("/accounts/{id}/activation", post(set_activation))
        .route("/accounts/{id}/stop", post(stop_account))
        .route("/accounts/{id}/restart", post(restart_account))
        .route(
            "/accounts/{id}/login",
            post(login_account).get(login_status),
        )
        .route("/accounts/{id}/login/callback", post(login_callback))
        .route("/accounts/{id}/login/cancel", post(cancel_login))
        .route(
            "/accounts/{id}/credentials",
            post(import_token).delete(logout_credentials),
        )
        .route(
            "/accounts/{id}/credentials/desktop",
            post(link_desktop_credentials),
        )
        .route("/jobs", post(start_job))
        .route("/operations", get(operations))
        .route("/capabilities", get(capabilities))
        .route("/accounts/{id}/features/verify", post(verify_feature))
        .route("/accounts/{id}/features", post(set_features))
        .route("/statistics", get(statistics))
        .route("/api-keys", get(api_keys).post(create_api_key))
        .route("/api-keys/export", get(export_api_keys))
        .route(
            "/api-keys/import",
            post(import_api_keys).layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/api-keys/{id}",
            post(update_api_key).delete(delete_api_key),
        )
        .route("/api-keys/{id}/group", post(set_api_key_group))
        .route("/api-keys/{id}/concurrency", post(set_api_key_concurrency))
        .route("/queue", get(queue))
        .route("/requests", axum::routing::delete(clear_request_history))
        .route("/requests/{id}/cancel", post(cancel_request))
        .route("/requests/{id}/content", get(request_content))
        .route("/settings/concurrency", post(set_concurrency))
        .route("/accounts/{id}/proxy", post(proxy::set_proxy))
        .route("/accounts/{id}/location", post(location::save))
        .route("/accounts/{id}/location/detect", post(location::detect))
        .route("/location/options", get(location::options))
        .route("/settings/queue", post(set_queue))
        .route("/settings/native-affinity", post(set_native_affinity))
        .route("/settings/retention", post(set_retention))
        .route(
            "/settings/debug",
            get(operations::debug_settings).post(operations::set_debug),
        )
        .route("/groups", post(create_group))
        .route("/groups/{id}", post(update_group).delete(delete_group))
        .route("/groups/{id}/check", post(check_group))
        .route("/accounts/{id}/policy", post(account_policy))
        .route("/accounts/{id}/check", post(check_account))
        .route("/accounts/{id}/route", post(set_route))
        .route("/accounts/{id}/connection-test", post(test_connection))
        .route("/accounts/{id}/chat-test", post(chat_test::start))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .layer(DefaultBodyLimit::max(64 * 1024));
    Router::new().nest("/api",api)
        .route("/api/session",get(session))
        .route("/api/session/login",post(crate::admin_access::login).layer(DefaultBodyLimit::max(4096)))
        .route("/healthz",get(|State(state):State<Arc<AdminState>>|async move{Json(json!({"status":"ok","implementation":"rust","inferenceEnabled":state.operations.has_inference_routes()}))}))
        .nest("/v1",crate::client_api::router(state.clone()))
        .fallback_service(ServeDir::new(web).append_index_html_on_directories(true))
        .layer(middleware::from_fn_with_state(state.clone(), crate::network::public_access))
        .layer(SetResponseHeaderLayer::overriding(axum::http::header::CACHE_CONTROL,HeaderValue::from_static("no-store")))
        .layer(SetResponseHeaderLayer::overriding(axum::http::header::X_CONTENT_TYPE_OPTIONS,HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(axum::http::header::REFERRER_POLICY,HeaderValue::from_static("no-referrer")))
        .layer(SetResponseHeaderLayer::overriding(axum::http::header::CONTENT_SECURITY_POLICY,HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")))
        .with_state(state)
}
