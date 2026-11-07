//! Adapter support is separate from account/provider capability verification.
use serde_json::{Value, json};
pub(crate) async fn report() -> Value {
    let installation = crate::installation::read().await.ok();
    let verified = installation.as_ref().is_some_and(|i| {
        i.app_server_sha256 == "1103e9e09c71fa2cd0afd6d8f3b9a2d63174fe8efd7cb5bd09f1cb14ef58a098"
    });
    let capabilities = [
        (
            "standaloneImages",
            "adapted",
            "POST images/generations and images/edits (JSON/multipart, non-streaming); relay HTTP with native account auth, proxy, admission and failure records",
        ),
        (
            "responsesHttp",
            "adapted",
            "JSON/SSE ingress; native owns model transport",
        ),
        (
            "responsesWebsocket",
            "adapted",
            "Connection-local cache, FIFO lanes, response.interrupt and text steering",
        ),
        (
            "function",
            "native",
            "Dynamic tools are yielded to the caller",
        ),
        (
            "namespace",
            "adapted",
            "Reserved native names use reversible catalog aliases",
        ),
        (
            "custom",
            "adapted",
            "Function-envelope registration; accepts native custom calls and wrapped function results, preserves freeform input; grammar is not constrained decoding",
        ),
        (
            "clientToolSearch",
            "adapted",
            "Function envelope; search results extend the dynamic catalog",
        ),
        (
            "additionalTools",
            "native",
            "Ordered thread/inject_items history",
        ),
        (
            "deferredTools",
            "adapted",
            "deferLoading with reversible namespace adaptation for bare deferred tools",
        ),
        (
            "configurationUpdate",
            "native",
            "History retained; latest reasoning effort applied to turn/start",
        ),
        (
            "encryptedHistory",
            "adapted",
            "Opaque content replayed from the client; durable key/account/provider/model fingerprints with 30-day sliding retention; no response bodies persisted",
        ),
        (
            "compaction",
            "adapted",
            "thread/compact/start with owned native rollout; provider must supply remote compaction or plaintext summary",
        ),
        (
            "structuredOutput",
            "native",
            "JSON Schema maps to outputSchema",
        ),
        (
            "jsonObject",
            "adapted",
            "Instructions and validation before emitting final text",
        ),
        (
            "imageInput",
            "native",
            "Inline images; automatic unless explicitly disabled on the account",
        ),
        (
            "responsesInterrupt",
            "adapted",
            "Native turn/interrupt, per-response acknowledgements and truthful incomplete/interrupted terminal",
        ),
        (
            "codeModeNotifications",
            "adapted",
            "Named functions.exec supplemental results preserve order without closing pending calls",
        ),
        (
            "responsePersistence",
            "unsupported",
            "store=true is ignored and responses report store=false; connection-local WebSocket continuation only; HTTP retrieval and deletion return 404",
        ),
        (
            "reasoningStreaming",
            "native",
            "Native summary deltas with final item reconciliation and opaque metadata retention",
        ),
        (
            "agentMessage",
            "adapted",
            "Schema-defined agent_message data retained; no relay-side agent execution",
        ),
        (
            "clientMetadata",
            "adapted",
            "Only unprotected x-codex-turn-metadata strings map to responsesapiClientMetadata",
        ),
        (
            "cyberAccessProgram",
            "native",
            "RPC program selection; native sends only for its OpenAI provider, subject to authentication and native API-key feature policy",
        ),
        (
            "toolArgumentStreaming",
            "adapted",
            "Installed RPC exposes completed raw calls; no fabricated incremental tool arguments",
        ),
        (
            "forcedToolChoice",
            "unsupported",
            "Existing requests fall back to auto; parameter handling is recorded",
        ),
        (
            "parallelToolControl",
            "unsupported",
            "No reliable caller-controlled native mapping",
        ),
        (
            "functionStrict",
            "unsupported",
            "Dynamic function registration uses non-strict schemas",
        ),
        (
            "maxOutputTokens",
            "unsupported",
            "Ignored with diagnostics; no truncation or synthetic token limits",
        ),
        (
            "hostedToolOptions",
            "adapted",
            "Search context size, domain filters, approximate location and external web access map to isolated thread settings; account location policy takes precedence; image-generation options remain unsupported",
        ),
        (
            "ultrafast",
            "adapted",
            "Explicit ultrafast requires the active native worker model catalog to advertise it; actual upstream access remains account-dependent",
        ),
        (
            "desktopGuiAcceptance",
            "unverified",
            "Synthetic SDK and official binary tests do not establish full desktop GUI acceptance",
        ),
        (
            "serverToolSearch",
            "unsupported",
            "Native dynamic tool RPC exposes no server-search registration",
        ),
        (
            "customGrammarEnforcement",
            "unsupported",
            "No grammar field in DynamicToolSpec",
        ),
        (
            "upstreamIncompleteDetails",
            "unsupported",
            "Installed raw completion RPC exposes usage and responseId only; native errors remain failures",
        ),
        (
            "realtimeVoice",
            "unsupported",
            "Outside Responses coding scope",
        ),
        (
            "chatCompletions",
            "unsupported",
            "No Chat Completions route",
        ),
    ];
    json!({"nativeVersion":installation.as_ref().and_then(|i|i.app_server_version.as_deref()),
        "nativeSha256":installation.as_ref().map(|i|i.app_server_sha256.as_str()),
        "sourceRevision":"adca78ddae79568279d0731dfdebef1df16f3cde",
        "limits":{"bodyBytes":crate::media::BODY_LIMIT,"outputBytes":crate::media::BODY_LIMIT,"historyItems":crate::media::HISTORY_COUNT,"tools":crate::media::TOOL_COUNT,"imageBytes":crate::media::IMAGE_LIMIT,"imagesBytes":crate::media::MEDIA_LIMIT,"images":crate::media::IMAGE_COUNT,"nativeFrameBytes":crate::rpc::MAX_BYTES},
        "verifiedInstallation":verified,"verificationScope":"Synthetic model integration; real provider and desktop GUI acceptance are separate",
        "capabilities":capabilities.into_iter().map(|(name,status,note)|json!({"name":name,"status":if verified || status=="unsupported"{status}else{"unverified"},"implementation":status,"note":note})).collect::<Vec<_>>()})
}
