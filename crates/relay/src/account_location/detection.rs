use super::{Config, Location, Source};
use crate::{
    app_server_proxy::{ProxyConfig, ResolvedProxy},
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

const TTL: u64 = 24 * 60 * 60 * 1000;
const RETRY: u64 = 5 * 60 * 1000;
// Old probes may have observed a different route under split proxy rules.
const CACHE_PREFIX: &str = "chatgpt-exit-v2:";
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(3);
const PREVIEW_TTL: u64 = 5 * 60 * 1000;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Preview {
    pub id: Uuid,
    pub location: Location,
    pub detected_at: u64,
    #[serde(skip)]
    key: String,
    #[serde(skip)]
    generation: u64,
}

#[derive(Clone, Copy)]
enum Provider {
    Ipinfo,
    IpWhois,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Entry {
    key: String,
    location: Option<Location>,
    detected_at: u64,
    attempted_at: u64,
    error: Option<String>,
    #[serde(skip)]
    generation: u64,
    #[serde(skip)]
    busy: bool,
}
struct State {
    entries: HashMap<Uuid, Entry>,
    previews: HashMap<Uuid, Preview>,
    db: rusqlite::Connection,
}
#[derive(Default)]
struct Shared {
    at: u64,
    result: Option<Result<Location>>,
}
pub(crate) struct Manager {
    state: Mutex<State>,
    probes: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Shared>>>>,
    endpoints: Vec<(Provider, String)>,
    trace_endpoint: Option<String>,
}
impl Manager {
    pub(crate) fn open(root: &Path) -> Result<Arc<Self>> {
        let db = rusqlite::Connection::open(root.join("location-cache.sqlite3"))
            .map_err(|_| failure())?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS locations (account TEXT PRIMARY KEY, document TEXT NOT NULL)").map_err(|_| failure())?;
        let mut entries = HashMap::new();
        {
            let mut stmt = db
                .prepare("SELECT account,document FROM locations")
                .map_err(|_| failure())?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|_| failure())?;
            for row in rows.flatten() {
                if let (Ok(id), Ok(entry)) = (
                    Uuid::parse_str(&row.0),
                    serde_json::from_str::<Entry>(&row.1),
                ) {
                    if entry.key.starts_with(CACHE_PREFIX)
                        && entry.location.as_ref().is_none_or(|l| l.validate().is_ok())
                    {
                        entries.insert(id, entry);
                    }
                }
            }
        }
        Ok(Arc::new(Self {
            state: Mutex::new(State { entries, db, previews: HashMap::new() }),
            probes: Mutex::new(HashMap::new()),
            trace_endpoint: Some("https://chatgpt.com/cdn-cgi/trace".into()),
            endpoints: vec![
                (Provider::Ipinfo, "https://ipinfo.io/{ip}/json".into()),
                (
                    Provider::IpWhois,
                    "https://ipwho.is/{ip}?fields=ip,success,message,country_code,region,city,timezone.id"
                        .into(),
                ),
            ],
        }))
    }
    pub(crate) fn token(&self, id: Uuid) -> u64 {
        self.state
            .lock()
            .unwrap()
            .entries
            .entry(id)
            .or_default()
            .generation
    }
    pub(crate) fn invalidate(&self, id: Uuid, clear: bool) {
        let mut state = self.state.lock().unwrap();
        state.previews.remove(&id);
        let entry = state.entries.entry(id).or_default();
        let generation = entry.generation.wrapping_add(1);
        if clear {
            *entry = Entry::default();
        }
        entry.generation = generation;
        entry.busy = false;
        persist(&state, id);
    }
    /// A preview never changes the effective location, configuration, or running worker.
    pub(crate) async fn preview(
        &self,
        id: Uuid,
        generation: u64,
        proxy: ProxyConfig,
    ) -> Result<Preview> {
        let (key, location) = tokio::time::timeout(Duration::from_secs(8), async {
            let resolved = proxy.resolved().await?;
            let key = format!("{CACHE_PREFIX}{}", resolved.fingerprint());
            Ok::<_, Error>((key, self.fetch(&resolved).await?))
        })
        .await
        .map_err(|_| failure())??;
        let preview = Preview {
            id: Uuid::new_v4(),
            location,
            detected_at: crate::now_ms(),
            key,
            generation,
        };
        let mut state = self.state.lock().unwrap();
        if state.entries.get(&id).map_or(0, |e| e.generation) != generation {
            return Err(stale_preview());
        }
        state
            .previews
            .retain(|_, p| crate::now_ms().saturating_sub(p.detected_at) < PREVIEW_TTL);
        state.previews.insert(id, preview.clone());
        Ok(preview)
    }
    pub(crate) async fn preview_for_save(
        &self,
        id: Uuid,
        preview_id: Uuid,
        proxy: ProxyConfig,
    ) -> Result<Preview> {
        let preview = {
            let state = self.state.lock().unwrap();
            state
                .previews
                .get(&id)
                .filter(|p| {
                    p.id == preview_id
                        && crate::now_ms().saturating_sub(p.detected_at) < PREVIEW_TTL
                        && state.entries.get(&id).map_or(0, |e| e.generation) == p.generation
                })
                .cloned()
                .ok_or_else(stale_preview)?
        };
        let resolved = tokio::time::timeout(Duration::from_secs(8), proxy.resolved())
            .await
            .map_err(|_| stale_preview())??;
        if preview.key != format!("{CACHE_PREFIX}{}", resolved.fingerprint()) {
            return Err(stale_preview());
        }
        Ok(preview)
    }
    pub(crate) fn accept_preview(&self, id: Uuid, preview: Preview) {
        let mut state = self.state.lock().unwrap();
        // An older automatic probe must not overwrite the result just confirmed.
        let generation = state
            .entries
            .get(&id)
            .map_or(0, |e| e.generation)
            .wrapping_add(1);
        state.entries.insert(
            id,
            Entry {
                key: preview.key,
                location: Some(preview.location),
                detected_at: preview.detected_at,
                attempted_at: preview.detected_at,
                generation,
                ..Default::default()
            },
        );
        state.previews.remove(&id);
        persist(&state, id);
    }
    pub(crate) fn view(&self, id: Uuid, config: &Config) -> Value {
        if !config.enabled {
            return json!({"state":"disabled","effective":null});
        }
        if config.source == Source::Custom {
            return json!({"state":"applied","effective":config.custom});
        }
        let state = self.state.lock().unwrap();
        let entry = state.entries.get(&id).cloned().unwrap_or_default();
        let status = if entry.busy {
            "detecting"
        } else if entry.error.is_some() {
            if entry.location.is_some() {
                "cached"
            } else {
                "failed"
            }
        } else if entry.location.is_some() {
            "applied"
        } else {
            "pending"
        };
        json!({"state":status,"effective":entry.location,"detectedAt":(entry.detected_at>0).then_some(entry.detected_at),"error":entry.error})
    }
    pub(crate) async fn resolve(
        self: &Arc<Self>,
        id: Uuid,
        generation: u64,
        config: Config,
        proxy: ProxyConfig,
        force: bool,
    ) -> Option<Location> {
        if !config.enabled {
            return None;
        }
        if config.source == Source::Custom {
            return Some(config.custom);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let resolved = match tokio::time::timeout_at(deadline, proxy.resolved()).await {
            Ok(Ok(value)) => value,
            _ => {
                self.failed(id, generation, "无法解析账号代理，保留原请求行为。".into());
                return None;
            }
        };
        let key = format!("{CACHE_PREFIX}{}", resolved.fingerprint());
        let now = crate::now_ms();
        let (cached, background) = {
            let mut state = self.state.lock().unwrap();
            let entry = state.entries.entry(id).or_default();
            if entry.generation != generation {
                return None;
            }
            if entry.key != key {
                *entry = Entry {
                    key: key.clone(),
                    generation,
                    ..Default::default()
                };
            }
            if entry.busy
                || (!force
                    && ((entry.error.is_some() && now.saturating_sub(entry.attempted_at) < RETRY)
                        || (entry.location.is_some()
                            && now.saturating_sub(entry.detected_at) < TTL)))
            {
                return entry.location.clone();
            }
            entry.busy = true;
            entry.attempted_at = now;
            (entry.location.clone(), !force && entry.location.is_some())
        };
        // A cancelled caller must not leave an entry permanently marked busy.
        // The bounded probe owns its completion even if the requesting turn exits.
        let owner = self.clone();
        let task =
            tokio::spawn(
                async move { owner.detect(id, generation, key, resolved, deadline).await },
            );
        if background {
            cached
        } else {
            task.await.ok().flatten()
        }
    }
    async fn detect(
        &self,
        id: Uuid,
        generation: u64,
        key: String,
        proxy: ResolvedProxy,
        deadline: tokio::time::Instant,
    ) -> Option<Location> {
        let probe = {
            let mut probes = self.probes.lock().unwrap();
            // Completed slots may be evicted; active probes keep their Arc.
            if probes.len() > 256 {
                probes.retain(|_, slot| Arc::strong_count(slot) > 1);
            }
            probes.entry(key.clone()).or_default().clone()
        };
        let result = tokio::time::timeout_at(deadline, async {
            let mut shared = probe.lock().await;
            let lifetime = if shared.result.as_ref().is_some_and(Result::is_err) {
                RETRY
            } else {
                10_000
            };
            if shared.result.is_none() || crate::now_ms().saturating_sub(shared.at) >= lifetime {
                shared.result = Some(self.fetch(&proxy).await);
                shared.at = crate::now_ms();
            }
            shared.result.clone().unwrap()
        })
        .await
        .unwrap_or_else(|_| {
            Err(Error::new(
                "LOCATION_TIMEOUT",
                "出口位置检测超时，可稍后重试或使用自定义位置。",
            ))
        });
        let mut state = self.state.lock().unwrap();
        let entry = state.entries.get_mut(&id)?;
        if entry.generation != generation || entry.key != key {
            return None;
        }
        entry.busy = false;
        match result {
            Ok(location) => {
                entry.location = Some(location);
                entry.detected_at = crate::now_ms();
                entry.error = None;
            }
            Err(error) => entry.error = Some(error.message),
        }
        let effective = entry.location.clone();
        persist(&state, id);
        effective
    }
    fn failed(&self, id: Uuid, generation: u64, error: String) {
        let mut state = self.state.lock().unwrap();
        let entry = state.entries.entry(id).or_default();
        if entry.generation == generation {
            entry.error = Some(error);
            entry.attempted_at = crate::now_ms();
            entry.busy = false;
            // With an unresolved route, do not claim a cached exit still matches.
            entry.location = None;
            persist(&state, id);
        }
    }
    async fn fetch(&self, proxy: &ResolvedProxy) -> Result<Location> {
        let client = proxy.client()?;
        let exit_ip = if let Some(endpoint) = &self.trace_endpoint {
            match fetch_bytes(&client, endpoint, Duration::from_secs(2)).await {
                Ok(bytes) => parse_trace(&bytes).ok(),
                Err(_) => None,
            }
        } else {
            None
        };
        for (provider, endpoint) in &self.endpoints {
            // Never use IPinfo's own observed address when the restricted-site probe failed.
            if self.trace_endpoint.is_some()
                && exit_ip.is_none()
                && matches!(provider, Provider::Ipinfo)
            {
                continue;
            }
            let endpoint = endpoint.replace(
                "{ip}",
                &exit_ip.map(|ip| ip.to_string()).unwrap_or_default(),
            );
            if let Ok(location) = fetch_endpoint(&client, *provider, &endpoint, exit_ip).await {
                return Ok(location);
            }
        }
        Err(failure())
    }
}

async fn fetch_endpoint(
    client: &reqwest::Client,
    provider: Provider,
    endpoint: &str,
    expected_ip: Option<IpAddr>,
) -> Result<Location> {
    let bytes = fetch_bytes(client, endpoint, PROVIDER_TIMEOUT).await?;
    if let Some(expected) = expected_ip {
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| failure())?;
        if value["ip"].as_str().and_then(|s| s.parse::<IpAddr>().ok()) != Some(expected) {
            return Err(failure());
        }
    }
    parse(provider, &bytes)
}

async fn fetch_bytes(
    client: &reqwest::Client,
    endpoint: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut response = client
        .get(endpoint)
        // Leave time for the fallback within the overall eight-second budget.
        .timeout(timeout)
        .send()
        .await
        .map_err(|_| failure())?;
    if !response.status().is_success() {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| failure())? {
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err(failure());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn parse_trace(bytes: &[u8]) -> Result<IpAddr> {
    let text = std::str::from_utf8(bytes).map_err(|_| failure())?;
    let ips = text
        .lines()
        .filter_map(|line| line.strip_prefix("ip="))
        .collect::<Vec<_>>();
    if ips.len() != 1 {
        return Err(failure());
    }
    let ip: IpAddr = ips[0].trim().parse().map_err(|_| failure())?;
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return Err(failure());
    }
    Ok(ip)
}
fn stale_preview() -> Error {
    Error::new(
        "LOCATION_PREVIEW_EXPIRED",
        "检测预览已失效，请重新检测后再保存。",
    )
}
fn persist(state: &State, id: Uuid) {
    let Some(entry) = state.entries.get(&id) else {
        return;
    };
    let Ok(document) = serde_json::to_string(entry) else {
        return;
    };
    if state
        .db
        .execute(
            "INSERT OR REPLACE INTO locations VALUES (?1,?2)",
            [id.to_string(), document],
        )
        .is_err()
    {
        eprintln!("location_cache_write_failed");
    }
}
fn failure() -> Error {
    Error::new(
        "LOCATION_DETECTION",
        "出口位置检测失败，可稍后重试或使用自定义位置。",
    )
}
fn parse(provider: Provider, bytes: &[u8]) -> Result<Location> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| failure())?;
    let text = |key: &str| value[key].as_str().unwrap_or("").to_owned();
    let (timezone, country) = match provider {
        Provider::Ipinfo => {
            if !value["error"].is_null() || value["bogon"] == true {
                return Err(failure());
            }
            (text("timezone"), text("country"))
        }
        Provider::IpWhois => {
            if value["success"] != true {
                return Err(failure());
            }
            (
                value["timezone"]["id"].as_str().unwrap_or("").into(),
                text("country_code"),
            )
        }
    };
    let location = Location {
        timezone,
        country,
        region: text("region"),
        city: text("city"),
    }
    .normalized();
    location.validate().map_err(|_| failure())?;
    Ok(location)
}

#[cfg(test)]
mod tests;
