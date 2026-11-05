use codex_relay::operations::Operations;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const ACCOUNT_A: &str = "10000000-0000-0000-0000-000000000001";
const ACCOUNT_B: &str = "10000000-0000-0000-0000-000000000002";
const KEY_A: &str = "30000000-0000-0000-0000-000000000001";
const KEY_B: &str = "30000000-0000-0000-0000-000000000002";
const GROUP: &str = "20000000-0000-0000-0000-000000000001";

fn insert(db: &Connection, overrides: Value) {
    let mut row = json!({
        "id": uuid::Uuid::new_v4().to_string(), "startedAt": 1000,
        "accountId": ACCOUNT_A, "accountName": "测试账户", "keyId": KEY_A,
        "keyName": "测试密钥", "groupId": GROUP, "kind": "inference",
        "status": "succeeded", "model": "test-model", "errorCode": null,
        "inputTokens": 10, "outputTokens": 2, "cachedInputTokens": 5,
    });
    for (key, value) in overrides.as_object().unwrap() {
        row[key] = value.clone();
    }
    db.execute(
        "INSERT INTO requests(id,started_at,account_id,account_name,key_id,key_name,
            group_id,kind,action,status,model,error_code,input_tokens,output_tokens,cached_input_tokens)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'responses',?9,?10,?11,?12,?13,?14)",
        params![
            row["id"].as_str(), row["startedAt"].as_i64(), row["accountId"].as_str(),
            row["accountName"].as_str(), row["keyId"].as_str(), row["keyName"].as_str(),
            row["groupId"].as_str(), row["kind"].as_str(), row["status"].as_str(),
            row["model"].as_str(), row["errorCode"].as_str(), row["inputTokens"].as_i64(),
            row["outputTokens"].as_i64(), row["cachedInputTokens"].as_i64(),
        ],
    ).unwrap();
}

fn statistics(ops: &Operations, overrides: Value) -> Value {
    let mut query = json!({"from": 1000, "to": 1999});
    for (key, value) in overrides.as_object().unwrap() {
        query[key] = value.clone();
    }
    ops.statistics(serde_json::from_value(query).unwrap())
        .unwrap()
}

#[test]
fn sparse_name_filters_keep_literal_matching_and_consistent_aggregates() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    let literal = "%' OR 1=1 --";
    insert(
        &db,
        json!({"id":"match","accountName":literal,"keyName":literal}),
    );
    insert(
        &db,
        json!({"id":"other","accountName":"other","keyName":"other"}),
    );
    db.execute("UPDATE requests SET group_name=account_name", [])
        .unwrap();
    for query in [
        json!({"keyName":literal}),
        json!({"accountName":literal}),
        json!({"groupName":literal}),
        json!({"groupId":GROUP,"keyName":literal,"groupName":literal}),
        json!({"accountId":ACCOUNT_A,"keyId":KEY_A,"model":"test-model","accountName":literal}),
    ] {
        let result = statistics(&ops, query);
        assert_eq!(result["summary"]["total"], 1);
        assert_eq!(result["summary"]["totalTokens"], 12);
        assert_eq!(result["records"][0]["id"], "match");
        for ranking in ["accountUsage", "keyUsage", "modelUsage"] {
            assert_eq!(result[ranking][0]["requestCount"], 1);
        }
        assert_eq!(result["trend"][0]["count"], 1);
    }
    // An empty substring matches every non-NULL name, preserving instr semantics.
    insert(&db, json!({"accountName":null,"keyName":null}));
    assert_eq!(
        statistics(&ops, json!({"accountName":""}))["summary"]["total"],
        2
    );
}

