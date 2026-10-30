//! Listener startup, stop requests and coordinated graceful shutdown.

use super::{AdminState, router};
use crate::error::{Error, Result};
use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

pub(super) async fn stop_service(
    State(state): State<Arc<AdminState>>,
) -> (StatusCode, Json<Value>) {
    // Trigger the same cleanup path as Ctrl+C; never kill the process in a handler.
    state.stop_requested.cancel();
    (StatusCode::ACCEPTED, Json(json!({"state":"stopping"})))
}
pub async fn serve(port: u16, web: PathBuf) -> Result<()> {
    serve_with_network(port, web, crate::network::NetworkConfig::default(), false).await
}

pub async fn serve_with_network(
    port: u16,
    web: PathBuf,
    network: crate::network::NetworkConfig,
    require_admin_token: bool,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?;
    let port = listener.local_addr()?.port();
    let endpoints = network.validate(port)?;
    // Validate TLS and bind every socket before starting any request processing.
    let mut clients = Vec::new();
    for config in &network.listeners {
        clients.push(crate::network::BoundListener::bind(config).await?);
    }
    let token = require_admin_token.then(crate::secret);
    let state = AdminState::new_with_network(
        port,
        std::env::current_dir()?.join("data/accounts"),
        token.clone(),
        endpoints.clone(),
    )
    .await?;
    super::activation::start_enabled(&state);
    super::activation::maintain_enabled(&state);
    super::quota::start(&state);
    state.tasks.spawn(
        state
            .operations
            .clone()
            .maintain_request_history(state.shutdown.clone()),
    );
    let mut servers = tokio::task::JoinSet::<Result<()>>::new();
    for client in clients {
        let state = state.clone();
        servers.spawn(async move { client.serve(state.clone(), state.shutdown.clone()).await });
    }
    let admin = state.clone();
    servers.spawn(async move {
        axum::serve(
            listener,
            router(admin.clone(), web)
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(admin.shutdown.clone().cancelled_owned())
        .await?;
        Ok(())
    });
    if let Some(token) = token {
        println!("管理界面：http://127.0.0.1:{port}/#token={token}");
    } else {
        println!("管理界面：http://127.0.0.1:{port}/");
    }
    for endpoint in &endpoints {
        println!(
            "客户端 API：{}（{}，监听 {}）",
            endpoint.base_url, endpoint.transport, endpoint.listen
        );
    }
    println!(
        "后台登录与公网访问可在设置中调整；客户端 API 需调用方 Key。退出时回收本进程管理的实例。"
    );
    let mut failure = None;
    tokio::select! {
        biased;
        _=state.stop_requested.cancelled()=>{},
        result=shutdown_signal()=>{if result.is_err(){failure=Some(Error::new("SHUTDOWN_SIGNAL","无法监听退出信号。"));}},
        result=servers.join_next()=>{failure=Some(match result {
            Some(Ok(Err(error)))=>error,
            _=>Error::new("NETWORK_SERVER_STOPPED","监听服务意外退出，正在关闭其他入口。"),
        });}
    }
    state.shutdown.cancel();
    state.profiles.cancel_inference();
    if let Err(error) = state.operations.start_shutdown() {
        failure.get_or_insert(error);
    }
    while let Some(result) = servers.join_next().await {
        match result {
            Ok(Err(error)) => {
                failure.get_or_insert(error);
            }
            Err(_) => {
                failure.get_or_insert(Error::new("NETWORK_SERVER_STOPPED", "监听服务异常退出。"));
            }
            _ => {}
        }
    }
    state.tasks.close();
    state.tasks.wait().await;
    state.desktop_control.stop().await;
    state.profiles.shutdown().await;
    failure.map_or(Ok(()), Err)
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
