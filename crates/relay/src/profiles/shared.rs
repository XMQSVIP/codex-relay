//! Account-scoped app-server hosts with independent, concurrently running threads.
use super::{Profiles, warm};
use crate::{
    backend::Backend,
    error::Error,
    execution::{ExecutionOutcome, PreflightTiming, RequestContext},
    inference::RequestJob,
    operations::Operations,
    responses::{self, Request, reuse::Reuse},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

impl Profiles {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn shared_response(
        &self,
        job: &RequestJob,
        ops: &Operations,
        request: &Request,
        history: &[Value],
        outgoing: tokio::sync::mpsc::Sender<Value>,
        cancel: CancellationToken,
        context: &RequestContext,
    ) -> ExecutionOutcome {
        let mut timing = PreflightTiming::new(context);
        let model = async {
            let installation = crate::installation::read_with_refresh().await?;
            let features = request.required_features(history)?;
            ops.check_capability_generation(job.account, &installation.app_server_sha256, &features)?;
            let oauth = self.is_oauth(job.account).await?;
            let proxy = self.account_proxy(job.account);
            let location = if let Some(settings) = &self.proxy_settings {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(cancelled()),
                    location = settings.resolve_location(job.account, false) => location,
                }
            } else { None };
            let images = request.tool_choice != "none" && request.tools.iter().any(|t| t["type"] == "image_generation");
            // These settings affect process startup. Request instructions, tools,
            // keys and histories instead belong exclusively to individual threads.
            let key = warm::digest(&json!({"installation":installation,"proxy":proxy,
                "images":images,"model":(!crate::native_cache::enabled()).then_some(&job.model),"oauth":oauth,"location":location}))?;
            let bytes = if crate::native_cache::enabled() { responses::reservation_bytes(None,&job.access,request,history,None)? } else {serde_json::to_vec(history)?.len()};
            let home = self.home(job.account);
            let workspace = self.root.join(job.account.to_string()).join("workspace");
            timing.mark("request_setup");
            // Retry preparation only; never replay an admitted native turn.
            for attempt in 0..2 {
                request.native_observation.check()?;
                let backends: Vec<_> = self.runtime_clients.lock().unwrap().get(&job.account)
                    .into_iter().flatten().filter_map(std::sync::Weak::upgrade).collect();
                let mut matches = std::collections::HashMap::new();
                let retained = if crate::native_cache::enabled() {
                    self.continuations.find(job.account,&job.access,request,history,&key)?
                } else { None };
                for backend in backends {
                    let sessions = backend.response_worker.lock().unwrap().as_ref()
                        .map(|state| state.sessions.clone()).unwrap_or_default();
                    matches.insert(backend.pid, responses::reuse::matching_sessions(&job.access, request, history, sessions)?);
                }
                timing.mark("session_match");
                let preparation = tokio::select! {
                    _ = cancel.cancelled() => return Err(cancelled()),
                    gate = self.response_preparation(job.account) => gate,
                };
                timing.mark("account_preparation_wait");
                let offers=if crate::native_cache::enabled() {Some(self.resource_offers(job.account,&key,&job.access,request,history,&matches,retained.as_ref().map(|e|(e.pid,&e.session))).await?)}else{None};
                let (slot, mut worker) = tokio::select! {
                    _ = cancel.cancelled() => return Err(cancelled()),
                    selected = self.shared_response_slot_planned(job.account, ops.account_concurrency(job.account)?, &key, bytes, offers.as_ref(), |backend| {
                        let state = backend.response_worker.lock().unwrap();
                        Ok(retained.as_ref().is_some_and(|e|e.pid==backend.pid) || matches.get(&backend.pid).is_some_and(|matched|
                            state.as_ref().unwrap().sessions.iter().any(|session| matched.contains(session))))
                    }) => selected?,
                };
                timing.mark("slot_selection");
                let reused = if let Some(backend) = worker.as_ref() {
                    let compatible = backend.response_worker.lock().unwrap().as_ref()
                        .is_some_and(|state| if let Some(offer)=offers.as_ref().and_then(|o|o.get(&backend.pid)) {state.accepts_plan(&key,offer.thread.as_deref(),offer.bytes)}else{state.accepts(&key, bytes) || ((retained.as_ref().is_some_and(|e|e.pid==backend.pid)
                            || matches.get(&backend.pid).is_some_and(|matched|state.sessions.iter().any(|session|matched.contains(session)))) && state.accepts_continuation(&key,bytes))});
                    compatible && backend.alive().await
                } else { false };
                if !reused {
                    if let Some(backend) = worker.as_ref() {
                        if let Some(state)=backend.response_worker.lock().unwrap().as_ref() {for token in state.retained.values(){token.cancel();}}
                        backend.stop().await?;
                    }
                    *worker = None;
                    if cancel.is_cancelled() { return Err(cancelled()); }
                    let directory = tempfile::Builder::new().prefix("worker-").tempdir_in(&workspace)?;
                    let backend = Arc::new(Backend::start_response_with_proxy(&installation, directory.path(),
                        if oauth { Some(&home) } else { None }, images, &proxy, &job.model).await?);
                    let mut state = warm::Worker::new(key, directory);
                    state.shared = true;
                    state.managed_contexts = true;
                    state.idle();
                    *backend.response_worker.lock().unwrap() = Some(state);
                    self.track_client(job.account, &backend);
                    *worker = Some(backend.clone());
                    warm::reap_when_idle(&slot, &backend);
                }
                let backend = worker.as_ref().unwrap().clone();
                // All models in this process use the sanitized startup catalog.
                // Never let an unknown model fall back to native host tools.
                if !backend.supports_response_model(&job.model) {
                    warm::retire(&slot, &backend);
                    drop(worker);
                    if attempt == 0 { continue; }
                    return Err(Error::new("NATIVE_CATALOG_CHANGED", "当前原生进程目录不包含请求模型，请刷新模型目录后重试。"));
                }
                backend.rpc.set_location(location.clone());
                let cwd = backend.response_worker.lock().unwrap().as_ref().unwrap().directory.path().to_path_buf();
                // Reserve a session and native-context budget while selection is exclusive.
                // Full session equality protects against snapshots consumed/extended by siblings.
                let mut session = {
                    let mut state = backend.response_worker.lock().unwrap();
                    let state = state.as_mut().unwrap();
                    let matched = state.sessions.iter().position(|session|
                        matches.get(&backend.pid).is_some_and(|matched| matched.contains(session)));
                    let session = matched.map(|index| state.sessions.swap_remove(index));
                    if !crate::native_cache::enabled() {
                        if session.is_none() && !retained.as_ref().is_some_and(|e|e.pid==backend.pid) { state.threads += 1; }
                        state.context_bytes = state.context_bytes.saturating_add(bytes);
                    }
                    session
                };
                let mut reservation=if crate::native_cache::enabled() {
                    let previous=session.as_ref().or_else(||retained.as_ref().filter(|e|e.pid==backend.pid).map(|e|&e.session));
                    let bytes=responses::reservation_bytes(Some(&backend.rpc),&job.access,request,history,previous)?;
                    let identity=if request.reuse_allowed {request.client_session.map(|id|warm::digest(&json!({"identity":id,"key":job.access.key_id,"model":job.model,"account":job.account,"route":job.provider}))).transpose()?}else{None};
                    let token=backend.response_worker.lock().unwrap().as_mut().unwrap().resources.reserve(previous.map(|s|s.thread.as_str()),bytes,identity)?;
                    Some(super::resources::Reservation {tracker:super::resources::Tracker {backend:Arc::downgrade(&backend),token},slot:slot.clone(),finished:false})
                }else{None};
                // Acquire lifecycle protection before releasing the slot. Network/disk
                // preparation then overlaps with siblings without permitting teardown.
                let mut activity = Some(backend.response_activity.clone().read_owned().await);
                self.pin_response(job.account, &backend);
                drop(worker);
                drop(preparation);
                timing.reused_process = reused;
                timing.native_process_id = Some(backend.pid);
                timing.cached_catalog = backend.uses_cached_catalog();
                timing.mark("native_worker");
                let prepared = async {
                    let (mut provider, credential, config) = self.read_route_with_config(job.account, &backend, &cwd,
                        if oauth { Some(&job.provider.model) } else { None }).await?;
                    ops.check_inference_route(job.account, &job.access, &provider, &job.model)?;
                    provider.model.clone_from(&job.model);
                    let default_summary = responses::reuse::default_summary(&config);
                    let config = warm::digest(&config)?;
                    let route = warm::digest(&job.provider)?;
                    let mut state = backend.response_worker.lock().unwrap();
                    let state = state.as_mut().unwrap();
                    if state.retired {
                        return Err(Error::new("NATIVE_WORKER_RETIRED", "原生进程正在回收，重新准备实例。"));
                    }
                    if state.config.is_some_and(|old| old != config) || state.route.is_some_and(|old| old != route) {
                        return Err(Error::new("NATIVE_CONFIG_CHANGED", "原生配置已变化，重新准备实例。"));
                    }
                    state.config = Some(config);
                    state.route = Some(route);
                    Ok::<_, Error>((provider, credential, default_summary))
                };
                let prepared = tokio::select! {
                    _ = cancel.cancelled() => Err(cancelled()),
                    result = prepared => result,
                };
                let (provider, credential, default_summary) = match prepared {
                    Ok(value) => value,
                    Err(error) => {
                        warm::retire(&slot, &backend);
                        drop(activity);
                        if attempt == 0 && matches!(error.code.as_str(), "NATIVE_CONFIG_CHANGED" | "NATIVE_WORKER_RETIRED") { continue; }
                        return Err(error);
                    }
                };
                timing.mark("account_and_route");
                let mut continuation=responses::continuation::Continuation {tracker:reservation.as_ref().map(|r|r.tracker.clone()),..Default::default()};
                let mut _claim=None;
                if let Some(entry)=retained.filter(|e|e.pid==backend.pid) {
                    if let Some((waiting,claim))=self.continuations.claim(&entry)? {
                        session=Some(entry.session.clone());
                        continuation.incoming=Some(waiting);
                        _claim=Some(claim);
                    } else {
                        if crate::native_cache::enabled() {drop(reservation);drop(activity);if attempt==0 {continue;}return Err(cancelled());}
                        // Expired after selection. Reserve a fresh thread only if this
                        // generation still has room; this is preparation, not a replay.
                        let mut state=backend.response_worker.lock().unwrap();
                        let state=state.as_mut().unwrap();
                        if state.threads>=warm::MAX_THREADS { return Err(Error::new("CONCURRENCY_LIMIT","原生会话已过期，执行器正在轮换。")); }
                        state.threads+=1;
                    }
                }
                if crate::native_cache::enabled() {
                    continuation.owner=Some(warm::Activity {backend:backend.clone(),slot:slot.clone(),
                        _guard:activity.take().unwrap()});
                }
                if continuation.incoming.is_some() {
                    ops.check_inference_route(job.account,&job.access,&job.provider,&job.model)?;
                    let route=warm::digest(&job.provider)?;
                    if !backend.response_worker.lock().unwrap().as_ref().is_some_and(|state|
                        !state.retired && state.key==key && state.route==Some(route)) {
                        return Err(Error::new("NATIVE_WORKER_RETIRED","原生续接执行器已变化，请使用完整历史重新请求。"));
                    }
                }
                let quota_identity = credential.as_ref().map(|c| crate::operations::quota::identity(job.account, Some(c)))
                    .or_else(|| provider.auth.starts_with(crate::oauth::AUTH).then(|| ops.quota_identity(job.account, &job.provider)));
                let mut quota_events = backend.rpc.subscribe_quota();
                let mut quota_open = true;
                let result = {
                    let work = responses::run_profiled(&backend.rpc, &provider, &cwd, request, history,
                        outgoing.clone(), cancel.clone(), Some(&mut timing), Some(Reuse { access: &job.access, session: &mut session, default_summary, continuation:if crate::native_cache::enabled(){Some(&mut continuation)}else{None} }));
                    tokio::pin!(work);
                    let mut checks = tokio::time::interval(std::time::Duration::from_secs(1));
                    loop { tokio::select! {
                        result = &mut work => break result,
                        event = quota_events.recv(), if quota_open => match event {
                            Ok(event) if event.claim() => {
                                if let Some(identity) = &quota_identity {
                                    let _ = ops.publish_quota_reading(job.account, &job.provider, identity.clone(),
                                        crate::operations::quota::normalize_reading(&event.message["params"], false));
                                }
                            },
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => quota_open = false,
                            _ => {},
                        },
                        _ = checks.tick() => {
                            if ops.check_inference_route(job.account, &job.access, &job.provider, &job.model).is_err() { cancel.cancel(); }
                        }
                    }}
                };
                // An unconfirmed cleanup poisons this generation, but existing
                // sibling turns finish before its process tree is reclaimed.
                let parked=continuation.waiting.is_some() && result.as_ref().is_ok_and(|s|s.outcome=="completed") && !cancel.is_cancelled();
                let mut clean = result.as_ref().is_ok_and(|s| s.cleanup_confirmed);
                if parked {
                    if let (Some(waiting),Some(session))=(continuation.waiting.take(),session.take()) {
                        self.retain_tools(job.account,job.access.clone(),job.provider.clone(),job.model.clone(),key,session,waiting,&context.native_handoff)?;
                        // An owned waiting turn is a distinct successful handoff, not a completed native turn.
                        clean=true;
                    }
                }
                if cancel.is_cancelled() { session = None; }
                if let Some(reservation)=reservation.as_mut() {
                    let successful=result.as_ref().is_ok_and(|s|s.outcome=="completed") && !cancel.is_cancelled();
                    let keep=backend.response_worker.lock().unwrap().as_mut().unwrap().resources.finish(reservation.tracker.token,successful,parked,session.is_some() || parked);
                    if !keep {session=None;}
                    if !parked && session.is_none() {
                        if let Ok(summary)=&result {
                            clean &= super::resources::close_context(&backend,&slot,&summary.thread_id).await;
                        } else { clean=false; }
                    }
                    reservation.finished=clean;
                } else if session.is_none() && !parked {
                    if let Ok(summary)=&result {let _=backend.rpc.request("thread/unsubscribe",json!({"threadId":summary.thread_id})).await;}
                }
                let (poisoned, full) = {
                    let mut state = backend.response_worker.lock().unwrap();
                    let state = state.as_mut().unwrap();
                    if clean && !cancel.is_cancelled() && let Some(session) = session { state.sessions.push(session); }
                    state.idle();
                    state.retired |= !clean;
                    (state.retired, if crate::native_cache::enabled(){state.resources.retire()}else{state.threads >= warm::MAX_THREADS || state.context_bytes >= warm::MAX_CONTEXT_BYTES})
                };
                if crate::native_cache::enabled() && clean {self.trim_contexts(job.account,&slot,&backend).await?;}
                drop(activity);
                if poisoned { warm::retire(&slot, &backend); }
                else if full { warm::recycle_after_activity(&slot, &backend); }
                if backend.uses_cached_catalog() && !cancel.is_cancelled()
                    && result.as_ref().is_ok_and(|s| s.outcome == "completed") && let Some(credential) = credential {
                    self.catalog_refresh.schedule(job.account, &home, &workspace, credential, proxy.clone()).await;
                }
                return result;
            }
            unreachable!("bounded preparation retry")
        }.await;
        ops.record_native_health(job.account, &model);
        ExecutionOutcome {
            model,
            cleanup_error: None,
        }
    }
}

fn cancelled() -> Error {
    Error::new("REQUEST_CANCELLED", "请求已取消。")
}
