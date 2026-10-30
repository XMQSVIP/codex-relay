CREATE TABLE settings (id INTEGER PRIMARY KEY CHECK(id=1), document TEXT NOT NULL);
CREATE TABLE requests (
    id TEXT PRIMARY KEY, started_at INTEGER NOT NULL, finished_at INTEGER,
    kind TEXT NOT NULL, action TEXT NOT NULL, account_id TEXT, account_name TEXT,
    group_id TEXT, group_name TEXT, status TEXT NOT NULL, duration_ms INTEGER,
    error_code TEXT, wait_ms INTEGER, input_tokens INTEGER, output_tokens INTEGER,
    cached_input_tokens INTEGER, key_id TEXT, native_thread_id TEXT, native_turn_id TEXT,
    model TEXT, response_model TEXT, response_service_tier TEXT, model_outcome TEXT, failure_stage TEXT,
    cleanup_error_code TEXT, first_delta_ms INTEGER, delivery_status TEXT, key_name TEXT,
    endpoint TEXT, client_transport TEXT, upstream_transport TEXT,
    reasoning_effort TEXT, user_agent TEXT, streaming INTEGER, parameter_handling TEXT, scheduling TEXT, subagent_kind TEXT, service_tier TEXT,
    request_content TEXT, response_content TEXT
);
CREATE INDEX request_time ON requests(started_at);
CREATE INDEX request_group_time ON requests(group_id,started_at);
CREATE INDEX request_account_time ON requests(account_id,started_at);
CREATE INDEX request_key_time ON requests(key_id,started_at);
CREATE TABLE api_keys (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, prefix TEXT NOT NULL,
    secret_hash BLOB NOT NULL UNIQUE, group_id TEXT NOT NULL, models TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('active','paused','revoked')),
    created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
    last_authenticated_at INTEGER, revision INTEGER NOT NULL,
    max_concurrency INTEGER NOT NULL DEFAULT 0 CHECK(max_concurrency BETWEEN 0 AND 256),
    secret TEXT,
    fast_mode INTEGER NOT NULL DEFAULT 0 CHECK(fast_mode IN (0,1))
);
CREATE INDEX api_keys_group ON api_keys(group_id);
CREATE TABLE native_capabilities(account_id TEXT PRIMARY KEY, document TEXT NOT NULL);
CREATE TABLE account_quotas(account_id TEXT PRIMARY KEY, document TEXT NOT NULL);
CREATE TABLE response_affinities (
    owner TEXT NOT NULL, group_id TEXT NOT NULL, model TEXT NOT NULL, fingerprint TEXT NOT NULL,
    account TEXT NOT NULL, binding TEXT NOT NULL, expires INTEGER NOT NULL,
    PRIMARY KEY(owner,group_id,model,fingerprint)
);
CREATE INDEX response_affinity_expiry ON response_affinities(expires);
CREATE TRIGGER response_key_delete AFTER DELETE ON api_keys BEGIN
    DELETE FROM response_affinities WHERE owner=OLD.id;
END;
CREATE TRIGGER response_key_revoke AFTER UPDATE OF state,group_id ON api_keys
    WHEN NEW.state='revoked' OR NEW.group_id!=OLD.group_id BEGIN
    DELETE FROM response_affinities WHERE owner=OLD.id;
END;
CREATE TABLE account_reset_automation (account_id TEXT PRIMARY KEY, document TEXT NOT NULL);
PRAGMA user_version=7;