#[test]
fn rankings_cover_all_matching_records_and_sort_by_tokens_not_request_count() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    for _ in 0..60 {
        insert(&db, json!({}));
    }
    insert(
        &db,
        json!({"accountId":ACCOUNT_B,"keyId":KEY_B,"inputTokens":1000,"outputTokens":100}),
    );
    // Unknown usage still counts as a request, including an admission rejection
    // before an account could be assigned.
    insert(
        &db,
        json!({"accountId":null,"keyId":KEY_B,"status":"rejected","inputTokens":null,"outputTokens":null}),
    );
    let first = statistics(&ops, json!({"pageSize":50}));
    let second = statistics(&ops, json!({"page":2,"pageSize":50}));
    assert_eq!(first["records"].as_array().unwrap().len(), 50);
    assert_eq!(second["records"].as_array().unwrap().len(), 12);
    for field in ["accountUsage", "keyUsage"] {
        assert_eq!(first[field], second[field]);
        assert_eq!(first[field][0]["totalTokens"], 1100);
        assert_eq!(first[field][0]["usageKnownRequests"], 1);
        assert_eq!(first[field][1]["requestCount"], 60);
        assert_eq!(first[field][1]["inputTokens"], 600);
        assert_eq!(first[field][1]["outputTokens"], 120);
        // Cached input is already part of input, so it must not be added again.
        assert_eq!(first[field][1]["totalTokens"], 720);
    }
    assert_eq!(first["accountUsage"][0]["id"], ACCOUNT_B);
    assert_eq!(first["accountUsage"][0]["requestCount"], 1);
    assert_eq!(first["keyUsage"][0]["id"], KEY_B);
    assert_eq!(first["keyUsage"][0]["requestCount"], 2);
}

#[test]
fn account_metrics_include_all_inference_rows_and_retained_last_use() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    for _ in 0..61 {
        insert(&db, json!({}));
    }
    insert(&db, json!({"kind":"diagnostic","inputTokens":10000}));
    insert(
        &db,
        json!({"status":"cancelled","inputTokens":null,"outputTokens":null}),
    );
    insert(
        &db,
        json!({"status":"failed","model":"other","inputTokens":null,"outputTokens":null}),
    );
    insert(&db, json!({"accountId":ACCOUNT_B,"startedAt":999}));
    let result = ops
        .account_metrics(serde_json::from_value(json!({"from":1000,"to":1999})).unwrap())
        .unwrap();
    assert_eq!(result["accounts"][0]["requestCount"], 63);
    assert_eq!(result["accounts"][0]["totalTokens"], 732);
    assert_eq!(result["accounts"][0]["successRate"], json!(61.0 / 62.0));
    assert_eq!(result["accounts"][1]["lastUsedAt"], 999);
    assert_eq!(result["accounts"][1]["requestCount"], 0);
    let result = ops
        .account_metrics(
            serde_json::from_value(json!({"from":1000,"to":1000,"accountId":ACCOUNT_A})).unwrap(),
        )
        .unwrap();
    assert_eq!(result["models"].as_array().unwrap().len(), 2);
    assert_eq!(result["models"][0]["requestCount"], 62);
    assert!(result["models"][1]["totalTokens"].is_null());
    assert!(
        ops.account_metrics(serde_json::from_value(json!({"from":2,"to":1})).unwrap())
            .is_err()
    );
}

#[test]
fn rankings_distinguish_unknown_from_zero_and_preserve_identity_and_latest_names() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    insert(
        &db,
        json!({"accountName":"旧名称","keyName":"旧名称","inputTokens":0,"outputTokens":0}),
    );
    insert(
        &db,
        json!({"startedAt":1001,"accountName":"新名称","keyName":"新名称","inputTokens":null,"outputTokens":null}),
    );
    insert(
        &db,
        json!({"startedAt":1002,"accountName":null,"keyName":null,"inputTokens":null,"outputTokens":null}),
    );
    insert(
        &db,
        json!({"accountId":ACCOUNT_B,"accountName":"新名称","keyId":KEY_B,"keyName":"新名称","inputTokens":null,"outputTokens":null}),
    );
    // The historical IDs are absent from current settings and api_keys.
    let result = statistics(&ops, json!({}));
    for field in ["accountUsage", "keyUsage"] {
        let rows = result[field].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "新名称");
        assert_eq!(rows[0]["requestCount"], 3);
        assert_eq!(rows[0]["usageKnownRequests"], 1);
        assert_eq!(rows[0]["totalTokens"], 0);
        assert_eq!(rows[1]["name"], "新名称");
        assert!(rows[1]["inputTokens"].is_null());
        assert!(rows[1]["outputTokens"].is_null());
        assert!(rows[1]["totalTokens"].is_null());
        assert_eq!(rows[1]["requestCount"], 1);
        assert_eq!(rows[1]["usageKnownRequests"], 0);
    }
}

