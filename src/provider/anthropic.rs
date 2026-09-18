// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The native Anthropic Messages API client implementation.
//!
//! Talks the `POST {base}/messages` contract — `x-api-key` +
//! `anthropic-version: 2023-06-01` headers (plus an optional
//! `anthropic-workspace-id` workspace-attribution header when the endpoint
//! configures `workspace_id` — never sent empty), a single hoisted `system` field,
//! content blocks (text/image/tool_use/tool_result), required `max_tokens`,
//! and SSE events (`message_start`, `content_block_*`, `message_delta`,
//! `message_stop`) — so any Anthropic-compatible provider (Anthropic itself,
//! OpenRouter, gateways) works through the same [`LlmClient`] abstraction as
//! the OpenAI-compatible path. The SSE stream is parsed from raw bytes (via
//! reqwest) into [`LlmEvent`]s, mirroring `openai.rs`.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::error::{Error, Result};
use crate::provider::sse_util::{
    error_chain, finish_reason_label, header_str, parse_data_url, provider_error,
    truncate_raw_stream, SseOutcome,
};
use crate::provider::stream::{
    bound_repetition_buffer, detect_repetition, REPETITION_BUFFER_CAP, REPETITION_THRESHOLD,
    REPETITION_WINDOW,
};
use crate::provider::trace::{LlmRequestLog, LlmUsage};
use crate::provider::{
    Capabilities, ContentPart, FinishReason, LlmClient, LlmEvent, Message, MessageContent,
    ProviderKind, Role, StallTracker, ToolChoice, ToolSchema,
};

/// Configuration for building an Anthropic Messages API client.
#[derive(Debug, Clone)]
pub struct AnthropicClientConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// The endpoint/provider name from `endpoints.toml` (e.g. "claude").
    /// Recorded on trace records so the Trace tab can attribute each request
    /// to its provider.
    pub provider: String,
    /// Optional max-context override from the endpoint config.
    pub max_context: Option<usize>,
    /// Optional max-output-tokens override from the endpoint config.
    pub max_output_tokens: Option<usize>,
    /// Whether the model at this endpoint accepts image inputs (multimodal).
    /// Flows into the capability set so the agent loop can decide whether to
    /// send image blocks or fall back to a separate vision model.
    pub multimodal: bool,
    /// Optional Anthropic workspace id (e.g. `ws_...`). When set, every
    /// request sends the `anthropic-workspace-id` HTTP header so usage is
    /// attributed to that workspace. Empty/unset omits the header entirely —
    /// it is NEVER sent empty (some gateways reject an empty header value).
    pub workspace_id: Option<String>,
}

/// A native Anthropic Messages API LLM client (`/v1/messages`).
pub struct AnthropicClient {
    config: AnthropicClientConfig,
    caps: Capabilities,
    /// A shared reqwest client — reused across all `complete()` calls so
    /// connection pooling (keep-alive + TLS session resumption) avoids the
    /// ~100-300ms TLS handshake overhead on every LLM request. A 10s
    /// *connect* timeout bounds the handshake; there is deliberately no
    /// total request timeout (the SSE body is long-lived). A per-chunk read
    /// timeout in the stream loop detects dead connections without killing
    /// long-but-active generations.
    http_client: reqwest::Client,
    /// Optional request/response trace log (the right-panel "Trace" tab).
    /// `None` disables capture entirely — `complete()` reads this once per
    /// request, so there is no per-chunk overhead when it's off. Wired at
    /// construction via [`Self::new_with_trace`]; behind an `RwLock` so a
    /// Settings rewire could swap it without rebuilding the client.
    trace: RwLock<Option<Arc<LlmRequestLog>>>,
    /// The trace record id of THIS client's most recent request. `record_tools_phase_ms`
    /// attributes the tools phase to exactly this id — never to whatever
    /// record happens to be newest in the shared ring (which concurrent
    /// agents / in-batch consolidation requests may have advanced past,
    /// review M1).
    last_record_id: std::sync::Mutex<Option<u64>>,
    /// Prep/compact timings measured by the turn loop BEFORE the request
    /// (and its trace record) exists. Parked here by
    /// `record_prep_ms`/`record_compact_ms`; the next `complete()` stamps
    /// them onto the record it creates and clears them. Same shared-client
    /// caveat as `last_record_id`: two agents sharing one client could
    /// interleave a park/stamp pair — a misattributed timing, never a
    /// correctness bug.
    pending_prep_ms: std::sync::Mutex<Option<u32>>,
    /// See [`Self::pending_prep_ms`].
    pending_compact_ms: std::sync::Mutex<Option<u32>>,
    /// See [`Self::pending_prep_ms`].
    pending_backoff_ms: std::sync::Mutex<Option<u32>>,
}

impl AnthropicClient {
    /// Connect (handshake) timeout. Bounds the TLS/TCP setup so a dead host
    /// doesn't stall the agent. Kept fast (10s) so an unreachable endpoint or
    /// stalled proxy fails fast rather than burning 30s per attempt before
    /// retry / fallback can kick in. This is the ONLY timeout on the client —
    /// there is deliberately no total request timeout, since the SSE body is
    /// long-lived (reasoning + generation can run well past any fixed cap).
    pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// Per-chunk read timeout used in the stream loop. If no bytes arrive for
    /// this long, the connection is considered dead (a reasoning model's
    /// thinking phase still sends keepalive chunks, so genuine silence means
    /// the connection broke). This bounds dead-connection detection without
    /// imposing a total lifetime on active streams.
    pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

    /// The `anthropic-version` header value required by the Messages API.
    pub const ANTHROPIC_VERSION: &'static str = "2023-06-01";

    /// The workspace-attribution headers for this client: the
    /// `anthropic-workspace-id` header when a workspace id is configured,
    /// empty otherwise. Applied to every request (`complete()` stream +
    /// non-streaming paths); a `None`/empty id sends nothing.
    pub fn workspace_headers(&self) -> reqwest::header::HeaderMap {
        let mut map = reqwest::header::HeaderMap::new();
        if let Some(ws) = self
            .config
            .workspace_id
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
        {
            match reqwest::header::HeaderValue::from_str(ws) {
                Ok(val) => {
                    map.insert("anthropic-workspace-id", val);
                }
                Err(_) => {
                    // Defense-in-depth for hand-edited endpoints.toml (which
                    // bypasses validate_endpoint's charset check): drop the
                    // header rather than fail the request, but SAY why —
                    // silent un-attributed usage is the failure mode this
                    // guard exists to prevent.
                    eprintln!(
                        "anthropic: ignoring invalid workspace_id (not a valid \
                         HTTP header value): {ws:?}"
                    );
                }
            }
        }
        map
    }

    /// Build a client from config. A single `reqwest::Client` is created here
    /// and reused for every streaming request. Capture is off (see
    /// [`Self::new_with_trace`]).
    pub fn new(config: AnthropicClientConfig) -> Self {
        Self::new_with_trace(config, None)
    }

