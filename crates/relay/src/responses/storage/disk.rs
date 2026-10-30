//! Persist routing fingerprints only; response bodies live in connection caches.
use super::{Binding, Store, missing};
use crate::{
    error::{Error, Result},
    operations::{ClientAccess, Operations},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::sync::Arc;

const RETENTION_MS: i64 = 30 * 86_400_000;
pub(super) struct Disk {
    pub ops: Arc<Operations>,
    pub budget: usize,
}
fn sql(_: rusqlite::Error) -> Error {
    Error::new("RESPONSE_STORAGE", "账户绑定数据库读写失败。")
}
fn mismatch() -> Error {
    Error::new(
        "RESPONSE_CHAIN_MISMATCH",
        "历史混合了不同账户或路由的响应；请重新开始会话。",
    )
}
fn cleanup(db: &Connection, now: i64) -> Result<()> {
    db.execute("DELETE FROM response_affinities WHERE expires<=?1", [now])
        .map_err(sql)?;
    Ok(())
}
fn authorized(db: &Connection, access: &ClientAccess, model: &str) -> Result<()> {
    let row: Option<(String, String, String, i64)> = db
        .query_row(
            "SELECT state,group_id,models,expires_at FROM api_keys WHERE id=?1",
            [access.key_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((state, group, models, expiry)) = row else {
        return Err(missing());
    };
    let models: Vec<String> = serde_json::from_str(&models)?;
    if state != "active"
        || group != access.group_id.to_string()
        || expiry <= crate::now_ms() as i64
        || !access.allows(model)
        || (!models.is_empty() && !models.iter().any(|m| m == model))
    {
        return Err(missing());
    }
    Ok(())
}
fn usage(db: &Connection) -> Result<i64> {
    db.query_row(
        "SELECT COALESCE(SUM(length(binding)+length(fingerprint)+160),0) FROM response_affinities",
        [],
        |r| r.get(0),
    )
    .map_err(sql)
}
fn check_budget(db: &Connection, budget: usize) -> Result<()> {
    if usage(db)? > budget as i64 {
        return Err(Error::new(
            "RESPONSE_STORAGE_FULL",
            &format!("账户绑定超过逻辑预算 {budget} 字节；请调整 BRIDGE_RESPONSE_BINDING_BYTES。"),
        ));
    }
    Ok(())
}
fn lookup(
    db: &Connection,
    access: &ClientAccess,
    model: &str,
    key: &str,
    now: i64,
) -> Result<Option<Binding>> {
    let value: Option<String>=db.query_row("SELECT binding FROM response_affinities WHERE owner=?1 AND group_id=?2 AND model=?3 AND fingerprint=?4 AND expires>?5",
        params![access.key_id.to_string(),access.group_id.to_string(),model,key,now],|r|r.get(0)).optional().map_err(sql)?;
    value
        .map(|v| serde_json::from_str(&v).map_err(Into::into))
        .transpose()
}
impl Disk {
    pub fn binding(
        &self,
        access: &ClientAccess,
        model: &str,
        history: &[Value],
        renew: Option<&Binding>,
    ) -> Result<Option<Binding>> {
        self.ops.with_response_db(|db|{
            let now=crate::now_ms() as i64;
            let tx=db.transaction().map_err(sql)?;
            authorized(&tx,access,model)?;
            let mut found=None;
            for item in history {
                for key in Store::history_keys(item) {
                    if let Some(binding)=lookup(&tx,access,model,&key,now)? {
                        if found.as_ref().is_some_and(|v|v!=&binding) {return Err(mismatch());}
                        found=Some(binding);
                    }
                    if renew.is_some() {
                        tx.execute("UPDATE response_affinities SET expires=?5 WHERE owner=?1 AND group_id=?2 AND model=?3 AND fingerprint=?4 AND expires>?6",params![access.key_id.to_string(),access.group_id.to_string(),model,key,now+RETENTION_MS,now]).map_err(sql)?;
                    }
                }
            }
            if let Some(expected)=renew {
                if found.as_ref().is_some_and(|v|v!=expected){return Err(mismatch());}
            }
            cleanup(&tx,now)?;
            tx.commit().map_err(sql)?;
            Ok(found)
        })
    }
    pub fn remember(
        &self,
        access: &ClientAccess,
        history: &[Value],
        response: &Value,
        binding: &Binding,
    ) -> Result<()> {
        self.ops.with_response_db(|db|{
            let now=crate::now_ms() as i64;
            let tx=db.transaction().map_err(sql)?;
            authorized(&tx,access,&binding.model)?;
            cleanup(&tx,now)?;
            let serialized=serde_json::to_string(binding)?;
            for key in Store::exchange_keys(history,response) {
                    if key.starts_with("encrypted:") && lookup(&tx,access,&binding.model,&key,now)?.is_some_and(|old|old!=*binding) {return Err(mismatch());}
                    tx.execute("INSERT INTO response_affinities(owner,group_id,model,fingerprint,account,binding,expires) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(owner,group_id,model,fingerprint) DO UPDATE SET binding=excluded.binding,account=excluded.account,expires=excluded.expires",params![access.key_id.to_string(),access.group_id.to_string(),binding.model,key,binding.account.to_string(),serialized,now+RETENTION_MS]).map_err(sql)?;
            }
            check_budget(&tx,self.budget)?;
            tx.commit().map_err(sql)?;
            Ok(())
        })
    }
    pub fn snapshot(&self) -> Result<Value> {
        self.ops.with_response_db(|db|{
            cleanup(db,crate::now_ms() as i64)?;
            let count:i64=db.query_row("SELECT COUNT(*) FROM response_affinities",[],|r|r.get(0)).map_err(sql)?;
            Ok(json!({"count":count,"bytes":usage(db)?,"maxBytes":self.budget,"ttlSeconds":RETENTION_MS/1000,"persistent":true,"bodiesStored":false}))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{ApiKeyCreate, DEFAULT_GROUP};
    use uuid::Uuid;
    fn fixture(root: &std::path::Path) -> (Arc<Operations>, Store, ClientAccess, String, Binding) {
        let ops = Operations::open(root).unwrap();
        let key = ops
            .create_api_key(ApiKeyCreate {
                fast_mode: false,
                name: "persistence test".into(),
                group_id: DEFAULT_GROUP,
                models: vec!["m".into()],
                expires_in_days: None,
                max_concurrency: 1,
            })
            .unwrap()
            .secret;
        let access = ops.authenticate_api_key(&key).unwrap();
        let store = Store::with_persistent_bindings(ops.clone()).unwrap();
        let binding = Binding {
            account: Uuid::new_v4(),
            model: "m".into(),
            provider: [3; 32],
        };
        (ops, store, access, key, binding)
    }
    fn response(id: &str) -> Value {
        json!({"id":id,"model":"m","store":false,"output":[{"type":"reasoning","id":format!("r_{id}"),"summary":[],"encrypted_content":format!("OPAQUE_{id}")}]})
    }
    #[test]
    fn unbound_history_is_remembered_after_acceptance_and_conflicts_roll_back() {
        let root = tempfile::tempdir().unwrap();
        let (ops, store, access, _, binding) = fixture(root.path());
        let history = vec![
            json!({"type":"compaction","encrypted_content":"IMPORTED_PRIVATE_COMPACTION"}),
            json!({"id":"client-message","role":"user","content":"IMPORTED_PRIVATE_PROMPT"}),
        ];
        assert_eq!(
            store.binding_for_history(&access, "m", &history).unwrap(),
            None
        );
        store.renew(&access, "m", &history, &binding).unwrap();
        assert_eq!(store.snapshot()["count"], 0);
        store
            .remember_exchange(&access, &history, &json!({"output":[]}), &binding)
            .unwrap();
        assert_eq!(
            store.binding_for_history(&access, "m", &history).unwrap(),
            Some(binding.clone())
        );
        assert_eq!(
            store
                .binding_for_history(&access, "m", &history[1..])
                .unwrap(),
            None
        );
        let other = Binding {
            account: Uuid::new_v4(),
            ..binding.clone()
        };
        let unknown = json!({"type":"compaction","encrypted_content":"STILL_UNBOUND"});
        let mixed = vec![unknown.clone(), history[0].clone()];
        assert_eq!(
            store
                .remember_exchange(&access, &mixed, &response("rejected"), &other)
                .unwrap_err()
                .code,
            "RESPONSE_CHAIN_MISMATCH"
        );
        assert_eq!(
            store.binding_for_history(&access, "m", &[unknown]).unwrap(),
            None
        );
        assert_eq!(store.snapshot()["count"], 1);
        ops.with_response_db(|db| {
            let serialized: String = db
                .query_row(
                    "SELECT group_concat(binding || fingerprint) FROM response_affinities",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!serialized.contains("IMPORTED_PRIVATE"));
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn only_fingerprints_survive_restart_and_legacy_bodies_are_inaccessible() {
        let root = tempfile::tempdir().unwrap();
        let (ops, store, access, key, binding) = fixture(root.path());
        let private = json!({"id":"private","model":"m","output":[
            {"type":"message","id":"answer","role":"assistant","content":[{"type":"output_text","text":"NEVER_WRITE_REPLY"}]},
            {"type":"reasoning","id":"private-item","summary":[],"encrypted_content":"NEVER_WRITE_ENCRYPTED"}
        ]});
        assert!(
            store
                .save_bound(
                    &access,
                    vec![json!({"role":"user","content":"NEVER_WRITE_PROMPT"})],
                    &private,
                    Some(binding.clone()),
                    None
                )
                .is_err()
        );
        store.remember_binding(&access, &private, &binding).unwrap();
        ops.with_response_db(|db| {
            let tables: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('response_records','response_refs')", [], |r| r.get(0)).unwrap();
            assert_eq!(tables, 0);
            let serialized: String = db.query_row("SELECT group_concat(binding || fingerprint) FROM response_affinities", [], |r| r.get(0)).unwrap();
            assert!(!serialized.contains("NEVER_WRITE"));
            // Existing data is left untouched, but cannot be read or resumed by ID.
            db.execute_batch("CREATE TABLE response_records(id TEXT, history TEXT); INSERT INTO response_records VALUES('legacy','OLD_BODY');").unwrap();
            Ok(())
        }).unwrap();
        assert_eq!(store.snapshot()["bodiesStored"], false);
        drop(store);
        drop(ops);
        let bytes = std::fs::read(root.path().join("operations.sqlite3")).unwrap();
        assert!(
            !bytes
                .windows(b"NEVER_WRITE".len())
                .any(|w| w == b"NEVER_WRITE")
        );
        let ops = Operations::open(root.path()).unwrap();
        let access = ops.authenticate_api_key(&key).unwrap();
        let store = Store::with_persistent_bindings(ops.clone()).unwrap();
        for id in ["private", "legacy"] {
            assert!(store.get(&access, id).is_err());
            assert!(store.continuation(&access, id).is_err());
            assert!(store.delete(&access, id).is_err());
        }
        assert_eq!(
            store
                .binding_for_history(&access, "m", private["output"].as_array().unwrap())
                .unwrap(),
            Some(binding)
        );
        ops.with_response_db(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT history FROM response_records WHERE id='legacy'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "OLD_BODY"
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn only_authorized_continuations_renew_and_expired_history_is_unbound() {
        let root = tempfile::tempdir().unwrap();
        let (ops, store, access, _, binding) = fixture(root.path());
        let saved = response("first");
        store.remember_binding(&access, &saved, &binding).unwrap();
        let expiry = crate::now_ms() as i64 + 60_000;
        ops.with_response_db(|db| {
            db.execute("UPDATE response_affinities SET expires=?1", [expiry])
                .unwrap();
            Ok(())
        })
        .unwrap();
        let history = saved["output"].as_array().unwrap();
        store.binding_for_history(&access, "m", history).unwrap();
        let wrong = Binding {
            account: Uuid::new_v4(),
            ..binding.clone()
        };
        assert!(store.renew(&access, "m", history, &wrong).is_err());
        let read_expiry = || {
            ops.with_response_db(|db| {
                Ok(db
                    .query_row("SELECT MIN(expires) FROM response_affinities WHERE fingerprint LIKE 'encrypted:%'", [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .unwrap())
            })
            .unwrap()
        };
        assert_eq!(read_expiry(), expiry);
        assert!(
            store
                .binding_for_history(&access, "other-model", history)
                .is_err()
        );
        store.renew(&access, "m", history, &binding).unwrap();
        assert!(read_expiry() > crate::now_ms() as i64 + 29 * 86_400_000);
        ops.with_response_db(|db| {
            db.execute("UPDATE response_affinities SET expires=0", [])
                .unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(
            store.binding_for_history(&access, "m", history).unwrap(),
            None
        );
    }
    #[test]
    fn key_deletion_clears_fingerprints() {
        let root = tempfile::tempdir().unwrap();
        let (ops, store, access, _, binding) = fixture(root.path());
        let saved = response("parent");
        store.remember_binding(&access, &saved, &binding).unwrap();
        let revision = ops.list_api_keys().unwrap()[0].revision;
        ops.delete_api_key(access.key_id, revision).unwrap();
        assert!(
            store
                .binding_for_history(&access, "m", saved["output"].as_array().unwrap())
                .is_err()
        );
        ops.with_response_db(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM response_affinities", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn revocation_and_account_deletion_clear_persisted_state_without_cross_key_access() {
        let root = tempfile::tempdir().unwrap();
        let (ops, store, access, _, binding) = fixture(root.path());
        ops.register_accounts(&[(binding.account, "persistent account".into())])
            .unwrap();
        let other = ops
            .create_api_key(ApiKeyCreate {
                fast_mode: false,
                name: "other".into(),
                group_id: DEFAULT_GROUP,
                models: vec!["m".into()],
                expires_in_days: None,
                max_concurrency: 1,
            })
            .unwrap();
        let other = ops.authenticate_api_key(&other.secret).unwrap();
        let saved = response("private-chain");
        for caller in [&access, &other] {
            store.remember_binding(caller, &saved, &binding).unwrap();
        }
        let unique = response("owner-only");
        store.remember_binding(&access, &unique, &binding).unwrap();
        assert!(store.get(&other, "owner-only").is_err());
        assert_eq!(
            store
                .binding_for_history(&other, "m", unique["output"].as_array().unwrap())
                .unwrap(),
            None
        );
        let revision = ops
            .list_api_keys()
            .unwrap()
            .into_iter()
            .find(|k| k.id == access.key_id)
            .unwrap()
            .revision;
        ops.update_api_key(
            access.key_id,
            crate::operations::ApiKeyUpdate {
                revision,
                state: Some("revoked".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            store
                .binding_for_history(&access, "m", saved["output"].as_array().unwrap())
                .is_err()
        );
        assert!(
            store
                .binding_for_history(&other, "m", saved["output"].as_array().unwrap())
                .is_ok()
        );
        ops.begin_account_delete(
            binding.account,
            ops.snapshot()["revision"].as_u64().unwrap(),
        )
        .unwrap();
        ops.finish_account_delete(binding.account).unwrap();
        assert_eq!(
            store
                .binding_for_history(&other, "m", saved["output"].as_array().unwrap())
                .unwrap(),
            None
        );
        ops.with_response_db(|db| {
            let count: i64 = db
                .query_row("SELECT COUNT(*) FROM response_affinities", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn budget_exhaustion_skips_new_bindings_and_cache_eviction_preserves_lane_heads() {
        let root = tempfile::tempdir().unwrap();
        let (ops, _, access, _, binding) = fixture(root.path());
        let store = Store {
            disk: Some(Disk {
                ops: ops.clone(),
                budget: 1,
            }),
            ..Store::default()
        };
        store
            .remember_binding(&access, &response("big"), &binding)
            .unwrap();
        ops.with_response_db(|db| {
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM response_affinities", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            Ok(())
        })
        .unwrap();
        let cache = Store::default();
        cache
            .save(
                &access,
                vec![],
                &json!({"id":"head","model":"m","output":[]}),
            )
            .unwrap();
        cache.pin_latest(Some("slow-lane"), "head");
        for i in 0..140 {
            cache
                .save(
                    &access,
                    vec![],
                    &json!({"id":format!("r{i}"),"model":"m","output":[]}),
                )
                .unwrap();
        }
        assert!(cache.get(&access, "head").is_ok());
        assert!(cache.get(&access, "r139").is_ok());
        assert!(cache.get(&access, "r0").is_err());
    }
}
