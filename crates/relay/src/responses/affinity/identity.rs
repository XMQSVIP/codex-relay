//! Caller identity is local routing/isolation data, never an upstream identity override.
use axum::http::HeaderMap;
use serde_json::Value;

type Digest = [u8; 32];

#[derive(Clone, Copy, Debug, Default)]
struct Field {
    value: Option<Digest>,
    conflict: bool,
}
impl Field {
    fn add(&mut self, value: &str) {
        // Check controls before trimming: a newline is not harmless whitespace.
        if value.chars().any(char::is_control) {
            return;
        }
        let value = value.trim();
        if value.is_empty() || value.len() > 512 {
            return;
        }
        let digest = crate::profiles::warm::digest(&value).expect("string digest");
        self.conflict |= self.value.is_some_and(|old| old != digest);
        self.value = Some(digest);
    }
    fn present(self) -> bool {
        self.value.is_some() || self.conflict
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Fields {
    session: Field,
    thread: Field,
    parent: Field,
    child: bool,
}
impl Fields {
    fn merge(&mut self, value: &Value) {
        for (field, names) in [
            (&mut self.session, &["session_id", "conversation_id"][..]),
            (&mut self.thread, &["thread_id"][..]),
            (
                &mut self.parent,
                &[
                    "parent_thread_id",
                    "forked_from_thread_id",
                    "forked_from_id",
                ][..],
            ),
        ] {
            for name in names {
                if let Some(value) = value[*name].as_str() {
                    field.add(value);
                }
            }
        }
        self.child |= value["subagent_kind"]
            .as_str()
            .is_some_and(|v| !v.trim().is_empty());
    }
    fn metadata(&mut self, raw: &str) {
        if raw.len() <= 16 * 1024
            && let Ok(value) = serde_json::from_str::<Value>(raw)
        {
            self.merge(&value);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Headers {
    metadata: Fields,
    plain: Fields,
}
impl Headers {
    pub(crate) fn parse(headers: &HeaderMap) -> Self {
        let mut result = Self::default();
        for raw in headers.get_all("x-codex-turn-metadata") {
            if let Ok(raw) = raw.to_str() {
                result.metadata.metadata(raw);
            }
        }
        for (field, names) in [
            (
                &mut result.plain.session,
                &["session-id", "session_id", "conversation_id"][..],
            ),
            (&mut result.plain.thread, &["thread-id", "thread_id"][..]),
            (&mut result.plain.parent, &["x-codex-parent-thread-id"][..]),
        ] {
            for name in names {
                for raw in headers.get_all(*name) {
                    if let Ok(raw) = raw.to_str() {
                        field.add(raw);
                    }
                }
            }
        }
        result.plain.child = crate::subagent::from_headers(headers).is_some();
        result
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Body {
    explicit: Fields,
    metadata: Fields,
    plain: Fields,
}
impl Body {
    pub(crate) fn parse(body: &Value) -> Self {
        let mut result = Self::default();
        result.explicit.merge(&body["client_metadata"]);
        result.plain.merge(body);
        for object in [body, &body["client_metadata"]] {
            for key in ["x-codex-turn-metadata", "turn_metadata", "turnMetadata"] {
                if let Some(raw) = object[key].as_str() {
                    result.metadata.metadata(raw);
                }
            }
        }
        result.plain.child |= crate::subagent::from_body(body).is_some();
        result
    }

    pub(crate) fn resolve(self, headers: Headers) -> Identity {
        let levels = [
            self.explicit,
            self.metadata,
            self.plain,
            headers.metadata,
            headers.plain,
        ];
        // A frame naming its own session must not inherit a connection's thread.
        // Still merge missing parent fields from connection defaults separately.
        let request_owns_identity = levels[..3]
            .iter()
            .any(|f| f.thread.present() || f.session.present());
        let select = |get: fn(Fields) -> Field, count: usize| {
            levels[..count]
                .iter()
                .copied()
                .map(get)
                .find(|f| f.present())
                .unwrap_or_default()
        };
        let count = if request_owns_identity { 3 } else { 5 };
        let session = select(|f| f.session, count);
        let thread = select(|f| f.thread, count);
        let parent = select(|f| f.parent, 5);
        let conflict = session.conflict || thread.conflict || parent.conflict;
        let own = thread.value.or(session.value);
        let child = parent.present() || levels.iter().any(|f| f.child);
        let request_child = levels[..3].iter().any(|f| f.child || f.parent.present());
        // A shared cache-routing header alone cannot identify an anonymous child.
        let logical_session = levels[..4].iter().any(|f| f.session.present());
        let distinct_child = own.is_some()
            && own != parent.value
            && (!request_child || request_owns_identity)
            && (thread.value.is_some() || (logical_session && parent.value.is_some()));
        Identity {
            own: if conflict { None } else { own },
            parent: if conflict || !distinct_child {
                None
            } else {
                parent.value
            },
            reusable: !conflict && (!child || distinct_child),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Identity {
    pub own: Option<Digest>,
    pub parent: Option<Digest>,
    pub reusable: bool,
}

#[cfg(test)]
mod tests;
