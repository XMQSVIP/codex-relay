use codex_relay::operations::{Operations, StatsQuery};
use rusqlite::{Connection, params};
use serde_json::json;

#[test]
fn calendar_range_filters_boundaries_usage_and_trend_together() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    let db = Connection::open(dir.path().join("operations.sqlite3")).unwrap();
    for time in [999, 1000, 1999, 2000] {
        db.execute(
            "INSERT INTO requests(id,started_at,kind,action,status,model,input_tokens,output_tokens) VALUES(?1,?2,'inference','responses','succeeded','range-model',10,1)",
            params![format!("record-{time}"), time],
        ).unwrap();
    }
    let stats = ops
        .statistics(
            serde_json::from_value(json!({"from":1000,"to":1999,"model":"range-model"})).unwrap(),
        )
        .unwrap();
    assert_eq!(stats["from"], 1000);
    assert_eq!(stats["to"], 1999);
    assert_eq!(stats["summary"]["total"], 2);
    assert_eq!(stats["summary"]["inputTokens"], 20);
    assert_eq!(stats["records"][0]["startedAt"], 1999);
    assert_eq!(stats["records"][1]["startedAt"], 1000);
    let trend = stats["trend"].as_array().unwrap();
    assert_eq!(trend.len(), 24);
    assert_eq!(
        trend
            .iter()
            .map(|b| b["count"].as_u64().unwrap())
            .sum::<u64>(),
        2
    );
    let point = ops
        .statistics(serde_json::from_value(json!({"from":1000,"to":1000})).unwrap())
        .unwrap();
    assert_eq!(point["summary"]["total"], 1);
}

#[test]
fn date_ranges_reject_incomplete_reversed_negative_and_overflowing_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    for value in [
        json!({"from":1000}),
        json!({"to":2000}),
        json!({"from":2000,"to":1000}),
        json!({"from":-1,"to":2000}),
        json!({"from":0,"to":i64::MAX}),
        json!({"from":1000,"to":2000,"hours":24}),
    ] {
        let query: StatsQuery = serde_json::from_value(value).unwrap();
        assert_eq!(ops.statistics(query).unwrap_err().code, "INVALID_INPUT");
    }
    for span in [23 * 3_600_000_i64, 25 * 3_600_000] {
        let result = ops
            .statistics(serde_json::from_value(json!({"from":1000,"to":1000+span-1})).unwrap())
            .unwrap();
        assert_eq!(result["trend"].as_array().unwrap().len(), 24);
    }
    assert!(ops.statistics(StatsQuery::default()).is_ok());
}

#[tokio::test]
async fn statistics_http_endpoint_accepts_calendar_dates_from_the_console() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use codex_relay::admin::{AdminState, router};
    use tower::ServiceExt;

    let dir = tempfile::tempdir().unwrap();
    let state = AdminState::new_with_network(4317, dir.path().join("accounts"), None, vec![])
        .await
        .unwrap();
    let app = router(state, dir.path().join("web"));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/statistics?from=1792987200000&to=1793073599999&page=1")
                .header("host", "127.0.0.1:4317")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 100_000).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let data: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(data["from"], 1792987200000_i64);
    assert_eq!(data["to"], 1793073599999_i64);
    assert_eq!(data["trend"].as_array().unwrap().len(), 24);
}

#[test]
fn high_page_numbers_clamp_to_the_last_available_page() {
    let dir = tempfile::tempdir().unwrap();
    let ops = Operations::open(dir.path()).unwrap();
    ops.begin(None, None, "management", "fixture", false)
        .unwrap()
        .complete(Ok(()))
        .unwrap();
    for page in [10_001, u32::MAX] {
        let result = ops
            .statistics(StatsQuery {
                page: Some(page),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result["page"], 1);
        assert_eq!(result["records"].as_array().unwrap().len(), 1);
    }
}