    /// Build a client from config with request/response capture. `trace` is
    /// the shared [`LlmRequestLog`] every request is recorded into; pass
    /// `None` (or use [`Self::new`]) to disable capture entirely.
    pub fn new_with_trace(
        config: AnthropicClientConfig,
        trace: Option<Arc<LlmRequestLog>>,
    ) -> Self {
        let caps = ProviderKind::Anthropic.capabilities_with_overrides(
            config.max_context,
            config.max_output_tokens,
            config.multimodal,
            // The Messages API has no strict schema enforcement (input
            // schemas are advisory) and no endpoint override exists for it.
            None,
        );
        let http_client = reqwest::Client::builder()
            // Connect-only timeout (handshake). We deliberately do NOT set a
            // total request timeout: the SSE body is long-lived — a reasoning
            // model can think for a long time before the first token, and a
            // long generation runs well past any fixed cap. A total timeout
            // kills active streams mid-flight ("operation timed out" with zero
            // bytes received). Instead, a per-chunk read timeout in the stream
            // loop detects genuinely dead connections without bounding the
            // stream's total lifetime.
            .connect_timeout(Self::CONNECT_TIMEOUT)
            // Disable Nagle's algorithm: send packets immediately instead of
            // buffering the tail of the request body for up to ~200ms. This
            // is the standard setting for low-latency HTTP clients (curl,
            // browsers set it by default) and shaves up to 200ms off the
            // "send" phase (record creation → response headers) per request.
            // Mirrors the OpenAI client (openai.rs).
            .tcp_nodelay(true)
            // Advertise + transparently decompress gzip/brotli/deflate. Without
            // this (reqwest built with default-features = false), a gateway
            // that sends compressed bytes makes reqwest's body decoder fail
            // immediately with "error decoding response body" — before any
            // data reaches us. Enabling the features + the builder flag makes
            // reqwest advertise Accept-Encoding and decode the stream itself.
            .gzip(true)
            .brotli(true)
            .deflate(true)
            // Never follow redirects: a cross-host redirect could carry the
            // `x-api-key` header to a redirected host. LLM endpoints don't
            // redirect, so `Policy::none()` is safe and closes the
            // implicit-redirect leak (Review L1, mirroring openai.rs).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            caps,
            http_client,
            trace: RwLock::new(trace),
            last_record_id: std::sync::Mutex::new(None),
            pending_prep_ms: std::sync::Mutex::new(None),
            pending_compact_ms: std::sync::Mutex::new(None),
            pending_backoff_ms: std::sync::Mutex::new(None),
        }
    }

    /// Build the request body as a JSON value for `POST {base}/messages`.
    ///
    /// Anthropic's contract differs from OpenAI's in four ways, all handled
    /// here at the single serialization choke point:
    /// - `system` is a single top-level field, NOT a message role — every
    ///   [`Role::System`] message's text is hoisted and concatenated;
    /// - `max_tokens` is required;
    /// - content is a blocks array (`text` / `image` / `tool_use` /
    ///   `tool_result`) — tool results ride on `user`-role messages;
    /// - tool `input` is a JSON *object*, not a JSON string.
    fn build_request_json(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<serde_json::Value> {
        // Hoist the FIRST system message into top-level `system` content
        // (the Messages API has no system role). That stable head
        // (instructions, tools, constitution) carries
        // `cache_control: {"type": "ephemeral"}` so Anthropic prefix-caches it.
        // Subsequent system messages (volatile tail: workflow state, memories,
        // plus the byte-stable CONTEXT_FOOTER) are RELOCATED into the final
        // message's content as trailing text blocks — see `tail_blocks` below.
        // Blank texts contribute nothing; the field is omitted entirely when
        // there is nothing to hoist.
        let mut system_blocks: Vec<serde_json::Value> = Vec::new();
        // Every LATER system message (the volatile tail: workflow state,
        // progress, memories — plus the byte-stable CONTEXT_FOOTER) is
        // RELOCATED into the final message's content as trailing text blocks
        // instead of joining `system`. The cache-prefix order is
        // tools -> system -> messages, so a per-turn-mutating block inside
        // `system` would invalidate every message-history breakpoint on
        // every turn; relocated, it becomes the uncached varying suffix
        // AFTER the conversation breakpoint (the documented "static prefix
        // + varying suffix" shape).
        let mut tail_blocks: Vec<serde_json::Value> = Vec::new();
        for m in messages.iter().filter(|m| m.role == Role::System) {
            let text = m.content.as_text();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if system_blocks.is_empty() {
                system_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": trimmed,
                    "cache_control": { "type": "ephemeral" },
                }));
            } else {
                tail_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": trimmed,
                }));
            }
        }

        // Non-system messages map to Anthropic content blocks.
        let rest: Vec<&Message> = messages.iter().filter(|m| m.role != Role::System).collect();
        if rest.is_empty() {
            return Err(Error::Provider(
                "cannot send request: no non-system messages".into(),
            ));
        }
        // Coalesce runs of adjacent tool-result messages into ONE `user`
        // message whose content array carries all the `tool_result` blocks
        // (order preserved). The canonical Messages-API shape for a parallel
        // tool batch is a single user turn with N tool_result blocks — N
        // consecutive user messages are non-canonical and can produce
        // intermittent 400s or semantic drift on strict Anthropic-compatible
        // gateways (the turn driver pushes one tool message per parallel
        // call, and this client advertises `supports_parallel_tools`).
        let mut messages_json: Vec<serde_json::Value> = Vec::new();
        for m in &rest {
            if m.role == Role::Tool {
                match messages_json.last_mut() {
                    // Fold into the preceding user message when it is a
                    // tool-result carrier (i.e. the previous message was also
                    // a tool result — the only user messages we build here
                    // with a blocks array).
                    Some(last)
                        if last["role"] == serde_json::json!("user")
                            && last["content"][0]["type"] == serde_json::json!("tool_result") =>
                    {
                        last["content"]
                            .as_array_mut()
                            .expect("tool-result user message has a content array")
                            .push(self.tool_result_block(m)?);
                    }
                    _ => messages_json.push(self.message_to_json(m)?),
                }
            } else {
                messages_json.push(self.message_to_json(m)?);
            }
        }

        // Anthropic Breakpoint #3 (conversation history): the relocated
        // volatile tail is appended AFTER the final message's real content,
        // and the breakpoint lands on the last REAL block — the cached
        // prefix covers tools + system head + history + the current turn's
        // content, while the volatile tail stays the uncached varying
        // suffix. The next request's 20-position lookback finds this write
        // a few positions back (assistant reply + coalesced tool_use /
        // tool_result runs + new content); a run of consecutive tool_use or
        // tool_result blocks counts as ONE position, so even a large
        // parallel tool batch stays inside the window. A breakpoint ON the
        // tail itself would be the documented "varying block" trap: a fresh
        // write every turn and never a read. Thinking / redacted_thinking
        // blocks cannot be marked directly, so the stamp walks back to the
        // last markable block (skipped entirely if none exists).
        if let Some(last) = messages_json.last_mut() {
            let content = last["content"]
                .as_array_mut()
                .expect("message content is always an array");
            let markable = content.iter_mut().rev().find(|b| {
                !matches!(
                    b["type"].as_str(),
                    Some("thinking") | Some("redacted_thinking")
                )
            });
            if let Some(block) = markable {
                block["cache_control"] = serde_json::json!({ "type": "ephemeral" });
            }
            for b in tail_blocks {
                content.push(b);
            }
        }

        let mut tools_json: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        // Anthropic Breakpoint: attach cache_control to the last tool definition
        // so all tool schemas are cached along with the stable system prompt.
        if let Some(last_tool) = tools_json.last_mut() {
            last_tool["cache_control"] = serde_json::json!({ "type": "ephemeral" });
        }

        // Breakpoint #3 (conversation history) is placed above, on the last
        // real block of the final message, with the volatile tail relocated
        // after it — see the comment there for the full rationale. The old
        // "deliberately avoid message breakpoints" note is obsolete: the
        // volatile tail no longer sits in `system`, so history breakpoints
        // hit reliably. Automatic caching (a top-level `cache_control`
        // field) was evaluated and rejected: it places the breakpoint on the
        // last cacheable block, which here is the varying volatile tail —
        // the documented "varying block" trap.

        /// Absolute ceiling on output tokens regardless of endpoint config.
        /// Most coding tasks need 2-8K; 32K is generous. Prevents wasteful
        /// 128K output budgets on large-context endpoints where the R9
        /// context-window cap never triggers.
        const SANE_MAX_OUTPUT_TOKENS: usize = 32_000;
        // `max_tokens` is REQUIRED by the Messages API (a 400 otherwise) —
        // use the capability budget, floored at 4096 like the OpenAI path.
        // Cap so input + output never exceeds the model's context window
        // (same conservative estimate as the OpenAI client). The sane
        // ceiling caps the budget even when the context-window cap doesn't.
        let prompt_est = crate::provider::estimate_prompt_tokens(messages, tools);
        // Quantize the remaining context budget to 2048-token buckets to avoid
        // per-turn parameter jitter (e.g. 7472 -> 7464 -> 7446), preventing
        // proxy-level cache key invalidations.
        const TOKEN_QUANTUM: usize = 2048;
        let context_budget = self
            .caps
            .max_context
            .saturating_sub(prompt_est)
            .saturating_sub(1024);
        let quantized_context_budget = (context_budget / TOKEN_QUANTUM) * TOKEN_QUANTUM;
        let max_tokens = self
            .caps
            .max_output_tokens
            .max(4096)
            .min(quantized_context_budget)
            .min(SANE_MAX_OUTPUT_TOKENS)
            .max(1024);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": max_tokens,
            "stream": true,
        });
        if !system_blocks.is_empty() {
            body["system"] = serde_json::json!(system_blocks);
        }
        body["messages"] = serde_json::json!(messages_json);
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools_json);
        }
        if let Some(tc) = tool_choice {
            match tc {
                // Auto is the API default — omit the field entirely.
                ToolChoice::Auto => {}
                ToolChoice::Function(name) => {
                    body["tool_choice"] = serde_json::json!({ "type": "tool", "name": name });
                }
                ToolChoice::Required => {
                    body["tool_choice"] = serde_json::json!({ "type": "any" });
                }
            }
        }

        Ok(body)
    }

    /// Map one non-system [`Message`] to its Anthropic wire form.
    fn message_to_json(&self, m: &Message) -> Result<serde_json::Value> {
        match m.role {
            Role::System => unreachable!("system messages are hoisted before mapping"),
            Role::User => {
                let mut blocks: Vec<serde_json::Value> = Vec::new();
                match &m.content {
                    MessageContent::Text(s) => {
                        blocks.push(serde_json::json!({ "type": "text", "text": s }));
                    }
                    MessageContent::Parts(parts) => {
                        for p in parts {
                            match p {
                                ContentPart::Text { text } => {
                                    blocks
                                        .push(serde_json::json!({ "type": "text", "text": text }));
                                }
                                ContentPart::ImageUrl { image_url } => {
                                    if !self.caps.multimodal {
                                        // Non-multimodal models can't process
                                        // images; strip them (the vision client
                                        // handles them separately).
                                        continue;
                                    }
                                    blocks.push(self.image_block(&image_url.url));
                                }
                            }
                        }
                    }
                }
                if blocks.is_empty() {
                    // A user message stripped of every image block must still
                    // carry content (the API rejects an empty array).
                    blocks.push(serde_json::json!({ "type": "text", "text": "" }));
                }
                Ok(serde_json::json!({ "role": "user", "content": blocks }))
            }
            Role::Assistant => {
                // Rule 1: echo the verbatim content array the provider returned
                // (reassembled from streamed content_block_* events). This
                // preserves thinking blocks with their per-block `signature`
                // and redacted_thinking blocks untouched — filtering either
                // causes a 400 (Rule 4: "Expected thinking or redacted_thinking,
                // but found text"). Synthetic messages (no raw) fall through to
                // field-based construction below.
                if let Some(raw) = &m.raw {
                    if let Some(content) = raw.get("content") {
                        // bd5eb19d: drop always-invalid thinking blocks before
                        // the echo — a thinking block that never received a
                        // thinking_delta stays thinking:"" (the block-start
                        // payload), and echoing it verbatim is a guaranteed
                        // non-retryable 400 "each thinking block must contain
                        // thinking". Valid blocks still echo verbatim (Rule 1);
                        // only the unsendable empty ones are filtered.
                        let sanitized: Vec<serde_json::Value> = content
                            .as_array()
                            .map(|blocks| {
                                blocks
                                    .iter()
                                    .filter(|b| !is_always_invalid_thinking_block(b))
                                    .cloned()
                                    .collect()
                            })
                            .unwrap_or_default();
                        // H1 (Anthropic): a null turn produces an empty content
                        // array []. Echoing it would bypass the "(no output)"
                        // placeholder. Fall through to field construction when
                        // the (sanitized) array is empty.
                        // H2/H3 (Anthropic): also require well-formed blocks.
                        // Note the cost, which differs from the OpenAI path:
                        // the field construction below deliberately does NOT
                        // re-emit `thinking` blocks (a fabricated unsigned one
                        // is itself a 400), so falling through drops reasoning
                        // for this turn. `raw_content_is_usable` is therefore
                        // kept deliberately narrow.
                        if !sanitized.is_empty() {
                            let sanitized_value = serde_json::Value::Array(sanitized);
                            if raw_content_is_usable(&sanitized_value) {
                                return Ok(serde_json::json!({
                                    "role": "assistant",
                                    "content": sanitized_value,
                                }));
                            }
                        }
                    }
                }
                let mut blocks: Vec<serde_json::Value> = Vec::new();
                // Synthetic assistant message (no raw): construct from fields.
                // Stored reasoning is NOT echoed — a fabricated unsigned
                // `thinking` block makes the API reject the request with HTTP
                // 400 `thinking.signature: Field required` (the signature is
                // only available on captured raw turns, echoed above).
                match &m.content {
                    MessageContent::Text(s) if !s.trim().is_empty() => {
                        blocks.push(serde_json::json!({ "type": "text", "text": s }));
                    }
                    MessageContent::Parts(parts) => {
                        for p in parts {
                            if let ContentPart::Text { text } = p {
                                if !text.trim().is_empty() {
                                    blocks
                                        .push(serde_json::json!({ "type": "text", "text": text }));
                                }
                            }
                        }
                    }
                    _ => {}
                }
                for tc in &m.tool_calls {
                    // Anthropic's tool_use `input` is a JSON object; the
                    // history stores arguments as a JSON string, so parse it
                    // here (falling back to `{}` on a malformed fragment —
                    // streamed partials can leave a non-JSON residue).
                    let input = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": input,
                    }));
                }
                if blocks.is_empty() {
                    return Err(Error::Provider(
                        "cannot send request: assistant message has no text or tool calls".into(),
                    ));
                }
                Ok(serde_json::json!({ "role": "assistant", "content": blocks }))
            }
            Role::Tool => {
                // Tool results are content blocks on a `user`-role message in
                // the Messages API (the tool_result block carries the
                // tool_use_id). Standalone tool messages (non-adjacent runs)
                // each become their own single-block user message; adjacent
                // runs are coalesced upstream in `build_request_json`.
                Ok(serde_json::json!({
                    "role": "user",
                    "content": [self.tool_result_block(m)?],
                }))
            }
        }
    }

    /// Build one `tool_result` content block for a tool-role message.
    fn tool_result_block(&self, m: &Message) -> Result<serde_json::Value> {
        let id = m.tool_call_id.clone().ok_or_else(|| {
            Error::Provider("cannot send request: tool message has no tool_call_id".into())
        })?;
        Ok(serde_json::json!({
            "type": "tool_result",
            "tool_use_id": id,
            "content": m.content.as_text(),
            "is_error": false,
        }))
    }

    /// Build an Anthropic image block from a URL. Base64 data URLs
    /// (`data:image/png;base64,....`) become `source.type: base64` blocks
    /// (media type from the prefix, `image/png` fallback); anything else is
    /// sent as a `source.type: url` block.
    fn image_block(&self, url: &str) -> serde_json::Value {
        if let Some((media_type, data)) = parse_data_url(url) {
            serde_json::json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data }
            })
        } else {
            serde_json::json!({
                "type": "image",
                "source": { "type": "url", "url": url }
            })
        }
    }
}

