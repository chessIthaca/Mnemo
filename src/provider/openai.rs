// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The OpenAI-compatible client implementation.
//!
//! One client, swapping the base URL. OpenAI is the primary path; Ollama/vLLM/
//! LM Studio work via the same client with `ProviderKind::Local` (degraded
//! capabilities). The stream is parsed from raw SSE (via reqwest) so we can
//! access non-standard fields from reasoning models — the thinking text is
//! recognized from `reasoning_content` (DeepSeek-direct / GLM), `reasoning`
//! (Ollama's OpenAI-compatible endpoint), or inline think-tag blocks inside
//! `content` (local Ollama / LM Studio, via the think-tag filter in [`sse`]).
//! The filter engages for `ProviderKind::Local` only — on those endpoints a
//! literal think-tag opener at answer position 0 is rerouted as reasoning
//! (the ecosystem-standard heuristic; a non-thinking local model that opens
//! its answer with a literal tag is an accepted false positive).
//!
//! Layout (backlog 76efacba split the former single-file implementation):
//! - this file — the client, its config, and the
//!   [`complete`](LlmClient::complete) pipeline stages: `prepare_request` →
//!   `open_stream` → `spawn_stream`;
//! - [`request`] — request-body building (params + provider special-casing),
//!   Local-provider sanitization, and pre-flight shape validation;
//! - [`sse`] — the pure SSE parsers for both wire formats + the think-tag
//!   filter;
//! - [`stream`] — the spawned parse loop and its captured state;
//! - [`guard`] — the client-side stream guard (stop-boundary truncation);
//! - [`tests`] — the test module.

mod guard;
mod request;
mod sse;
mod stream;

#[cfg(test)]
mod tests;

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::error::{Error, Result};
use crate::provider::sse_util::{header_str, provider_error};
use crate::provider::trace::LlmRequestLog;
use crate::provider::{
    Capabilities, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice, ToolSchema,
};

use request::validate_request_messages;
use stream::{pump_sse_stream, StreamTask};

pub use guard::{apply_stream_guard, find_boundary_cutoff};

/// Configuration for building an OpenAI-compatible client.
#[derive(Debug, Clone)]
pub struct OpenAiClientConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub kind: ProviderKind,
    /// The endpoint/provider name from `endpoints.toml` (e.g. "openai",
    /// "ollama-local"). Recorded on trace records so the Trace tab can
    /// attribute each request to its provider.
    pub provider: String,
    /// Optional max-context override from the endpoint config.
    pub max_context: Option<usize>,
    /// Optional max-output-tokens override from the endpoint config.
    pub max_output_tokens: Option<usize>,
    /// Whether the model at this endpoint accepts image inputs (multimodal).
    /// Flows into the capability set so the agent loop can decide whether to
    /// send image blocks or fall back to a separate vision model.
    pub multimodal: bool,
    /// The `reasoning_effort` to send in the request body (e.g. `max`, `high`,
    /// `medium`, `low`, `minimal`). `None` omits the field entirely (endpoints
    /// that don't accept it, or the toolbar's "off" choice). Values pass
    /// through verbatim — the endpoint decides which it supports — except
    /// `"off"`, which the builder never sends verbatim: it encodes per
    /// provider — the `reasoning_effort_off_wire` config when set, else
    /// `"none"` for DeepSeek-family models, omitted otherwise (a literal
    /// `"off"` is an instant non-retryable 400 on DeepSeek).
    pub reasoning_effort: Option<String>,
    /// The configured wire value for the effort `"off"` (from the
    /// endpoint/model `reasoning_effort_off_wire` config — the escape hatch
    /// for renamed/aliased/fine-tuned models the name-based policy can't
    /// see). `None` = not configured; the builder then falls back to the
    /// built-in provider policy (DeepSeek-family model names → `"none"`),
    /// else omits the field.
    pub reasoning_effort_off_wire: Option<String>,
    /// Whether to use the OpenAI Responses API (`/v1/responses`) instead of
    /// chat completions (`/v1/chat/completions`). When `true`, the client uses
    /// server-side stateful mode (Rule 3): it captures the response `id` and
    /// sends `previous_response_id` on the next request, so the server holds
    /// the reasoning state and full history need not be resent. When `false`
    /// (the default), the stateless raw-echo path is used. Both paths must pass
    /// the same sequential-tool-call test (acceptance test #5).
    pub use_responses_api: bool,
    /// Optional temperature override for request sampling (0.0 to 2.0).
    pub temperature: Option<f64>,
    /// Optional top_p override for request sampling (0.0 to 1.0).
    pub top_p: Option<f64>,
    /// Optional explicit stop sequences sent in the request body.
    pub stop: Vec<String>,
    /// Optional explicit stop token IDs sent in the request body (e.g. for vLLM/SGLang).
    pub stop_token_ids: Vec<u64>,
    /// Vendor stop-boundary strings for models whose tokenizer emits its
    /// stop boundaries as raw text (e.g. GLM-5.3's role-tag tokens plus
    /// newline cascades). Sent in the request `stop` list (ahead of the
    /// user's `stop`, deduped) AND enforced by the SSE stream guard.
    /// Resolved from the endpoint/model config (`stop_boundary_strings_for`)
    /// — never matched by model-name prefix, so aliases and fine-tunes are
    /// covered.
    pub stop_boundary_strings: Vec<String>,
    /// Arbitrary extra body parameters merged into the request payload.
    /// Merged last: keys here can override any request field including model/messages/stop.
    pub extra_body: Option<serde_json::Map<String, serde_json::Value>>,
}