#[test]
fn rankings_apply_every_statistics_filter_and_date_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    insert(&db, json!({}));
    let changes = [
        json!({"startedAt":999}),
        json!({"startedAt":2000}),
        json!({"accountId":ACCOUNT_B}),
        json!({"keyId":KEY_B}),
        json!({"groupId":"20000000-0000-0000-0000-000000000002"}),
        json!({"kind":"diagnostic"}),
        json!({"status":"failed"}),
        json!({"model":"other-model"}),
        json!({"errorCode":"OTHER"}),
    ];
    for change in changes {
        insert(&db, change);
    }
    insert(&db, json!({"startedAt":1999,"errorCode":"TARGET"}));
    let result = statistics(
        &ops,
        json!({
            "groupId":GROUP,"accountId":ACCOUNT_A,"keyId":KEY_A,"kind":"inference",
            "status":"succeeded","model":"test-model","errorCode":"TARGET",
        }),
    );
    for field in ["accountUsage", "keyUsage"] {
        assert_eq!(result[field].as_array().unwrap().len(), 1);
        assert_eq!(result[field][0]["requestCount"], 1);
        assert_eq!(result[field][0]["totalTokens"], 12);
    }
    // Independently check each filter so a restrictive errorCode cannot mask bugs.
    for (field, value) in [
        ("accountId", ACCOUNT_B),
        ("keyId", KEY_B),
        ("groupId", "20000000-0000-0000-0000-000000000002"),
        ("kind", "diagnostic"),
        ("status", "failed"),
        ("model", "other-model"),
        ("errorCode", "OTHER"),
    ] {
        let mut query = json!({});
        query[field] = value.into();
        let filtered = statistics(&ops, query);
        for ranking in ["accountUsage", "keyUsage"] {
            assert_eq!(filtered[ranking].as_array().unwrap().len(), 1, "{field}");
            assert_eq!(filtered[ranking][0]["requestCount"], 1, "{field}");
        }
    }
    let all = statistics(&ops, json!({}));
    assert_eq!(all["summary"]["total"], 9);
}

#[test]
fn rankings_are_empty_without_attributed_requests_and_after_history_clear() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    insert(
        &db,
        json!({"accountId":null,"keyId":null,"inputTokens":null,"outputTokens":null}),
    );
    let result = statistics(&ops, json!({}));
    assert_eq!(result["accountUsage"], json!([]));
    assert_eq!(result["keyUsage"], json!([]));
    insert(&db, json!({}));
    assert_eq!(
        statistics(&ops, json!({}))["keyUsage"][0]["requestCount"],
        1
    );
    assert_eq!(ops.clear_request_history().unwrap(), 2);
    let result = statistics(&ops, json!({}));
    assert_eq!(result["accountUsage"], json!([]));
    assert_eq!(result["keyUsage"], json!([]));
}

