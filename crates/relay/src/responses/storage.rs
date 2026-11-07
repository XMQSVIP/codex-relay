use crate::{
    error::{Error, Result},
    operations::ClientAccess,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use uuid::Uuid;
mod disk;
const TTL: Duration = Duration::from_secs(3600);
const MAX_BYTES: usize = 128 * 1024 * 1024;
type AffinityKey = (Uuid, String, String);
type Affinities = HashMap<AffinityKey, (Binding, Instant)>;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Binding {
    pub account: Uuid,
    pub model: String,
    pub provider: [u8; 32],
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn deletion_invalidates_live_caches_and_closed_connections_release_capacity() {
        let global = Store::default();
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec![],
        };
        let cache = Arc::new(Store::default());
        global.connect_cache(&cache).unwrap();
        let response = json!({"id":"stored","model":"m","output":[]});
        global.save(&access, vec![], &response).unwrap();
        cache.save(&access, vec![], &response).unwrap();
        global.delete(&access, "stored").unwrap();
        assert!(cache.continuation(&access, "stored").is_err());
        let mut others = vec![];
        for _ in 1..32 {
            let other = Arc::new(Store::default());
            global.connect_cache(&other).unwrap();
            others.push(other);
        }
        assert!(global.connect_cache(&Arc::new(Store::default())).is_err());
        drop(others);
        drop(cache);
        global.connect_cache(&Arc::new(Store::default())).unwrap();
    }
    #[test]
    fn encrypted_history_affinity_is_scoped_and_mixed_accounts_are_rejected() {
        let store = Store::default();
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec!["m".into()],
        };
        let binding = Binding {
            account: Uuid::new_v4(),
            model: "m".into(),
            provider: [1; 32],
        };
        let item = json!({"type":"compaction","id":"c1","encrypted_content":"opaque"});
        store
            .remember_binding(&access, &json!({"output":[item]}), &binding)
            .unwrap();
        assert_eq!(
            store
                .binding_for_history(&access, "m", std::slice::from_ref(&item))
                .unwrap(),
            Some(binding.clone())
        );
        let unknown = json!({"type":"compaction","encrypted_content":"foreign"});
        assert_eq!(
            store.binding_for_history(&access, "m", &[unknown]).unwrap(),
            None
        );
        let other = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            ..access.clone()
        };
        assert_eq!(
            store
                .binding_for_history(&other, "m", std::slice::from_ref(&item))
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .binding_for_history(&access, "other", std::slice::from_ref(&item))
                .unwrap(),
            None
        );
        let other_group = ClientAccess {
            group_id: Uuid::new_v4(),
            ..access.clone()
        };
        assert_eq!(
            store
                .binding_for_history(&other_group, "m", std::slice::from_ref(&item))
                .unwrap(),
            Some(binding.clone())
        );
        let second = json!({"type":"reasoning","id":"r2","encrypted_content":"second"});
        store
            .remember_binding(
                &access,
                &json!({"output":[second]}),
                &Binding {
                    account: Uuid::new_v4(),
                    ..binding
                },
            )
            .unwrap();
        assert!(
            store
                .binding_for_history(&access, "m", &[item, second])
                .is_err()
        );
    }
    #[test]
    fn full_text_history_does_not_require_its_original_account_but_item_references_do() {
        let store = Store::default();
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec![],
        };
        let first = Binding {
            account: Uuid::new_v4(),
            model: "m".into(),
            provider: [1; 32],
        };
        let second = Binding {
            account: Uuid::new_v4(),
            ..first.clone()
        };
        let a = json!({"type":"message","id":"a","role":"assistant","content":[{"type":"output_text","text":"first"}]});
        let b = json!({"type":"message","id":"b","role":"assistant","content":[{"type":"output_text","text":"second"}]});
        store
            .remember_binding(&access, &serde_json::json!({"output":[a]}), &first)
            .unwrap();
        store
            .remember_binding(&access, &serde_json::json!({"output":[b]}), &second)
            .unwrap();
        assert_eq!(
            store.binding_for_history(&access, "m", &[a, b]).unwrap(),
            None
        );
        let call = json!({"type":"function_call","id":"fc","call_id":"call","name":"tool","arguments":"{}"});
        store
            .remember_binding(&access, &json!({"output":[call]}), &first)
            .unwrap();
        assert_eq!(
            store.binding_for_history(&access, "m", &[call]).unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            store
                .binding_for_history(&access, "m", &[json!({"type":"item_reference","id":"a"})])
                .unwrap(),
            Some(first)
        );
    }
    #[test]
    fn a_full_protected_connection_cache_does_not_fail_a_completed_response() {
        let cache = Store::default();
        let access = ClientAccess {
            fast_mode: false,
            key_id: Uuid::new_v4(),
            group_id: Uuid::new_v4(),
            models: vec![],
        };
        cache
            .save(
                &access,
                vec![],
                &serde_json::json!({"id":"head","output":[]}),
            )
            .unwrap();
        cache.pin_latest(Some("old"), "head");
        cache.entries.lock().unwrap().get_mut("head").unwrap().bytes = MAX_BYTES;
        cache
            .save(
                &access,
                vec![],
                &serde_json::json!({"id":"completed","status":"completed","output":[]}),
            )
            .unwrap();
        assert!(cache.get(&access, "head").is_ok());
        assert!(cache.get(&access, "completed").is_err());
        assert_eq!(
            cache.protected.lock().unwrap().get("stream:old").unwrap(),
            "head"
        );
    }
}
#[derive(Clone)]
pub(super) struct Continuation {
    pub history: Vec<Value>,
    pub binding: Option<Binding>,
}
struct Stored {
    owner: Uuid,
    history: Vec<Value>,
    response: Value,
    expires: Instant,
    bytes: usize,
    binding: Option<Binding>,
    defaults: Option<Value>,
}
#[derive(Default)]
pub struct Store {
    entries: Mutex<HashMap<String, Stored>>,
    affinities: Mutex<Affinities>,
    connections: Mutex<Vec<Weak<Store>>>,
    disk: Option<disk::Disk>,
    protected: Mutex<HashMap<String, String>>,
}
fn missing() -> Error {
    Error::new("RESPONSE_NOT_FOUND", "响应不存在、已过期，或不属于此密钥。")
}
impl Store {
    pub(super) fn pin_latest(&self, lane: Option<&str>, id: &str) {
        let entries = self.entries.lock().unwrap();
        let lane = lane.map_or_else(|| "default".into(), |s| format!("stream:{s}"));
        let mut protected = self.protected.lock().unwrap();
        if entries.contains_key(id) {
            protected.insert(lane, id.into());
        } else {
            protected.remove(&lane);
        }
    }
    pub fn with_persistent_bindings(ops: Arc<crate::operations::Operations>) -> Result<Self> {
        let budget = std::env::var("BRIDGE_RESPONSE_BINDING_BYTES")
            .ok()
            .map(|v| v.parse::<usize>())
            .transpose()
            .map_err(|_| {
                Error::new(
                    "RESPONSE_STORAGE_CONFIG",
                    "BRIDGE_RESPONSE_BINDING_BYTES 必须是字节数。",
                )
            })?
            .unwrap_or(1024 * 1024 * 1024);
        if budget < 1024 * 1024 || budget > i64::MAX as usize {
            return Err(Error::new(
                "RESPONSE_STORAGE_CONFIG",
                "账户绑定存储预算至少为 1 MiB。",
            ));
        }
        Ok(Self {
            disk: Some(disk::Disk { ops, budget }),
            ..Self::default()
        })
    }
    pub(super) fn renew(
        &self,
        access: &ClientAccess,
        model: &str,
        history: &[Value],
        binding: &Binding,
    ) -> Result<()> {
        if let Some(disk) = &self.disk {
            disk.binding(access, model, history, Some(binding))?;
        }
        Ok(())
    }
    pub fn invalidate_owner(&self, owner: Uuid) {
        self.entries.lock().unwrap().retain(|_, s| s.owner != owner);
        self.affinities
            .lock()
            .unwrap()
            .retain(|(key, _, _), _| *key != owner);
        for cache in self
            .connections
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            cache.invalidate_owner(owner);
        }
    }
    pub fn invalidate_account(&self, account: Uuid) {
        self.entries
            .lock()
            .unwrap()
            .retain(|_, s| s.binding.as_ref().is_none_or(|b| b.account != account));
        self.affinities
            .lock()
            .unwrap()
            .retain(|_, (b, _)| b.account != account);
        for cache in self
            .connections
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            cache.invalidate_account(account);
        }
    }
    pub(super) fn connect_cache(&self, cache: &Arc<Store>) -> Result<()> {
        let mut connections = self.connections.lock().unwrap();
        connections.retain(|cache| cache.strong_count() > 0);
        if connections.len() >= 32 {
            return Err(Error::new(
                "CONCURRENCY_LIMIT",
                "Responses WebSocket 连接已达到 32 个上限。",
            ));
        }
        connections.push(Arc::downgrade(cache));
        Ok(())
    }
    fn item_keys(item: &Value) -> Vec<String> {
        use sha2::{Digest, Sha256};
        let mut keys = vec![];
        if let Some(id) = item["id"].as_str() {
            keys.push(format!("id:{}", hex::encode(Sha256::digest(id.as_bytes()))));
        }
        let opaque = item["encrypted_content"]
            .as_str()
            .into_iter()
            .chain(
                item["encrypted_function_args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            )
            .chain(
                item["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["encrypted_content"].as_str()),
            );
        for content in opaque.filter(|s| !s.is_empty()) {
            keys.push(format!(
                "encrypted:{}",
                hex::encode(Sha256::digest(content.as_bytes()))
            ));
        }
        keys
    }
    fn history_keys(item: &Value) -> impl Iterator<Item = String> {
        // Full text can be replayed independently. References and tool items
        // still require their original route and callback ownership checks.
        let reference = !matches!(item["type"].as_str(), None | Some("message" | "reasoning"));
        Self::item_keys(item)
            .into_iter()
            .filter(move |key| reference || key.starts_with("encrypted:"))
    }
    fn exchange_keys<'a>(
        history: &'a [Value],
        response: &'a Value,
    ) -> impl Iterator<Item = String> + 'a {
        // Replayed input proves acceptance of opaque content, not ownership of
        // arbitrary client-supplied message IDs. Output IDs route references and tools.
        history
            .iter()
            .flat_map(Self::item_keys)
            .filter(|key| key.starts_with("encrypted:"))
            .chain(
                response["output"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(Self::item_keys),
            )
    }
    pub(super) fn binding_for_history(
        &self,
        access: &ClientAccess,
        model: &str,
        history: &[Value],
    ) -> Result<Option<Binding>> {
        if let Some(disk) = &self.disk {
            return disk.binding(access, model, history, None);
        }
        let mut affinities = self.affinities.lock().unwrap();
        affinities.retain(|_, (_, expiry)| *expiry > Instant::now());
        let mut found = None;
        for item in history {
            for key in Self::history_keys(item) {
                if let Some((binding, _)) = affinities.get(&(access.key_id, model.into(), key)) {
                    if found.as_ref().is_some_and(|prior| prior != binding) {
                        return Err(Error::new(
                            "RESPONSE_CHAIN_MISMATCH",
                            "历史混合了不同账户或路由的响应。",
                        ));
                    }
                    found = Some(binding.clone());
                }
            }
        }
        Ok(found)
    }
    #[cfg(test)]
    pub(super) fn remember_binding(
        &self,
        access: &ClientAccess,
        response: &Value,
        binding: &Binding,
    ) -> Result<()> {
        self.remember_exchange(access, &[], response, binding)
    }
    pub(super) fn remember_exchange(
        &self,
        access: &ClientAccess,
        history: &[Value],
        response: &Value,
        binding: &Binding,
    ) -> Result<()> {
        if let Some(disk) = &self.disk {
            return match disk.remember(access, history, response, binding) {
                Err(error) if error.code == "RESPONSE_STORAGE_FULL" => Ok(()),
                result => result,
            };
        }
        let mut affinities = self.affinities.lock().unwrap();
        affinities.retain(|_, (_, expiry)| *expiry > Instant::now());
        let keys: Vec<_> = Self::exchange_keys(history, response).collect();
        for key in &keys {
            if key.starts_with("encrypted:")
                && affinities
                    .get(&(access.key_id, binding.model.clone(), key.clone()))
                    .is_some_and(|(old, _)| old != binding)
            {
                return Err(Error::new(
                    "RESPONSE_CHAIN_MISMATCH",
                    "历史混合了不同账户或路由的响应。",
                ));
            }
        }
        for key in keys {
            if affinities.len() >= 4096 {
                if let Some(key) = affinities
                    .iter()
                    .min_by_key(|(_, (_, expiry))| *expiry)
                    .map(|(key, _)| key.clone())
                {
                    affinities.remove(&key);
                }
            }
            affinities.insert(
                (access.key_id, binding.model.clone(), key),
                (binding.clone(), Instant::now() + TTL),
            );
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Value {
        if let Some(disk) = &self.disk {
            return disk.snapshot().unwrap_or_else(
                |e| serde_json::json!({"persistent":true,"bodiesStored":false,"error":e.code}),
            );
        }
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, v| v.expires > Instant::now());
        serde_json::json!({"count":entries.len(),"bytes":entries.values().map(|v|v.bytes).sum::<usize>(),"maxCount":128,"maxBytes":MAX_BYTES,"maxEntryBytes":crate::media::BODY_LIMIT*3,"ttlSeconds":TTL.as_secs(),"persistent":false})
    }

    fn read<T>(&self, access: &ClientAccess, id: &str, f: impl FnOnce(&Stored) -> T) -> Result<T> {
        if self.disk.is_some() {
            return Err(missing());
        }
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, v| v.expires > Instant::now());
        let entry = entries
            .get(id)
            .filter(|v| {
                v.owner == access.key_id
                    && access.allows(v.response["model"].as_str().unwrap_or(""))
            })
            .ok_or_else(missing)?;
        Ok(f(entry))
    }
    pub fn history(&self, access: &ClientAccess, id: &str) -> Result<Vec<Value>> {
        self.read(access, id, |e| e.history.clone())
    }
    pub(super) fn continuation(&self, access: &ClientAccess, id: &str) -> Result<Continuation> {
        self.read(access, id, |e| Continuation {
            history: e.history.clone(),
            binding: e.binding.clone(),
        })
    }
    pub(super) fn defaults(&self, access: &ClientAccess, id: &str) -> Result<Option<Value>> {
        self.read(access, id, |e| e.defaults.clone())
    }
    pub fn get(&self, access: &ClientAccess, id: &str) -> Result<Value> {
        self.read(access, id, |e| e.response.clone())
    }
    pub fn delete(&self, access: &ClientAccess, id: &str) -> Result<()> {
        if self.disk.is_some() {
            return Err(missing());
        }
        let mut entries = self.entries.lock().unwrap();
        if !entries.get(id).is_some_and(|e| {
            e.owner == access.key_id
                && e.expires > Instant::now()
                && access.allows(e.response["model"].as_str().unwrap_or(""))
        }) {
            return Err(missing());
        }
        entries.remove(id);
        drop(entries);
        // Deleting a stored response also invalidates its copy on every live
        // connection. store=false responses remain connection-only and are not
        // exposed through the HTTP delete endpoint.
        let mut connections = self.connections.lock().unwrap();
        connections.retain(|cache| {
            if let Some(cache) = cache.upgrade() {
                let mut entries = cache.entries.lock().unwrap();
                if entries
                    .get(id)
                    .is_some_and(|entry| entry.owner == access.key_id)
                {
                    entries.remove(id);
                }
                true
            } else {
                false
            }
        });
        Ok(())
    }
    pub fn save(&self, access: &ClientAccess, history: Vec<Value>, response: &Value) -> Result<()> {
        self.save_bound(access, history, response, None, None)
    }
    pub(super) fn save_bound(
        &self,
        access: &ClientAccess,
        mut history: Vec<Value>,
        response: &Value,
        binding: Option<Binding>,
        defaults: Option<Value>,
    ) -> Result<()> {
        if self.disk.is_some() {
            return Err(Error::new(
                "INVALID_INPUT",
                "聊天正文只允许保存在连接内存缓存中。",
            ));
        }
        history.extend(response["output"].as_array().cloned().unwrap_or_default());
        let bytes = serde_json::to_vec(&history)?.len()
            + serde_json::to_vec(response)?.len()
            + serde_json::to_vec(&defaults)?.len();
        if bytes > crate::media::BODY_LIMIT * 3 {
            return Ok(());
        }
        let stored = Stored {
            owner: access.key_id,
            history,
            response: response.clone(),
            expires: Instant::now() + TTL,
            bytes,
            binding,
            defaults,
        };
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, v| v.expires > Instant::now());
        let id = response["id"].as_str().ok_or_else(missing)?;
        let mut protected = self.protected.lock().unwrap();
        let mut next_protected = protected.clone();
        if let Some(previous) = response["previous_response_id"].as_str() {
            for pinned in next_protected
                .values_mut()
                .filter(|p| p.as_str() == previous)
            {
                *pinned = id.into();
            }
        }
        let mut count = entries.len() - usize::from(entries.contains_key(id));
        let mut total = entries
            .iter()
            .filter(|(key, _)| key.as_str() != id)
            .map(|(_, e)| e.bytes)
            .sum::<usize>()
            + bytes;
        let mut candidates: Vec<_> = entries
            .iter()
            .filter(|(key, _)| key.as_str() != id && !next_protected.values().any(|p| p == *key))
            .map(|(key, entry)| (key.clone(), entry.bytes, entry.expires))
            .collect();
        candidates.sort_by_key(|(_, _, expiry)| *expiry);
        let mut victims = Vec::new();
        let mut candidates = candidates.into_iter();
        while count >= 128 || total > MAX_BYTES {
            let Some((old, old_bytes, _)) = candidates.next() else {
                return Ok(());
            };
            victims.push(old);
            count -= 1;
            total -= old_bytes;
        }
        for old in victims {
            entries.remove(&old);
        }
        *protected = next_protected;
        entries.insert(id.into(), stored);
        Ok(())
    }
}