impl Default for OpenAiClientConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1/".into(),
            api_key: String::new(),
            model: "gpt-4o".into(),
            kind: ProviderKind::OpenAI,
            provider: "openai".into(),
            max_context: None,
            max_output_tokens: None,
            multimodal: false,
            reasoning_effort: None,
            reasoning_effort_off_wire: None,
            use_responses_api: false,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            stop_token_ids: Vec::new(),
            stop_boundary_strings: Vec::new(),
            extra_body: None,
        }
    }
}

impl OpenAiClientConfig {
    /// Test-only baseline: an OpenAI-kind client pointed at a local stub
    /// endpoint (`http://localhost/v1/`, api key "dummy", model
    /// "test-model", provider "test").
    ///
    /// Tests override just the fields they care about via struct-update
    /// syntax (`..OpenAiClientConfig::test_default()`), so adding a field to
    /// [`OpenAiClientConfig`] only requires updating the struct, its
    /// [`Default`] impl, and this factory — never the dozens of test call
    /// sites. Compiled for this crate's own tests (`cfg(test)`) and for
    /// dependents' tests via the `test-support` cargo feature (enabled
    /// through dev-dependencies); never in release builds.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_default() -> Self {
        Self {
            base_url: "http://localhost/v1/".into(),
            api_key: "dummy".into(),
            model: "test-model".into(),
            kind: ProviderKind::OpenAI,
            provider: "test".into(),
            ..Default::default()
        }
    }
}

/// An OpenAI-compatible LLM client (works with OpenAI and local OpenAI-compatible endpoints).
pub struct OpenAiClient {
    config: OpenAiClientConfig,
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
    /// The serialized message prefix memo (perf review L3, 2027-01-09):
    /// history is append-mostly across a turn loop's iterations, so the
    /// serialized JSON of the stable prefix is byte-identical from request
    /// to request. Single-slot, fingerprint-validated (a mismatch rebuilds —
    /// a stale hit is impossible by construction); see `request::PrefixCache`.
    prefix_cache: std::sync::Mutex<Option<request::PrefixCache>>,
    /// Test-visible cache-hit counter (review round-1, Finding 2): the cache
    /// tests must assert a HIT actually occurs — warm==cold equality holds
    /// trivially on a miss too, so a regression that always mismatches would
    /// otherwise silently disable the optimization while staying green.
    #[cfg(test)]
    hit_count: std::sync::atomic::AtomicUsize,
}

impl OpenAiClient {
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

    /// Build a client from config. A single `reqwest::Client` is created here
    /// and reused for every streaming request. Capture is off (see
    /// [`Self::new_with_trace`]).
    pub fn new(config: OpenAiClientConfig) -> Self {
        Self::new_with_trace(config, None)
    }

