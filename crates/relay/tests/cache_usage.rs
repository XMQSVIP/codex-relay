use codex_relay::{
    execution::ExecutionOutcome,
    operations::{ApiKeyCreate, DEFAULT_GROUP, Operations, StatsQuery},
    streaming::{StreamSummary, Usage},
};
use serde_json::{Value, json};

fn summary(input: u64, cached: Option<u64>) -> StreamSummary {
    StreamSummary {
        response_model: None,
        response_service_tier: None,
        thread_id: "thread".into(),
        turn_id: "turn".into(),
        item_ids: vec![],
        outcome: "completed".into(),
        native_status: Some("completed".into()),
        error_code: None,
        error_message: None,
        delta_count: 1,
        text_bytes: 3,
        first_delta_at: None,
        usage: Some(Usage {
            input_tokens: input,
            output_tokens: 4,
            total_tokens: input + 4,
            cached_input_tokens: cached,
            reasoning_output_tokens: None,
            cache_write_input_tokens: None,
        }),
        interrupt_acknowledged: false,
        cleanup_confirmed: true,
        guard: Default::default(),
    }
}
fn stats(ops: &Operations) -> Value {
    ops.statistics(serde_json::from_value::<StatsQuery>(json!({"kind":"inference"})).unwrap())
        .unwrap()
}

#[test]
fn cache_usage_is_weighted_persisted_and_does_not_treat_missing_data_as_zero() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    let key = ops
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"cache test","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap();
    let unknown = stats(&ops);
    assert!(unknown["summary"]["cachedInputTokens"].is_null());
    assert!(unknown["summary"]["cacheHitRate"].is_null());
    // Cover both legacy inference settlement and HTTP/native execution settlement.
    for (index, input, cached) in [(0, 100, Some(90)), (1, 900, Some(10)), (2, 500, None)] {
        let lease = ops
            .begin(None, None, "inference", "responses", true)
            .unwrap();
        lease.bind_client(key.key.id).unwrap();
        let summary = summary(input, cached);
        if index == 0 {
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
    let value = stats(&ops);
    let s = &value["summary"];
    assert_eq!(s["inputTokens"], 1500);
    assert_eq!(s["cachedInputTokens"], 100);
    assert_eq!(s["cacheKnownInputTokens"], 1000);
    assert_eq!(s["cacheKnownRequests"], 2);
    assert_eq!(s["cacheHitRate"], 0.1);
    assert_eq!(s["usageKnownRequests"], 3);
    assert_eq!(
        value["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["cachedInputTokens"].is_number())
            .count(),
        2
    );
    let access = ops.authenticate_api_key(&key.secret).unwrap();
    assert_eq!(
        ops.client_usage(&access).unwrap()["summary"]["cacheHitRate"],
        0.1
    );
    let other = ops
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(json!({"name":"other","groupId":DEFAULT_GROUP}))
                .unwrap(),
        )
        .unwrap();
    let access = ops.authenticate_api_key(&other.secret).unwrap();
    assert!(ops.client_usage(&access).unwrap()["summary"]["cachedInputTokens"].is_null());
    let filtered = ops
        .statistics(serde_json::from_value::<StatsQuery>(json!({"keyId":other.key.id})).unwrap())
        .unwrap();
    assert!(filtered["summary"]["cacheHitRate"].is_null());
    drop(ops);
    let ops = Operations::open(temp.path()).unwrap();
    assert_eq!(stats(&ops)["summary"]["cachedInputTokens"], 100);
    assert_eq!(stats(&ops)["summary"]["cacheHitRate"], 0.1);
    ops.clear_request_history().unwrap();
    assert!(stats(&ops)["summary"]["cachedInputTokens"].is_null());
    assert_eq!(stats(&ops)["summary"]["cacheKnownRequests"], 0);
}

#[test]
fn a_reported_zero_is_a_real_cache_miss_and_zero_input_has_no_hit_rate() {
    let temp = tempfile::tempdir().unwrap();
    let ops = Operations::open(temp.path()).unwrap();
    ops.begin(None, None, "inference", "responses", true)
        .unwrap()
        .finish_inference(&Ok(summary(0, Some(0))))
        .unwrap();
    let value = stats(&ops);
    assert_eq!(value["summary"]["cachedInputTokens"], 0);
    assert!(value["summary"]["cacheHitRate"].is_null());
    ops.begin(None, None, "inference", "responses", true)
        .unwrap()
        .finish_execution(
            &ExecutionOutcome {
                model: Ok(summary(40, Some(0))),
                cleanup_error: None,
            },
            None,
        )
        .unwrap();
    let value = stats(&ops);
    assert_eq!(value["summary"]["cachedInputTokens"], 0);
    assert_eq!(value["summary"]["cacheHitRate"], 0.0);
}
