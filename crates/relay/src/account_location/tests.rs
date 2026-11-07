use super::*;

fn ny() -> Location {
    Location {
        timezone: "America/New_York".into(),
        country: "US".into(),
        city: "New York".into(),
        ..Default::default()
    }
}
#[test]
fn defaults_and_validation() {
    assert!(!serde_json::from_str::<Config>("{}").unwrap().enabled);
    assert!(
        Config {
            enabled: true,
            source: Source::Custom,
            ..Default::default()
        }
        .validated()
        .is_err()
    );
    assert!(ny().validate().is_ok());
    assert!(
        Location {
            country: String::new(),
            ..ny()
        }
        .validate()
        .is_err()
    );
    assert!(
        Location {
            timezone: "+08:00".into(),
            ..ny()
        }
        .validate()
        .is_err()
    );
    assert!(
        Location {
            country: "ZZ".into(),
            ..ny()
        }
        .validate()
        .is_err()
    );
}
#[test]
fn dates_follow_dst_and_midnight() {
    let date = |s: &str| ny().date(s.parse::<DateTime<Utc>>().unwrap());
    assert_eq!(date("2026-07-01T03:59:59Z"), "2026-06-30");
    assert_eq!(date("2026-07-01T04:00:00Z"), "2026-07-01");
    assert_eq!(date("2026-01-01T04:59:59Z"), "2025-12-31");
    assert_eq!(date("2026-01-01T05:00:00Z"), "2026-01-01");
}
#[test]
fn only_complete_time_blocks_are_aligned() {
    let now = "2026-07-01T03:00:00Z".parse().unwrap();
    let context = "<environment_context><timezone>Asia/Shanghai</timezone><cwd>/workspace</cwd><current_date>2026-07-01</current_date></environment_context>";
    let result = ny().align_text(context, "user", now);
    assert!(result.contains("America/New_York"));
    assert!(result.contains("2026-06-30"));
    assert!(result.contains("<cwd>/workspace</cwd>"));
    assert_eq!(ny().align_text(context, "assistant", now), context);
    let quoted = format!("Please explain {context}");
    assert_eq!(ny().align_text(&quoted, "user", now), quoted);
    let mut items = json!([{"type":"message","role":"user","internal_chat_message_metadata_passthrough":{"content_item_kinds":["text"]},"content":[{"type":"input_text","text":context}]}]);
    let original = items.clone();
    ny().align_items(&mut items, now);
    assert_eq!(items, original);
}
#[test]
fn search_override_does_not_enable_search_or_replace_instructions() {
    let mut params = json!({"config":{"web_search":"disabled"},"developerInstructions":"Keep my existing instructions."});
    ny().apply_thread(&mut params, Utc::now());
    assert_eq!(params["config"]["web_search"], "disabled");
    assert_eq!(
        params["developerInstructions"],
        "Keep my existing instructions."
    );
    assert_eq!(
        params["config"]["tools.web_search.location"]["city"],
        "New York"
    );
    let custom = Location {
        timezone: "Asia/Tokyo".into(),
        ..Default::default()
    };
    custom.apply_thread(&mut params, Utc::now());
    assert!(
        params["config"]["tools.web_search.location"]
            .get("city")
            .is_none()
    );
}

#[test]
fn search_advertisement_requires_both_an_override_and_a_requested_tool() {
    let (rpc, _) = crate::rpc::Rpc::new();
    let tools = vec![json!({"type":"web_search"})];
    assert!(rpc.location_search_item(&tools, false).is_none());
    rpc.set_location(Some(ny()));
    assert!(rpc.location_search_item(&tools, true).is_none());
    assert!(rpc.location_search_item(&[], false).is_none());
    assert_eq!(
        rpc.location_search_item(&tools, false).unwrap()["tools"][0]["user_location"]["country"],
        "US"
    );
    rpc.set_location(None);
    assert!(rpc.location_search_item(&tools, false).is_none());
}
