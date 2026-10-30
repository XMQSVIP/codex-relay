//! Responses/native session execution, warm-worker reuse and LAN streaming.

use super::{Profiles, warm};
use crate::{
    backend::Backend,
    error::{Error, Result},
    installation,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

enum NativeInput<'a> {
    Response {
        request: &'a crate::responses::Request,
        history: &'a [Value],
        outgoing: tokio::sync::mpsc::Sender<Value>,
    },
    Session {
        start: Value,
        incoming: &'a mut tokio::sync::mpsc::Receiver<Value>,
        outgoing: tokio::sync::mpsc::Sender<Value>,
    },
}

impl Profiles {
    #[allow(clippy::too_many_arguments)]
    pub async fn direct_response(
        &self,
        job: &crate::inference::RequestJob,
        ops: &crate::operations::Operations,
        request: &crate::responses::Request,
        history: &[Value],
        outgoing: tokio::sync::mpsc::Sender<Value>,
        cancel: CancellationToken,
        context: &crate::execution::RequestContext,
    ) -> crate::execution::ExecutionOutcome {
        if job.provider.execution == crate::provider::Execution::ApiKey {
            return crate::execution::ExecutionOutcome::failed(Error::new(
                "INVALID_INPUT",
                "API Key 账户须通过直连接口执行。",
            ));
        }
        if !request.compact {
            return self
                .shared_response(job, ops, request, history, outgoing, cancel, context)
                .await;
        }
        self.execute_direct(
            job,
            ops,
            NativeInput::Response {
                request,
                history,
                outgoing,
            },
            cancel,
            Some(context),
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn direct_session(
        &self,
        job: &crate::inference::RequestJob,
        ops: &crate::operations::Operations,
        start: Value,
        incoming: &mut tokio::sync::mpsc::Receiver<Value>,
        outgoing: tokio::sync::mpsc::Sender<Value>,
        cancel: CancellationToken,
    ) -> crate::execution::ExecutionOutcome {
        self.execute_direct(
            job,
            ops,
            NativeInput::Session {
                start,
                incoming,
                outgoing,
            },
            cancel,
            None,
        )
        .await
    }
    async fn execute_direct(
        &self,
        job: &crate::inference::RequestJob,
        ops: &crate::operations::Operations,
        input: NativeInput<'_>,
        cancel: CancellationToken,
        context: Option<&crate::execution::RequestContext>,
    ) -> crate::execution::ExecutionOutcome {
        let mut cleanup_error = None;
        let mut timing = context.map(crate::execution::PreflightTiming::new);
        let compact_request = matches!(&input,NativeInput::Response{request,..} if request.compact);
        if compact_request {
            if let NativeInput::Response {
                request,
                history,
                outgoing,
            } = input
            {
                return self
                    .compaction_request(job, ops, request, history, outgoing, cancel)
                    .await;
            }
        }
        let model = async {
            let oauth = self.is_oauth(job.account).await?;
            let (slot, mut worker) = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "请求已取消。")),
                claimed = self.response_slot(job.account, ops.account_concurrency(job.account)?, |backend| {
                let state = backend.response_worker.lock().unwrap();
                match (state.as_ref().and_then(|state| state.session.as_ref()), &input) {
                    (Some(session), NativeInput::Response { request, history, .. }) =>
                        Ok(session.continuation(&job.access, request, history)?.is_some()),
                    _ => Ok(false),
                }
                }) => claimed?,
            };
            if cancel.is_cancelled() { return Err(Error::new("REQUEST_CANCELLED", "请求已取消。")); }
            if let Some(timing) = &mut timing { timing.mark("ingress_queue_and_slot"); }
            let installation = installation::read_with_refresh().await?;
            // Prepared metadata only: full validation stays in setup/diagnostics.
            let features = match &input {
                NativeInput::Response {request, history, ..} => request.required_features(history)?,
                NativeInput::Session {start, ..} => crate::native_session::required_features(start)?,
            };
            ops.check_capability_generation(job.account, &installation.app_server_sha256, &features)?;
            let http = matches!(&input, NativeInput::Response {..});
            let images = matches!(&input, NativeInput::Response {request, ..} if request.tool_choice != "none" && request.tools.iter().any(|t| t["type"] == "image_generation"));
            let proxy = self.account_proxy(job.account);
            let location = if let Some(settings) = &self.proxy_settings {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "请求已取消。")),
                    location = settings.resolve_location(job.account, false) => location,
                }
            } else { None };
            let key = warm::digest(&json!({"installation":installation,"proxy":proxy,
                "images":images,"model":job.model,"oauth":oauth,"location":location}))?;
            let workspace = self.root.join(job.account.to_string()).join("workspace");
            let home = self.home(job.account);
            let mut input = Some(input);
            // Only setup may retry. Never replay an inference after turn/start.
            for attempt in 0..2 {
                let mut reuse = false;
                if let Some(backend) = worker.as_ref() {
                    let matches = {
                        let state = backend.response_worker.lock().unwrap();
                        match (state.as_ref(), input.as_ref()) {
                            (Some(state), Some(NativeInput::Response {request, history, ..})) if state.matches(&key) =>
                                match &state.session {
                                    Some(session) => session.continuation(&job.access, request, history)?.is_some(),
                                    None => false,
                                },
                            _ => false,
                        }
                    };
                    reuse = matches && backend.alive().await;
                    if !reuse { backend.stop().await?; *worker = None; }
                }
                let (backend, mut state) = if reuse {
                    let backend = worker.as_ref().unwrap().clone();
                    let state = backend.response_worker.lock().unwrap().take().unwrap();
                    (backend, state)
                } else {
                    let directory = tempfile::Builder::new().prefix("worker-").tempdir_in(&workspace)?;
                    let backend = Arc::new(Backend::start_response_with_proxy(&installation,
                        directory.path(), if oauth {Some(&home)} else {None}, images, &proxy, &job.model).await?);
                    (backend, warm::Worker::new(key, directory))
                };
                backend.rpc.set_location(location.clone());
                self.track_client(job.account, &backend);
        *worker = Some(backend.clone());
                if let Some(timing) = &mut timing {
                    timing.reused_process = reuse;
                    timing.native_process_id = Some(backend.pid);
                    timing.cached_catalog = backend.uses_cached_catalog();
                    timing.mark("native_worker");
                }
                let cwd = state.directory.path().to_path_buf();
                let mut refresh_credential = None;
                let work = async {
                    let (mut provider, credential, config) = self.read_route_with_config(job.account, &backend, &cwd,
                        if oauth {Some(&job.provider.model)} else {None}).await?;
                    ops.check_inference_route(job.account, &job.access, &provider, &job.model)?;
                    let quota_identity = if credential.is_some() {
                        Some(crate::operations::quota::identity(job.account,credential.as_ref()))
                    } else if provider.auth.starts_with(crate::oauth::AUTH) {
                        Some(ops.quota_identity(job.account,&job.provider))
                    } else {None};
                    let mut quota_events = backend.rpc.subscribe_quota();
                    let mut quota_events_open=true;
                    refresh_credential = credential;
                    provider.model.clone_from(&job.model);
                    if http {
                        // Native config is still read on every request. A changed effective
                        // config cannot reuse a thread or the process's startup configuration.
                        let config = warm::digest(&config)?;
                        if state.config.is_some_and(|old| old != config) {
                            return Err(Error::new("NATIVE_CONFIG_CHANGED", "原生配置已变化，重新准备实例。"));
                        }
                        state.config = Some(config);
                    }
                    if let Some(timing) = &mut timing { timing.mark("account_and_route"); }
                    let work = async { match input.take().unwrap() {
                        NativeInput::Response {request, history, outgoing} => crate::responses::run_profiled(
                            &backend.rpc, &provider, &cwd, request, history, outgoing, cancel.clone(), timing.as_mut(),
                            Some(crate::responses::reuse::Reuse {access:&job.access, session:&mut state.session, default_summary:crate::responses::reuse::default_summary(&config), continuation:None})).await,
                        NativeInput::Session {start, incoming, outgoing} => crate::native_session::run(
                            &backend.rpc, &provider, &cwd, start, incoming, outgoing, cancel.clone()).await,
                    }};
                    tokio::pin!(work);
                    let mut checks = tokio::time::interval(std::time::Duration::from_secs(1));
                    loop { tokio::select! {
                        result = &mut work => break result,
                        event = quota_events.recv(), if quota_events_open => {
                            match event {
                                Ok(event) if event.claim() => {
                                    if let Some(identity)=&quota_identity { let _=ops.publish_quota_reading(job.account,&job.provider,identity.clone(),crate::operations::quota::normalize_reading(&event.message["params"], false)); }
                                },
                                Err(tokio::sync::broadcast::error::RecvError::Closed)=>quota_events_open=false,
                                _=>{},
                            }
                        },
                        _ = checks.tick() => { if ops.check_inference_route(job.account, &job.access, &job.provider, &job.model).is_err() {cancel.cancel();} }
                    }}
                };
                let work = tokio::select! {
                    biased;
                    _ = cancel.cancelled(), if !compact_request => Err(Error::new("REQUEST_CANCELLED", "请求已取消。")),
                    result = work => result,
                };
                let retry_setup = attempt == 0 && work.as_ref().is_err_and(|e| e.code == "NATIVE_CONFIG_CHANGED");
                let keep = http && !compact_request && state.session.is_some() && !cancel.is_cancelled() && work.as_ref().is_ok_and(|s| s.outcome == "completed");
                let start_reaper = keep && state.idle();
                // Backend owns the directory until its process tree has exited, including
                // failure/cancellation on Windows where native files may still be open.
                *backend.response_worker.lock().unwrap() = Some(state);
                if keep {
                    if start_reaper { warm::reap_when_idle(&slot, &backend); }
                } else {
                    cleanup_error = backend.stop().await.err();
                    *worker = None;
                }
                if retry_setup && cleanup_error.is_none() { continue; }
                // Discovery starts only after the turn, never while the caller is
                // waiting for its first model request. It has its own short-lived
                // process and temporary home, with no access to refresh tokens.
                if backend.uses_cached_catalog() && !cancel.is_cancelled()
                    && work.as_ref().is_ok_and(|s| s.outcome == "completed")
                    && let Some(credential) = refresh_credential
                {
                    self.catalog_refresh.schedule(job.account, &home, &workspace, credential, proxy.clone()).await;
                }
                return work;
            }
            unreachable!("setup retries are bounded")
        }.await;
        ops.record_native_health(job.account, &model);
        crate::execution::ExecutionOutcome {
            model,
            cleanup_error,
        }
    }
    pub async fn stream_lan(
        &self,
        job: &crate::inference::RequestJob,
        ops: &crate::operations::Operations,
        sender: tokio::sync::mpsc::Sender<Value>,
        cancel: CancellationToken,
    ) -> Result<crate::streaming::StreamSummary> {
        let id = job.account;
        async {
            if cancel.is_cancelled() {
                return Err(Error::new("STREAM_CANCELLED", "请求在实例启动前已取消。"));
            }
            self.require_source(id, "native-config").await?;
            let (_, mut worker) = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Error::new("STREAM_CANCELLED", "请求已取消。")),
                claimed = self.response_slot(id, ops.account_concurrency(id)?, |_| Ok(false)) => claimed?,
            };
            if cancel.is_cancelled() {
                return Err(Error::new("STREAM_CANCELLED", "请求在实例启动前已取消。"));
            }
            if let Some(old) = worker.as_ref() {
                old.stop().await?;
            }
            *worker = None;
            let installation = installation::read_with_refresh().await?;
            let workspace = self.root.join(id.to_string()).join("workspace");
            let directory = tempfile::Builder::new()
                .prefix("lan-worker-")
                .tempdir_in(&workspace)?;
            *worker = Some(Arc::new(
                Backend::start_with_proxy(
                    &installation,
                    directory.path(),
                    None,
                    false,
                    &self.account_proxy(id),
                )
                .await?,
            ));
            let backend = worker
                .as_ref()
                .ok_or_else(|| Error::new("WORKER_STOPPED", "实例已停止。"))?;
            self.track_client(id, backend);
            let cwd = directory.path().to_path_buf();
            let result = async {
                let mut provider = crate::provider::read(&backend.rpc, &cwd).await?;
                if provider != job.provider {
                    return Err(Error::new(
                        "ROUTE_CHANGED",
                        "原生提供方配置已变化，请重新启用路由。",
                    ));
                }
                ops.check_inference_route(id, &job.access, &provider, &job.model)?;
                provider.model.clone_from(&job.model);
                let guard = crate::guard::Guard::lan(
                    &provider,
                    Arc::new(std::sync::atomic::AtomicU32::new(backend.pid)),
                    crate::installation::read_with_refresh()
                        .await?
                        .app_server_executable,
                )
                .await?;
                let guard = guard.with_client(job.client_payload.as_ref(), sender.clone());
                let result = crate::streaming::guarded_stream(
                    &backend.rpc,
                    &guard,
                    crate::streaming::StreamRequest {
                        provider: &provider,
                        cwd: &cwd,
                        prompt: &job.prompt,
                        timeout: std::time::Duration::from_secs(120),
                    },
                    sender,
                    cancel,
                )
                .await;
                guard.close().await;
                result
            }
            .await;
            let stopped = backend.stop().await;
            *worker = None;
            stopped?;
            result
        }
        .await
    }
}