#[test]
fn model_usage_and_token_trend_cover_the_filtered_snapshot_with_unknown_and_zero() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    // Enough rows to prove the trend and models are independent of pagination.
    for _ in 0..25 {
        insert(&db, json!({}));
    }
    insert(
        &db,
        json!({"startedAt":1999,"model":"large-model","inputTokens":1000,"outputTokens":100}),
    );
    insert(
        &db,
        json!({"model":"unknown-model","inputTokens":null,"outputTokens":null,"cachedInputTokens":null}),
    );
    insert(
        &db,
        json!({"model":null,"inputTokens":0,"outputTokens":0,"cachedInputTokens":0}),
    );
    // A partially reported request must not manufacture a complete total.
    insert(
        &db,
        json!({"model":"partial-model","inputTokens":5,"outputTokens":null,"cachedInputTokens":null}),
    );
    insert(
        &db,
        json!({"kind":"diagnostic","inputTokens":10000,"outputTokens":10000}),
    );
    insert(&db, json!({"startedAt":2000,"inputTokens":50000}));
    db.execute("UPDATE requests SET wait_ms=50,first_delta_ms=200,finished_at=started_at+1500 WHERE model='large-model'", []).unwrap();

    let result = statistics(&ops, json!({"pageSize":20}));
    assert_eq!(result["records"].as_array().unwrap().len(), 20);
    assert_eq!(result["summary"]["total"], 30);
    assert_eq!(result["summary"]["inferenceRequests"], 29);
    assert_eq!(result["summary"]["usageKnownRequests"], 27);
    assert_eq!(result["summary"]["totalTokens"], 1400);
    assert_eq!(result["summary"]["inputTokens"], 1255);
    assert_eq!(result["summary"]["averageFirstResponseMs"], 250.0);
    assert_eq!(result["summary"]["averageModelFirstResponseMs"], 200.0);
    assert_eq!(result["summary"]["averageTotalMs"], 1500.0);
    let models = result["modelUsage"].as_array().unwrap();
    assert_eq!(models.len(), 5);
    assert_eq!(models[0]["id"], "large-model");
    assert_eq!(models[0]["totalTokens"], 1100);
    let unknown = models.iter().find(|m| m["id"] == "unknown-model").unwrap();
    assert!(unknown["totalTokens"].is_null());
    let unnamed = models.iter().find(|m| m["id"] == "").unwrap();
    assert_eq!(unnamed["totalTokens"], 0);
    assert!(unnamed["name"].is_null());
    let trend = result["trend"].as_array().unwrap();
    assert_eq!(
        trend
            .iter()
            .filter_map(|b| b["totalTokens"].as_i64())
            .sum::<i64>(),
        1400
    );
    assert_eq!(
        trend
            .iter()
            .filter_map(|b| b["inferenceRequests"].as_i64())
            .sum::<i64>(),
        29
    );
    assert_eq!(trend[23]["totalTokens"], 1100);
    assert!(trend[1]["totalTokens"].is_null());

    let filtered = statistics(&ops, json!({"model":"unknown-model"}));
    assert!(filtered["summary"]["totalTokens"].is_null());
    assert_eq!(filtered["summary"]["usageKnownRequests"], 0);
    assert_eq!(filtered["modelUsage"].as_array().unwrap().len(), 1);
    assert!(filtered["trend"][0]["totalTokens"].is_null());
    let diagnostics = statistics(&ops, json!({"kind":"diagnostic"}));
    assert_eq!(diagnostics["modelUsage"], json!([]));
    assert!(diagnostics["summary"]["totalTokens"].is_null());
    assert_eq!(diagnostics["summary"]["usageKnownRequests"], 0);
    assert!(diagnostics["summary"]["inputTokens"].is_null());
    ops.clear_request_history().unwrap();
    assert_eq!(statistics(&ops, json!({}))["modelUsage"], json!([]));
}

#[test]
fn cache_trends_are_weighted_per_bucket_and_preserve_unknown_zero_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    for _ in 0..25 {
        insert(&db, json!({"inputTokens":100,"cachedInputTokens":90}));
    }
    insert(&db, json!({"inputTokens":900,"cachedInputTokens":0}));
    insert(&db, json!({"inputTokens":10000,"cachedInputTokens":null}));
    insert(
        &db,
        json!({"kind":"management","inputTokens":50000,"cachedInputTokens":50000}),
    );
    insert(
        &db,
        json!({"startedAt":1042,"inputTokens":200,"cachedInputTokens":null}),
    );
    insert(
        &db,
        json!({"startedAt":1084,"inputTokens":0,"outputTokens":0,"cachedInputTokens":0}),
    );
    insert(
        &db,
        json!({"startedAt":1126,"inputTokens":100,"cachedInputTokens":0}),
    );
    insert(
        &db,
        json!({"startedAt":1999,"accountId":ACCOUNT_B,"inputTokens":100,"cachedInputTokens":100}),
    );
    let value = statistics(&ops, json!({}));
    assert_eq!(value["records"].as_array().unwrap().len(), 20);
    let buckets = value["trend"].as_array().unwrap();
    assert_eq!(buckets[0]["cacheKnownRequests"], 26);
    assert_eq!(buckets[0]["cachedInputTokens"], 2250);
    assert_eq!(buckets[0]["cacheKnownInputTokens"], 3400);
    assert_eq!(
        buckets[0]["cacheHitRate"].as_f64().unwrap(),
        2250.0 / 3400.0
    );
    assert!(buckets[1]["cachedInputTokens"].is_null());
    assert_eq!(buckets[1]["cacheKnownRequests"], 0);
    assert!(buckets[1]["cacheHitRate"].is_null());
    assert_eq!(buckets[2]["cachedInputTokens"], 0);
    assert_eq!(buckets[2]["cacheKnownRequests"], 1);
    assert!(buckets[2]["cacheHitRate"].is_null());
    assert_eq!(buckets[3]["cacheHitRate"], 0.0);
    assert!(buckets[4]["cacheHitRate"].is_null());
    assert_eq!(buckets[4]["cacheKnownRequests"], 0);
    assert_eq!(buckets[23]["cacheHitRate"], 1.0);
    assert_eq!(
        buckets
            .iter()
            .filter_map(|b| b["cachedInputTokens"].as_i64())
            .sum::<i64>(),
        value["summary"]["cachedInputTokens"]
    );
    let filtered = statistics(&ops, json!({"accountId":ACCOUNT_A}));
    assert!(filtered["trend"][23]["cacheHitRate"].is_null());
    assert_eq!(filtered["trend"][0]["cachedInputTokens"], 2250);
    ops.clear_request_history().unwrap();
    assert!(
        statistics(&ops, json!({}))["trend"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["cacheHitRate"].is_null() && b["cacheKnownRequests"] == 0)
    );
}