/// Parse one `data:` JSON payload (the `event:` line is tracked separately by
/// [`parse_sse_buffer`], which passes it in) into [`LlmEvent`]s.
///
/// Anthropic's Messages SSE protocol:
/// - `message_start` → carries `message.usage.input_tokens` + the cache
///   fields (`cache_read_input_tokens` → the Usage event's `cached_tokens`,
///   `cache_creation_input_tokens` folded into `prompt_tokens`) — captured
///   for the final Usage event.
/// - `content_block_start` → carries `index` + `content_block`; a
///   `tool_use` block emits [`LlmEvent::ToolCallStart`], a `text` block is
///   noted so `content_block_delta` text lands on the right stream.
/// - `content_block_delta` → `delta.type`: `text_delta` → TextDelta,
///   `thinking_delta` → ReasoningDelta, `input_json_delta` →
///   ToolCallArgumentDelta.
/// - `content_block_stop` → nothing (tool blocks close without an event).
/// - `message_delta` → carries `usage.output_tokens` (captured for the final
///   Usage event) + `delta.stop_reason`.
/// - `message_stop` → emits Finish (from the captured stop_reason, or a Stop
///   fallback) + Usage (captured input/output tokens, timings enriched by the
///   stream loop).
/// - `error` → [`LlmEvent::Error`].
///
/// `state` accumulates usage + stop_reason across the events of one stream.
/// Unknown event types and unknown fields are ignored.
fn parse_sse_event(
    event_name: &str,
    json: &serde_json::Value,
    state: &mut StreamState,
) -> Vec<LlmEvent> {
    let mut events = Vec::new();
    match event_name {
        "message_start" => {
            if let Some(usage) = json.get("message").and_then(|m| m.get("usage")) {
                state.input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                // Cache accounting (backlog 648051bf): Anthropic's
                // input_tokens EXCLUDES the cached prefix.
                state.cache_read_tokens = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                state.cache_creation_tokens = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
            }
        }
        "content_block_start" => {
            let index = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if let Some(block) = json.get("content_block") {
                // Store the verbatim block for the raw content array (Rule 1),
                // preserving the thinking `signature` / redacted_thinking `data`
                // the old parser dropped (root cause of the Anthropic 400).
                let idx = index as usize;
                while state.content_blocks.len() <= idx {
                    state.content_blocks.push(serde_json::Value::Null);
                }
                state.content_blocks[idx] = build_raw_content_block(block);
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => {
                        let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        if !id.is_empty() && !name.is_empty() {
                            events.push(LlmEvent::ToolCallStart {
                                index,
                                id: id.to_string(),
                                name: name.to_string(),
                            });
                        }
                    }
                    Some("text") => {
                        state.text_block_index = Some(index);
                    }
                    _ => {}
                }
            }
        }
        "content_block_delta" => {
            let index = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if let Some(delta) = json.get("delta") {
                // Accumulate the fragment into the raw content block (Rule 1).
                accumulate_raw_block_delta(state, index as usize, delta);
                match delta.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                events.push(LlmEvent::TextDelta {
                                    text: text.to_string(),
                                });
                            }
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                events.push(LlmEvent::ReasoningDelta {
                                    text: text.to_string(),
                                });
                            }
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(fragment) = delta.get("partial_json").and_then(|t| t.as_str()) {
                            if !fragment.is_empty() {
                                events.push(LlmEvent::ToolCallArgumentDelta {
                                    index,
                                    fragment: fragment.to_string(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        "content_block_stop" => {
            // Tool/text blocks close without an event; the accumulated text
            // and tool arguments are already streamed.
        }
        "message_delta" => {
            if let Some(usage) = json.get("usage") {
                state.output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
            }
            if let Some(delta) = json.get("delta") {
                if let Some(reason) = delta.get("stop_reason").and_then(|r| r.as_str()) {
                    state.stop_reason = Some(reason.to_string());
                }
            }
        }
        "message_stop" => {
            // Finalize + emit the verbatim assistant message (Rule 1 raw): parse
            // tool_use input fragments into objects, drop always-invalid
            // thinking blocks (bd5eb19d: a thinking block that never received
            // a thinking_delta stays thinking:"" — storing it poisons the
            // history for every later echo), then emit the content array with
            // thinking `signature` / redacted_thinking `data` preserved.
            // Emitted before Finish/Usage so those remain the last
            // two events (existing assertions rely on that).
            if !state.content_blocks.is_empty() {
                let mut content = std::mem::take(&mut state.content_blocks);
                for block in &mut content {
                    if let Some(obj) = block.as_object_mut() {
                        if obj.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            if let Some(serde_json::Value::String(s)) = obj.get_mut("input") {
                                match serde_json::from_str::<serde_json::Value>(s) {
                                    Ok(parsed) => {
                                        obj.insert("input".to_string(), parsed);
                                    }
                                    Err(_) => {
                                        obj.insert("input".to_string(), serde_json::json!({}));
                                    }
                                }
                            }
                        }
                    }
                }
                content.retain(|block| !is_always_invalid_thinking_block(block));
                if !content.is_empty() {
                    let mut message = serde_json::Map::new();
                    message.insert("role".to_string(), "assistant".into());
                    message.insert("content".to_string(), serde_json::Value::Array(content));
                    events.push(LlmEvent::RawAssistantDelta {
                        delta: serde_json::Value::Object(message),
                    });
                }
            }
            let reason = match state.stop_reason.as_deref() {
                Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
                Some("tool_use") => FinishReason::ToolCalls,
                Some("max_tokens") => FinishReason::Length,
                Some(other) => FinishReason::Other(other.to_string()),
                None => FinishReason::Stop,
            };
            events.push(LlmEvent::Finish { reason });
            events.push(LlmEvent::Usage {
                // Parity with the OpenAI path (backlog 648051bf): Anthropic's
                // input_tokens EXCLUDES the cached prefix, so prompt_tokens
                // here carries ALL billed prompt tokens (fresh + cache-read +
                // cache-creation) — the same meaning OpenAI's prompt_tokens
                // has. cached_tokens is the cache-read hit only (creation is
                // a write, not a hit).
                prompt_tokens: state.input_tokens
                    + state.cache_read_tokens
                    + state.cache_creation_tokens,
                completion_tokens: state.output_tokens,
                reasoning_tokens: 0,
                cached_tokens: state.cache_read_tokens,
                // Timing is enriched by the stream loop (it owns the
                // timestamps); the parser has no clock context.
                ttft_ms: None,
                generation_ms: None,
            });
        }
        "error" => {
            let message = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown Anthropic stream error");
            events.push(LlmEvent::Error {
                error: message.to_string(),
            });
        }
        _ => {
            // Unknown event types are ignored (forward-compatible).
        }
    }
    events
}

/// Per-stream accumulator for the Anthropic event protocol: usage totals
/// arrive split across `message_start` (input) and `message_delta` (output),
/// and the stop_reason arrives just before `message_stop`.
#[derive(Debug, Default)]
struct StreamState {
    input_tokens: u32,
    output_tokens: u32,
    /// Prompt tokens served from the provider's cache
    /// (`usage.cache_read_input_tokens` in `message_start`). Maps to
    /// [`LlmEvent::Usage`]'s `cached_tokens` — the same "served from cache"
    /// meaning the OpenAI path's `prompt_tokens_details.cached_tokens` has.
    cache_read_tokens: u32,
    /// Prompt tokens written to the provider's cache this request
    /// (`usage.cache_creation_input_tokens` in `message_start`). Billed
    /// prompt tokens; folded into the Usage event's `prompt_tokens`.
    cache_creation_tokens: u32,
    stop_reason: Option<String>,
    /// Index of the currently-open text block (unused beyond tracking — text
    /// deltas already carry their block's index).
    text_block_index: Option<u32>,
    /// The verbatim assistant content array, assembled from `content_block_*`
    /// events (Rule 1 raw). Each entry is a block object (text / thinking /
    /// redacted_thinking / tool_use) with its `signature` / `data` preserved
    /// untouched. Emitted as a single [`LlmEvent::RawAssistantDelta`] at
    /// `message_stop`.
    content_blocks: Vec<serde_json::Value>,
}

/// Assistant content-block types this client knows how to reason about.
///
/// Used only to recognise a *corrupted* discriminator (see
/// [`raw_content_is_usable`]) — an unfamiliar type is NOT rejected, because
/// echoing unknown shapes verbatim is the whole point of Rule 1.
const KNOWN_BLOCK_TYPES: [&str; 4] = ["text", "thinking", "redacted_thinking", "tool_use"];

/// Whether `ty` is a known block type concatenated with itself two or more
/// times (`"tool_usetool_use"`, `"texttext"`, ...).
///
/// That exact shape is the fingerprint of a raw payload assembled by a delta
/// merge that appended an identity field instead of setting it once. Matching
/// the repetition rather than merely "not in [`KNOWN_BLOCK_TYPES`]" keeps a
/// legitimate future type (`server_tool_use`, say) echoing untouched.
fn is_repeated_block_type(ty: &str) -> bool {
    KNOWN_BLOCK_TYPES.iter().any(|base| {
        ty.len() > base.len()
            && ty.len().is_multiple_of(base.len())
            && ty == base.repeat(ty.len() / base.len())
    })
}

/// Check whether a stored raw assistant `content` array is safe to echo
/// verbatim (the Anthropic counterpart to `openai::raw_is_usable`).
///
/// Returns `false` (→ fall through to field construction) when the array is
/// not an array of objects, when a block has no usable `type`, when a `type`
/// is a self-concatenated known type, or when a `tool_use` block is missing a
/// usable `id`/`name` or carries a non-object `input` (the Messages API takes
/// tool input as a JSON *object*, not a string).
///
/// This is defense-in-depth for stored or legacy payloads, not a live bug:
/// this client never assembles raw through the generic scalar merge. Blocks
/// are built once from `content_block_start` (see [`build_raw_content_block`])
/// and deltas accumulate through an explicit allowlist keyed on delta type
/// (see [`accumulate_raw_block_delta`]), so `type`/`id`/`name` are never
/// touched by a delta and cannot be corrupted on this path.
fn raw_content_is_usable(content: &serde_json::Value) -> bool {
    let Some(blocks) = content.as_array() else {
        // Not the Messages API assistant shape at all.
        return false;
    };
    blocks.iter().all(|block| {
        let Some(obj) = block.as_object() else {
            return false;
        };
        let Some(ty) = obj.get("type").and_then(|t| t.as_str()) else {
            return false;
        };
        if ty.is_empty() || is_repeated_block_type(ty) {
            return false;
        }
        if ty != "tool_use" {
            // Known-good or unfamiliar-but-well-formed: echo it (Rule 1).
            return true;
        }
        let non_empty_str = |key: &str| {
            obj.get(key)
                .and_then(|v| v.as_str())
                .map(|v| !v.is_empty())
                .unwrap_or(false)
        };
        non_empty_str("id")
            && non_empty_str("name")
            && obj.get("input").map(|v| v.is_object()).unwrap_or(false)
    })
}

/// True when a content block is always invalid on the wire and carries no
/// information: a `thinking` block whose `thinking` string is missing or
/// empty, or a `redacted_thinking` block whose `data` is missing or empty.
///
/// Anthropic rejects these on echo with a non-retryable 400 —
/// "messages.N.content.M.thinking: each thinking block must contain thinking"
/// (claude-opus-5, backlog bd5eb19d) — so they must never be captured and
/// never be echoed. This is deliberately NOT the Rule 4 case: filtering a
/// VALID thinking block causes "Expected thinking or redacted_thinking, but
/// found text"; an empty one is unsendable either way, so dropping it is
/// never worse than echoing it.
fn is_always_invalid_thinking_block(block: &serde_json::Value) -> bool {
    let Some(obj) = block.as_object() else {
        return false;
    };
    let content_field_empty = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .map(|v| v.is_empty())
            .unwrap_or(true)
    };
    match obj.get("type").and_then(|t| t.as_str()) {
        Some("thinking") => content_field_empty("thinking"),
        Some("redacted_thinking") => content_field_empty("data"),
        _ => false,
    }
}

/// Build the verbatim raw form of one content block from a `content_block_start`
/// payload (Rule 1).
///
/// - `text` → `{type:"text", text}` (initial text, usually `""`).
/// - `thinking` → `{type:"thinking", thinking, signature}` — the `signature`
///   arrives here in `content_block_start` and is captured verbatim. This is
///   the field the old parser dropped, causing `thinking.signature: Field
///   required` 400s on multi-turn replay.
/// - `redacted_thinking` → `{type:"redacted_thinking", data}` — `data` preserved.
/// - `tool_use` → `{type:"tool_use", id, name, input:""}` — `input` accumulates
///   `input_json_delta` fragments as a string, parsed to an object at
///   `message_stop`.
/// - unknown block type → preserved verbatim.
fn build_raw_content_block(block: &serde_json::Value) -> serde_json::Value {
    match block.get("type").and_then(|t| t.as_str()) {
        Some("text") => {
            let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
            serde_json::json!({ "type": "text", "text": text })
        }
        Some("thinking") => {
            let thinking = block.get("thinking").and_then(|t| t.as_str()).unwrap_or("");
            let mut obj = serde_json::Map::new();
            obj.insert("type".to_string(), "thinking".into());
            obj.insert("thinking".to_string(), thinking.into());
            if let Some(sig) = block.get("signature") {
                if !sig.is_null() {
                    obj.insert("signature".to_string(), sig.clone());
                }
            }
            serde_json::Value::Object(obj)
        }
        Some("redacted_thinking") => {
            let mut obj = serde_json::Map::new();
            obj.insert("type".to_string(), "redacted_thinking".into());
            if let Some(data) = block.get("data") {
                obj.insert("data".to_string(), data.clone());
            }
            serde_json::Value::Object(obj)
        }
        Some("tool_use") => {
            let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
            serde_json::json!({ "type": "tool_use", "id": id, "name": name, "input": "" })
        }
        _ => block.clone(),
    }
}

/// Accumulate one `content_block_delta` fragment into the raw content block at
/// `index` (Rule 1). `text_delta` → `text`, `thinking_delta` → `thinking`,
/// `input_json_delta` → `input` (string, parsed later), `signature_delta` →
/// `signature` (defensive: some providers stream the signature across deltas).
fn accumulate_raw_block_delta(
    state: &mut StreamState,
    index: usize,
    delta: &serde_json::Value,
) {
    let Some(block) = state.content_blocks.get_mut(index) else {
        return;
    };
    let Some(obj) = block.as_object_mut() else {
        return;
    };
    match delta.get("type").and_then(|t| t.as_str()) {
        Some("text_delta") => {
            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                if let Some(serde_json::Value::String(existing)) = obj.get_mut("text") {
                    existing.push_str(text);
                }
            }
        }
        Some("thinking_delta") => {
            if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                if let Some(serde_json::Value::String(existing)) = obj.get_mut("thinking") {
                    existing.push_str(text);
                }
            }
        }
        Some("input_json_delta") => {
            if let Some(fragment) = delta.get("partial_json").and_then(|t| t.as_str()) {
                let input = obj
                    .entry("input".to_string())
                    .or_insert_with(|| serde_json::Value::String(String::new()));
                if let serde_json::Value::String(s) = input {
                    s.push_str(fragment);
                }
            }
        }
        Some("signature_delta") => {
            if let Some(sig) = delta.get("signature").and_then(|s| s.as_str()) {
                let entry = obj
                    .entry("signature".to_string())
                    .or_insert_with(|| serde_json::Value::String(String::new()));
                if let serde_json::Value::String(s) = entry {
                    s.push_str(sig);
                }
            }
        }
        _ => {}
    }
}

/// Process all complete SSE lines currently in `buffer`, draining each as it's
/// consumed and returning the parsed outcomes. Incomplete trailing data (no
/// trailing newline) is left in the buffer for the next chunk.
///
/// Unlike the OpenAI parser, the `event:` line is significant here (Anthropic
/// names every data frame), so the current event name is tracked across lines
/// within the buffer; only `data:` lines carry payload. Uses
/// `String::drain()` to drop processed bytes in-place — O(1) amortized per
/// line instead of the O(n²) re-copy of `buffer = buffer[pos+1..].to_string()`.
fn parse_sse_buffer(buffer: &mut String, state: &mut StreamState) -> Vec<SseOutcome> {
    let mut outcomes = Vec::new();
    // The event name of the data frame currently being assembled. SSE frames
    // can span lines (event: X\n data: {...}), so a `data:` line may follow an
    // `event:` line in a later buffer chunk.
    let mut current_event: Option<String> = None;
    while let Some(pos) = buffer.find('\n') {
        // Extract the line up to (but not including) the newline, then drop
        // the processed bytes (line + newline) in-place via drain().
        let line = buffer[..pos].trim().to_string();
        buffer.drain(..=pos);

        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(name) = line.strip_prefix("event: ") {
            current_event = Some(name.to_string());
            continue;
        }
        if let Some(data) = line.strip_prefix("data: ") {
            let event_name = current_event.take().unwrap_or_default();
            match serde_json::from_str::<serde_json::Value>(data) {
                Ok(json) => {
                    for event in parse_sse_event(&event_name, &json, state) {
                        outcomes.push(SseOutcome::Event(event));
                    }
                }
                Err(e) => {
                    // Include the raw data so the failure is debuggable.
                    outcomes.push(SseOutcome::ParseError(format!(
                        "failed to parse Anthropic SSE chunk: {e}\n\
                         raw stream data:\n{}",
                        truncate_raw_stream(data, 2000)
                    )));
                }
            }
        }
    }
    outcomes
}

#[async_trait]
impl LlmClient for AnthropicClient {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    /// The endpoint name from `endpoints.toml` this client was built from —
    /// see [`LlmClient::provider_name`].
    fn provider_name(&self) -> &str {
        &self.config.provider
    }

    fn record_tools_phase_ms(&self, ms: u32) {
        // Attribute the tools phase (stream end → tool batch done) to THIS
        // client's most recent trace record — the request whose response
        // produced the tool calls (review M1: never "newest in the ring").
        let id = *self
            .last_record_id
            .lock()
            .expect("AnthropicClient last_record_id lock poisoned");
        if let Some(id) = id {
            if let Some(log) = self
                .trace
                .read()
                .expect("AnthropicClient trace lock poisoned")
                .clone()
            {
                log.set_tools_ms(id, ms);
            }
        }
    }

    fn record_raw_tool_calls(&self, calls: Vec<(String, String, String)>) {
        // The generation-vs-harness discrimination tap (backlog e8b39d72 H1):
        // attribute the delivered tool-call args — verbatim, pre-normalization
        // — to THIS client's most recent trace record, the request whose
        // response produced them (review M1 pattern: never "newest in the
        // ring").
        let id = *self
            .last_record_id
            .lock()
            .expect("AnthropicClient last_record_id lock poisoned");
        if let Some(id) = id {
            if let Some(log) = self
                .trace
                .read()
                .expect("AnthropicClient trace lock poisoned")
                .clone()
            {
                log.set_raw_tool_calls(id, calls);
            }
        }
    }

    fn record_prep_ms(&self, ms: u32) {
        // Park the value — the trace record it belongs to is created by the
        // next `complete()` call (stamped + cleared there). Overwrites any
        // un-stamped value; the turn loop calls this once per iteration.
        *self
            .pending_prep_ms
            .lock()
            .expect("AnthropicClient pending_prep_ms lock poisoned") = Some(ms);
    }

    fn record_compact_ms(&self, ms: u32) {
        // Parked + stamped exactly like `record_prep_ms`.
        *self
            .pending_compact_ms
            .lock()
            .expect("AnthropicClient pending_compact_ms lock poisoned") = Some(ms);
    }

