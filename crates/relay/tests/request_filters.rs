use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::admin::{AdminState, router};
use rusqlite::{Connection, params};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

async fn get(app: &axum::Router, query: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/statistics?{query}"))
                .header("host", "127.0.0.1:4317")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn name_search_uses_history_snapshots_for_pages_and_aggregates() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let state = AdminState::new_with_network(4317, root.clone(), None, vec![])
        .await
        .unwrap();
    let app = router(state, temp.path().join("web"));
    let account_id = Uuid::new_v4();
    let group_id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    let db = Connection::open(root.join("operations/operations.sqlite3")).unwrap();
    // No current account/group exists: search must use historical names.
    for i in 0..25 {
        db.execute(
            "INSERT INTO requests(id,started_at,kind,action,status,account_id,account_name,group_id,group_name,key_id,key_name,input_tokens,output_tokens)
             VALUES(?1,?2,'inference','responses','succeeded',?3,?4,?5,?6,?7,'test key',10,2)",
            params![Uuid::new_v4().to_string(), codex_relay::now_ms() as i64,
                account_id.to_string(), if i < 22 { Some("nn%_历史账户") } else if i == 24 { None } else { Some("其他账户") },
                group_id.to_string(), if i < 22 { Some("归档%_分组") } else if i == 24 { None } else { Some("其他分组") }, key_id.to_string()],
        ).unwrap();
    }
    // This is the invalid parameter the old search box sent for the screenshot.
    assert_eq!(
        get(&app, "accountId=nn").await.status(),
        StatusCode::BAD_REQUEST
    );
    for query in [
        "accountName=nn&page=2",
        "accountName=%25_&page=2",
        "groupName=%25_&page=2",
        "accountName=nn&groupName=%25_&status=succeeded&page=2",
    ] {
        let response = get(&app, query).await;
        assert_eq!(response.status(), StatusCode::OK, "{query}");
        let result: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(result["summary"]["total"], 22, "{query}");
        assert_eq!(result["summary"]["inputTokens"], 220);
        assert_eq!(result["records"].as_array().unwrap().len(), 2);
        assert_eq!(result["accountUsage"][0]["requestCount"], 22);
        assert_eq!(result["keyUsage"][0]["totalTokens"], 264);
        assert_eq!(
            result["trend"]
                .as_array()
                .unwrap()
                .iter()
                .map(|bucket| bucket["count"].as_u64().unwrap())
                .sum::<u64>(),
            22
        );
    }
    for (query, expected) in [
        ("accountName=missing".to_owned(), 0),
        ("groupName=missing".to_owned(), 0),
        ("accountName=NN".to_owned(), 0),
        (format!("accountId={account_id}"), 25),
        (format!("groupId={group_id}"), 25),
        (format!("accountId={account_id}&accountName=nn"), 22),
        (format!("groupId={}&accountName=nn", Uuid::new_v4()), 0),
    ] {
        let response = get(&app, &query).await;
        assert_eq!(response.status(), StatusCode::OK, "{query}");
        let result: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(result["summary"]["total"], expected, "{query}");
    }
}