#[test]
fn ranking_diagnostics_use_known_inference_samples_in_each_dimension() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    insert(
        &db,
        json!({"inputTokens":100,"cachedInputTokens":90,"startedAt":1000}),
    );
    insert(
        &db,
        json!({"inputTokens":900,"cachedInputTokens":0,"startedAt":1001}),
    );
    insert(
        &db,
        json!({"inputTokens":10000,"cachedInputTokens":null,"startedAt":1002,"status":"failed"}),
    );
    insert(
        &db,
        json!({"kind":"management","status":"failed","inputTokens":0,"cachedInputTokens":0,"startedAt":1003}),
    );
    db.execute("UPDATE requests SET first_delta_ms=200,wait_ms=50,finished_at=started_at+1000 WHERE started_at=1000", []).unwrap();
    db.execute("UPDATE requests SET first_delta_ms=900,wait_ms=100,finished_at=started_at+2000 WHERE started_at=1001", []).unwrap();
    db.execute("UPDATE requests SET first_delta_ms=9999,finished_at=started_at+9999 WHERE kind='management'", []).unwrap();
    let result = statistics(&ops, json!({}));
    for field in ["accountUsage", "keyUsage", "modelUsage"] {
        let row = &result[field][0];
        assert_eq!(row["averageFirstResponseMs"], 625.0);
        assert_eq!(row["averageModelFirstResponseMs"], 550.0);
        assert_eq!(row["averageTotalMs"], 1500.0);
        assert_eq!(row["cachedInputTokens"], 90);
        assert_eq!(row["cacheKnownInputTokens"], 1000);
        assert_eq!(row["cacheKnownRequests"], 2);
        assert_eq!(row["cacheHitRate"], 0.09);
        assert_eq!(
            row["failedRequests"],
            if field == "modelUsage" { 1 } else { 2 }
        );
    }
    assert_eq!(result["summary"]["inferenceSucceeded"], 2);
    assert_eq!(result["summary"]["inferenceFailed"], 1);
    assert_eq!(result["summary"]["averageModelFirstResponseMs"], 550.0);
    let unknown = statistics(&ops, json!({"status":"failed"}));
    assert!(unknown["keyUsage"][0]["cacheHitRate"].is_null());
    assert!(unknown["keyUsage"][0]["averageFirstResponseMs"].is_null());
    assert!(unknown["keyUsage"][0]["averageModelFirstResponseMs"].is_null());
    assert!(unknown["summary"]["averageModelFirstResponseMs"].is_null());
    db.execute(
        "UPDATE requests SET first_delta_ms=0 WHERE started_at=1001",
        [],
    )
    .unwrap();
    let zero = statistics(&ops, json!({"from":1001,"to":1001}));
    assert_eq!(zero["keyUsage"][0]["cacheHitRate"], 0.0);
    assert_eq!(zero["keyUsage"][0]["averageModelFirstResponseMs"], 0.0);
    assert_eq!(zero["summary"]["averageModelFirstResponseMs"], 0.0);
    assert_eq!(zero["summary"]["averageFirstResponseMs"], 100.0);
}
