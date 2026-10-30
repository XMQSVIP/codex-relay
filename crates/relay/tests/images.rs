use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_relay::{
    admin::{AdminState, router},
    operations::{ApiKeyCreate, DEFAULT_GROUP, StatsQuery},
};
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn images_routes_authenticate_and_record_rejections_instead_of_static_405() {
    let temp = tempfile::tempdir().unwrap();
    let state = AdminState::new(4317, temp.path().join("accounts"), "a".repeat(64))
        .await
        .unwrap();
    let key = state
        .operations
        .create_api_key(
            serde_json::from_value::<ApiKeyCreate>(
                json!({"name":"image caller","groupId":DEFAULT_GROUP}),
            )
            .unwrap(),
        )
        .unwrap()
        .secret;
    let app = router(state.clone(), temp.path().join("web"));
    for path in ["/v1/images/generations", "/v1/images/edits"] {
        for authorized in [false, true] {
            let body = json!({"prompt":"PRIVATE_IMAGE_PROMPT"});
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("host", "127.0.0.1:4317")
                        .header("content-type", "application/json")
                        .header(
                            "authorization",
                            format!("Bearer {}", if authorized { &key } else { "invalid" }),
                        )
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if authorized {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
            let id = response.headers()["x-request-id"]
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(value["error"].is_object());
            if authorized {
                let saved = state.operations.request_content(id).unwrap();
                assert_eq!(saved["response"]["statusCode"], 400);
                assert_eq!(saved["response"]["body"], value);
                assert!(!saved.to_string().contains("PRIVATE_IMAGE_PROMPT"));
            } else {
                assert!(state.operations.request_content(id).is_err());
            }
        }
    }
    let records = state.operations.statistics(StatsQuery::default()).unwrap();
    assert_eq!(records["summary"]["total"], 2);
}
