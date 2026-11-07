use super::*;
use crate::operations::{ApiKeyCreate, DEFAULT_GROUP, Operations};
use uuid::Uuid;

#[test]
fn default_model_changes_preserve_live_and_persisted_response_bindings() {
    for persistent in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let ops = Operations::open(root.path()).unwrap();
        let revision = || ops.snapshot()["revision"].as_u64().unwrap();
        let account = Uuid::new_v4();
        ops.register_accounts(&[(account, "route identity".into())])
            .unwrap();
        let mut provider: Provider = serde_json::from_value(json!({
            "id":"fixture", "model":"native-default", "baseUrl":"http://127.0.0.1:9/v1",
            "auth":"none", "forwardHeaders":[], "requiredHeaders":[], "mcpServers":[]
        }))
        .unwrap();
        ops.set_inference_route(account, revision(), Some(provider.clone()))
            .unwrap();
        let key = ops
            .create_api_key(ApiKeyCreate {
                name: "owner".into(),
                group_id: DEFAULT_GROUP,
                models: vec![],
                expires_in_days: None,
                max_concurrency: 1,
                fast_mode: false,
            })
            .unwrap();
        let access = ops.authenticate_api_key(&key.secret).unwrap();
        let store = if persistent {
            Store::with_persistent_bindings(ops.clone()).unwrap()
        } else {
            Store::default()
        };
        let response = json!({"id":"first", "model":"requested-model", "output":[{
            "type":"reasoning", "id":"rs_first", "summary":[], "encrypted_content":"opaque"
        }]});
        remember_upstream_output(
            &store,
            &access,
            account,
            "requested-model",
            &provider,
            &[],
            &response,
        )
        .unwrap();
        if !persistent {
            store
                .save_bound(
                    &access,
                    vec![],
                    &response,
                    Some(storage::Binding {
                        account,
                        model: "requested-model".into(),
                        provider: reuse::provider_digest(&provider).unwrap(),
                    }),
                    None,
                )
                .unwrap();
        }
        let history = json!({"model":"requested-model", "input":response["output"]});
        provider.model = "new-native-default".into();
        ops.set_inference_route(account, revision(), Some(provider.clone()))
            .unwrap();
        assert_eq!(
            route_account(&store, &ops, &access, &history).unwrap(),
            Some(account)
        );
        let previous =
            json!({"model":"requested-model", "previous_response_id":"first", "input":[]});
        if !persistent {
            assert_eq!(
                route_account(&store, &ops, &access, &previous).unwrap(),
                Some(account)
            );
        }
        assert!(
            store
                .binding_for_history(
                    &access,
                    "other-model",
                    response["output"].as_array().unwrap()
                )
                .unwrap()
                .is_none(),
            "the explicit model remains part of the response binding"
        );
        provider.auth = "other-identity".into();
        ops.set_inference_route(account, revision(), Some(provider))
            .unwrap();
        assert_eq!(
            route_account(&store, &ops, &access, &history)
                .unwrap_err()
                .code,
            "RESPONSE_CHAIN_MISMATCH"
        );
        if !persistent {
            assert_eq!(
                route_account(&store, &ops, &access, &previous)
                    .unwrap_err()
                    .code,
                "RESPONSE_CHAIN_MISMATCH"
            );
        }
    }
}
