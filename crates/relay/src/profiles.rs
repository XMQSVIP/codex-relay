//! Account state shared by metadata, authentication and worker modules.
//!
//! Worker slots and lock ordering remain shared across lifecycle and execution
//! paths. Public profile types and methods keep their original import paths.

use crate::backend::Backend;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

mod chat_test;
mod compaction;
pub(crate) mod continuations;
mod direct;
mod images;
mod inspection;
mod login;
mod native_tools;
mod oauth;
mod pool;
mod quota;
pub(crate) mod resources;
mod search;
mod shared;
mod standalone;
mod standby;
mod store;
mod subscription;
mod upstream;
pub(crate) mod warm;
mod workers;
pub(crate) use inspection::RouteCheck;

fn isolated_source() -> String {
    "isolated".into()
}
fn is_isolated(source: &String) -> bool {
    source == "isolated"
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: uuid::Uuid,
    pub name: String,
    pub created_at: u64,
    #[serde(default = "isolated_source", skip_serializing_if = "is_isolated")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_request_id: Option<uuid::Uuid>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    #[serde(flatten)]
    pub profile: Profile,
    pub state: String,
    pub pid: Option<u32>,
    pub account: Option<Value>,
    pub credential_source: Option<String>,
    pub plan_type: Option<String>,
    pub subscription: Option<Value>,
    pub clients: Vec<RuntimeClient>,
    pub upstream: Option<Value>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeClient {
    pub pid: u32,
    pub client_info: crate::installation::NativeClientInfo,
    pub user_agent: Option<String>,
}
type Slot = Arc<Mutex<Option<Arc<Backend>>>>;
pub struct Profiles {
    proxy_settings: Option<Arc<crate::operations::Operations>>,
    root: PathBuf,
    records: Mutex<Vec<Profile>>,
    slots: Mutex<HashMap<uuid::Uuid, Slot>>,
    response_slots: Mutex<HashMap<uuid::Uuid, Vec<Slot>>>,
    response_preparations: Mutex<HashMap<uuid::Uuid, Arc<Mutex<()>>>>,
    native_tool_gates: std::sync::Mutex<HashMap<[u8; 32], std::sync::Weak<Mutex<()>>>>,
    auth_binding: Mutex<()>,
    standby_start_limit: tokio::sync::Semaphore,
    deletion: Mutex<()>,
    inference: Arc<std::sync::Mutex<InferenceRegistry>>,
    continuations: Arc<continuations::Store>,
    catalog_refresh: crate::native_catalog::Refreshes,
    runtime_clients: std::sync::Mutex<HashMap<uuid::Uuid, Vec<std::sync::Weak<Backend>>>>,
    logins: login::Logins,
}

#[derive(Default)]
struct InferenceRegistry {
    active: HashMap<uuid::Uuid, HashMap<uuid::Uuid, CancellationToken>>,
    retained: HashMap<uuid::Uuid, HashMap<uuid::Uuid, CancellationToken>>,
    stopping: HashSet<uuid::Uuid>,
}

pub(crate) struct InferenceRegistration {
    owner: Arc<std::sync::Mutex<InferenceRegistry>>,
    account: uuid::Uuid,
    id: uuid::Uuid,
    retained: bool,
}
impl Drop for InferenceRegistration {
    fn drop(&mut self) {
        let mut registry = self.owner.lock().unwrap();
        let requests_by_account = if self.retained {
            &mut registry.retained
        } else {
            &mut registry.active
        };
        if let Some(requests) = requests_by_account.get_mut(&self.account) {
            requests.remove(&self.id);
            if requests.is_empty() {
                requests_by_account.remove(&self.account);
            }
        }
    }
}

impl Profiles {
    pub(crate) fn inspection_directory(&self) -> crate::error::Result<tempfile::TempDir> {
        Ok(tempfile::Builder::new()
            .prefix("local-inspection-")
            .tempdir_in(&self.root)?)
    }
    pub(crate) fn use_proxy_settings(&mut self, operations: Arc<crate::operations::Operations>) {
        self.proxy_settings = Some(operations);
    }
    fn account_proxy(&self, id: uuid::Uuid) -> crate::app_server_proxy::ProxyConfig {
        self.proxy_settings
            .as_ref()
            .map(|ops| ops.account_proxy(id))
            .unwrap_or_default()
    }
}