    /// Build a client from config with request/response capture. `trace` is
    /// the shared [`LlmRequestLog`] every request is recorded into; pass
    /// `None` (or use [`Self::new`]) to disable capture entirely.
    pub fn new_with_trace(config: OpenAiClientConfig, trace: Option<Arc<LlmRequestLog>>) -> Self {
        let caps = config.kind.capabilities_with_overrides(
            config.max_context,
            config.max_output_tokens,
            config.multimodal,
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
            // `Authorization: Bearer` header to a redirected host. LLM /
            // embeddings endpoints don't redirect, so `Policy::none()` is
            // safe and closes the implicit-redirect leak (Review L1).
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
            prefix_cache: std::sync::Mutex::new(None),
            #[cfg(test)]
            hit_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// A reference to the shared HTTP client (connection-pooled). Exposed so
    /// the [`VisionClient`](crate::provider::vision::VisionClient) can reuse
    /// the same pool for its non-streaming image-description requests.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }
}


#[async_trait]
impl LlmClient for OpenAiClient {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn kind(&self) -> ProviderKind {
        self.config.kind
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
            .expect("OpenAiClient last_record_id lock poisoned");
        if let Some(id) = id {
            if let Some(log) = self
                .trace
                .read()
                .expect("OpenAiClient trace lock poisoned")
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
            .expect("OpenAiClient last_record_id lock poisoned");
        if let Some(id) = id {
            if let Some(log) = self
                .trace
                .read()
                .expect("OpenAiClient trace lock poisoned")
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
            .expect("OpenAiClient pending_prep_ms lock poisoned") = Some(ms);
    }

    fn record_compact_ms(&self, ms: u32) {
        // Parked + stamped exactly like `record_prep_ms`.
        *self
            .pending_compact_ms
            .lock()
            .expect("OpenAiClient pending_compact_ms lock poisoned") = Some(ms);
    }

    fn record_backoff_ms(&self, ms: u32) {
        // Parked + stamped exactly like `record_prep_ms`.
        *self
            .pending_backoff_ms
            .lock()
            .expect("OpenAiClient pending_backoff_ms lock poisoned") = Some(ms);
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<BoxStream<'_, LlmEvent>> {
        // The pipeline, one stage per concern (backlog 76efacba):
        // 1. `prepare_request` — validate the shape + build the request body
        //    (params + provider special-casing) + open the trace record.
        // 2. `open_stream` — POST + status check + reasoning_effort retry.
        // 3. `spawn_stream` — channel + spawn the parse loop
        //    (`pump_sse_stream`) and return the event stream.
        let mut prepared = self.prepare_request(messages, tools, tool_choice.clone()).await?;
        let response = self
            .open_stream(&mut prepared, messages, tools, tool_choice)
            .await?;
        Ok(self.spawn_stream(prepared, response))
    }
}

/// The request phase's output: everything the stream phases need.
///
/// Built by [`OpenAiClient::prepare_request`], consumed by
/// [`OpenAiClient::open_stream`] (which may rebuild `body` without the
/// `reasoning_effort` field for the fallback retry) and
/// [`OpenAiClient::spawn_stream`].
struct PreparedRequest {
    /// The request body as its wire JSON string (chat-completions or
    /// Responses API format) — pre-serialized (perf review L3, 2027-01-09),
    /// shipped via reqwest `.body()` without a re-serialization pass.
    body: String,
    /// The full request URL.
    url: String,
    /// The trace log this request records into (`None` = recording disabled).
    trace: Option<Arc<LlmRequestLog>>,
    /// The trace record id for this request (`None` = no record).
    rec_id: Option<u64>,
    /// When the trace record was created — the connect-bucket anchor.
    record_created: std::time::Instant,
    /// When the POST was sent — the ttft anchor (perf review L1, 2027-01-09:
    /// SSE providers only begin the response when the first token is ready,
    /// so headers arrive with the first chunk and a headers-arrived anchor
    /// measures ~0). Stamped by [`OpenAiClient::open_stream`] immediately
    /// before the first send; initialized to `record_created` by
    /// [`OpenAiClient::prepare_request`] as a placeholder that is never
    /// read before the stamp (open_stream always runs before any send).
    post_sent: std::time::Instant,
}

impl OpenAiClient {
    /// Stage 1 of the [`complete`](LlmClient::complete) pipeline: build the
    /// request (params + provider special-casing) and open the trace record.
    ///
    /// Consumes the prep/compact/backoff timings parked on this client,
    /// validates the request shape, builds the request body (chat vs
    /// Responses API format), starts the trace record, and stamps the parked
    /// timings onto it. Returns everything the later stages need — see
    /// [`PreparedRequest`].
    async fn prepare_request(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<PreparedRequest> {
        // Entry anchor for the prep sliver: everything from here until the
        // record is created (request-body build + trace-log start) is local
        // work that belongs in `prep_ms`, not in the connect window.
        let entry_at = std::time::Instant::now();
        // Consume the prep/compact/backoff timings parked on this
        // client (before the request — and its record — existed). Taken
        // unconditionally at ENTRY so a parked value's lifetime is bounded
        // to the very next complete() call: a turn that ends without
        // creating a record (validation failure on every retry, interrupt
        // during record creation) can never leak a stale timing onto a
        // later, unrelated request.
        let parked_prep = self
            .pending_prep_ms
            .lock()
            .expect("OpenAiClient pending_prep_ms lock poisoned")
            .take();
        let parked_compact = self
            .pending_compact_ms
            .lock()
            .expect("OpenAiClient pending_compact_ms lock poisoned")
            .take();
        let parked_backoff = self
            .pending_backoff_ms
            .lock()
            .expect("OpenAiClient pending_backoff_ms lock poisoned")
            .take();

        // Guard the request shape before anything touches the network: a
        // malformed messages array must fail HERE with a descriptive local
        // error, not upstream as an opaque 400 (GLM's code-1214 "The messages
        // parameter is illegal") after a wasted round-trip + retry storm.
        validate_request_messages(messages)?;

        // Build the request body as its wire JSON string. We send it via
        // reqwest (not async-openai) so we can access non-standard fields
        // like `reasoning_content` in the streamed SSE response. When
        // `use_responses_api` is set, build the Responses API format (Rule 3
        // stateful path: `previous_response_id` + `input` array) — assembled
        // field-wise as a Value and serialized once here. The
        // chat-completions format is pre-serialized (perf review L3,
        // 2027-01-09): messages spliced verbatim via RawValue, never cloned
        // into a Value tree.
        let body = if self.config.use_responses_api {
            serde_json::to_string(&self.build_responses_request_json(messages, tools, tool_choice)?)
                .map_err(|e| Error::Provider(format!("request body serialization failed: {e}")))?
        } else {
            self.build_request_body(messages, tools, tool_choice, false)?
        };

        // Trace capture (optional — None disables recording entirely). The
        // Arc is cloned once per request; the record id ties later stream
        // events back to this request. `start` applies the request-body cap,
        // which needs the body as a Value — the pre-serialized wire string
        // (perf review L3, 2027-01-09) is cloned (a memcpy; the async worker
        // no longer deep-clones a Value tree) and parsed back on the
        // blocking pool, replacing the serialize-for-cap pass the cap logic
        // already paid here (review I1, 2026-08-18). A join error (task
        // panicked — not reachable in practice) or a re-parse failure (the
        // body was just serialized by this process) maps to `None`:
        // recording for this request simply stops.
        let trace = self
            .trace
            .read()
            .expect("OpenAiClient trace lock poisoned")
            .clone();
        let rec_id = if let Some(log) = trace.clone() {
            let model = self.config.model.clone();
            let base_url = self.config.base_url.clone();
            let provider = self.config.provider.clone();
            let body_for_trace = body.clone();
            tokio::task::spawn_blocking(move || {
                serde_json::from_str::<serde_json::Value>(&body_for_trace)
                    .map(|body_value| log.start(&model, &base_url, &provider, body_value))
                    .ok()
            })
            .await
            .ok()
            .flatten()
        } else {
            None
        };
        // The connect-phase anchor: record creation (just above) → stream
        // open (the POST response arrives; captured in `spawn_stream` at
        // `request_start`). The stream task computes `connect_ms` from it
        // at the Usage event — the full network round trip, including the
        // reasoning_effort fallback retry when one fired (review L2).
        let record_created = std::time::Instant::now();
        // Remember this request's record id so the later tools-phase
        // attribution lands on THIS record, not on whatever a concurrent
        // agent created in the shared ring meanwhile (review M1).
        if let Some(id) = rec_id {
            *self
                .last_record_id
                .lock()
                .expect("OpenAiClient last_record_id lock poisoned") = Some(id);
            // Stamp the prep/compact/backoff timings parked by the turn
            // loop and the retry loop (taken at prepare_request entry
            // above) onto the freshly created record. Prep additionally
            // absorbs the local sliver from entry to record creation
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

        let url = if self.config.use_responses_api {
            format!(
                "{}/responses",
                self.config.base_url.trim_end_matches('/')
            )
        } else {
            format!(
                "{}/chat/completions",
                self.config.base_url.trim_end_matches('/')
            )
        };

        Ok(PreparedRequest {
            body,
            url,
            trace,
            rec_id,
            record_created,
            // Placeholder until open_stream stamps the real send time
            // immediately before the first POST (see the field doc).
            post_sent: record_created,
        })
    }

    /// Stage 2 of the pipeline: POST the prepared request, check the
    /// response status, and run the reasoning_effort fallback retry.
    ///
    /// Takes `&mut prepared` plus the request inputs — the retry rebuilds
    /// the body without the `reasoning_effort` field (the pre-serialized
    /// wire string cannot be field-edited in place). On failure the trace
    /// record is stamped (connect bucket + fail) before the error surfaces.
    async fn open_stream(
        &self,
        prepared: &mut PreparedRequest,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<reqwest::Response> {
        // POST-send anchor (perf review L1): stamped immediately before the
        // FIRST send — the reasoning_effort fallback retry below does NOT
        // re-stamp, so a retry's extra wait lands in ttft (the honest
        // user-perceived first-token wait).
        prepared.post_sent = std::time::Instant::now();
        let mut response = match self
            .http_client
            .post(&prepared.url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .body(prepared.body.clone())
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) => {
                if let (Some(log), Some(id)) = (&prepared.trace, prepared.rec_id) {
                    // Stamp the connect bucket: the request died before any
                    // response headers arrived (e.g. the full 10s connect
                    // timeout) — the same window success attributes to
                    // connect_ms, so failures must too.
                    log.set_connect_ms(id, prepared.record_created.elapsed().as_millis() as u32);
                    // Status 0 = no HTTP response at all (transport failure).
                    log.fail(id, 0, &format!("failed to start stream: {e}"));
                }
                return Err(Error::Provider(format!("failed to start stream: {e}")));
            }
        };

        if !response.status().is_success() {
            // Self-healing fallback for providers that reject the
            // `reasoning_effort` field: some local builds (e.g. Qwen3 via LM
            // Studio) accept only none/minimal/low/medium/high/xhigh and
            // return HTTP 400 (`invalid_value` on `reasoning_effort`) for the
            // app default "max". A misconfigured effort must never fail a
            // turn — retry ONCE with the field omitted. The retry cannot
            // recur: the field is gone from the body, so the same rejection
            // can't repeat, and only the final outcome is traced (the
            // retried first rejection stays invisible).
            //
            // The failure body is read exactly once here; the non-fallback
            // error path below builds its message from the already-read text
            // so the body is never lost to a double consume (the GLM
            // code-1214 "messages parameter is illegal" diagnostics depend on
            // the full body surviving into the error).
            let fail_response = |status: reqwest::StatusCode, body: &str| -> Error {
                if let (Some(log), Some(id)) = (&prepared.trace, prepared.rec_id) {
                    // For 401/403 the body may echo the Authorization header
                    // (the API key). The user-visible `error` already
                    // suppresses it; do the same for the trace, which IS
                    // surfaced in the UI "Trace" tab — never log the raw
                    // body for auth failures.
                    let trace_msg = if status.as_u16() == 401 || status.as_u16() == 403 {
                        format!("HTTP {status} — unauthorized (body suppressed)")
                    } else {
                        body.to_string()
                    };
                    // Stamp the connect bucket: the server took time to
                    // process the request before rejecting it (e.g. a
                    // 502 context-overflow after ~8s of processing) — the
                    // whole in-flight window, matching the success path's
                    // connect attribution.
                    log.set_connect_ms(id, prepared.record_created.elapsed().as_millis() as u32);
                    log.fail(id, status.as_u16(), &trace_msg);
                }
                provider_error(status, body, &prepared.url, "stream request")
            };

            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            let effort_rejected = status.as_u16() == 400
                && self.config.reasoning_effort.is_some()
                && body_text.to_lowercase().contains("reasoning_effort");
            if effort_rejected {
                // Rebuild the body without `reasoning_effort` (perf review
                // L3, 2027-01-09): the body is a pre-serialized wire string —
                // the retry re-runs the builder with the field omitted (the
                // prefix cache makes the rebuild cheap; the retry is rare —
                // one per endpoint misconfiguration).
                prepared.body = if self.config.use_responses_api {
                    let mut v =
                        self.build_responses_request_json(messages, tools, tool_choice)?;
                    if let Some(obj) = v.as_object_mut() {
                        obj.remove("reasoning_effort");
                    }
                    serde_json::to_string(&v).map_err(|e| {
                        Error::Provider(format!("request body serialization failed: {e}"))
                    })?
                } else {
                    self.build_request_body(messages, tools, tool_choice, true)?
                };
                response = match self
                    .http_client
                    .post(&prepared.url)
                    .header("Authorization", format!("Bearer {}", self.config.api_key))
                    .header("Content-Type", "application/json")
                    .body(prepared.body.clone())
                    .send()
                    .await
                {
                    Ok(response) => response,
                    Err(e) => {
                        if let (Some(log), Some(id)) = (&prepared.trace, prepared.rec_id) {
                            log.set_connect_ms(
                                id,
                                prepared.record_created.elapsed().as_millis() as u32,
                            );
                            // Status 0 = no HTTP response at all (transport failure).
                            log.fail(id, 0, &format!("failed to start stream: {e}"));
                        }
                        return Err(Error::Provider(format!("failed to start stream: {e}")));
                    }
                };
                if !response.status().is_success() {
                    let status = response.status();
                    let body_text = response.text().await.unwrap_or_default();
                    return Err(fail_response(status, &body_text));
                }
            } else {
                return Err(fail_response(status, &body_text));
            }
        }
        if let (Some(log), Some(id)) = (&prepared.trace, prepared.rec_id) {
            log.set_status(id, response.status().as_u16());
        }
        Ok(response)
    }

    /// Stage 3 of the pipeline: bridge the SSE byte stream into the
    /// [`LlmEvent`] stream.
    ///
    /// Captures the status + transport-relevant headers, builds the stream
    /// guard's stop-boundary list, and spawns the parse loop
    /// ([`pump_sse_stream`]) on a tokio task. Returns the receiving end as
    /// the event stream.
    fn spawn_stream(
        &self,
        prepared: PreparedRequest,
        response: reqwest::Response,
    ) -> BoxStream<'static, LlmEvent> {
        let PreparedRequest {
            trace,
            rec_id,
            record_created,
            post_sent,
            ..
        } = prepared;
        // Bridge the SSE byte stream into our LlmEvent stream via a channel.
        let (tx, rx) = mpsc::channel::<LlmEvent>(128);
        // Capture the status + transport-relevant headers so a mid-stream
        // decode error is self-diagnosing — reqwest's "error decoding response
        // body" gives no context about what encoding the gateway sent, which
        // is usually the cause (compressed bytes the client can't decode).
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
        // measured ~0). generation = first chunk → last. Captured here
        // (before the spawn) and moved into the task below.
        let request_start = std::time::Instant::now();
        // The (log, record id) pair the stream task mirrors events into.
        let trace_ctx = match (trace, rec_id) {
            (Some(log), Some(id)) => Some((log, id)),
            _ => None,
        };
        // Inline think-tag extraction is only needed for LOCAL providers
        // (local Ollama builds / LM Studio) — OpenAI-kind gateways
        // (DeepSeek-direct, GLM, ollama.com) use proper reasoning fields, and
        // for them a literal think-tag opener at answer position 0 must NOT
        // be swallowed into reasoning (false positive: the whole answer
        // would be rerouted and echoed back as reasoning_content — review
        // Low 3).
        let think_tags_enabled = matches!(self.config.kind, crate::provider::ProviderKind::Local);
        // Capture the Responses API flag for the stream task (Rule 3): when
        // true, the stream loop uses parse_responses_sse_buffer instead of
        // parse_sse_buffer.
        let use_responses_api = self.config.use_responses_api;
        // Capture the configured stop boundaries for the stream guard: the
        // user's `stop` sequences plus the model's `stop_boundary_strings`
        // (vendor boundaries the tokenizer emits as raw text — e.g.
        // GLM-5.3's role tags and newline cascades — which some backends
        // pass through as text instead of translating to
        // finish_reason=stop). Config-driven: any model with leaky stop
        // tokens (including aliases and fine-tunes) is guarded by editing
        // endpoints.toml, not code.
        let mut stream_stop_boundaries: Vec<String> = self.config.stop.clone();
        for stop in &self.config.stop_boundary_strings {
            if !stream_stop_boundaries.iter().any(|s| s == stop) {
                stream_stop_boundaries.push(stop.clone());
            }
        }
        let task = StreamTask {
            tx,
            trace_ctx,
            status,
            content_encoding,
            transfer_encoding,
            content_type,
            request_start,
            record_created,
            post_sent,
            think_tags_enabled,
            use_responses_api,
            stop_boundaries: stream_stop_boundaries,
        };
        tokio::spawn(pump_sse_stream(response, task));

        Box::pin(ReceiverStream::new(rx))
    }
}