    fn record_backoff_ms(&self, ms: u32) {
        // Parked + stamped exactly like `record_prep_ms`.
        *self
            .pending_backoff_ms
            .lock()
            .expect("AnthropicClient pending_backoff_ms lock poisoned") = Some(ms);
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<BoxStream<'_, LlmEvent>> {
        // Entry anchor for the prep sliver: everything from here until the
        // record is created (request-body build + trace-log start) is local
        // work that belongs in `prep_ms`, not in the connect window.
        let entry_at = std::time::Instant::now();
        // Consume the prep/compact/backoff timings parked on this
        // client (before the request — and its record — existed). Taken
        // unconditionally at ENTRY so a parked value's lifetime is bounded
        // to the very next complete() call: a turn that ends without
        // creating a record (body-build failure on every retry, interrupt
        // during record creation) can never leak a stale timing onto a
        // later, unrelated request.
        let parked_prep = self
            .pending_prep_ms
            .lock()
            .expect("AnthropicClient pending_prep_ms lock poisoned")
            .take();
        let parked_compact = self
            .pending_compact_ms
            .lock()
            .expect("AnthropicClient pending_compact_ms lock poisoned")
            .take();
        let parked_backoff = self
            .pending_backoff_ms
            .lock()
            .expect("AnthropicClient pending_backoff_ms lock poisoned")
            .take();

        // Build the request body as JSON (hoists system messages, maps
        // content blocks). A malformed message list must fail HERE with a
        // descriptive local error, not upstream as an opaque 400 after a
        // wasted round-trip.
        let body = self.build_request_json(messages, tools, tool_choice)?;

        // Trace capture (optional — None disables recording entirely). The
        // Arc is cloned once per request; the record id ties later stream
        // events back to this request. `start` applies the request-body cap,
        // which serializes the (potentially ~MiB) body — run it on the
        // blocking pool so the async worker never pays that CPU.
        let trace = self
            .trace
            .read()
            .expect("AnthropicClient trace lock poisoned")
            .clone();
        let rec_id = if let Some(log) = trace.clone() {
            let model = self.config.model.clone();
            let base_url = self.config.base_url.clone();
            let provider = self.config.provider.clone();
            let body_for_trace = body.clone();
            tokio::task::spawn_blocking(move || {
                log.start(&model, &base_url, &provider, body_for_trace)
            })
            .await
            .ok()
        } else {
            None
        };
        // The connect-phase anchor: record creation (just above) → stream
        // open (the POST response arrives; captured below at
        // `request_start`). The stream task computes `connect_ms` from it
        // at the Usage event — the full network round trip.
        let record_created = std::time::Instant::now();
        // Remember this request's record id so the later tools-phase
        // attribution lands on THIS record, not on whatever a concurrent
        // agent created in the shared ring meanwhile (review M1).
        if let Some(id) = rec_id {
            *self
                .last_record_id
                .lock()
                .expect("AnthropicClient last_record_id lock poisoned") = Some(id);
            // Stamp the prep/compact/backoff timings parked by the turn
            // loop and the retry loop (taken at complete() entry above)
            // onto the freshly created record. Prep additionally absorbs
            // the local sliver from complete() entry to record creation
            // (body build + trace-log start) so prep + connect covers the
            // full wall time with no invisible gap.
            if let Some(log) = trace.as_ref() {
                if let Some(ms) = parked_prep {
                    log.set_prep_ms(
                        id,
                        ms + record_created.duration_since(entry_at).as_millis() as u32,
                    );
                }
                if let Some(ms) = parked_compact {
                    log.set_compact_ms(id, ms);
                }
                if let Some(ms) = parked_backoff {
                    log.set_backoff_ms(id, ms);
                }
            }
        }

        let url = format!("{}/messages", self.config.base_url.trim_end_matches('/'));

        // POST-send anchor (perf review L1): stamped immediately before the
        // send — the ttft anchor. SSE providers only begin the response when
        // the first token is ready, so headers arrive with the first chunk
        // and a headers-arrived anchor measures ~0.
        let post_sent = std::time::Instant::now();
        let response = match self
            .http_client
            .post(&url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", Self::ANTHROPIC_VERSION)
            // Workspace attribution (optional, endpoints.toml
            // `workspace_id`): routes usage to the configured workspace.
            // Omitted entirely when unset — never sent empty.
            .headers(self.workspace_headers())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) => {
                if let (Some(log), Some(id)) = (&trace, rec_id) {
                    // Stamp the connect bucket: the request died before any
                    // response headers arrived — the same window success
                    // attributes to connect_ms, so failures must too.
                    log.set_connect_ms(id, record_created.elapsed().as_millis() as u32);
                    // Status 0 = no HTTP response at all (transport failure).
                    log.fail(id, 0, &format!("failed to start stream: {e}"));
                }
                return Err(Error::Provider(format!("failed to start stream: {e}")));
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            if let (Some(log), Some(id)) = (&trace, rec_id) {
                // For 401/403 the body may echo the x-api-key header (the API
                // key). The user-visible `error` already suppresses it; do the
                // same for the trace, which IS surfaced in the UI "Trace" tab
                // — never log the raw body for auth failures.
                let trace_msg = if status.as_u16() == 401 || status.as_u16() == 403 {
                    format!("HTTP {status} — unauthorized (body suppressed)")
                } else {
                    body_text.clone()
                };
                // Stamp the connect bucket: the whole in-flight window
                // (connect + the server's reject processing), matching the
                // success path's connect attribution.
                log.set_connect_ms(id, record_created.elapsed().as_millis() as u32);
                log.fail(id, status.as_u16(), &trace_msg);
            }
            return Err(provider_error(status, &body_text, &url, "stream request"));
        }
        if let (Some(log), Some(id)) = (&trace, rec_id) {
            log.set_status(id, response.status().as_u16());
        }

        // Bridge the SSE byte stream into our LlmEvent stream via a channel.
        let (tx, rx) = mpsc::channel::<LlmEvent>(128);
        // Capture the status + transport-relevant headers so a mid-stream
        // decode error is self-diagnosing.
        let status = response.status();
        let content_encoding = header_str(&response, "content-encoding");
        let transfer_encoding = header_str(&response, "transfer-encoding");
        let content_type = header_str(&response, "content-type");
        // Headers-arrived anchor: the POST response headers just arrived —
        // the END of the connect bucket. connect_ms = record_created →
        // request_start (body build + upload + server prefill/queue — the
        // Waiting bucket). ttft_ms = post_sent → first chunk (the real
        // first-token wait — perf review L1: headers arrive WITH the first
        // chunk for SSE providers, so the old headers→first-chunk window
        // measured ~0). generation = first chunk → last.
        let request_start = std::time::Instant::now();
        // The (log, record id) pair the stream task mirrors events into.
        let trace_ctx = match (trace, rec_id) {
            (Some(log), Some(id)) => Some((log, id)),
            _ => None,
        };
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            // Per-stream Anthropic event state (usage split across events).
            let mut state = StreamState::default();
            // First-chunk timestamp (set when the first bytes arrive).
            let mut first_chunk: Option<std::time::Instant> = None;
            // Reasoning-window tracking for `reasoning_ms`: whether any
            // thinking/ReasoningDelta streamed, and the first chunk that
            // carried an answer/tool delta (the reasoning→answer boundary).
            let mut saw_reasoning = false;
            let mut first_content_chunk: Option<std::time::Instant> = None;
            // Byte-level stall tracking for `stall_ms`: gaps between chunk
            // arrivals longer than StallTracker::THRESHOLD are waiting, not
            // generation — split out of `generation_ms` in the graph.
            let mut stall_tracker = StallTracker::default();
            // Per-chunk read timeout: if no bytes arrive for this long, the
            // connection is dead (a reasoning model's thinking phase still
            // sends keepalive chunks).
            let read_timeout = Self::READ_TIMEOUT;
            // Whether a Finish was already mirrored into the trace log. The
            // fallback Finish at stream end must NOT overwrite the real
            // finish reason logged by the event arm.
            let mut finish_logged = false;
            // Whether a real Finish was already DELIVERED to the consumer.
            // `message_stop` emits the authoritative Finish (stop_reason
            // mapped from the provider); the fallback below must not send a
            // second Finish{Stop} over it — the consumer assigns
            // `finish_reason` on every Finish, so a trailing Stop would
            // overwrite tool_use/max_tokens and break Length-driven retries.
            let mut saw_finish = false;
            // Live usage overlay (backlog b5503915): chars/4 estimates of the
            // deltas streamed so far, pushed onto the trace record at most
            // every 500ms (throttled in the Ok(bytes) arm) so the Trace tab
            // shows live token progress during a long thinking phase — the
            // only stretch where nothing else in the record changes. The
            // authoritative set_usage at stream end overwrites these.
            // `final_usage_seen` stops the overlay dead once the real usage
            // landed: a non-conforming server that sends usage without a
            // prior terminal event, then more bytes, must not have its
            // estimates clobber the real numbers.
            let mut est_reasoning_chars: usize = 0;
            let mut est_completion_chars: usize = 0;
            let mut last_usage_push: Option<std::time::Instant> = None;
            let mut final_usage_seen = false;

            // R10 repetition-guard accumulator — tracks the response text for
            // stuck-loop detection (ANY repeating unit of ≤ 200 bytes spans
            // the last 600 tail bytes). Bounded by `bound_repetition_buffer`
            // to ~2 KB.
            let mut response_text = String::new();

            loop {
                // Wrap the next-chunk read in a timeout. Elapsed → dead
                // connection; emit a clear error and stop.
                let chunk_result = match tokio::time::timeout(read_timeout, stream.next()).await {
                    Ok(Some(result)) => result,
                    Ok(None) => break, // stream ended normally
                    Err(_elapsed) => {
                        let partial = if buffer.is_empty() {
                            String::from("(no data received before the timeout)")
                        } else {
                            truncate_raw_stream(&buffer, 2000)
                        };
                        let error = format!(
                            "stream stalled — no data for {read_timeout:?} \
                             (HTTP {status}). The connection appears dead.\n\
                             content-encoding: {content_encoding}\n\
                             transfer-encoding: {transfer_encoding}\n\
                             content-type: {content_type}\n\
                             partial stream data received so far:\n{partial}"
                        );
                        if let Some((log, id)) = &trace_ctx {
                            if first_chunk.is_none() {
                                // Headers arrived but no chunk ever followed:
                                // connect covers record→headers, ttft the
                                // full POST-send wait (upload + prefill +
                                // silence).
                                log.set_connect_ms(
                                    *id,
                                    request_start
                                        .duration_since(record_created)
                                        .as_millis() as u32,
                                );
                                log.set_ttft_ms(*id, post_sent.elapsed().as_millis() as u32);
                            } else {
                                // The trailing silence (last chunk →
                                // timeout) is a stall too — finalize the
                                // tracker so stall_ms covers it.
                                stall_tracker.finalize(std::time::Instant::now());
                                log.set_stall_ms(*id, stall_tracker.ms());
                                if let Some(fc) = first_chunk {
                                    log.set_generation_ms(*id, fc.elapsed().as_millis() as u32);
                                }
                            }
                            log.fail(*id, status.as_u16(), &error);
                        }
                        let _ = tx.send(LlmEvent::Error { error }).await;
                        return;
                    }
                };
                match chunk_result {
                    Ok(bytes) => {
                        // Stamp the first-chunk timestamp for usage timing.
                        let now = std::time::Instant::now();
                        if first_chunk.is_none() {
                            first_chunk = Some(now);
                        }
                        stall_tracker.on_chunk(now);
                        let chunk_text = String::from_utf8_lossy(&bytes);
                        if let Some((log, id)) = &trace_ctx {
                            log.append_response(*id, &chunk_text);
                            // Live usage overlay (backlog b5503915): push the
                            // chars/4 estimate at most every 500ms — during a
                            // long thinking phase this is the only thing that
                            // moves in the record until the stream finishes.
                            // The authoritative set_usage (the Usage event
                            // arm below) overwrites it at stream end.
                            if !final_usage_seen
                                && last_usage_push.map_or(true, |t| {
                                    now.duration_since(t) >= std::time::Duration::from_millis(500)
                                })
                            {
                                last_usage_push = Some(now);
                                log.update_streaming_usage(
                                    *id,
                                    (est_completion_chars / 4) as u32,
                                    (est_reasoning_chars / 4) as u32,
                                );
                            }
                        }
                        buffer.push_str(&chunk_text);
                        // Process complete SSE lines. The line-splitting +
                        // parsing is factored into `parse_sse_buffer` so it's
                        // unit-testable without a live HTTP stream; it uses
                        // `String::drain()` to drop processed bytes in-place.
                        for outcome in parse_sse_buffer(&mut buffer, &mut state) {
                            match outcome {
                                SseOutcome::Event(event) => {
                                    // Track the reasoning→answer transition
                                    // for reasoning_ms (first thinking delta →
                                    // first answer/tool delta).
                                    match &event {
                                        LlmEvent::ReasoningDelta { text } => {
                                            saw_reasoning = true;
                                            est_reasoning_chars += text.chars().count();
                                        }
                                        LlmEvent::TextDelta { text } => {
                                            est_completion_chars += text.chars().count();
                                            if first_content_chunk.is_none() {
                                                first_content_chunk = Some(now);
                                            }
                                        }
                                        LlmEvent::ToolCallStart { .. }
                                        | LlmEvent::ToolCallArgumentDelta { .. } => {
                                            if first_content_chunk.is_none() {
                                                first_content_chunk = Some(now);
                                            }
                                        }
                                        _ => {}
                                    }
                                    // R10: abort stuck loops. When ANY repeating
                                    // unit of ≤ 200 bytes spans the last 600
                                    // tail bytes, the model is stuck — emit an
                                    // error and stop to prevent token waste.
                                    if let LlmEvent::TextDelta { text } = &event {
                                        response_text.push_str(text);
                                        if detect_repetition(
                                            &response_text,
                                            REPETITION_WINDOW,
                                            REPETITION_THRESHOLD,
                                        ) {
                                            let msg = "repetition detected — \
                                                stream aborted to prevent token waste";
                                            if let Some((log, id)) = &trace_ctx {
                                                if let Some(fc) = first_chunk {
                                                    log.set_generation_ms(
                                                        *id,
                                                        fc.elapsed().as_millis() as u32,
                                                    );
                                                    log.set_stall_ms(*id, stall_tracker.ms());
                                                } else {
                                                    // No chunk: connect = record→headers,
                                                    // ttft = POST-send→error.
                                                    log.set_connect_ms(
                                                        *id,
                                                        request_start
                                                            .duration_since(record_created)
                                                            .as_millis() as u32,
                                                    );
                                                    log.set_ttft_ms(
                                                        *id,
                                                        post_sent.elapsed().as_millis() as u32,
                                                    );
                                                }
                                                log.fail(*id, status.as_u16(), msg);
                                            }
                                            let _ = tx
                                                .send(LlmEvent::Error {
                                                    error: msg.to_string(),
                                                })
                                                .await;
                                            return;
                                        }
                                        response_text = bound_repetition_buffer(
                                            response_text,
                                            REPETITION_WINDOW * REPETITION_THRESHOLD,
                                            REPETITION_BUFFER_CAP,
                                        );
                                    }
                                    // Enrich the Usage event with timing from
                                    // the stream loop (the parser leaves
                                    // ttft_ms/generation_ms None).
                                    let event = match event {
                                        LlmEvent::Usage {
                                            prompt_tokens,
                                            completion_tokens,
                                            reasoning_tokens,
                                            cached_tokens,
                                            ..
                                        } => {
                                            // TTFT = POST-send → first chunk
                                            // (perf review L1: headers arrive
                                            // WITH the first chunk for SSE
                                            // providers, so the old
                                            // headers→first-chunk window
                                            // measured ~0).
                                            let ttft_ms = first_chunk.map(|fc| {
                                                fc.duration_since(post_sent).as_millis() as u32
                                            });
                                            let generation_ms = first_chunk.map(|fc| {
                                                now.duration_since(fc).as_millis() as u32
                                            });
                                            // connect = record created → stream open
                                            // (the response headers arrived; includes
                                            // the full round trip).
                                            let connect_ms = request_start
                                                .duration_since(record_created)
                                                .as_millis()
                                                as u32;
                                            // reasoning = first chunk → first
                                            // answer/tool delta (or → the final
                                            // chunk when the model thought but
                                            // never answered).
                                            let reasoning_ms = if saw_reasoning {
                                                first_chunk.map(|fc| {
                                                    let end = first_content_chunk.unwrap_or(now);
                                                    end.duration_since(fc).as_millis() as u32
                                                })
                                            } else {
                                                None
                                            };
                                            // The authoritative usage landed — the
                                            // streaming overlay must never fire again.
                                            final_usage_seen = true;
                                            if let Some((log, id)) = &trace_ctx {
                                                log.set_usage(
                                                    *id,
                                                    LlmUsage {
                                                        prompt: prompt_tokens,
                                                        completion: completion_tokens,
                                                        reasoning: reasoning_tokens,
                                                        cached: cached_tokens,
                                                    },
                                                    ttft_ms,
                                                    generation_ms,
                                                    reasoning_ms,
                                                    Some(connect_ms),
                                                );
                                                // Split the byte-silence gaps
                                                // out of generation so the
                                                // graph shows actual
                                                // streaming vs. waiting.
                                                log.set_stall_ms(*id, stall_tracker.ms());
                                            }
                                            LlmEvent::Usage {
                                                prompt_tokens,
                                                completion_tokens,
                                                reasoning_tokens,
                                                cached_tokens,
                                                ttft_ms,
                                                generation_ms,
                                            }
                                        }
                                        other => {
                                            // Mirror terminal events into the
                                            // trace log (finish reason / error).
                                            match &other {
                                                LlmEvent::Finish { reason } => {
                                                    saw_finish = true;
                                                    if let Some((log, id)) = &trace_ctx {
                                                        log.finish(
                                                            *id,
                                                            &finish_reason_label(reason),
                                                        );
                                                        finish_logged = true;
                                                    }
                                                }
                                                LlmEvent::Error { error } => {
                                                    if let Some((log, id)) = &trace_ctx {
                                                        if let Some(fc) = first_chunk {
                                                            log.set_generation_ms(
                                                                *id,
                                                                fc.elapsed().as_millis() as u32,
                                                            );
                                                            log.set_stall_ms(
                                                                *id,
                                                                stall_tracker.ms(),
                                                            );
                                                        } else {
                                                            // No chunk: connect =
                                                            // record→headers, ttft =
                                                            // POST-send→error.
                                                            log.set_connect_ms(
                                                                *id,
                                                                request_start
                                                                    .duration_since(
                                                                        record_created,
                                                                    )
                                                                    .as_millis() as u32,
                                                            );
                                                            log.set_ttft_ms(
                                                                *id,
                                                                post_sent.elapsed().as_millis()
                                                                    as u32,
                                                            );
                                                        }
                                                        log.fail(*id, status.as_u16(), error);
                                                    }
                                                }
                                                _ => {}
                                            }
                                            other
                                        }
                                    };
                                    if tx.send(event).await.is_err() {
                                        // Receiver dropped — a USER interrupt
                                        // (Interrupt/Cancel/Compact/Clear), not
                                        // a provider failure (D1). Stamp the
                                        // terminal-cancelled marker: no error
                                        // text (so provider-errors.jsonl never
                                        // grows for a user cancel), the healthy
                                        // http_status preserved, and a genuine
                                        // stream error that landed just before
                                        // the drop is not masked.
                                        if let Some((log, id)) = &trace_ctx {
                                            if let Some(fc) = first_chunk {
                                                log.set_generation_ms(
                                                    *id,
                                                    fc.elapsed().as_millis() as u32,
                                                );
                                            } else {
                                                // No chunk: connect = record→headers,
                                                // ttft = POST-send→cancel.
                                                log.set_connect_ms(
                                                    *id,
                                                    request_start
                                                        .duration_since(record_created)
                                                        .as_millis() as u32,
                                                );
                                                log.set_ttft_ms(
                                                    *id,
                                                    post_sent.elapsed().as_millis() as u32,
                                                );
                                            }
                                            log.cancelled(*id, status.as_u16());
                                        }
                                        return;
                                    }
                                }
                                SseOutcome::ParseError(error) => {
                                    if let Some((log, id)) = &trace_ctx {
                                        if let Some(fc) = first_chunk {
                                            log.set_generation_ms(
                                                *id,
                                                fc.elapsed().as_millis() as u32,
                                            );
                                        } else {
                                            // No chunk: connect = record→headers,
                                            // ttft = POST-send→error.
                                            log.set_connect_ms(
                                                *id,
                                                request_start
                                                    .duration_since(record_created)
                                                    .as_millis() as u32,
                                            );
                                            log.set_ttft_ms(
                                                *id,
                                                post_sent.elapsed().as_millis() as u32,
                                            );
                                        }
                                        log.fail(*id, status.as_u16(), &error);
                                    }
                                    let _ = tx.send(LlmEvent::Error { error }).await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // reqwest's body-decode error ("error decoding
                        // response body") is a wrapper — walk the whole cause
                        // chain so the failure is debuggable.
                        let chain = error_chain(&e);
                        let partial = if buffer.is_empty() {
                            String::from("(no data received before the error)")
                        } else {
                            truncate_raw_stream(&buffer, 2000)
                        };
                        let error = format!(
                            "stream error (HTTP {status}): {e}\n\
                             cause chain: {chain}\n\
                             content-encoding: {content_encoding}\n\
                             transfer-encoding: {transfer_encoding}\n\
                             content-type: {content_type}\n\
                             partial stream data received so far:\n{partial}"
                        );
                        if let Some((log, id)) = &trace_ctx {
                            if let Some(fc) = first_chunk {
                                log.set_generation_ms(*id, fc.elapsed().as_millis() as u32);
                            } else {
                                // No chunk: connect = record→headers, ttft = POST-send→error.
                                log.set_connect_ms(
                                    *id,
                                    request_start
                                        .duration_since(record_created)
                                        .as_millis() as u32,
                                );
                                log.set_ttft_ms(*id, post_sent.elapsed().as_millis() as u32);
                            }
                            log.fail(*id, status.as_u16(), &error);
                        }
                        let _ = tx.send(LlmEvent::Error { error }).await;
                        return;
                    }
                }
            }
            // If the stream ended without a Finish event, emit a fallback —
            // but only when no real Finish was already delivered (a trailing
            // Stop over tool_use/max_tokens would corrupt the finish reason
            // the turn driver acts on). Mirroring into the trace log is gated
            // separately (`finish_logged`) so the fallback never overwrites
            // the real reason there either.
            if !saw_finish {
                if !finish_logged {
                    if let Some((log, id)) = &trace_ctx {
                        log.finish(*id, "stop");
                    }
                }
                let _ = tx
                    .send(LlmEvent::Finish {
                        reason: crate::provider::FinishReason::Stop,
                    })
                    .await;
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ImageUrl, ToolCall};

    /// A client with the default Anthropic capabilities (200k context, 32k
    /// output) and the given multimodal flag.
    fn client(multimodal: bool) -> AnthropicClient {
        AnthropicClient::new(AnthropicClientConfig {
            base_url: "https://api.anthropic.com/v1/".into(),
            api_key: "sk-test".into(),
            model: "claude-sonnet-4-5".into(),
            provider: "anthropic".into(),
            max_context: None,
            max_output_tokens: None,
            multimodal,
            workspace_id: None,
        })
    }

    #[test]
    fn build_request_json_malformed_tool_use_block_falls_through() {
        // H2 (Anthropic): the Messages API takes tool_use `input` as a JSON
        // object, not a string. A stored raw carrying the OpenAI string shape
        // must not be echoed — field construction parses it into an object.
        let client = client(false);
        let msg = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": [
                    { "type": "tool_use", "id": "t1", "name": "read", "input": "{\"x\":1}" }
                ]
            })),
            ..Message::assistant("calling", vec![ToolCall::new("t1", "read", "{\"x\":1}")])
        };
        let body = client.build_request_json(&[msg], &[], None).unwrap();
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        let tool_use = blocks
            .iter()
            .find(|b| b["type"] == serde_json::json!("tool_use"))
            .expect("a tool_use block");
        assert!(
            tool_use["input"].is_object(),
            "input must be an object, got {}",
            tool_use["input"]
        );
        assert_eq!(tool_use["input"]["x"], serde_json::json!(1));
    }

    #[test]
    fn build_request_json_repeated_block_type_falls_through() {
        // H3 (Anthropic): a self-concatenated discriminator is the fingerprint
        // of a delta merge that appended an identity field instead of setting
        // it once. Defense-in-depth for stored payloads — this client cannot
        // produce the shape itself (its accumulator uses an allowlist).
        let client = client(false);
        let msg = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": [{ "type": "texttext", "text": "answer" }]
            })),
            ..Message::assistant_text("answer")
        };
        let body = client.build_request_json(&[msg], &[], None).unwrap();
        assert_eq!(
            body["messages"][0]["content"][0]["type"],
            serde_json::json!("text"),
            "corrupted block type must not be echoed"
        );
    }

    #[test]
    fn build_request_json_unknown_block_type_still_echoed() {
        // The guard rejects only genuinely malformed blocks, never merely
        // unfamiliar ones: an unknown-but-well-formed type (a future
        // server-side tool, say) still round-trips verbatim per Rule 1. This
        // is the more important half of the guard — over-rejecting here would
        // silently drop provider state.
        let client = client(false);
        let raw_content = serde_json::json!([
            { "type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"q": "x"} },
            { "type": "text", "text": "answer" }
        ]);
        let msg = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": raw_content.clone()
            })),
            ..Message::assistant_text("answer")
        };
        let body = client.build_request_json(&[msg], &[], None).unwrap();
        // Breakpoint #3 lands on the last markable block of the final
        // message (the text block here); the unknown-but-well-formed
        // server_tool_use block still echoes verbatim.
        assert_eq!(
            &body["messages"][0]["content"],
            &serde_json::json!([
                { "type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"q": "x"} },
                { "type": "text", "text": "answer", "cache_control": { "type": "ephemeral" } }
            ])
        );
    }

    #[test]
    fn build_request_json_echoes_raw_content_array_verbatim() {
        // Rule 1 + Rule 4: the verbatim content array (with thinking signature
        // and redacted_thinking) is echoed unchanged — filtering
        // redacted_thinking causes a 400.
        let client = AnthropicClient::new(AnthropicClientConfig {
            base_url: "https://api.anthropic.com/v1/".into(),
            api_key: "sk-test".into(),
            model: "claude-sonnet-4-5".into(),
            provider: "anthropic".into(),
            max_context: None,
            max_output_tokens: None,
            multimodal: false,
            workspace_id: None,
        });
        let raw_content = serde_json::json!([
            { "type": "thinking", "thinking": "reasoning", "signature": "sig" },
            { "type": "redacted_thinking", "data": "<enc>" },
            { "type": "text", "text": "answer" },
            { "type": "tool_use", "id": "t1", "name": "read", "input": {"x": 1} }
        ]);
        let msg = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": raw_content.clone()
            })),
            ..Message::assistant_text("answer")
        };
        let body = client.build_request_json(&[msg], &[], None).unwrap();
        let echoed = &body["messages"][0]["content"];
        // Breakpoint #3 lands on the last MARKABLE block (tool_use here):
        // thinking / redacted_thinking cannot be marked directly, so the
        // stamp walks back past them; everything else echoes verbatim.
        assert_eq!(
            echoed,
            &serde_json::json!([
                { "type": "thinking", "thinking": "reasoning", "signature": "sig" },
                { "type": "redacted_thinking", "data": "<enc>" },
                { "type": "text", "text": "answer" },
                { "type": "tool_use", "id": "t1", "name": "read", "input": {"x": 1},
                  "cache_control": { "type": "ephemeral" } }
            ])
        );
        // redacted_thinking preserved (Rule 4: filtering it causes a 400).
        assert_eq!(echoed[1]["type"], serde_json::json!("redacted_thinking"));
        // thinking signature preserved verbatim.
        assert_eq!(echoed[0]["signature"], serde_json::json!("sig"));
        // Thinking blocks carry no cache_control (they cannot be marked).
        assert!(echoed[0].get("cache_control").is_none());
        assert!(echoed[1].get("cache_control").is_none());
    }

    #[test]
    fn build_request_json_drops_empty_thinking_block_from_echoed_history() {
        // claude-opus-5 400 (backlog bd5eb19d, provider-errors ids 1022-1030 +
        // 879-888): "messages.3.content.0.thinking: each thinking block must
        // contain thinking". A stream whose thinking block never received a
        // thinking_delta leaves the captured block with thinking:"" (the
        // content_block_start payload); echoing it verbatim is a guaranteed
        // non-retryable 400 that poisons the conversation — every retry fails
        // identically. The echo must drop the always-invalid block while the
        // valid thinking block (with content + signature) and text blocks
        // still echo verbatim (Rule 1/Rule 4 keep applying to VALID blocks).
        let client = AnthropicClient::new(AnthropicClientConfig {
            base_url: "https://api.anthropic.com/v1/".into(),
            api_key: "sk-test".into(),
            model: "claude-opus-5".into(),
            provider: "anthropic".into(),
            max_context: None,
            max_output_tokens: None,
            multimodal: false,
            workspace_id: None,
        });
        let msg = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "", "signature": "sig-empty" },
                    { "type": "thinking", "thinking": "real reasoning", "signature": "sig-valid" },
                    { "type": "text", "text": "raw answer text" }
                ]
            })),
            ..Message::assistant_text("answer")
        };
        let body = client.build_request_json(&[msg], &[], None).unwrap();
        assert_eq!(
            &body["messages"][0]["content"],
            &serde_json::json!([
                { "type": "thinking", "thinking": "real reasoning", "signature": "sig-valid" },
                { "type": "text", "text": "raw answer text",
                  "cache_control": { "type": "ephemeral" } }
            ]),
            "the empty thinking block must be dropped (Anthropic 400s 'each \
             thinking block must contain thinking') while valid thinking and \
             text blocks echo verbatim; breakpoint #3 lands on the text block"
        );
    }

    #[test]
    fn build_request_json_retains_thinking_signatures_across_tool_calls() {
        // Acceptance test #1 (Anthropic): sequential tool call — the second
        // outbound request still contains the provider's reasoning field
        // (thinking blocks with signatures) with the same value the model
        // returned.
        let client = AnthropicClient::new(AnthropicClientConfig {
            base_url: "https://api.anthropic.com/v1/".into(),
            api_key: "sk-test".into(),
            model: "claude-sonnet-4-5".into(),
            provider: "anthropic".into(),
            max_context: None,
            max_output_tokens: None,
            multimodal: false,
            workspace_id: None,
        });
        let assistant1 = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "first reasoning", "signature": "sig1" },
                    { "type": "text", "text": "" },
                    { "type": "tool_use", "id": "t1", "name": "read", "input": {"x": 1} }
                ]
            })),
            ..Message::assistant_text("")
        };
        let tool1 = Message::tool_result("t1", "read", "42");
        let assistant2 = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "second reasoning", "signature": "sig2" },
                    { "type": "text", "text": "" },
                    { "type": "tool_use", "id": "t2", "name": "write", "input": {"y": 2} }
                ]
            })),
            ..Message::assistant_text("")
        };
        let tool2 = Message::tool_result("t2", "write", "ok");
        let body = client
            .build_request_json(&[assistant1, tool1, assistant2, tool2], &[], None)
            .unwrap();
        // Both assistant turns retain their thinking signatures verbatim.
        let content0 = &body["messages"][0]["content"];
        assert_eq!(content0[0]["type"], serde_json::json!("thinking"));
        assert_eq!(content0[0]["signature"], serde_json::json!("sig1"));
        assert_eq!(content0[0]["thinking"], serde_json::json!("first reasoning"));
        let content2 = &body["messages"][2]["content"];
        assert_eq!(content2[0]["type"], serde_json::json!("thinking"));
        assert_eq!(content2[0]["signature"], serde_json::json!("sig2"));
        assert_eq!(content2[0]["thinking"], serde_json::json!("second reasoning"));
    }

    // ── build_request_json ────────────────────────────────────────────────

    #[test]
    fn hoists_system_messages_and_leaves_none_in_messages() {
        let body = client(false)
            .build_request_json(
                &[
                    Message::text(Role::System, "You are helpful."),
                    Message::user_text("hi"),
                    Message::text(Role::System, "Be concise."),
                ],
                &[],
                None,
            )
            .unwrap();
        // Only the FIRST system message is hoisted into `system` (breakpoint
        // 1); the later one (the volatile tail) is relocated into the final
        // message's content instead of joining `system`.
        assert_eq!(
            body["system"],
            serde_json::json!([
                {
                    "type": "text",
                    "text": "You are helpful.",
                    "cache_control": { "type": "ephemeral" }
                }
            ])
        );
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "only the user message remains");
        assert_eq!(messages[0]["role"], serde_json::json!("user"));
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "user text + relocated volatile tail");
        assert_eq!(content[0]["text"], serde_json::json!("hi"));
        assert_eq!(
            content[0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" }),
            "breakpoint #3 on the last real block"
        );
        assert_eq!(content[1]["text"], serde_json::json!("Be concise."));
        assert!(content[1].get("cache_control").is_none());
    }

    #[test]
    fn omits_system_field_when_no_system_messages() {
        let body = client(false)
            .build_request_json(&[Message::user_text("hi")], &[], None)
            .unwrap();
        assert!(body.get("system").is_none(), "no system field when empty");
    }

    #[test]
    fn rejects_empty_non_system_messages() {
        let err = client(false)
            .build_request_json(&[Message::text(Role::System, "sys")], &[], None)
            .unwrap_err();
        assert!(err.to_string().contains("no non-system messages"));
    }

    #[test]
    fn max_tokens_uses_capability_budget() {
        let body = client(false)
            .build_request_json(&[Message::user_text("hi")], &[], None)
            .unwrap();
        // Default Anthropic caps: 32k output → max_tokens = 32_000.
        assert_eq!(body["max_tokens"], serde_json::json!(32_000));
        assert_eq!(body["model"], serde_json::json!("claude-sonnet-4-5"));
        assert_eq!(body["stream"], serde_json::json!(true));
    }

    #[test]
    fn workspace_headers_set_only_when_configured() {
        // Regression (2026-12-06): the workspace id must ride as the
        // anthropic-workspace-id HTTP header (LiteLLM #29272 — body fields
        // are invalid), never sent empty, and be absent entirely when unset.
        let with = AnthropicClient::new(AnthropicClientConfig {
            base_url: "https://api.anthropic.com/v1/".into(),
            api_key: "sk-test".into(),
            model: "claude-sonnet-4-5".into(),
            provider: "anthropic".into(),
            max_context: None,
            max_output_tokens: None,
            multimodal: false,
            workspace_id: Some("ws_abc".into()),
        });
        let headers = with.workspace_headers();
        assert_eq!(
            headers.get("anthropic-workspace-id").unwrap(),
            "ws_abc",
            "configured workspace id must become the header value"
        );
        let without = client(false);
        assert!(
            without.workspace_headers().is_empty(),
            "unset workspace id must omit the header entirely (never empty)"
        );
        let blank = AnthropicClient::new(AnthropicClientConfig {
            workspace_id: Some("   ".into()),
            ..with.config.clone()
        });
        assert!(
            blank.workspace_headers().is_empty(),
            "whitespace-only workspace id must omit the header"
        );
    }

    #[test]
    fn max_tokens_capped_at_sane_ceiling() {
        // A large-context endpoint (1M context) with a huge output budget
        // (131_072) would send 128K output tokens — wasteful for coding
        // tasks. The sane ceiling (32K) kicks in even though the R9
        // context-window cap never triggers (1M − tiny prompt − 1024 ≫ 131_072).
        let client = AnthropicClient::new(AnthropicClientConfig {
            base_url: "https://api.anthropic.com/v1/".into(),
            api_key: "sk-test".into(),
            model: "claude-sonnet-4-5".into(),
            provider: "anthropic".into(),
            max_context: Some(1_000_000),
            max_output_tokens: Some(131_072),
            multimodal: false,
            workspace_id: None,
        });
        let body = client.build_request_json(&[Message::user_text("hi")], &[], None).unwrap();
        assert_eq!(
            body["max_tokens"],
            serde_json::json!(32_000),
            "sane ceiling should cap 131_072 → 32_000"
        );
    }

    #[test]
    fn max_tokens_quantized_when_prompt_large() {
        let client = AnthropicClient::new(AnthropicClientConfig {
            base_url: "https://api.anthropic.com/v1/".into(),
            api_key: "sk-test".into(),
            model: "claude-sonnet-4-5".into(),
            provider: "anthropic".into(),
            max_context: Some(10_000),
            max_output_tokens: Some(8_000),
            multimodal: false,
            workspace_id: None,
        });
        // 6000 chars -> prompt_est 1504 -> context_budget = 10000 - 1504 - 1024 = 7472 -> quantized 6144
        let body = client
            .build_request_json(&[Message::user_text("x".repeat(6_000))], &[], None)
            .unwrap();
        assert_eq!(
            body["max_tokens"],
            serde_json::json!(6144),
            "should cap to context window and quantize to 2048-token bucket"
        );
    }

    #[test]
    fn tool_choice_variants() {
        let c = client(false);
        let mk = |tc: Option<ToolChoice>| c.build_request_json(&[Message::user_text("hi")], &[], tc).unwrap();
        // Auto → field omitted entirely (the API default).
        assert!(mk(Some(ToolChoice::Auto)).get("tool_choice").is_none());
        // Function(name) → {"type":"tool","name":...}.
        assert_eq!(
            mk(Some(ToolChoice::Function("read_file".into())))["tool_choice"],
            serde_json::json!({ "type": "tool", "name": "read_file" })
        );
        // Required → {"type":"any"}.
        assert_eq!(
            mk(Some(ToolChoice::Required))["tool_choice"],
            serde_json::json!({ "type": "any" })
        );
        // None → omitted.
        assert!(mk(None).get("tool_choice").is_none());
    }

    #[test]
    fn tools_render_input_schema_without_strict() {
        let tools = vec![ToolSchema::new(
            "read_file",
            "Read a file",
            serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        )];
        let body = client(false)
            .build_request_json(&[Message::user_text("hi")], &tools, None)
            .unwrap();
        let t = &body["tools"][0];
        assert_eq!(t["name"], serde_json::json!("read_file"));
        assert_eq!(t["description"], serde_json::json!("Read a file"));
        assert_eq!(
            t["input_schema"]["properties"]["path"]["type"],
            serde_json::json!("string")
        );
        // `strict` must not leak into the Anthropic shape (it is a
        // ToolSchema field but the Messages API has no strict enforcement).
        assert!(t.get("strict").is_none());
        // The last (and only) tool carries ephemeral cache_control.
        assert_eq!(t["cache_control"], serde_json::json!({ "type": "ephemeral" }));
    }

    #[test]
    fn last_tool_has_ephemeral_cache_control() {
        let tools = vec![
            ToolSchema::new(
                "first_tool",
                "First",
                serde_json::json!({ "type": "object" }),
            ),
            ToolSchema::new(
                "last_tool",
                "Last",
                serde_json::json!({ "type": "object" }),
            ),
        ];
        let body = client(false)
            .build_request_json(&[Message::user_text("hi")], &tools, None)
            .unwrap();
        let tools_arr = body["tools"].as_array().unwrap();
        assert_eq!(tools_arr.len(), 2);
        assert!(
            tools_arr[0].get("cache_control").is_none(),
            "first tool must not have cache_control"
        );
        assert_eq!(
            tools_arr[1]["cache_control"],
            serde_json::json!({ "type": "ephemeral" }),
            "last tool must have ephemeral cache_control"
        );
    }

    #[test]
    fn volatile_tail_relocates_into_final_message_and_breakpoint_lands_on_last_real_block() {
        // Breakpoint #3 (conversation history): the volatile tail (later
        // system messages) is relocated into the final message's content as
        // trailing text blocks, and the breakpoint lands on the last REAL
        // block — the cached prefix covers tools + system head + history +
        // the current turn's content, while the tail stays the uncached
        // varying suffix (the documented "static prefix + varying suffix"
        // shape). The old design (tail in `system`, no message breakpoint)
        // re-billed the whole history at full input price every turn.
        let history = vec![
            Message::text(Role::System, "Stable head."),
            Message::user_text("Turn 0: user prompt"),
            Message::text(Role::Assistant, "Turn 1: assistant answer"),
            Message::text(Role::System, "Volatile tail: workflow state."),
            Message::user_text("Turn 2: user follow-up"),
        ];
        let body = client(false)
            .build_request_json(&history, &[], None)
            .unwrap();
        // system holds ONLY the stable head (breakpoint 1).
        assert_eq!(
            body["system"],
            serde_json::json!([
                {
                    "type": "text",
                    "text": "Stable head.",
                    "cache_control": { "type": "ephemeral" }
                }
            ])
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        // Earlier messages carry no breakpoint...
        for msg in &msgs[..2] {
            for block in msg["content"].as_array().unwrap() {
                assert!(
                    block.get("cache_control").is_none(),
                    "history blocks must not carry cache_control"
                );
            }
        }
        // ...the final message's last REAL block carries breakpoint #3...
        let final_content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(final_content.len(), 2);
        assert_eq!(
            final_content[0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" }),
            "breakpoint #3 on the last real block of the final message"
        );
        // ...and the relocated volatile tail follows it, uncached.
        assert_eq!(
            final_content[1]["text"],
            serde_json::json!("Volatile tail: workflow state.")
        );
        assert!(final_content[1].get("cache_control").is_none());
    }

    #[test]
    fn breakpoint_lands_on_last_tool_result_block_with_tail_after_it() {
        // Mid-turn request shape: the final message is the coalesced
        // tool-result carrier. Breakpoint #3 lands on the LAST tool_result
        // block (the docs' tool-use caching example marks exactly this
        // block); the relocated volatile tail follows it, uncached.
        let history = vec![
            Message::text(Role::System, "Stable head."),
            Message::user_text("Find the answer."),
            {
                let mut m = Message::text(Role::Assistant, "");
                m.tool_calls = vec![
                    ToolCall::new("t1", "read", r#"{"path":"a"}"#),
                    ToolCall::new("t2", "read", r#"{"path":"b"}"#),
                ];
                m
            },
            Message::tool_result("t1", "read", "one"),
            Message::tool_result("t2", "read", "two"),
            Message::text(Role::System, "Volatile tail."),
        ];
        let body = client(false)
            .build_request_json(&history, &[], None)
            .unwrap();
        let msgs = body["messages"].as_array().unwrap();
        // [user prompt, assistant tool_use, coalesced tool-result carrier]
        assert_eq!(msgs.len(), 3);
        let carrier = msgs[2]["content"].as_array().unwrap();
        assert_eq!(
            carrier.len(),
            3,
            "two tool_result blocks + the relocated tail"
        );
        assert_eq!(carrier[0]["type"], serde_json::json!("tool_result"));
        assert_eq!(carrier[1]["type"], serde_json::json!("tool_result"));
        assert_eq!(
            carrier[1]["cache_control"],
            serde_json::json!({ "type": "ephemeral" }),
            "breakpoint #3 on the last tool_result block"
        );
        assert_eq!(carrier[2]["type"], serde_json::json!("text"));
        assert_eq!(carrier[2]["text"], serde_json::json!("Volatile tail."));
        assert!(carrier[2].get("cache_control").is_none());
    }

    #[test]
    fn breakpoint_lands_on_final_block_when_no_volatile_tail() {
        // No later system messages → nothing to relocate; breakpoint #3
        // lands directly on the final message's last (only) block.
        let history = vec![
            Message::user_text("Turn 0"),
            Message::text(Role::Assistant, "Turn 1"),
            Message::user_text("Turn 2"),
        ];
        let body = client(false)
            .build_request_json(&history, &[], None)
            .unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        for msg in &msgs[..2] {
            assert!(msg["content"][0].get("cache_control").is_none());
        }
        assert_eq!(
            msgs[2]["content"][0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
    }

    #[test]
    fn breakpoint_stays_within_lookback_window_on_long_tool_history() {
        // The 20-block lookback constraint: the next request's breakpoint
        // walks back at most 20 positions to find this request's write. Our
        // placement (the final message's last real block) is always within a
        // few blocks of the end — the assistant reply + the coalesced
        // tool_use/tool_result runs (each run counts as ONE position) + the
        // new content. Assert the breakpoint sits on the final message's
        // last real block even after many tool-heavy turns.
        let mut history = vec![Message::text(Role::System, "Stable head.")];
        for i in 0..12 {
            history.push(Message::user_text(format!("prompt {i}")));
            let mut m = Message::text(Role::Assistant, "");
            m.tool_calls = vec![ToolCall::new(format!("t{i}"), "read", "{}")];
            history.push(m);
            history.push(Message::tool_result(format!("t{i}"), "read", "ok"));
        }
        history.push(Message::user_text("final prompt"));
        history.push(Message::text(Role::System, "Volatile tail."));
        let body = client(false)
            .build_request_json(&history, &[], None)
            .unwrap();
        let msgs = body["messages"].as_array().unwrap();
        // 12 turns x (user + assistant + tool-result carrier) + final user.
        assert_eq!(msgs.len(), 12 * 3 + 1);
        let final_content = msgs[msgs.len() - 1]["content"].as_array().unwrap();
        assert_eq!(final_content.len(), 2, "real text block + relocated tail");
        assert_eq!(
            final_content[0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" }),
            "breakpoint #3 on the last real block — never 20+ blocks from the end"
        );
        assert!(final_content[1].get("cache_control").is_none());
    }

    #[test]
    fn assistant_tool_calls_round_trip_as_tool_use_blocks() {
        let mut m = Message::text(Role::Assistant, "Let me check.");
        m.tool_calls = vec![ToolCall::new("toolu_01", "read_file", r#"{"path":"/tmp/x.txt"}"#)];
        let body = client(false)
            .build_request_json(&[Message::user_text("hi"), m], &[], None)
            .unwrap();
        let content = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], serde_json::json!("text"));
        assert_eq!(content[1]["type"], serde_json::json!("tool_use"));
        assert_eq!(content[1]["id"], serde_json::json!("toolu_01"));
        assert_eq!(content[1]["name"], serde_json::json!("read_file"));
        // Arguments are a JSON *object* on the wire (not a string).
        assert_eq!(
            content[1]["input"],
            serde_json::json!({ "path": "/tmp/x.txt" })
        );
    }

    #[test]
    fn malformed_tool_arguments_fall_back_to_empty_object() {
        let mut m = Message::text(Role::Assistant, "");
        m.tool_calls = vec![ToolCall::new("toolu_02", "search", r#"{not valid json"#)];
        let body = client(false)
            .build_request_json(&[Message::user_text("hi"), m], &[], None)
            .unwrap();
        let content = body["messages"][1]["content"].as_array().unwrap();
        let tool_use = content
            .iter()
            .find(|b| b["type"] == serde_json::json!("tool_use"))
            .unwrap();
        assert_eq!(tool_use["input"], serde_json::json!({}));
    }

    #[test]
    fn tool_results_map_to_user_role_tool_result_blocks() {
        let mut m = Message::text(Role::Tool, "file contents here");
        m.tool_call_id = Some("toolu_01".into());
        let body = client(false)
            .build_request_json(&[Message::user_text("hi"), m], &[], None)
            .unwrap();
        let last = &body["messages"][1];
        assert_eq!(last["role"], serde_json::json!("user"));
        let block = &last["content"][0];
        assert_eq!(block["type"], serde_json::json!("tool_result"));
        assert_eq!(block["tool_use_id"], serde_json::json!("toolu_01"));
        assert_eq!(block["content"], serde_json::json!("file contents here"));
        assert_eq!(block["is_error"], serde_json::json!(false));
    }

    #[test]
    fn parallel_tool_results_coalesce_into_one_user_message() {
        // A parallel tool batch (supports_parallel_tools) pushes N adjacent
        // tool messages; the canonical Messages-API shape is ONE user message
        // carrying all N tool_result blocks in order — not N consecutive
        // user messages (a non-canonical shape that can 400 on strict
        // Anthropic-compatible gateways).
        let mut t1 = Message::text(Role::Tool, "first result");
        t1.tool_call_id = Some("toolu_01".into());
        let mut t2 = Message::text(Role::Tool, "second result");
        t2.tool_call_id = Some("toolu_02".into());
        let body = client(false)
            .build_request_json(&[Message::user_text("hi"), t1, t2], &[], None)
            .unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(
            messages.len(),
            2,
            "user + one coalesced tool-result message"
        );
        let last = &messages[1];
        assert_eq!(last["role"], serde_json::json!("user"));
        let content = last["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "both tool_result blocks in one message");
        assert_eq!(content[0]["type"], serde_json::json!("tool_result"));
        assert_eq!(content[0]["tool_use_id"], serde_json::json!("toolu_01"));
        assert_eq!(content[0]["content"], serde_json::json!("first result"));
        assert_eq!(content[1]["type"], serde_json::json!("tool_result"));
        assert_eq!(content[1]["tool_use_id"], serde_json::json!("toolu_02"));
        assert_eq!(content[1]["content"], serde_json::json!("second result"));
    }

    #[test]
    fn tool_results_separated_by_text_do_not_coalesce() {
        // Coalescing must only merge ADJACENT tool messages — a user message
        // between two tool results keeps them as separate user messages.
        let mut t1 = Message::text(Role::Tool, "first");
        t1.tool_call_id = Some("toolu_01".into());
        let mut t2 = Message::text(Role::Tool, "second");
        t2.tool_call_id = Some("toolu_02".into());
        let body = client(false)
            .build_request_json(&[Message::user_text("hi"), t1, Message::user_text("between"), t2], &[], None)
            .unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(
            messages[1]["content"][0]["tool_use_id"],
            serde_json::json!("toolu_01")
        );
        assert_eq!(messages[2]["role"], serde_json::json!("user"));
        assert_eq!(messages[2]["content"][0]["type"], serde_json::json!("text"));
        assert_eq!(
            messages[3]["content"][0]["tool_use_id"],
            serde_json::json!("toolu_02")
        );
    }

    #[test]
    fn tool_message_without_id_errors() {
        let m = Message::text(Role::Tool, "x");
        let err = client(false)
            .build_request_json(&[Message::user_text("hi"), m], &[], None)
            .unwrap_err();
        assert!(err.to_string().contains("tool_call_id"));
    }

    #[test]
    fn empty_assistant_message_errors() {
        let m = Message::text(Role::Assistant, "");
        let err = client(false)
            .build_request_json(&[Message::user_text("hi"), m], &[], None)
            .unwrap_err();
        assert!(err.to_string().contains("assistant message has no text"));
    }

    #[test]
    fn image_base64_data_url_becomes_base64_block() {
        let url = "data:image/png;base64,iVBORw0KGgo=";
        let parts = vec![crate::provider::ContentPart::ImageUrl {
            image_url: ImageUrl { url: url.into() },
        }];
        let body = client(true)
            .build_request_json(
                &[Message {
                    content: MessageContent::Parts(parts),
                    ..Message::user_text("")
                }],
                &[],
                None,
            )
            .unwrap();
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], serde_json::json!("image"));
        assert_eq!(block["source"]["type"], serde_json::json!("base64"));
        assert_eq!(
            block["source"]["media_type"],
            serde_json::json!("image/png")
        );
        assert_eq!(block["source"]["data"], serde_json::json!("iVBORw0KGgo="));
    }

    #[test]
    fn image_plain_url_becomes_url_block() {
        let url = "https://example.com/pic.jpg";
        let parts = vec![crate::provider::ContentPart::ImageUrl {
            image_url: ImageUrl { url: url.into() },
        }];
        let body = client(true)
            .build_request_json(
                &[Message {
                    content: MessageContent::Parts(parts),
                    ..Message::user_text("")
                }],
                &[],
                None,
            )
            .unwrap();
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["source"]["type"], serde_json::json!("url"));
        assert_eq!(block["source"]["url"], serde_json::json!(url));
    }

    #[test]
    fn images_stripped_when_not_multimodal() {
        let parts = vec![
            crate::provider::ContentPart::Text {
                text: "what is this?".into(),
            },
            crate::provider::ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,AAAA".into(),
                },
            },
        ];
        let body = client(false)
            .build_request_json(
                &[Message {
                    content: MessageContent::Parts(parts),
                    ..Message::user_text("")
                }],
                &[],
                None,
            )
            .unwrap();
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "image block stripped");
        assert_eq!(content[0]["type"], serde_json::json!("text"));
        assert_eq!(content[0]["text"], serde_json::json!("what is this?"));
    }

    #[test]
    fn assistant_reasoning_is_not_echoed_as_an_unsigned_thinking_block() {
        // Regression (2026-12-19): the request builder used to fabricate
        // `{"type":"thinking","thinking":…}` from stored reasoning_content on
        // every prior assistant message. Anthropic requires any thinking block
        // sent in conversation history to carry the original per-block
        // `signature` (which the stream parser never captures), so the
        // fabricated unsigned echo made every multi-turn request with stored
        // reasoning fail with HTTP 400 `thinking.signature: Field required`.
        // The echo must NOT be serialized on the Anthropic path.
        let mut m = Message::text(Role::Assistant, "answer");
        m.reasoning_content = Some("thinking step".into());
        let body = client(false)
            .build_request_json(&[Message::user_text("hi"), m], &[], None)
            .unwrap();
        let content = body["messages"][1]["content"].as_array().unwrap();
        assert!(
            !content
                .iter()
                .any(|b| b["type"] == serde_json::json!("thinking")),
            "no fabricated thinking block may be sent: {content:?}"
        );
        assert_eq!(content[0]["type"], serde_json::json!("text"));
        assert_eq!(content[0]["text"], serde_json::json!("answer"));
    }

    // ── parse_sse_buffer / parse_sse_event ────────────────────────────────

    fn sse(events: &[(&str, &str)]) -> String {
        events
            .iter()
            .map(|(name, data)| format!("event: {name}\ndata: {data}\n"))
            .collect()
    }

    fn parse_all(s: &str) -> (Vec<LlmEvent>, StreamState) {
        let mut state = StreamState::default();
        let mut buffer = s.to_string();
        let outcomes = parse_sse_buffer(&mut buffer, &mut state);
        let events: Vec<LlmEvent> = outcomes
            .into_iter()
            .map(|o| match o {
                SseOutcome::Event(e) => e,
                SseOutcome::ParseError(e) => panic!("parse error: {e}"),
            })
            .collect();
        (events, state)
    }

    #[test]
    fn parses_canonical_stream_fixture() {
        let raw = sse(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":25,"output_tokens":1}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"read_file","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"/tmp/x\"}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":1}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":30}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let (events, state) = parse_all(&raw);

        // Text deltas in order.
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                LlmEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Hello", " world"]);

        // Tool call start + argument fragments.
        let starts: Vec<&LlmEvent> = events
            .iter()
            .filter(|e| matches!(e, LlmEvent::ToolCallStart { .. }))
            .collect();
        assert_eq!(starts.len(), 1);
        match &starts[0] {
            LlmEvent::ToolCallStart { index, id, name } => {
                assert_eq!(*index, 1);
                assert_eq!(id, "toolu_01");
                assert_eq!(name, "read_file");
            }
            _ => unreachable!(),
        }
        let fragments: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                LlmEvent::ToolCallArgumentDelta { index, fragment } => {
                    assert_eq!(*index, 1);
                    Some(fragment.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(fragments.len(), 2);

        // Finish from stop_reason=tool_use, then Usage with the split counts.
        match &events[events.len() - 2] {
            LlmEvent::Finish { reason } => assert_eq!(*reason, FinishReason::ToolCalls),
            other => panic!("expected Finish, got {other:?}"),
        }
        match &events[events.len() - 1] {
            LlmEvent::Usage {
                prompt_tokens,
                completion_tokens,
                ttft_ms,
                generation_ms,
                ..
            } => {
                assert_eq!(*prompt_tokens, 25, "input from message_start");
                assert_eq!(*completion_tokens, 30, "output from message_delta");
                assert_eq!(*ttft_ms, None);
                assert_eq!(*generation_ms, None);
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert_eq!(state.input_tokens, 25);
        assert_eq!(state.output_tokens, 30);
        assert_eq!(state.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn usage_includes_anthropic_cache_tokens() {
        // The 3-breakpoint caching setup (plan ee33d615) makes Anthropic
        // report cache activity in message_start.usage: input_tokens
        // EXCLUDES the cached prefix, cache_read_input_tokens is the hit,
        // and cache_creation_input_tokens is written to the cache this
        // request (backlog 648051bf).
        let raw = sse(&[
            (
                "message_start",
                r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":30}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let (events, state) = parse_all(&raw);
        match &events[events.len() - 1] {
            LlmEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                ..
            } => {
                // Parity with the OpenAI path: prompt_tokens carries ALL
                // billed prompt tokens (fresh + cache-read + cache-creation).
                assert_eq!(
                    *prompt_tokens, 130,
                    "input + cache_read + cache_creation"
                );
                assert_eq!(*cached_tokens, 100, "the cache-read hit only");
                assert_eq!(*completion_tokens, 30, "output from message_delta");
            }
            other => panic!("expected Usage, got {other:?}"),
        }
        assert_eq!(state.input_tokens, 25);
        assert_eq!(state.cache_read_tokens, 100);
        assert_eq!(state.cache_creation_tokens, 5);
    }

    #[test]
    fn thinking_delta_emits_reasoning_delta() {
        let raw = sse(&[
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            ),
        ]);
        let (events, _) = parse_all(&raw);
        let reasoning: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                LlmEvent::ReasoningDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, vec!["hmm"]);
    }

    #[test]
    fn error_event_emits_error() {
        let raw = sse(&[(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )]);
        let (events, _) = parse_all(&raw);
        assert_eq!(events.len(), 1);
        match &events[0] {
            LlmEvent::Error { error } => assert_eq!(error, "Overloaded"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn stop_reason_mapping() {
        let mk = |reason: &str| {
            let raw = sse(&[
                (
                    "message_delta",
                    &format!(
                        r#"{{"type":"message_delta","delta":{{"stop_reason":"{reason}"}},"usage":{{"output_tokens":5}}}}"#
                    ),
                ),
                ("message_stop", r#"{"type":"message_stop"}"#),
            ]);
            let (events, _) = parse_all(&raw);
            match &events[0] {
                LlmEvent::Finish { reason } => reason.clone(),
                other => panic!("expected Finish, got {other:?}"),
            }
        };
        assert_eq!(mk("end_turn"), FinishReason::Stop);
        assert_eq!(mk("stop_sequence"), FinishReason::Stop);
        assert_eq!(mk("tool_use"), FinishReason::ToolCalls);
        assert_eq!(mk("max_tokens"), FinishReason::Length);
        assert_eq!(mk("pause_turn"), FinishReason::Other("pause_turn".into()));
    }

    #[test]
    fn unknown_event_types_are_ignored() {
        let raw = sse(&[("ping", r#"{"type":"ping","foo":"bar"}"#)]);
        let (events, _) = parse_all(&raw);
        assert!(events.is_empty(), "ping must be ignored");
    }

    #[test]
    fn retains_incomplete_trailing_line() {
        let mut buffer = String::from(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}\n\
             event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"inde",
        );
        let mut state = StreamState::default();
        let outcomes = parse_sse_buffer(&mut buffer, &mut state);
        // The first frame (message_start) yields no events — it only captures
        // state — so the real assertion is on the captured input tokens.
        assert!(outcomes.is_empty());
        assert_eq!(state.input_tokens, 7);
        // The incomplete trailing frame (including its event: line) remains.
        assert!(buffer.contains("content_block_delta"));
        assert!(buffer.contains("content_block_delta\",\"inde"));
    }

    #[test]
    fn multi_line_chunk_parses_all_frames() {
        let mut buffer = String::new();
        buffer.push_str("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3}}}\n");
        buffer.push_str("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n");
        let mut state = StreamState::default();
        let outcomes = parse_sse_buffer(&mut buffer, &mut state);
        assert_eq!(outcomes.len(), 1);
        assert!(buffer.is_empty(), "buffer fully drained");
        assert_eq!(state.input_tokens, 3);
    }

    #[test]
    fn parse_errors_report_raw_data() {
        let mut buffer = String::from("event: message_start\ndata: {not valid json}\n");
        let mut state = StreamState::default();
        let outcomes = parse_sse_buffer(&mut buffer, &mut state);
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            SseOutcome::ParseError(msg) => {
                assert!(msg.contains("failed to parse Anthropic SSE chunk"));
                assert!(msg.contains("not valid json"));
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
        assert!(buffer.is_empty());
    }

    #[test]
    fn detect_repetition_fires_on_repeating_stream() {
        // Build a 200-char segment and repeat it 3× — the guard must fire.
        let segment = "a".repeat(REPETITION_WINDOW);
        let repeating = segment.repeat(REPETITION_THRESHOLD);
        assert!(detect_repetition(
            &repeating,
            REPETITION_WINDOW,
            REPETITION_THRESHOLD
        ));

        // 2× repetitions should NOT fire (below threshold).
        let too_few = segment.repeat(REPETITION_THRESHOLD - 1);
        assert!(!detect_repetition(
            &too_few,
            REPETITION_WINDOW,
            REPETITION_THRESHOLD
        ));

        // A short-period loop must fire too (any-period scan): the cycling
        // alphabet is 26-periodic — the old exact-window check was blind to
        // any period that does not divide the window.
        let mut cycling = String::new();
        for i in 0..REPETITION_WINDOW * REPETITION_THRESHOLD {
            cycling.push(((i % 26) as u8 + b'a') as char);
        }
        assert!(detect_repetition(
            &cycling,
            REPETITION_WINDOW,
            REPETITION_THRESHOLD
        ));

        // Genuinely non-periodic text of the same length should NOT fire.
        // (The old fixture used the cycling alphabet as "non-repeating" —
        // it is 26-periodic, which the any-period guard correctly flags.)
        let mut non_repeating = String::new();
        for i in 0..60 {
            non_repeating.push_str(&format!("line {i} of the sample text\n"));
        }
        assert!(!detect_repetition(
            &non_repeating,
            REPETITION_WINDOW,
            REPETITION_THRESHOLD
        ));
    }

    #[test]
    fn bound_repetition_buffer_caps_growth() {
        // A buffer exceeding the cap is truncated to the needed suffix.
        let needed = REPETITION_WINDOW * REPETITION_THRESHOLD;
        let big = "x".repeat(REPETITION_BUFFER_CAP + 100);
        let bounded = bound_repetition_buffer(big, needed, REPETITION_BUFFER_CAP);
        assert!(bounded.len() <= REPETITION_BUFFER_CAP);
        // The tail is preserved.
        assert!(bounded.ends_with(&"x".repeat(needed)));
    }

    #[test]
    fn raw_captures_thinking_signature_text_and_tool_use() {
        // A thinking block carries its `signature` in content_block_start —
        // the field the old parser dropped (root cause of the Anthropic 400).
        // The raw content array must preserve it verbatim (Rule 1).
        let raw = sse(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":"sig-abc"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"reasoning"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Answer"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_01","name":"read","input":{}}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"/x\"}"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":2}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let (events, _state) = parse_all(&raw);
        let raw_msg = events
            .iter()
            .find_map(|e| match e {
                LlmEvent::RawAssistantDelta { delta } => Some(delta.clone()),
                _ => None,
            })
            .expect("RawAssistantDelta should be emitted at message_stop");
        assert_eq!(raw_msg["role"], serde_json::json!("assistant"));
        let content = raw_msg["content"].as_array().expect("content array");
        assert_eq!(content.len(), 3);
        // thinking block with signature preserved verbatim (Rule 1).
        assert_eq!(content[0]["type"], serde_json::json!("thinking"));
        assert_eq!(content[0]["thinking"], serde_json::json!("reasoning"));
        assert_eq!(content[0]["signature"], serde_json::json!("sig-abc"));
        // text block.
        assert_eq!(content[1]["type"], serde_json::json!("text"));
        assert_eq!(content[1]["text"], serde_json::json!("Answer"));
        // tool_use with input parsed to an object.
        assert_eq!(content[2]["type"], serde_json::json!("tool_use"));
        assert_eq!(content[2]["id"], serde_json::json!("toolu_01"));
        assert_eq!(content[2]["name"], serde_json::json!("read"));
        assert_eq!(content[2]["input"], serde_json::json!({"path": "/x"}));
    }

    #[test]
    fn raw_drops_empty_thinking_block_that_never_received_content() {
        // The capture-side half of backlog bd5eb19d: a thinking block whose
        // content_block_start arrived (with signature) but which never got a
        // thinking_delta stays thinking:"" — storing it poisons the history
        // for every later echo. The message_stop finalization must drop it
        // (zero information, always invalid on the wire); blocks with content
        // are unaffected (see raw_captures_thinking_signature_text_and_tool_use).
        let raw = sse(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":"sig-abc"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Answer"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let (events, _state) = parse_all(&raw);
        let raw_msg = events
            .iter()
            .find_map(|e| match e {
                LlmEvent::RawAssistantDelta { delta } => Some(delta.clone()),
                _ => None,
            })
            .expect("RawAssistantDelta should be emitted at message_stop");
        let content = raw_msg["content"].as_array().expect("content array");
        assert_eq!(
            content,
            &vec![serde_json::json!({ "type": "text", "text": "Answer" })],
            "a thinking block that never received a thinking_delta must not be \
             captured — echoing it 400s (backlog bd5eb19d)"
        );
    }

    #[test]
    fn raw_emits_nothing_when_the_only_block_was_an_empty_thinking_block() {
        // Pins the empty-after-retain branch (backlog bd5eb19d, review
        // finding): a stream whose ONLY content block is a thinking
        // block-start (with signature) that never receives a thinking_delta
        // must emit NO RawAssistantDelta — the retain at message_stop
        // empties the array, matching the raw-none-when-no-blocks semantics.
        // The stream still finishes normally (Finish/Usage emit).
        let raw = sse(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":"sig-abc"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let (events, _) = parse_all(&raw);
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, LlmEvent::RawAssistantDelta { .. })),
            "no RawAssistantDelta may be emitted when the only content block \
             was an empty thinking block (backlog bd5eb19d)"
        );
        // The stream still completes — the parser did not bail.
        assert!(events
            .iter()
            .any(|e| matches!(e, LlmEvent::Finish { .. })));
    }

    #[test]
    fn raw_preserves_redacted_thinking_block() {
        // redacted_thinking blocks are not human-readable, but Anthropic 400s
        // if they are filtered out during tool use (Rule 4). Preserve verbatim.
        let raw = sse(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":3}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"<encrypted>"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"ok"}}"#),
            ("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let (events, _) = parse_all(&raw);
        let raw_msg = events
            .iter()
            .find_map(|e| match e {
                LlmEvent::RawAssistantDelta { delta } => Some(delta.clone()),
                _ => None,
            })
            .expect("raw");
        let content = raw_msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], serde_json::json!("redacted_thinking"));
        assert_eq!(content[0]["data"], serde_json::json!("<encrypted>"));
        assert_eq!(content[1]["type"], serde_json::json!("text"));
        assert_eq!(content[1]["text"], serde_json::json!("ok"));
    }

    #[test]
    fn raw_none_when_no_content_blocks() {
        // A turn with no content blocks (e.g. an immediate stop) emits no raw.
        let raw = sse(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":0}}"#),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]);
        let (events, _) = parse_all(&raw);
        assert!(events
            .iter()
            .all(|e| !matches!(e, LlmEvent::RawAssistantDelta { .. })));
    }

    /// A minimal HTTP server that delays the SSE head AND the first event
    /// ~300ms after the POST (the realistic provider pattern — headers and
    /// the first chunk arrive together), then streams a complete Anthropic
    /// Messages transcript and closes — so a full-stream test can assert
    /// the POST-send→first-chunk TTFT (perf review L1: the old
    /// headers-arrived anchor measured ~0).
    struct DelayedAnthropicSseServer {
        addr: std::net::SocketAddr,
    }

    impl DelayedAnthropicSseServer {
        async fn start() -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind delayed anthropic sse server");
            let addr = listener
                .local_addr()
                .expect("delayed anthropic sse server addr");
            tokio::spawn(async move {
                let (mut socket, _peer) = listener.accept().await.expect("accept");
                // Read the request head (up to the \r\n\r\n terminator).
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.expect("read head");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                // The prefill/queue wait: headers AND the first event
                // arrive together, only after the delay.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let head =
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
                socket.write_all(head.as_bytes()).await.expect("write head");
                let sse = "event: message_start\n\
                          data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-5\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\
                          \n\
                          event: content_block_start\n\
                          data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
                          \n\
                          event: content_block_delta\n\
                          data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\
                          \n\
                          event: message_delta\n\
                          data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":30}}\n\
                          \n\
                          event: message_stop\n\
                          data: {\"type\":\"message_stop\"}\n\
                          \n";
                socket.write_all(sse.as_bytes()).await.expect("write transcript");
                // Drop the socket: the stream ends cleanly.
            });
            Self { addr }
        }
    }

    #[tokio::test]
    async fn anthropic_ttft_measures_post_send_to_first_chunk() {
        // L1 regression (anthropic path): the ttft anchor was stamped when
        // the POST response HEADERS arrived — but Anthropic only begins
        // the response when the first token is ready, so headers and the
        // first SSE event arrived in the same burst and the measured window
        // was structurally ~0. The anchor is now POST-send: with the server
        // holding headers+first event ~300ms after the POST, ttft_ms must
        // cover that wait.
        use futures::StreamExt;

        let log = Arc::new(LlmRequestLog::new());
        let server = DelayedAnthropicSseServer::start().await;
        let client = AnthropicClient::new_with_trace(
            AnthropicClientConfig {
                base_url: format!("http://{}/v1", server.addr),
                api_key: "sk-test".into(),
                model: "claude-sonnet-4-5".into(),
                provider: "anthropic".into(),
                max_context: None,
                max_output_tokens: None,
                multimodal: false,
                workspace_id: None,
            },
            Some(log.clone()),
        );

        let mut stream = client
            .complete(&[Message::user_text("hi")], &[], None)
            .await
            .expect("complete must succeed");
        let mut usage_ttft = None;
        while let Some(ev) = stream.next().await {
            if let LlmEvent::Usage { ttft_ms, .. } = ev {
                usage_ttft = ttft_ms;
            }
        }
        let ttft = usage_ttft.expect("a Usage event with ttft_ms");
        assert!(
            ttft >= 200,
            "ttft must cover the POST→first-chunk wait (got {ttft}ms; the old headers-arrived anchor measured ~0)"
        );

        // connect_ms keeps its record_created→headers window (unchanged
        // semantics — the delay lands there too).
        let summaries = log.list();
        assert_eq!(summaries.len(), 1);
        let connect = summaries[0].connect_ms.expect("connect_ms set");
        assert!(
            connect >= 200,
            "connect keeps its record→headers window (got {connect}ms)"
        );
    }
}
