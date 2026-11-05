pub mod account_location;
pub mod admin;
mod admin_access;
pub mod app_server_proxy;
pub mod backend;
mod capability_probe;
pub mod client_api;
pub mod client_tools;
mod debug_capture;
pub mod desktop;
mod desktop_process;
pub mod error;
pub mod execution;
pub mod guard;
pub(crate) mod images;
pub mod inference;
pub mod installation;
pub mod media;
mod native_cache;
mod native_catalog;
pub mod native_metadata;
pub mod native_session;
pub mod network;
pub mod oauth;
pub mod operations;
pub mod platform;
pub mod probe;
mod process_tree;
pub mod profiles;
pub mod protocol_tools;
pub mod provider;
pub mod responses;
pub mod rpc;
mod search;
pub mod shared_api;
pub mod streaming;
mod subagent;
pub mod upstream;
#[cfg(test)]
mod wire_capture;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
