//! Shared operations state and public API.
//!
//! The submodules separate persistence, policy, admission and request lifecycle.
//! All transitions still use the same Inner mutex so a policy update, queue
//! cancellation or lease settlement cannot race admission. See docs/ARCHITECTURE.md.

use crate::error::Error;
use rusqlite::Connection;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};
use tokio::sync::Notify;
use uuid::Uuid;

mod account_metrics;
mod activation;
mod admission;
mod api_keys;
mod batch;
mod debug_content;
mod history;
pub(crate) mod images;
mod inference;
mod lease;
mod proxy;
mod queue;
pub mod quota;
pub mod quota_reset;
mod rate_limit;
pub(crate) mod request_content;
mod response_state;
mod retention;
pub mod runtime;
mod scheduler;
mod schema;
mod settings;
mod storage;
pub(crate) mod subscription;
mod telemetry;

pub use account_metrics::AccountMetricsQuery;
pub use admission::RoutingRequest;
pub use api_keys::{
    ApiKeyBackup, ApiKeyConcurrency, ApiKeyCreate, ApiKeyCreated, ApiKeyGroup, ApiKeyImport,
    ApiKeyImportResult, ApiKeyUpdate, ApiKeyView, ClientAccess,
};
pub use history::StatsQuery;
pub use queue::QueueQuery;
pub use runtime::{Feature, NativeCapabilities};
pub use scheduler::RequestClass;
pub use settings::{
    AccountInput, AccountPolicy, DEFAULT_ACCOUNT_CONCURRENCY, DEFAULT_GROUP, DESKTOP_ACCOUNT,
    Group, GroupInput, Settings,
};

use settings::valid_name;
use storage::storage;

struct Active {
    account: Option<Uuid>,
    group: Option<Uuid>,
    limited: bool,
    key_id: Option<Uuid>,
    exclusive: bool,
}
struct Inner {
    debug_epoch: u64,
    debug_enabled: bool,
    runtime: runtime::Runtime,
    proxy_changes: HashMap<Uuid, proxy::Change>,
    db: Connection,
    settings: Settings,
    active: HashMap<Uuid, Active>,
    cursor: HashMap<Uuid, Uuid>,
    storage_healthy: bool,
    pending: VecDeque<Pending>,
    changed: Arc<Notify>,
    draining: bool,
}
#[derive(Clone)]
struct Pending {
    routing: Option<RoutingRequest>,
    id: Uuid,
    account: Option<Uuid>,
    group: Option<Uuid>,
    client: Option<ClientAccess>,
    deadline: tokio::time::Instant,
    started: Instant,
    cancellation: Arc<OnceLock<Error>>,
}
struct Selection {
    account: Option<AccountPolicy>,
    group: Option<Group>,
    rejection: Option<Error>,
    scheduling: Option<scheduler::Decision>,
}
enum Entry {
    Ready(Lease),
    Waiting(Waiter),
}
struct Waiter {
    owner: Arc<Operations>,
    id: Uuid,
    started: Instant,
    deadline: tokio::time::Instant,
    finished: bool,
    cancellation: Arc<OnceLock<Error>>,
}
pub struct Operations {
    pub(crate) locations: Arc<crate::account_location::Manager>,
    inner: Mutex<Inner>,
    history_db: Mutex<Connection>,
    history_queries: Arc<tokio::sync::Semaphore>,
    changed: Arc<Notify>,
    _lock: storage::DirectoryLock,
}
pub struct Lease {
    pub provider: Option<crate::provider::Provider>,
    owner: Arc<Operations>,
    id: Uuid,
    started: Instant,
    pub account_id: Option<Uuid>,
    finished: bool,
}
