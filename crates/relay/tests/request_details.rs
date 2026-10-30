use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use codex_relay::admin::{AdminState, router};
use codex_relay::{
    execution::ExecutionOutcome,
    operations::{ApiKeyCreate, DEFAULT_GROUP, Operations, StatsQuery},
    streaming::StreamSummary,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn stats(ops: &Operations) -> Value {
    ops.statistics(serde_json::from_value::<StatsQuery>(json!({})).unwrap())
        .unwrap()
}

#[tokio::test]
async fn desktop_history_effort_is_recorded_before_native_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let key = state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"effort caller","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    for (context, expected) in [(json!("all_turns"), "high"), (Value::Null, "low")] {
        let body = json!({"model":"test-model","reasoning":{"effort":"low","context":context},"input":[
            {"role":"user","content":"synthetic prompt"},
            {"type":"configuration_update","reasoning":{"effort":"medium"}},
            {"type":"configuration_update","reasoning":{"effort":"high"}}
        ]});
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {}", key.secret))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let id = response.headers()["x-request-id"].to_str().unwrap();
        let data = stats(&state.operations);
        let row = data["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap();
        assert_eq!(row["reasoningEffort"], expected);
    }
}

#[tokio::test]
async fn request_service_tier_snapshots_forced_and_client_fast_mode() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    for (forced, requested, expected) in [
        (true, Some("default"), Some("priority")),
        (true, None, Some("priority")),
        (false, Some("priority"), Some("priority")),
        (false, Some("default"), Some("default")),
        (false, None, None),
    ] {
        let key = state
            .operations
            .create_api_key(
                serde_json::from_value::<ApiKeyCreate>(
                    json!({"name":"tier caller","groupId":DEFAULT_GROUP,"fastMode":forced}),
                )
                .unwrap(),
            )
            .unwrap();
        let mut body = json!({"model":"test-model","input":"hello"});
        if let Some(tier) = requested {
            body["service_tier"] = json!(tier);
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("host", "127.0.0.1:4317")
                    .header("authorization", format!("Bearer {}", key.secret))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let id = response.headers()["x-request-id"].to_str().unwrap();
        // Later key edits must not rewrite the historical request mode.
        state
            .operations
            .update_api_key(
                key.key.id,
                serde_json::from_value(json!({
                    "revision":key.key.revision,"fastMode":!forced
                }))
                .unwrap(),
            )
            .unwrap();
        let data = stats(&state.operations);
        let record = data["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap();
        assert_eq!(record["serviceTier"], json!(expected));
        assert!(record["responseServiceTier"].is_null());
    }
}

#[tokio::test]
async fn subagent_sources_are_recorded_before_rejection_without_guessing_from_ua() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let key = state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"agent caller","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let cases = [
        (
            Some(("x-openai-subagent", "review")),
            json!({}),
            Some("review"),
        ),
        (
            Some(("x-codex-turn-metadata", r#"{"subagent_kind":"worker"}"#)),
            json!({}),
            Some("worker"),
        ),
        (
            None,
            json!({"client_metadata":{"x-openai-subagent":"guardian"}}),
            Some("guardian"),
        ),
        (
            None,
            json!({"client_metadata":{"x-codex-turn-metadata":r#"{"subagent_kind":"thread_spawn"}"#}}),
            Some("thread_spawn"),
        ),
        (
            Some(("x-openai-subagent", "review")),
            json!({"turn_metadata":r#"{"subagent_kind":"worker"}"#}),
            Some("worker"),
        ),
        (
            None,
            json!({"client_metadata":{"x-codex-turn-metadata":"not json"}}),
            None,
        ),
        (None, json!({}), None),
    ];
    for (header, mut body, expected) in cases {
        body["model"] = json!("test-model");
        body["input"] = json!("subagent_kind: worker (plain user text is not a signal)");
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("host", "127.0.0.1:4317")
            .header("authorization", format!("Bearer {}", key.secret))
            .header("content-type", "application/json")
            .header("user-agent", "subagent/worker");
        if let Some((name, value)) = header {
            request = request.header(name, value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let id = response.headers()["x-request-id"].to_str().unwrap();
        let data = stats(&state.operations);
        let record = data["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap();
        assert_eq!(record["subagentKind"], json!(expected));
    }
}

#[test]
fn request_latency_includes_queue_time_and_keeps_unavailable_values_null() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let db = Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    let start = codex_relay::now_ms() as i64 - 6000;
    for (id, status, finished, duration, wait, first) in [
        (
            "completed",
            "succeeded",
            Some(start + 5000),
            Some(4000),
            Some(1000),
            Some(500),
        ),
        ("queued", "queued", None, None, None, None),
        ("running", "running", None, None, Some(0), None),
        (
            "failed",
            "failed",
            Some(start + 200),
            Some(200),
            Some(0),
            None,
        ),
        (
            "cancelled",
            "cancelled",
            Some(start + 900),
            None,
            Some(900),
            None,
        ),
        ("zero", "succeeded", Some(start), Some(0), Some(0), Some(0)),
        (
            "clock-adjustment",
            "succeeded",
            Some(start - 1),
            Some(0),
            Some(0),
            None,
        ),
    ] {
        db.execute("INSERT INTO requests(id,started_at,finished_at,kind,action,status,duration_ms,wait_ms,first_delta_ms,model) VALUES(?1,?2,?3,'inference','responses',?4,?5,?6,?7,'test-model')", params![id,start,finished,status,duration,wait,first]).unwrap();
    }
    let data = stats(&ops);
    let records = data["records"].as_array().unwrap();
    let get = |id| records.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(get("completed")["totalMs"], 5000);
    assert_eq!(get("completed")["firstResponseMs"], 1500);
    assert_eq!(get("completed")["durationMs"], 4000);
    assert_eq!(get("completed")["firstDeltaMs"], 500);
    assert_eq!(get("completed")["model"], "test-model");
    assert!(get("completed")["responseModel"].is_null());
    assert!(get("completed")["streaming"].is_null());
    for id in ["queued", "running"] {
        assert!(get(id)["totalMs"].is_null());
        assert!(get(id)["firstResponseMs"].is_null());
    }
    assert_eq!(get("failed")["totalMs"], 200);
    assert!(get("failed")["firstResponseMs"].is_null());
    assert_eq!(get("cancelled")["totalMs"], 900);
    assert!(get("cancelled")["firstResponseMs"].is_null());
    assert_eq!(get("zero")["totalMs"], 0);
    assert_eq!(get("zero")["firstResponseMs"], 0);
    assert_eq!(get("clock-adjustment")["totalMs"], 0);
    assert_eq!(data["summary"]["total"], 7);
}

#[test]
fn both_model_settlement_paths_preserve_the_first_text_timestamp() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let db = Connection::open(temp.path().join("operations.sqlite3")).unwrap();
    for legacy in [false, true] {
        let lease = ops
            .begin(None, None, "inference", "responses", true)
            .unwrap();
        let start = codex_relay::now_ms() as i64 - 1000;
        db.execute(
            "UPDATE requests SET started_at=?1,wait_ms=250,model='requested-model' WHERE status='running'",
            [start],
        )
        .unwrap();
        let summary = StreamSummary {
            response_model: Some("actual-model".into()),
            response_service_tier: Some("default".into()),
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            item_ids: vec![],
            outcome: "completed".into(),
            native_status: Some("completed".into()),
            error_code: None,
            error_message: None,
            delta_count: 1,
            text_bytes: 1,
            first_delta_at: Some((start + 750) as u64),
            usage: None,
            interrupt_acknowledged: false,
            cleanup_confirmed: true,
            guard: Default::default(),
        };
        if legacy {
            lease.finish_inference(&Ok(summary)).unwrap();
        } else {
            lease
                .finish_execution(
                    &ExecutionOutcome {
                        model: Ok(summary),
                        cleanup_error: None,
                    },
                    None,
                )
                .unwrap();
        }
    }
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    for record in stats(&ops)["records"].as_array().unwrap() {
        assert_eq!(record["model"], "requested-model");
        assert_eq!(record["responseModel"], "actual-model");
        assert_eq!(record["responseServiceTier"], "default");
        assert_eq!(record["firstDeltaMs"], 500);
        assert_eq!(record["firstResponseMs"], 750);
        assert!(record["totalMs"].as_i64().unwrap() >= 1000);
    }
}

#[tokio::test]
async fn request_key_names_survive_key_rename_deletion_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let account = Uuid::new_v4();
    ops.register_accounts(&[(account, "Account A".into())])
        .unwrap();
    let key = ops
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"Original key","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap();
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    let lease = ops.acquire_client(account, &access).await.unwrap();
    let renamed = ops
        .update_api_key(
            key.key.id,
            serde_json::from_value(json!({"revision":key.key.revision,"name":"Renamed key"}))
                .unwrap(),
        )
        .unwrap();
    lease.complete(Ok(())).unwrap();
    let lease = ops
        .begin(None, None, "inference", "responses", true)
        .unwrap();
    lease.bind_client(key.key.id).unwrap();
    lease.complete(Ok(())).unwrap();
    ops.delete_api_key(key.key.id, renamed.revision).unwrap();
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    let data = stats(&ops);
    let names = data["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["keyName"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Original key"));
    assert!(names.contains(&"Renamed key"));
    assert!(!data.to_string().contains(&key.secret));
}

#[tokio::test]
async fn http_requests_record_reasoning_effort_and_bounded_user_agent_before_routing_rejection() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let key = state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"HTTP caller","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    for (agent, effort, stream) in [
        (
            Some("codex_cli_rs/1.0".into()),
            Some("high"),
            Some(json!(true)),
        ),
        (None, None, None),
        (Some("x".repeat(700)), Some("none"), Some(json!(false))),
        (Some(String::new()), Some("minimal"), Some(Value::Null)),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("host", "127.0.0.1:4317")
            .header("authorization", format!("Bearer {}", key.secret))
            .header("content-type", "application/json");
        if let Some(agent) = &agent {
            request = request.header("user-agent", agent);
        }
        let mut body = json!({"model":"test-model","input":"Message body must never enter statistics","reasoning":{"effort":effort}});
        if let Some(stream) = &stream {
            body["stream"] = stream.clone();
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let id = response.headers()["x-request-id"].to_str().unwrap();
        let data = stats(&state.operations);
        let record = data["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap();
        assert_eq!(record["model"], "test-model");
        assert_eq!(record["reasoningEffort"], effort.unwrap_or("default"));
        assert_eq!(
            record["streaming"],
            stream.and_then(|s| s.as_bool()).unwrap_or(false)
        );
        assert_eq!(record["keyName"], "HTTP caller");
        match agent {
            Some(agent) if agent.len() > 512 => {
                assert_eq!(record["userAgent"], format!("{}…", "x".repeat(512)))
            }
            Some(agent) if !agent.is_empty() => assert_eq!(record["userAgent"], agent),
            _ => assert!(record["userAgent"].is_null()),
        }
        assert!(
            !data
                .to_string()
                .contains("Message body must never enter statistics")
        );
        assert!(!data.to_string().contains(&key.secret));
    }
    assert_eq!(stats(&state.operations)["summary"]["total"], 4);
}
