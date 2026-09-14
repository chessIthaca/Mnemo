// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! In-memory log of LLM HTTP request/response pairs — the backend of the
//! right-panel "Trace" tool tab.
//!
//! Every request the [`OpenAiClient`](crate::provider::openai::OpenAiClient)
//! sends to the OpenAI-compatible `/chat/completions` endpoint can be recorded
//! here: the exact request JSON that went out, plus the raw response (SSE/JSON)
//! that came back, enriched with the parsed usage (`prompt`/`completion`/
//! `reasoning`/`cached` tokens), finish reason, HTTP status, timing, and any
//! error. The UI lists lightweight [`LlmRequestSummary`]s and fetches a full
//! [`LlmRequestDetail`] on demand.
//!
//! The log is a bounded ring buffer: at most [`MAX_RECORDS`] records, each
//! raw response capped at [`MAX_RAW_RESPONSE_BYTES`] (with a truncation flag
//! so the UI can say "showing the first N bytes"), the TOTAL raw-response
//! bytes across the ring capped by a configurable budget (oldest payloads
//! evicted first, flagged `response_evicted`), and each request body capped
//! by a configurable structure-preserving truncation (flagged
//! `request_truncated`) — both knobs default to
//! [`DEFAULT_TRACE_MEMORY_BUDGET_BYTES`] / [`DEFAULT_TRACE_REQUEST_CAP_BYTES`]
//! and are configurable via the `[trace]` section of config.toml. The log is
//! in-process + session-only by default, but the Trace tab's "Log to file"
//! checkbox can opt into mirroring every record to
//! `.coding/logs/traces.jsonl` (newline-delimited JSON, one row per request,
//! rewritten as it streams) so traces survive restarts for offline cache-hit
//! analysis. File logging is off by default — it persists potentially
//! sensitive request/response bodies to disk in plaintext. The one exception:
//! FAILED requests are always force-mirrored (full body, secrets redacted)
//! even when logging is off, so a provider rejection is diagnosable with its
//! exact request body after the fact.
//!
//! File mirroring is **asynchronous**: record updates only enqueue a cheap
//! message (a record id) on a channel; a background writer thread owns all
//! redaction, serialization, and disk I/O, coalescing many updates to the
//! same record into one write per drain. Enabling logging therefore never
//! blocks the request/streaming path.
//!
//! Two F3 (2027-01-12) persistence companions to the live mirror, both
//! written by the same background writer thread:
//! - `.coding/logs/cancels.jsonl` — **always-on** (no toggle): every request
//!   the consumer drops mid-stream (a user interrupt — the D1 classification)
//!   is appended as one compact [`LlmRequestSummary`] line at the moment it
//!   is stamped, so a cancel survives the ring's rotation. D1 classifies
//!   cancels as cancelled (not failed), so they never land in
//!   provider-errors.jsonl; this file is their permanent record.
//! - `.coding/logs/traces-history.jsonl` — append-only archive of every
//!   TERMINAL record (finish reason, error, or cancelled), gated on the same
//!   opt-in as the mirror (full bodies are sensitive). Unlike the mirror it
//!   is never rewritten: each record is handed to the writer at its terminal
//!   transition — under the records lock, while it is guaranteed still in
//!   the ring — and appended exactly once, so a busy turn rotating the ring
//!   (or the mirror's over-cap fresh-start) cannot erase earlier records
//!   retrievably. Rotated at [`HISTORY_ROTATE_BYTES`], keeping the newest
//!   [`HISTORY_ARCHIVE_KEEP`] archives. Ids restart per session — archive
//!   lines carry timestamps.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::keys::restrict_permissions;

/// Maximum number of records kept. New records past this evict the oldest.
pub const MAX_RECORDS: usize = 32;

/// Cap for the on-disk `traces.jsonl` mirror (F2, 2026-04-19 freeze
/// diagnosis): the file used to grow forever while every writer wakeup
/// re-read + rewrote the whole thing — quadratic disk I/O and an unbounded
/// RAM backlog in the writer. When the existing file exceeds this size the
/// next write starts fresh instead of merging into it (the in-memory ring is
/// rewritten on every batch anyway, so nothing live is lost).
pub const MAX_TRACE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Rotation threshold for the append-only `traces-history.jsonl` archive
/// (F3, 2027-01-12): when the file exceeds this size it is renamed to
/// `traces-history-<unixts>.jsonl` and only the newest
/// [`HISTORY_ARCHIVE_KEEP`] archives are retained, so the archive stays
/// bounded while every terminal record remains retrievably on disk.
pub const HISTORY_ROTATE_BYTES: u64 = 64 * 1024 * 1024;

/// How many rotated history archives to retain (see [`HISTORY_ROTATE_BYTES`]).
pub const HISTORY_ARCHIVE_KEEP: usize = 4;

/// Cap for the always-on `provider-errors.jsonl` log (N4, 2026-06-14): one
/// compact JSON line per failed request, appended forever with no toggle. On
/// a flaky gateway this file grows without limit (the pre-cap traces.jsonl
/// hit 84 MB the same way). Over the cap the next append starts fresh —
/// same "recent history past the cap" rule as traces.jsonl, and cheap
/// because each failure is a single line.
pub const MAX_ERROR_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Maximum bytes of raw response text stored per record (2 MiB). When a
/// response exceeds this, the remainder is dropped and `response_truncated`
/// is set so the UI can surface the cut.
pub const MAX_RAW_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Default total raw-response byte budget across the ring (16 MiB), matching
/// `[trace] memory_budget_mb = 16` in config.toml. Overridden at startup and
/// on every Settings save via [`LlmRequestLog::set_memory_budget`]; when the
/// sum of all records' raw payloads exceeds it, the OLDEST payloads are
/// evicted first (their rows survive — only the raw body goes).
pub const DEFAULT_TRACE_MEMORY_BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// Default per-record request-body cap (256 KiB), matching
/// `[trace] request_body_cap_kb = 256` in config.toml. Overridden the same
/// way; oversized bodies keep their JSON structure (keys survive) while the
/// longest string leaves are truncated into diagnosable stubs.
pub const DEFAULT_TRACE_REQUEST_CAP_BYTES: usize = 256 * 1024;

/// Token usage for one request — the fields of [`crate::provider::LlmEvent::Usage`]
/// that matter for cost/cache analysis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct LlmUsage {
    /// Input (prompt) tokens.
    pub prompt: u32,
    /// Output (completion) tokens.
    pub completion: u32,
    /// Tokens spent on reasoning (completion_tokens_details.reasoning_tokens).
    pub reasoning: u32,
    /// Prompt tokens served from the provider's cache (OpenAI
    /// prompt_tokens_details/input_tokens_details cached_tokens; Anthropic
    /// cache_read_input_tokens). 0 = no cache hit reported.
    pub cached: u32,
}

impl LlmUsage {
    /// The cache-hit ratio (0.0–1.0) of the prompt: `cached / prompt`.
    /// Returns 0.0 when the provider reported no prompt tokens.
    pub fn cache_hit_ratio(&self) -> f64 {
        if self.prompt == 0 {
            0.0
        } else {
            self.cached as f64 / self.prompt as f64
        }
    }
}

/// Per-call cap for a delivered tool-call's raw arguments string (the
/// generation-vs-harness discrimination tap, backlog e8b39d72 H1).
const MAX_RAW_TOOL_CALL_BYTES: usize = 4096;
/// Count cap for the delivered tool-calls list per record.
const MAX_RAW_TOOL_CALLS: usize = 32;

/// One tool call AS DELIVERED by the model — the raw arguments string
/// verbatim (post-JSON-parse, pre-any-normalization), captured at stream
/// completion into the request's trace record (backlog e8b39d72 H1: the
/// generation-vs-harness discrimination tap — comparing these against the
/// executed/normalized args separates model-emission decay from
/// harness-layer mutation).
#[derive(Debug, Clone, Serialize)]
pub struct RawToolCallRecord {
    /// The tool-call id assigned by the model.
    pub id: String,
    /// The tool/function name.
    pub name: String,
    /// The arguments JSON string VERBATIM as delivered (capped at
    /// [`MAX_RAW_TOOL_CALL_BYTES`], char-boundary safe).
    pub arguments: String,
    /// True when the arguments string was cut at the per-call cap.
    pub truncated: bool,
}

/// One recorded request/response pair. Serialized verbatim as the "detail"
/// wire type (see [`LlmRequestDetail`]).
#[derive(Debug, Clone, Serialize)]
pub struct LlmRequestRecord {
    /// Monotonic id — ids increase with request order (the id space is global
    /// across all agents/providers, so `id - 1` is the previous request
    /// *iff* it wasn't evicted; the UI must tolerate a `None` from `get`).
    pub id: u64,
    /// Unix epoch milliseconds at capture time.
    pub ts_ms: u64,
    /// The model id the request targeted.
    pub model: String,
    /// The endpoint base URL (host only for display; no credentials).
    pub base_url: String,
    /// The endpoint/provider name from `endpoints.toml` (e.g. "openai",
    /// "ollama-local") — the provider attribution shown in the Trace tab.
    pub provider: String,
    /// The exact JSON body POSTed to `/chat/completions`.
    pub request_json: serde_json::Value,
    /// The raw response text (SSE lines / JSON error body), capped at
    /// [`MAX_RAW_RESPONSE_BYTES`]. `None` = no response bytes yet.
    pub response_raw: Option<String>,
    /// HTTP status of the response (None before the request completes).
    pub http_status: Option<u16>,
    /// Parsed token usage from the stream's final chunk, if any — or live
    /// chars/4 streaming estimates (completion/reasoning) while the stream
    /// is in flight; the authoritative final usage overwrites them.
    pub usage: Option<LlmUsage>,
    /// The finish reason string (e.g. `"stop"`, `"tool_calls"`, `"length"`).
    pub finish_reason: Option<String>,
    /// Time-to-first-token in ms (POST sent → first streamed chunk — the
    /// real first-token wait: upload + server prefill/queue; perf review
    /// L1: headers arrive WITH the first chunk for SSE providers, so the old
    /// headers→first-chunk window measured ~0). The record-created →
    /// headers window is `connect_ms`.
    pub ttft_ms: Option<u32>,
    /// Generation time in ms (first chunk → usage/final chunk).
    pub generation_ms: Option<u32>,
    /// Reasoning time in ms — the portion of the generation window spent
    /// streaming reasoning deltas (first chunk → first answer/tool delta, or
    /// → the final chunk when the model thought but never answered).
    /// `None` when the request streamed no reasoning.
    pub reasoning_ms: Option<u32>,
    /// Connect time in ms (record created → the POST response headers
    /// arrived): TCP/TLS connect + request upload + server ack — the POST
    /// in flight. Pure network waiting (not local work); includes the full
    /// round trip and the reasoning_effort fallback retry when one fires.
    pub connect_ms: Option<u32>,
    /// Tools time in ms (stream end → the tool batch this response triggered
    /// finished executing). Attributed by the turn loop via
    /// [`LlmRequestLog::set_tools_ms`] — the record for the request whose
    /// response produced the tool calls, identified by id.
    pub tools_ms: Option<u32>,
    /// Local prompt-prep time in ms (turn-loop iteration start → record
    /// created): model resolution, token accounting, auto-recall, prompt
    /// build, request-body serialization, and the trace-log start — plus
    /// `compact_ms` when auto-compaction fired inside this window (measured
    /// before this record existed; parked on the client and stamped here
    /// via [`LlmRequestLog::set_prep_ms`], which adds the local sliver
    /// from complete() entry to record creation).
    pub prep_ms: Option<u32>,
    /// Auto-compaction time in ms (the summarization LLM call) when it fired
    /// before this request. Stamped via [`LlmRequestLog::set_compact_ms`].
    pub compact_ms: Option<u32>,
    /// Retry-backoff sleep in ms that preceded this attempt — the 1s/2s
    /// sleeps in `complete_with_retry` between failed attempts. Parked on
    /// the client (like `prep_ms`) and stamped onto the NEXT attempt's
    /// record via [`LlmRequestLog::set_backoff_ms`], so the waiting between
    /// attempts is visible in the graph instead of an invisible gap between
    /// per-attempt records.
    pub backoff_ms: Option<u32>,
    /// Mid-stream stall time in ms — byte-silence gaps longer than
    /// [`crate::provider::StallTracker::THRESHOLD`] inside the generation
    /// window (a subset of `generation_ms`). Split out of "generate" so the
    /// graph distinguishes actual streaming from waiting on a silent
    /// connection. `None` when the request never streamed.
    pub stall_ms: Option<u32>,
    /// Error text for failed requests (HTTP error body or stream error).
    pub error: Option<String>,
    /// True when the consumer dropped the stream mid-flight (a user
    /// interrupt/cancel/compact/clear): the HTTP response was healthy and
    /// this is NOT a provider failure — `provider-errors.jsonl` is never
    /// written for these rows. Terminal: the UI stops polling. A genuine
    /// stream error that lands before the drop keeps its `error` state and
    /// is never masked by this flag.
    pub cancelled: bool,
    /// True when the raw response exceeded [`MAX_RAW_RESPONSE_BYTES`] and was
    /// cut (the stored text is a prefix of the real response).
    pub response_truncated: bool,
    /// True when the request body exceeded the configured request-body cap
    /// (`[trace] request_body_cap_kb`) and its longest string leaves were
    /// truncated (the JSON structure — all keys — is preserved).
    pub request_truncated: bool,
    /// True when this record's raw response was EVICTED by the global memory
    /// budget (`[trace] memory_budget_mb`): the payload is gone (`None`) but
    /// the row (status, usage, timings) survives for the list view.
    pub response_evicted: bool,
    /// Whether the request reached a terminal state (`finish_reason` or
    /// `error` present). Maintained on every mutation so the wire type
    /// carries it — the UI uses it to stop polling a completed record.
    pub is_complete: bool,
    /// Mutation counter (perf review L5, 2027-01-09): bumped on every change
    /// to this record — every `with_record` mutation and every raw-budget
    /// payload eviction. The Trace tab's cheap list poll carries it, so the
    /// FE can skip re-fetching the full detail (up to ~2 MiB) while it is
    /// unchanged.
    pub version: u64,
    /// The tool calls AS DELIVERED by the model for this response — the raw
    /// (id, name, arguments) triple per call, captured at stream completion
    /// BEFORE any harness normalization/execution (backlog e8b39d72 H1: the
    /// generation-vs-harness discriminator). Budget-exempt on purpose: this
    /// is the incident evidence, and must survive `response_raw` eviction.
    pub raw_tool_calls: Option<Vec<RawToolCallRecord>>,
    /// The client-side stream-guard cut for this response, when one fired:
    /// the matched boundary, cut position, and cut size (see
    /// [`GuardCutDetails`]). `None` on natural stops — this field is what
    /// distinguishes a guard-cut turn from a clean empty completion.
    /// Budget-exempt like `raw_tool_calls`: incident evidence.
    pub guard_cut: Option<GuardCutDetails>,
}

/// Details of a client-side stream-guard termination (the boundary-cut
/// observability tap): recorded when the OpenAI-compatible client's stream
/// guard finds a stop boundary in a text delta and terminates the turn
/// early. Lets the Trace tab and `traces.jsonl` distinguish guard cuts
/// from natural stops and empty completions — the discriminator behind
/// the 2026-12-31 empty-output stall diagnosis.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GuardCutDetails {
    /// The stop sequence that matched (a configured stop or a model-family
    /// boundary tag).
    pub boundary: String,
    /// Byte index of the match within the text delta that triggered the
    /// cut.
    pub byte_idx: usize,
    /// Total byte length of that delta (everything after `byte_idx` was
    /// cut).
    pub delta_len: usize,
    /// Answer characters streamed before the triggering delta's batch —
    /// sourced from the unbounded completion-chars counter (not the
    /// bounded repetition buffer), so it stays accurate however long the
    /// answer. How far into the turn the cut happened.
    pub chars_before: usize,
    /// True when nothing of the triggering delta survived (the match was
    /// at its start) — the invisible-cut case: the turn can end with no
    /// visible text at all.
    pub prefix_empty: bool,
}

/// The full wire type for one request (everything, including the big JSON
/// payloads). A type alias — the record *is* the detail shape.
pub type LlmRequestDetail = LlmRequestRecord;

/// The lightweight list-row wire type: everything the trace list needs to
/// render one row, without the request/response JSON payloads (which can be
/// hundreds of KB each and are fetched on demand via `get`).
#[derive(Debug, Clone, Serialize)]
pub struct LlmRequestSummary {
    /// Monotonic id — ids increase with request order.
    pub id: u64,
    /// Unix epoch milliseconds at capture time.
    pub ts_ms: u64,
    /// The model id the request targeted.
    pub model: String,
    /// The endpoint base URL (host only for display; no credentials).
    pub base_url: String,
    /// The endpoint/provider name from `endpoints.toml` (see
    /// [`LlmRequestRecord::provider`]).
    pub provider: String,
    /// HTTP status of the response (None before the request completes).
    pub http_status: Option<u16>,
    /// Parsed token usage from the stream's final chunk, if any — or live
    /// chars/4 streaming estimates (completion/reasoning) while the stream
    /// is in flight; the authoritative final usage overwrites them.
    pub usage: Option<LlmUsage>,
    /// The finish reason string (e.g. `"stop"`, `"tool_calls"`, `"length"`).
    pub finish_reason: Option<String>,
    /// True once the record is terminal (finish_reason or error present) —
    /// in-flight (streaming) rows carry live estimate usage instead of the
    /// final numbers, so the FE can render them differently.
    pub is_complete: bool,
    /// Mutation counter (see [`LlmRequestRecord::version`]) — carried by the
    /// cheap list poll so the FE can skip re-fetching an unchanged detail.
    pub version: u64,
    /// Time-to-first-token in ms (POST sent → first streamed chunk — the
    /// real first-token wait; see [`LlmRequestRecord::ttft_ms`]).
    pub ttft_ms: Option<u32>,
    /// Generation time in ms (first chunk → usage/final chunk).
    pub generation_ms: Option<u32>,
    /// Reasoning time in ms — the portion of the generation window spent
    /// streaming reasoning deltas (first chunk → first answer/tool delta; see
    /// [`LlmRequestRecord::reasoning_ms`]).
    pub reasoning_ms: Option<u32>,
    /// Connect time in ms (record created → the POST response headers
    /// arrived): TCP/TLS connect + request upload + server ack — the POST
    /// in flight. Pure network waiting (not local work); includes the full
    /// round trip and the reasoning_effort fallback retry when one fires.
    pub connect_ms: Option<u32>,
    /// Tools time in ms (stream end → the tool batch this response triggered
    /// finished executing).
    pub tools_ms: Option<u32>,
    /// Local prompt-prep time in ms (see [`LlmRequestRecord::prep_ms`]).
    pub prep_ms: Option<u32>,
    /// Auto-compaction time in ms (see [`LlmRequestRecord::compact_ms`]).
    pub compact_ms: Option<u32>,
    /// Retry-backoff sleep in ms (see [`LlmRequestRecord::backoff_ms`]).
    pub backoff_ms: Option<u32>,
    /// Mid-stream stall time in ms (see [`LlmRequestRecord::stall_ms`]).
    pub stall_ms: Option<u32>,
    /// Error text for failed requests (HTTP error body or stream error).
    pub error: Option<String>,
    /// True when the consumer dropped the stream mid-flight (a user
    /// interrupt/cancel/compact/clear) — not a provider failure.
    pub cancelled: bool,
    /// True when the raw response exceeded [`MAX_RAW_RESPONSE_BYTES`] and was
    /// cut (the stored text is a prefix of the real response).
    pub response_truncated: bool,
    /// True when the raw response was EVICTED by the global memory budget
    /// (`[trace] memory_budget_mb`): the payload is gone but the row's
    /// metadata survives. The list surfaces this so the user knows why the
    /// detail view shows no raw body.
    pub response_evicted: bool,
    /// The client-side stream-guard cut, when one fired (see
    /// [`LlmRequestRecord::guard_cut`]) — lets list rows badge cut turns.
    pub guard_cut: Option<GuardCutDetails>,
}

impl From<&LlmRequestRecord> for LlmRequestSummary {
    fn from(r: &LlmRequestRecord) -> Self {
        Self {
            id: r.id,
            ts_ms: r.ts_ms,
            model: r.model.clone(),
            base_url: r.base_url.clone(),
            provider: r.provider.clone(),
            http_status: r.http_status,
            usage: r.usage,
            finish_reason: r.finish_reason.clone(),
            is_complete: r.is_complete,
            version: r.version,
            ttft_ms: r.ttft_ms,
            generation_ms: r.generation_ms,
            reasoning_ms: r.reasoning_ms,
            connect_ms: r.connect_ms,
            tools_ms: r.tools_ms,
            prep_ms: r.prep_ms,
            compact_ms: r.compact_ms,
            backoff_ms: r.backoff_ms,
            stall_ms: r.stall_ms,
            error: r.error.clone(),
            cancelled: r.cancelled,
            response_truncated: r.response_truncated,
            response_evicted: r.response_evicted,
            guard_cut: r.guard_cut.clone(),
        }
    }
}

/// State shared between the [`LlmRequestLog`] handles and the background
/// file-writer thread: the ring buffer itself plus the log configuration.
/// Split out so the writer thread can hold an `Arc` to this state without
/// keeping the public handle (and its writer channel) alive.
struct Shared {
    records: Mutex<VecDeque<LlmRequestRecord>>,
    next_id: AtomicU64,
    /// Where records are mirrored on disk when file logging is on. The path is
    /// always set at startup (`<coding_dir>/logs/traces.jsonl`); writes are
    /// gated on `log_enabled` so the Trace tab's "Log to file" checkbox can
    /// toggle writing without re-resolving the path.
    log_path: Mutex<Option<PathBuf>>,
    /// Whether records are mirrored to `log_path`. Off by default — writing
    /// full request/response bodies to disk is opt-in (a debugging aid).
    /// Failed requests bypass this flag: their full bodies are force-mirrored
    /// even when this is false (see [`maybe_log_error`](crate::provider::trace::LlmRequestLog)
    /// and `Msg::DirtyForced`).
    log_enabled: AtomicBool,
    /// Where FAILED requests are appended unconditionally
    /// (`<coding_dir>/logs/provider-errors.jsonl`, set at startup). Unlike
    /// `log_path` there is NO enable flag: every request that ends in a
    /// terminal error state (HTTP status >= 400, or a transport/stream error)
    /// gets one compact JSON line, so provider errors are diagnosable after
    /// the fact without opting into full trace logging.
    error_log_path: Mutex<Option<PathBuf>>,
    /// Record ids already appended to `error_log_path`. Dedupes so a request
    /// is logged exactly once even when `fail()` is called multiple times
    /// (e.g. a status failure followed by a stream error).
    error_logged_ids: Mutex<std::collections::HashSet<u64>>,
    /// Total raw-response byte budget across the ring (from
    /// `[trace] memory_budget_mb`; see [`DEFAULT_TRACE_MEMORY_BUDGET_BYTES`]).
    /// When the sum of all records' `response_raw` lengths exceeds it, the
    /// oldest payloads are evicted first. Atomic so a Settings save can push
    /// a new value with no lock juggling.
    raw_budget: AtomicUsize,
    /// Per-record request-body cap (from `[trace] request_body_cap_kb`; see
    /// [`DEFAULT_TRACE_REQUEST_CAP_BYTES`]). Applied at `start()`.
    request_body_cap: AtomicUsize,
    /// Where user-cancel records are appended unconditionally
    /// (`<coding_dir>/logs/cancels.jsonl`, set at startup). Like
    /// `error_log_path` there is NO enable flag: every request the consumer
    /// drops mid-stream (a user interrupt — the D1 classification) gets one
    /// compact JSON line at the moment it is stamped, so a cancel survives
    /// the trace ring's rotation (F3).
    cancels_log_path: Mutex<Option<PathBuf>>,
    /// Where terminal records archive append-only (F3,
    /// `<coding_dir>/logs/traces-history.jsonl`, set at startup). Gated on
    /// `log_enabled` like the live mirror (full request/response bodies are
    /// opt-in); unlike the mirror it is append-only and rotated at
    /// [`HISTORY_ROTATE_BYTES`], so a busy turn cannot erase earlier records
    /// retrievably.
    history_log_path: Mutex<Option<PathBuf>>,
    /// Rotation threshold for `history_log_path` (default
    /// [`HISTORY_ROTATE_BYTES`]; overridable via `set_history_rotate_bytes`).
    history_rotate_bytes: AtomicU64,
}

impl Shared {
    /// Empty shared state (no file logging configured).
    fn new() -> Self {
        Self {
            records: Mutex::new(VecDeque::new()),
            next_id: AtomicU64::new(1),
            log_path: Mutex::new(None),
            log_enabled: AtomicBool::new(false),
            error_log_path: Mutex::new(None),
            error_logged_ids: Mutex::new(std::collections::HashSet::new()),
            raw_budget: AtomicUsize::new(DEFAULT_TRACE_MEMORY_BUDGET_BYTES),
            request_body_cap: AtomicUsize::new(DEFAULT_TRACE_REQUEST_CAP_BYTES),
            cancels_log_path: Mutex::new(None),
            history_log_path: Mutex::new(None),
            history_rotate_bytes: AtomicU64::new(HISTORY_ROTATE_BYTES),
        }
    }
}

/// The shared, bounded log. Clone the `Arc` around it into every provider +
/// the IPC layer (same instance everywhere, like the browser manager).
pub struct LlmRequestLog {
    shared: Arc<Shared>,
    /// Sender to the background file-writer thread (created lazily on the
    /// first file write). The thread holds the receiving end plus an
    /// `Arc<Shared>` — never a reference back to this handle — so dropping
    /// the last `LlmRequestLog` closes the channel and the thread exits.
    writer: Mutex<Option<std::sync::mpsc::Sender<Msg>>>,
}

/// A message from the request/streaming path to the background file writer.
/// Deliberately cheap to build: `Dirty` carries only a record id (the writer
/// re-reads the record under the lock, so no payload is cloned on the hot
/// path), `ErrorLine` carries one small pre-redacted JSON line.
enum Msg {
    /// The record with this id changed — mirror its latest state to disk.
    Dirty(u64),
    /// Mirror this record to disk REGARDLESS of the logging toggle: a failed
    /// request's full body must always be diagnosable, even when the Trace
    /// tab's "Log to file" checkbox is off. Only sent when the toggle is off
    /// (when on, the ordinary `Dirty` queued for the same mutation already
    /// covers it).
    DirtyForced(u64),
    /// Append this pre-serialized line to the provider-error log.
    ErrorLine(String),
    /// Append this pre-serialized line to the cancels log (F3): a user
    /// cancel's persistent record, written at stamping time.
    CancelLine(String),
    /// Archive this terminal record to the append-only history file (F3).
    /// The record is cloned at its terminal transition (under the records
    /// lock, while it is guaranteed still in the ring) and the writer thread
    /// does the redaction/serialization/append — heavy work stays off the
    /// request path, per the module's writer-thread invariant.
    ArchiveRecord(LlmRequestRecord),
    /// Acknowledge once every write queued before this message is on disk.
    Flush(std::sync::mpsc::Sender<()>),
}

impl LlmRequestLog {
    /// Create an empty log (no file logging configured).
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared::new()),
            writer: Mutex::new(None),
        }
    }

    /// Get (or lazily spawn) the background writer thread's sender. The
    /// thread is created on the first message ever sent — an app run that
    /// never enables logging never pays for it.
    fn ensure_writer(&self) -> std::sync::mpsc::Sender<Msg> {
        let mut w = self.writer.lock().expect("LlmRequestLog lock poisoned");
        if let Some(tx) = w.as_ref() {
            return tx.clone();
        }
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        let shared = Arc::clone(&self.shared);
        std::thread::Builder::new()
            .name("llm-trace-writer".into())
            .spawn(move || writer_main(shared, rx))
            .expect("failed to spawn LlmRequestLog writer thread");
        *w = Some(tx.clone());
        tx
    }

    /// Begin a new record for a request about to be POSTed. Returns the
    /// record id; every later update method takes this id. Evicts the oldest
    /// record when the ring is full, caps the request body to the configured
    /// per-record limit (structure-preserving; see [`cap_request_json`]),
    /// and enforces the global raw-response budget (oldest payloads first).
    pub fn start(
        &self,
        model: &str,
        base_url: &str,
        provider: &str,
        mut request_json: serde_json::Value,
    ) -> u64 {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // Apply the per-record request-body cap BEFORE storing: an oversized
        // prompt (huge tool outputs) must never enter the ring verbatim.
        let cap = self.shared.request_body_cap.load(Ordering::Relaxed);
        let request_truncated = cap_request_json(&mut request_json, cap);
        let record = LlmRequestRecord {
            id,
            ts_ms,
            model: model.to_string(),
            base_url: base_url.to_string(),
            provider: provider.to_string(),
            request_json,
            response_raw: None,
            http_status: None,
            usage: None,
            finish_reason: None,
            ttft_ms: None,
            generation_ms: None,
            reasoning_ms: None,
            connect_ms: None,
            tools_ms: None,
            prep_ms: None,
            compact_ms: None,
            backoff_ms: None,
            stall_ms: None,
            error: None,
            cancelled: false,
            response_truncated: false,
            request_truncated,
            response_evicted: false,
            is_complete: false,
            version: 0,
            raw_tool_calls: None,
            guard_cut: None,
        };
        let mut records = self
            .shared
            .records
            .lock()
            .expect("LlmRequestLog lock poisoned");
        records.push_back(record);
        while records.len() > MAX_RECORDS {
            records.pop_front();
        }
        // Enforce the global raw-response budget: the sum of all records'
        // payloads must stay under it, oldest payloads evicted first.
        enforce_raw_budget(&mut records, self.shared.raw_budget.load(Ordering::Relaxed));
        // Mirror the fresh record so a crash before the first update doesn't
        // lose the request line (the row is rewritten on every later update).
        // Just a channel send — the writer thread does the disk I/O.
        if self.shared.log_enabled.load(Ordering::Relaxed) {
            let _ = self.ensure_writer().send(Msg::Dirty(id));
        }
        id
    }

    /// Record the response's HTTP status.
    pub fn set_status(&self, id: u64, status: u16) {
        self.with_record(id, |r| r.http_status = Some(status));
    }

    /// Record the parsed usage + timing from the stream's final chunk.
    ///
    /// `connect_ms` is the time from record creation until the stream
    /// opened (the POST response headers arrived), measured by the
    /// provider — TCP/TLS connect + upload + server ack, including the
    /// reasoning_effort fallback retry when one fires (review L2).
    pub fn set_usage(
        &self,
        id: u64,
        usage: LlmUsage,
        ttft_ms: Option<u32>,
        generation_ms: Option<u32>,
        reasoning_ms: Option<u32>,
        connect_ms: Option<u32>,
    ) {
        self.with_record(id, |r| {
            r.usage = Some(usage);
            r.ttft_ms = ttft_ms;
            r.generation_ms = generation_ms;
            r.reasoning_ms = reasoning_ms;
            r.connect_ms = connect_ms;
        });
    }

    /// Overlay streaming token estimates on an in-flight record — the
    /// provider stream loops call this with chars/4 counts of the deltas
    /// streamed so far (the same convention as the frontend toolbar's live
    /// token estimates, `liveCompletionTokens`/`liveReasoningTokens`) so the
    /// Trace tab shows live token
    /// progress during a long reasoning phase, where nothing else changes
    /// until the stream finishes. Each push carries the accumulated totals
    /// so far (monotonic overwrite). A no-op for unknown/evicted ids and
    /// terminal records — the authoritative [`Self::set_usage`] at stream
    /// end overwrites the estimates, and a finished record must never be
    /// resurrected. Routed through `with_record` so the file-writer mirror
    /// sees the mutation.
    pub fn update_streaming_usage(&self, id: u64, completion: u32, reasoning: u32) {
        self.with_record(id, |r| {
            if r.is_complete {
                return;
            }
            let prev = r.usage.unwrap_or_default();
            r.usage = Some(LlmUsage {
                completion,
                reasoning,
                ..prev
            });
        });
    }

    /// Record the tools phase (stream end → tool batch finished) on the record
    /// with the given id, in milliseconds.
    ///
    /// The id is the trace record of the request whose response produced the
    /// tool calls — the CALLER (the provider client that streamed that
    /// response) remembers it; attribution by id is what keeps the phase on
    /// the right record when concurrent agents (or a memory-consolidation
    /// extraction inside the batch) create newer records in the shared ring
    /// between the stream end and the batch end (review M1). Routed through
    /// `with_record` so the file-writer mirror sees the mutation too
    /// (review L1). A no-op for unknown/evicted ids.
    pub fn set_tools_ms(&self, id: u64, ms: u32) {
        self.with_record(id, |r| r.tools_ms = Some(ms));
    }

    /// Stamp the TTFT bucket — response headers arrived → first streamed
    /// chunk (the server thinking before its first token; the network round
    /// trip to the headers is `connect_ms`, stamped via
    /// [`set_connect_ms`](Self::set_connect_ms)). Also used on error paths
    /// where the stream died before any chunk arrived (headers → the
    /// error) so a silent server shows in the waiting bucket instead of
    /// being lost. Routed through `with_record` so the file-writer mirror
    /// sees the mutation. A no-op for unknown/evicted ids.
    pub fn set_ttft_ms(&self, id: u64, ms: u32) {
        self.with_record(id, |r| r.ttft_ms = Some(ms));
    }

    /// Stamp the connect bucket — record creation → the POST response
    /// headers arrived (TCP/TLS connect + upload + server ack). Also used
    /// on error paths where the request died before headers arrived
    /// (record creation → the error), so the same window lands in the same
    /// bucket on success and failure. Routed through `with_record` so the
    /// file-writer mirror sees the mutation. A no-op for unknown/evicted
    /// ids.
    pub fn set_connect_ms(&self, id: u64, ms: u32) {
        self.with_record(id, |r| r.connect_ms = Some(ms));
    }

    /// Stamp the generation bucket on an error record — the elapsed time
    /// from the first chunk to a mid-stream stall/error, so a dead connection
    /// after streaming started is attributed to the generation bucket instead
    /// of being lost. Routed through `with_record`; a no-op for unknown/evicted
    /// ids.
    pub fn set_generation_ms(&self, id: u64, ms: u32) {
        self.with_record(id, |r| r.generation_ms = Some(ms));
    }

    /// Record the local prompt-prep time (turn-loop iteration start → POST)
    /// on the record with the given id, in milliseconds.
    ///
    /// The turn loop measures this window BEFORE the request exists (model
    /// resolution, token accounting, auto-recall, prompt build — including
    /// `compact_ms` when auto-compaction fired inside it), so the client
    /// parks the value and stamps it onto the record it next creates (see
    /// `record_prep_ms` on the provider clients). Same attribution-by-id
    /// discipline as [`set_tools_ms`](Self::set_tools_ms): routed through
    /// `with_record` so the file-writer mirror sees the mutation. A no-op
    /// for unknown/evicted ids.
    pub fn set_prep_ms(&self, id: u64, ms: u32) {
        self.with_record(id, |r| r.prep_ms = Some(ms));
    }

    /// Record the auto-compaction time (the summarization LLM call) on the
    /// record with the given id, in milliseconds. Parked + stamped by the
    /// client exactly like [`set_prep_ms`](Self::set_prep_ms); a no-op for
    /// unknown/evicted ids.
    pub fn set_compact_ms(&self, id: u64, ms: u32) {
        self.with_record(id, |r| r.compact_ms = Some(ms));
    }

    /// Record the retry-backoff sleep (the `complete_with_retry` sleeps
    /// between failed attempts) that preceded this attempt, on the record
    /// with the given id, in milliseconds. Parked + stamped by the client
    /// exactly like [`set_prep_ms`](Self::set_prep_ms); a no-op for
    /// unknown/evicted ids.
    pub fn set_backoff_ms(&self, id: u64, ms: u32) {
        self.with_record(id, |r| r.backoff_ms = Some(ms));
    }

    /// Record the mid-stream stall time (byte-silence gaps inside the
    /// generation window) on the record with the given id, in
    /// milliseconds. Stamped by the stream task at the Usage event and on
    /// the read-timeout error path; a no-op for unknown/evicted ids.
    pub fn set_stall_ms(&self, id: u64, ms: u32) {
        self.with_record(id, |r| r.stall_ms = Some(ms));
    }

    /// Append raw response bytes (SSE/JSON text) to the record. Enforces
    /// [`MAX_RAW_RESPONSE_BYTES`]: once the cap is hit the remainder is
    /// dropped and `response_truncated` is set (and stays set). The cut is
    /// backed up to a UTF-8 char boundary so the stored text is always valid
    /// (a cap that lands mid-character never panics — the partial char is
    /// simply not stored, which the truncation flag already signals).
    pub fn append_response(&self, id: u64, text: &str) {
        self.with_record(id, |r| {
            // Evicted records don't re-grow: once the global budget dropped
            // this payload, re-appending would only churn it back over the
            // budget and evict it again on every chunk.
            if r.response_truncated || r.response_evicted {
                return; // already cut — don't grow past the cap
            }
            let existing_len = r.response_raw.as_ref().map_or(0, |s| s.len());
            let remaining = MAX_RAW_RESPONSE_BYTES.saturating_sub(existing_len);
            if text.len() > remaining {
                // `remaining` is a byte count that may split a multi-byte char
                // — back up to a char boundary (floor) so the slice is valid.
                let mut cut = remaining;
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                r.response_raw
                    .get_or_insert_with(String::new)
                    .push_str(&text[..cut]);
                r.response_truncated = true;
            } else {
                r.response_raw
                    .get_or_insert_with(String::new)
                    .push_str(text);
            }
        });
    }

    /// Record the finish reason (e.g. `"stop"`, `"tool_calls"`).
    pub fn finish(&self, id: u64, reason: &str) {
        self.with_record(id, |r| r.finish_reason = Some(reason.to_string()));
    }

    /// Record the tool calls AS DELIVERED by the model (backlog e8b39d72 H1)
    /// — the raw id/name/arguments triple per call, captured before any
    /// harness normalization or execution. The generation-side tap: comparing
    /// these against the executed/normalized args separates model-emission
    /// decay from harness-layer mutation. Per-call and count caps apply; the
    /// field survives the `response_raw` memory-budget eviction on purpose.
    pub fn set_raw_tool_calls(&self, id: u64, calls: Vec<(String, String, String)>) {
        let records: Vec<RawToolCallRecord> = calls
            .into_iter()
            .take(MAX_RAW_TOOL_CALLS)
            .map(|(call_id, name, arguments)| {
                let mut cut = MAX_RAW_TOOL_CALL_BYTES.min(arguments.len());
                while cut > 0 && !arguments.is_char_boundary(cut) {
                    cut -= 1;
                }
                let truncated = cut < arguments.len();
                RawToolCallRecord {
                    id: call_id,
                    name,
                    arguments: arguments[..cut].to_string(),
                    truncated,
                }
            })
            .collect();
        self.with_record(id, |r| r.raw_tool_calls = Some(records));
    }

    /// Record a client-side stream-guard cut on this request: the matched
    /// boundary, cut position, and cut size (see [`GuardCutDetails`]).
    /// Distinguishes guard-cut terminations from natural stops so empty
    /// turns are diagnosable after the fact.
    pub fn set_guard_cut(&self, id: u64, details: GuardCutDetails) {
        self.with_record(id, |r| r.guard_cut = Some(details));
    }

    /// Record a failed request: HTTP status + error text (the API error body
    /// or the stream error message).
    pub fn fail(&self, id: u64, status: u16, error: &str) {
        self.with_record(id, |r| {
            r.http_status = Some(status);
            r.error = Some(error.to_string());
        });
    }

    /// Stamp a consumer-drop cancellation (D1): the turn loop dropped the
    /// event receiver mid-stream (a user interrupt/cancel/compact/clear), so
    /// the SSE pump's next send failed. The HTTP response was healthy — this
    /// is NOT a provider failure: no error text is recorded (so
    /// `maybe_log_error` never writes a provider-errors.jsonl row) and the
    /// record is marked terminal-cancelled instead of failed, keeping the
    /// healthy http_status. A no-op when the record already reached a genuine
    /// terminal state (an error or a finish reason) — a real stream error
    /// that lands just before the consumer exits must not be masked.
    pub fn cancelled(&self, id: u64, status: u16) {
        let mut stamped = false;
        self.with_record(id, |r| {
            if r.error.is_some() || r.finish_reason.is_some() {
                return;
            }
            r.http_status = Some(status);
            r.cancelled = true;
            stamped = true;
        });
        if !stamped {
            return;
        }
        // F3: enqueue the cancel's persistent row — the dedicated always-on
        // cancels log is the record that survives the ring's rotation. One
        // compact summary line (same shape/redaction as provider-errors),
        // appended by the writer at drain time. Re-reading the record under
        // a fresh lock (never nested — the lock-ordering invariant above);
        // a record evicted between stamp and now has nothing to persist.
        let line = {
            let records = self
                .shared
                .records
                .lock()
                .expect("LlmRequestLog lock poisoned");
            match records.iter().find(|r| r.id == id) {
                Some(r) => serde_json::to_string(&LlmRequestSummary::from(r)).ok(),
                None => None,
            }
        };
        if let Some(line) = line {
            let _ = self
                .ensure_writer()
                .send(Msg::CancelLine(redact_text(&line)));
        }
    }

    /// All records as lightweight summaries, oldest first (the UI reverses
    /// for display). A record whose raw payload was evicted by the memory
    /// budget is still listed (flagged `response_evicted`, no body) — only
    /// records evicted from the ring itself are absent.
    pub fn list(&self) -> Vec<LlmRequestSummary> {
        let records = self
            .shared
            .records
            .lock()
            .expect("LlmRequestLog lock poisoned");
        records.iter().map(LlmRequestSummary::from).collect()
    }

    /// The full detail for one id, or `None` when it was never recorded or
    /// has been evicted from the ring buffer.
    pub fn get(&self, id: u64) -> Option<LlmRequestDetail> {
        let records = self
            .shared
            .records
            .lock()
            .expect("LlmRequestLog lock poisoned");
        records.iter().find(|r| r.id == id).cloned()
    }

    /// Drop all records (ids continue counting up — no reuse).
    pub fn clear(&self) {
        let mut records = self
            .shared
            .records
            .lock()
            .expect("LlmRequestLog lock poisoned");
        records.clear();
    }

    /// Run a mutation against the record with the given id (no-op when the
    /// record was evicted — a long-lived stream may outlive the ring). When
    /// file logging is enabled, the updated record is mirrored to disk; when
    /// the record is in a terminal error state, it is also appended to the
    /// always-on provider-error log.
    ///
    /// **Lock-ordering invariant:** this path holds `records` while
    /// `maybe_log_error` takes `log_path` and/or `error_log_path`. No code
    /// anywhere may hold `log_path`/`error_log_path` while acquiring
    /// `records` — that ordering (log-path then records) is the reverse of
    /// this one and forms an AB-BA cycle. `writer_main` used to do exactly
    /// that (it held the `log_path` guard across `records.lock()` + the
    /// file write) and deadlocked the app — the regression test
    /// `writer_never_holds_log_path_while_waiting_for_records` pins it down;
    /// `writer_main` now clones the path before touching `records`.
    fn with_record(&self, id: u64, f: impl FnOnce(&mut LlmRequestRecord)) {
        let mut records = self
            .shared
            .records
            .lock()
            .expect("LlmRequestLog lock poisoned");
        if let Some(r) = records.iter_mut().find(|r| r.id == id) {
            let was_complete = r.is_complete;
            f(r);
            // Maintain the terminal-state flag on every mutation so the wire
            // type always carries it (the UI stops polling complete records).
            r.is_complete = r.finish_reason.is_some() || r.error.is_some() || r.cancelled;
            // Bump the mutation counter on every mutation — the FE's cheap
            // list poll compares it against the last-fetched detail's version
            // to skip re-fetching an unchanged record (perf review L5).
            r.version += 1;
            // F3: on the terminal transition, hand the record to the writer
            // for the append-only history archive. The enqueue happens HERE
            // — under the records lock, while the record is guaranteed still
            // in the ring — because a busy turn can evict it before the
            // writer's next drain (a drain-time ring scan would miss it).
            // Gated on log_enabled like the mirror (full bodies are opt-in);
            // is_complete never flips back (finish_reason / error / cancelled
            // are terminal), so each record is archived exactly once. The
            // clone is a memcpy; the writer thread owns the redaction,
            // serialization, and I/O.
            if r.is_complete
                && !was_complete
                && self.shared.log_enabled.load(Ordering::Relaxed)
            {
                let snapshot = r.clone();
                let _ = self.ensure_writer().send(Msg::ArchiveRecord(snapshot));
            }
        }
        // Re-enforce the global raw budget — `append_response` may have grown
        // a record's payload past it (oldest payloads evicted first). Runs
        // under the same lock; eviction only drops payloads, never records,
        // so the id re-find below still succeeds.
        enforce_raw_budget(&mut records, self.shared.raw_budget.load(Ordering::Relaxed));
        if let Some(r) = records.iter().find(|r| r.id == id) {
            // Mirror asynchronously — just a channel send (the record id);
            // the writer thread re-reads the latest state under the lock and
            // does all redaction/serialization/disk I/O off this thread.
            if self.shared.log_enabled.load(Ordering::Relaxed) {
                let _ = self.ensure_writer().send(Msg::Dirty(id));
            }
            self.maybe_log_error(r);
        }
    }

    /// Set the total raw-response byte budget across the ring (from
    /// `[trace] memory_budget_mb`). Takes effect immediately — the next
    /// mutation re-enforces it, evicting oldest payloads when the stored sum
    /// already exceeds the new (smaller) budget.
    pub fn set_memory_budget(&self, bytes: usize) {
        self.shared.raw_budget.store(bytes, Ordering::Relaxed);
        let mut records = self
            .shared
            .records
            .lock()
            .expect("LlmRequestLog lock poisoned");
        enforce_raw_budget(&mut records, bytes);
    }

    /// Set the per-record request-body cap (from `[trace]
    /// request_body_cap_kb`). Applies to records started AFTER the call
    /// (already-stored bodies are left alone — re-truncating them would
    /// destroy history the user may be inspecting).
    pub fn set_request_body_cap(&self, bytes: usize) {
        self.shared.request_body_cap.store(bytes, Ordering::Relaxed);
    }

    /// Append one compact JSON line per FAILED request to the always-on
    /// provider-error log (`.coding/logs/provider-errors.jsonl`) and, when a
    /// traces path is configured but logging is off, force-mirror the request's
    /// FULL record (body included) to traces.jsonl. A request reaches a
    /// terminal error state via an HTTP status >= 400 or an error string
    /// (transport / stream failures — including status 0, "no HTTP response at
    /// all") and is handled at most once, deduped by id, at its FIRST such
    /// state (a status failure followed by a stream error logs exactly once).
    /// The summary carries no request/response payloads, so serializing +
    /// redacting it here is cheap; the actual appends land on the writer
    /// thread and I/O errors are tolerated there (the request path must never
    /// break because logging failed).
    fn maybe_log_error(&self, record: &LlmRequestRecord) {
        let is_error = record.http_status.map_or(false, |s| s >= 400) || record.error.is_some();
        if !is_error {
            return;
        }
        // The error line is gated on the error path being configured, checked
        // BEFORE the dedupe insert: a request that fails while no path is set
        // (unreachable in the app — it is configured at startup before any
        // request) can still be captured once the path is configured later,
        // because its id was never deduped away.
        let error_path_configured = self
            .shared
            .error_log_path
            .lock()
            .expect("LlmRequestLog lock poisoned")
            .is_some();
        if error_path_configured {
            {
                let mut seen = self
                    .shared
                    .error_logged_ids
                    .lock()
                    .expect("LlmRequestLog lock poisoned");
                // Bounded dedup (N3, 2026-06-14): ids are monotonic and never
                // reused, so this set would otherwise grow by one entry per
                // failed request for the process lifetime. Clearing at 10k
                // risks at most one re-appended diagnostic line per ~10k
                // failures — harmless for a log that exists for diagnosis.
                if seen.len() > 10_000 {
                    seen.clear();
                }
                if !seen.insert(record.id) {
                    return; // already handled this request's failure
                }
            }
            let line = match serde_json::to_string(&LlmRequestSummary::from(record)) {
                Ok(l) => l,
                Err(_) => return, // a record that can't serialize is skipped
            };
            // Redact secret-shaped substrings from the error text before it
            // lands on disk (an error body may echo back a request's
            // Authorization header or an api_key field).
            let line = redact_text(&line);
            let _ = self.ensure_writer().send(Msg::ErrorLine(line));
        }
        // Force-mirror the full record to traces.jsonl even when file logging
        // is disabled: a failed request must always be diagnosable with its
        // exact request body (the 1214-style opaque upstream rejections only
        // make sense with the payload in hand). Gated on a configured
        // log_path; skipped when logging is ON (the `with_record` that
        // mutated this record into its error state already queued a `Dirty`).
        // When the error path is configured the dedupe above limits this to
        // the first error state; without it the mirror may re-fire on later
        // error-state mutations, which is harmless — the traces write is an
        // idempotent upsert of the record's latest state.
        if !self.shared.log_enabled.load(Ordering::Relaxed) {
            let has_path = self
                .shared
                .log_path
                .lock()
                .expect("LlmRequestLog lock poisoned")
                .is_some();
            if has_path {
                let _ = self.ensure_writer().send(Msg::DirtyForced(record.id));
            }
        }
    }

    /// Configure where records are mirrored when file logging is enabled.
    /// Stores the path without enabling writes — writing is toggled separately
    /// via [`set_logging_enabled`](Self::set_logging_enabled) so the Trace
    /// tab's checkbox doesn't re-resolve the path on every toggle.
    pub fn set_log_file_path(&self, path: PathBuf) {
        let mut p = self
            .shared
            .log_path
            .lock()
            .expect("LlmRequestLog lock poisoned");
        *p = Some(path);
    }

    /// Configure where FAILED requests are appended. **Always-on**: unlike the
    /// traces.jsonl mirror there is no enable flag — every request that ends
    /// in a terminal error state (HTTP >= 400, transport or stream error) is
    /// appended as one compact JSON line. Called once at startup with
    /// `<coding_dir>/logs/provider-errors.jsonl`.
    pub fn set_error_log_path(&self, path: PathBuf) {
        let mut p = self
            .shared
            .error_log_path
            .lock()
            .expect("LlmRequestLog lock poisoned");
        *p = Some(path);
    }

    /// Configure where user-cancel records are appended. **Always-on** (like
    /// [`Self::set_error_log_path`]): every request the consumer drops
    /// mid-stream (a user interrupt/cancel/compact/clear — the D1
    /// classification) is appended as one compact JSON line at the moment it
    /// is stamped, so a cancel survives the trace ring's rotation (F3).
    /// Called once at startup with `<coding_dir>/logs/cancels.jsonl`.
    pub fn set_cancels_log_path(&self, path: PathBuf) {
        let mut p = self
            .shared
            .cancels_log_path
            .lock()
            .expect("LlmRequestLog lock poisoned");
        *p = Some(path);
    }

    /// Configure where terminal records archive append-only (F3). Gated on
    /// [`Self::set_logging_enabled`] like the live mirror — full
    /// request/response bodies are opt-in — but unlike the mirror the file
    /// is append-only and rotated at [`HISTORY_ROTATE_BYTES`], so a busy
    /// turn cannot erase earlier records retrievably. Called once at startup
    /// with `<coding_dir>/logs/traces-history.jsonl`.
    pub fn set_history_log_path(&self, path: PathBuf) {
        let mut p = self
            .shared
            .history_log_path
            .lock()
            .expect("LlmRequestLog lock poisoned");
        *p = Some(path);
    }

    /// Override the history-archive rotation threshold (default
    /// [`HISTORY_ROTATE_BYTES`]). Takes effect on the next writer batch.
    pub fn set_history_rotate_bytes(&self, bytes: u64) {
        self.shared
            .history_rotate_bytes
            .store(bytes, Ordering::Relaxed);
    }

    /// Turn file logging on/off. On enable, ensures the parent directory of
    /// the configured `log_path` exists. On disable, drains the writer queue
    /// first so the file is complete once this returns. No-op when no path
    /// is configured.
    pub fn set_logging_enabled(&self, enabled: bool) {
        if !enabled {
            // Any writes queued while enabled must land before the toggle
            // flips (the writer skips still-queued mirrors once it's off).
            self.flush_file_writes();
        }
        if enabled {
            // Clone the path in a scope that drops the guard BEFORE the disk
            // I/O below — never hold `log_path` across I/O (the same
            // lock-order rule as `writer_main`'s scoped clones; an if-let
            // scrutinee temporary would keep the guard alive through the
            // whole if-let statement — review L1, 2026-08-22).
            let log_path = self
                .shared
                .log_path
                .lock()
                .expect("LlmRequestLog lock poisoned")
                .clone();
            if let Some(path) = log_path {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
        }
        self.shared.log_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Whether file logging is currently on (the Trace tab's checkbox state).
    pub fn logging_enabled(&self) -> bool {
        self.shared.log_enabled.load(Ordering::Relaxed)
    }

    /// Block until every file write queued before this call has completed on
    /// the background writer thread. No-op when no writer thread was ever
    /// spawned (nothing has been queued). These are the seams between the
    /// asynchronous mirroring and code that needs the files on disk NOW —
    /// tests asserting on file contents, and the Trace tab turning logging
    /// off (so the file is final once the checkbox flips).
    pub fn flush_file_writes(&self) {
        let tx = {
            let w = self.writer.lock().expect("LlmRequestLog lock poisoned");
            match w.as_ref() {
                Some(tx) => tx.clone(),
                None => return,
            }
        };
        let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
        if tx.send(Msg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }
}

/// Enforce the global raw-response byte budget across the ring: while the
/// sum of all records' `response_raw` lengths exceeds `budget`, drop the
/// OLDEST records' payloads first (replace with `None` + set
/// `response_evicted`). Records themselves are never removed here — only
/// their raw bodies — so the list view keeps every row (status, usage,
/// timings) and only the inspectable body is lost, flagged as evicted.
///
/// Called with the records lock held from `start`/`with_record`/
/// `set_memory_budget` — single-writer discipline, no re-entrancy.
fn enforce_raw_budget(records: &mut VecDeque<LlmRequestRecord>, budget: usize) {
    let total: usize = records
        .iter()
        .map(|r| r.response_raw.as_ref().map_or(0, |s| s.len()))
        .sum();
    if total <= budget {
        return;
    }
    let mut excess = total - budget;
    // Evict oldest first. `front_mut` walks the deque head — records are
    // appended at the back, so the head is the oldest.
    for r in records.iter_mut() {
        if excess == 0 {
            break;
        }
        if let Some(raw) = r.response_raw.take() {
            excess = excess.saturating_sub(raw.len());
            r.response_evicted = true;
            // An eviction is a wire-visible change (response_raw → None +
            // response_evicted) — bump the mutation counter so the FE's
            // version-skip cannot keep showing an evicted body.
            r.version += 1;
        }
    }
}

/// Cap a request body to `cap_bytes` while preserving its JSON structure:
/// repeatedly find the LONGEST string leaf and truncate it (char-boundary
/// safe, with a `…(+N chars)` marker) until the serialized body fits or
/// nothing worth cutting remains. Returns whether anything was truncated.
///
/// Structure-preserving matters: the trace UI (and the person reading it)
/// needs the request's keys — model, messages, tool schemas — to make sense
/// of a failed request; only the bulky values (huge tool outputs) are cut.
fn cap_request_json(value: &mut serde_json::Value, cap_bytes: usize) -> bool {
    /// Cut at least this many chars per pass, else the marker alone would
    /// make the body LONGER and the loop would never converge.
    const MIN_CUT_CHARS: usize = 32;
    if serde_json::to_string(value).map_or(true, |s| s.len() <= cap_bytes) {
        return false; // already fits (or not serializable — leave verbatim)
    }
    let mut truncated_any = false;
    loop {
        if let Some(leaf) = longest_string_leaf_mut(value) {
            let total_chars = leaf.chars().count();
            // Aim for roughly a quarter of the cap per pass so a few big
            // leaves converge quickly without overshooting on tiny bodies.
            let keep_chars = (cap_bytes / 4).min(total_chars);
            // Progress guarantee: only cut when the savings exceed the
            // marker's own length. A body of many short strings (nothing
            // sensible to cut) falls through and is accepted over-cap.
            if total_chars - keep_chars < MIN_CUT_CHARS {
                break;
            }
            let mut kept: String = leaf.chars().take(keep_chars).collect();
            kept.push_str(&format!("…(+{} chars truncated)", total_chars - keep_chars));
            *leaf = kept;
            truncated_any = true;
        } else {
            break; // no string leaves left to cut
        }
        if serde_json::to_string(value).map_or(true, |s| s.len() <= cap_bytes) {
            break; // fits now
        }
    }
    truncated_any
}

/// Find the longest string leaf in a JSON value and return a mutable
/// reference to it. `None` when the value contains no strings.
fn longest_string_leaf_mut(value: &mut serde_json::Value) -> Option<&mut String> {
    /// Depth-first walk; both borrows share the caller's lifetime so the
    /// winning reference can be returned.
    fn walk<'a>(
        v: &'a mut serde_json::Value,
        best: &mut Option<&'a mut String>,
        best_len: &mut usize,
    ) {
        match v {
            serde_json::Value::String(s) => {
                if s.len() > *best_len {
                    *best_len = s.len();
                    *best = Some(s);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, best, best_len);
                }
            }
            serde_json::Value::Object(map) => {
                for (_, item) in map.iter_mut() {
                    walk(item, best, best_len);
                }
            }
            _ => {}
        }
    }
    let mut best: Option<&mut String> = None;
    let mut best_len = 0;
    walk(value, &mut best, &mut best_len);
    best
}

/// The background file-writer thread. Wakes on a message, then drains the
/// channel dry — coalescing every queued [`Msg::Dirty`] for the same record
/// id into ONE disk write (a stream that enqueues 200 chunk updates pays one
/// rewrite, not 200) — performs at most one batched upsert to the traces
/// mirror and one append per error line, then acknowledges any flushes.
///
/// The thread exits when the channel closes (the last `LlmRequestLog` handle
/// dropped). All disk I/O, redaction, and payload serialization live here —
/// the request/streaming path only ever sends ids / small pre-serialized
/// lines over the channel.
///
/// **Lock-ordering (root cause of the 2026-08-22 freezes):** `with_record`
/// holds `records` while `maybe_log_error` takes `log_path`, so this writer
/// must NEVER hold `log_path` while acquiring `records` (and never across
/// disk I/O). The `log_path`/`error_log_path` clones below run in a scoped
/// block: the guard is dropped BEFORE `records.lock()` and before
/// `write_records_to_file` (previously the `if let` scrutinee temporary held
/// it through both — an AB-BA deadlock against `with_record`, surfaced as 10 s
/// main-thread hangs while sync trace commands waited on `records`; see the
/// regression test `writer_never_holds_log_path_while_waiting_for_records`).
fn writer_main(shared: Arc<Shared>, rx: std::sync::mpsc::Receiver<Msg>) {
    while let Ok(first) = rx.recv() {
        // Drain everything already queued so one wakeup handles it all.
        let mut dirty: Vec<Msg> = vec![first];
        while let Ok(msg) = rx.try_recv() {
            dirty.push(msg);
        }

        // Collect the batch: which record ids to upsert, which error lines to
        // append, and which flush senders to acknowledge at the end. Ids keep
        // arrival order but are deduped (only the latest state is written).
        let mut dirty_ids: Vec<u64> = Vec::new();
        let mut forced_ids: Vec<u64> = Vec::new();
        let mut error_lines: Vec<String> = Vec::new();
        let mut cancel_lines: Vec<String> = Vec::new();
        let mut archive_records: Vec<LlmRequestRecord> = Vec::new();
        let mut flush_acks: Vec<std::sync::mpsc::Sender<()>> = Vec::new();
        for msg in dirty {
            match msg {
                Msg::Dirty(id) => {
                    if !dirty_ids.contains(&id) {
                        dirty_ids.push(id);
                    }
                }
                Msg::DirtyForced(id) => {
                    if !forced_ids.contains(&id) {
                        forced_ids.push(id);
                    }
                }
                Msg::ErrorLine(line) => error_lines.push(line),
                Msg::CancelLine(line) => cancel_lines.push(line),
                Msg::ArchiveRecord(rec) => archive_records.push(rec),
                Msg::Flush(ack) => flush_acks.push(ack),
            }
        }

        // Mirror to traces.jsonl. Ordinary `Dirty` ids land only while the
        // toggle is still on at drain time (a disable between enqueue and
        // drain must skip the mirror — same semantics as the old synchronous
        // write, which checked the flag inline). Forced ids (failed requests)
        // land REGARDLESS of the toggle — that is their entire purpose.
        let mut mirror_ids: Vec<u64> = Vec::new();
        if shared.log_enabled.load(Ordering::Relaxed) {
            mirror_ids.extend(dirty_ids.iter().copied());
        }
        for id in forced_ids {
            if !mirror_ids.contains(&id) {
                mirror_ids.push(id);
            }
        }
        if !mirror_ids.is_empty() {
            // Clone the path in a scope that drops the guard BEFORE
            // `records.lock()` — never hold `log_path` while acquiring
            // `records` or while writing to disk (see the invariant on
            // `with_record`; the old `if let` scrutinee held the guard across
            // both and deadlocked).
            let log_path = shared
                .log_path
                .lock()
                .expect("LlmRequestLog lock poisoned")
                .clone();
            if let Some(path) = log_path {
                // Snapshot the latest state of each mirror id. An id that
                // was evicted from the ring before this point is skipped —
                // its row (if any) keeps the last-written state, which is the
                // documented eventual-latest behavior.
                let snapshot: Vec<LlmRequestRecord> = {
                    let records = shared.records.lock().expect("LlmRequestLog lock poisoned");
                    mirror_ids
                        .iter()
                        .filter_map(|id| records.iter().find(|r| r.id == *id).cloned())
                        .collect()
                };
                if !snapshot.is_empty() {
                    write_records_to_file(&path, &snapshot);
                }
            }
        }

        // Append queued provider-error lines (always-on log, no toggle).
        if !error_lines.is_empty() {
            // Same scoping: clone the path, drop the guard, then do I/O with
            // no `error_log_path` lock held.
            let error_path = shared
                .error_log_path
                .lock()
                .expect("LlmRequestLog lock poisoned")
                .clone();
            if let Some(path) = error_path {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                // N4 (2026-06-14): this log has no toggle and used to grow
                // forever — over the cap, start fresh instead of appending
                // (mirrors the traces.jsonl over-cap rule; each failure is a
                // single compact line, so the file is "recent history past
                // 8 MiB").
                let over_cap =
                    std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_ERROR_FILE_BYTES;
                let mut opts = std::fs::OpenOptions::new();
                if over_cap {
                    opts.write(true).truncate(true);
                } else {
                    opts.create(true).append(true);
                }
                if let Ok(mut f) = opts.open(&path) {
                    use std::io::Write;
                    for line in &error_lines {
                        let _ = writeln!(f, "{line}");
                    }
                }
                // Restrict the log file to the current user (mirrors keys.toml)
                // so the persisted bodies are not world-readable on a shared
                // machine.
                restrict_log_file(&path);
            }
        }

        // Append queued cancel lines (F3: always-on log, no toggle — a user
        // cancel must leave a persistent record even with file logging off;
        // D1 classifies cancels as cancelled, not failed, so they never land
        // in provider-errors.jsonl). Append-only, never rewritten: cancels
        // are rare user actions, so the file needs no cap.
        if !cancel_lines.is_empty() {
            let cancels_path = shared
                .cancels_log_path
                .lock()
                .expect("LlmRequestLog lock poisoned")
                .clone();
            if let Some(path) = cancels_path {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    use std::io::Write;
                    for line in &cancel_lines {
                        let _ = writeln!(f, "{line}");
                    }
                }
                // Restrict the log file to the current user (mirrors keys.toml)
                // so the persisted metadata is not world-readable on a shared
                // machine.
                restrict_log_file(&path);
            }
        }

        // F3: append terminal records to the history archive, so the ring's
        // rotation and the mirror's over-cap fresh-start cannot erase history
        // retrievably. The records were cloned at their terminal transitions
        // (under the records lock — see `with_record`), so eviction before
        // the drain cannot lose them. Gated on log_enabled like the mirror
        // (full request/response bodies are opt-in) — the drain-time check
        // also covers the disable-between-enqueue-and-drain race, mirroring
        // the live mirror's own drain-time check.
        if !archive_records.is_empty() && shared.log_enabled.load(Ordering::Relaxed) {
            let history_path = shared
                .history_log_path
                .lock()
                .expect("LlmRequestLog lock poisoned")
                .clone();
            if let Some(path) = history_path {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                rotate_history_if_over(
                    &path,
                    shared.history_rotate_bytes.load(Ordering::Relaxed),
                );
                let lines: Vec<String> =
                    archive_records.iter().filter_map(redacted_record_line).collect();
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    use std::io::Write;
                    for line in &lines {
                        let _ = writeln!(f, "{line}");
                    }
                }
                // Restrict the log file to the current user (mirrors
                // keys.toml) so the persisted bodies are not
                // world-readable on a shared machine.
                restrict_log_file(&path);
            }
        }

        // Everything queued before the flush is now on disk.
        for ack in flush_acks {
            let _ = ack.send(());
        }
    }
}

/// Serialize one record to its redacted one-line JSON form — the shape shared
/// by the live mirror ([`write_records_to_file`]) and the F3 history archive.
/// Redacts secrets before persisting: the request body may embed an api_key /
/// Authorization header, and the raw response may echo one back. The caller
/// passes a reference; the record is cloned here so the in-memory record
/// keeps its verbatim value. Returns `None` when the record can't serialize
/// (serde_json::Value always serializes, so this is unreachable in practice).
fn redacted_record_line(record: &LlmRequestRecord) -> Option<String> {
    let mut redacted = record.clone();
    redacted.request_json = redact_json(redacted.request_json.take());
    if let Some(raw) = redacted.response_raw.take() {
        redacted.response_raw = Some(redact_text(&raw));
    }
    if let Some(err) = redacted.error.take() {
        redacted.error = Some(redact_text(&err));
    }
    serde_json::to_string(&redacted).ok()
}

/// Upsert a batch of records into the traces mirror as one-line JSON rows —
/// ONE read of the existing file, ONE write, ONE permission restriction,
/// regardless of batch size.
///
/// The file is newline-delimited JSON (`.jsonl`): one compact object per
/// line, keyed by `id`. Because a record is rewritten on every update (status
/// → usage → finish → streamed chunks), the line for a given `id` is replaced
/// in place rather than appended, so the file always holds each request's
/// *latest* state. Keeping a record's JSON to exactly one line preserves the
/// one-row-per-request invariant (pretty-printing would break it).
///
/// Deliberate debugging-aid tradeoffs (the asynchronous writer below is newer
/// than the original synchronous implementation; the caps are unchanged):
/// - **The file is capped** at [`MAX_TRACE_FILE_BYTES`], with residual
///   behavior to be aware of: below the cap each writer wakeup still re-reads
///   + rewrites the whole file (up to ~`MAX_TRACE_FILE_BYTES` of I/O per
///   batch). When the file exceeds the cap the merge is skipped and every row
///   not in the current batch is dropped — so the file is effectively
///   "recent history past 8 MiB", not a complete log. With a hot ring
///   (`MAX_RECORDS` × 2 MiB bodies can exceed the cap on their own) the file
///   can oscillate grow→fresh-start.
/// - **The write is atomic** — the merged file goes to a temp file in the same
///   directory and is renamed over the target, so a reader sees either the old
///   file or the new one, never a truncated or half-written one
///   ([`write_mirror_atomically`]). A failed temp write or rename leaves the
///   previous mirror in place, intact, and the batch is retried on the next
///   writer wakeup.
/// - **The write is asynchronous** — it happens on the writer thread, after
///   the update that queued it returned. The file is eventually-latest per
///   id; a crash before the writer drains can lose the most recent state.
fn write_records_to_file(path: &Path, records: &[LlmRequestRecord]) {
    // Redact secrets before persisting (see `redacted_record_line`): the
    // request body may embed an api_key / Authorization header, and the raw
    // response may echo one back. The record is cloned inside the helper so
    // the in-memory record keeps its verbatim value.
    let lines: Vec<String> = records.iter().filter_map(redacted_record_line).collect();
    if lines.is_empty() {
        return;
    }

    // Read existing lines once, replacing those whose `id` matches a batch id.
    // F2: when the existing file exceeds the cap, skip the merge entirely —
    // rewrite from the batch alone. Re-reading + rewriting an 80 MB file on
    // every writer wakeup was quadratic I/O; starting fresh bounds the file
    // at roughly cap + one batch.
    let over_cap = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_TRACE_FILE_BYTES;
    let existing = if over_cap {
        String::new()
    } else {
        std::fs::read_to_string(path).unwrap_or_default()
    };
    // A kill during a previous rewrite leaves a half-written final line, and a
    // concurrent reader can observe one while a rewrite is in flight. Such a
    // fragment has no id, so it can never be merged — and keeping it verbatim
    // (the behavior until 2027-01-11) carried the corruption forward into every
    // later rewrite. The writer always terminates its output with '\n', so a
    // missing trailing newline is exactly the fragment signature.
    // (LF spelled as 0x0a byte: an escape sequence in this literal kept getting
    // mangled by the editing tooling, and a byte compare on ASCII LF is exact.)
    let existing = if existing.is_empty() || existing.as_bytes().last() == Some(&0x0a) {
        existing
    } else {
        let keep = existing.rfind('\n').map(|i| i + 1).unwrap_or(0);
        existing[..keep].to_string()
    };
    let mut replaced: Vec<bool> = vec![false; lines.len()];
    let mut out: Vec<String> = Vec::new();
    for l in existing.lines() {
        if l.trim().is_empty() {
            continue;
        }
        // Cheap id match without a full parse of every line.
        let id = serde_json::from_str::<serde_json::Value>(l)
            .ok()
            .and_then(|v| v.get("id").and_then(|i| i.as_u64()));
        let hit = id.and_then(|id| {
            records
                .iter()
                .position(|r| r.id == id)
                .filter(|&i| !replaced[i])
        });
        if let Some(i) = hit {
            out.push(lines[i].clone());
            replaced[i] = true;
        } else {
            out.push(l.to_string());
        }
    }
    for (i, line) in lines.iter().enumerate() {
        if !replaced[i] {
            out.push(line.clone());
        }
    }
    let mut content = out.join("\n");
    content.push('\n');
    write_mirror_atomically(path, &content);
}

/// Replace the mirror file atomically: write a temp file in the same directory,
/// restrict it to the current user, then rename it over the target.
///
/// Why not `std::fs::write`: the mirror MERGES the ring into the file, so every
/// writer wakeup rewrites the whole thing — and `fs::write` truncates first. A
/// concurrent reader (the analysis tooling reads this file live) could therefore
/// observe an empty file or a half-written final line, and a kill mid-write left
/// the truncation on disk permanently: the 444 KB unterminated line found in
/// `.coding/logs/traces.jsonl` on 2027-01-11 (cache-hit round 6, R6-3). A rename
/// is atomic on both Unix and Windows (std uses MoveFileEx with
/// replace-existing), so a reader observes either the old file or the new one;
/// a failed temp write leaves the previous mirror untouched, and a failed
/// rename (e.g. a Windows reader holding the target open without
/// FILE_SHARE_DELETE) drops the temp and leaves the same previous mirror in
/// place — degraded, never torn, retried on the next writer wakeup.
///
/// The temp name carries the PID so two instances on the same project (the
/// double-start conflict dialog warns, it does not lock) cannot interleave
/// temp writes and rename a torn temp into place.
fn write_mirror_atomically(path: &Path, content: &str) {
    let Some(dir) = path.parent() else {
        return;
    };
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("traces.jsonl");
    let tmp = dir.join(format!("{file_name}.tmp{}", std::process::id()));
    if std::fs::write(&tmp, content).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // Restrict the log file to the current user (mirrors keys.toml) so the
    // persisted bodies are not world-readable on a shared machine — applied to
    // the temp file, because the rename carries its metadata to the target.
    restrict_log_file(&tmp);
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Rotate the F3 history archive when it exceeds `threshold`: rename it to
/// `<stem>-<unix-micros>.<ext>` and prune old archives, keeping the newest
/// [`HISTORY_ARCHIVE_KEEP`]. Microsecond timestamps keep consecutive
/// rotations in one writer thread collision-free. Best-effort — a failed
/// rotation must never break the append (the file just keeps growing until
/// the next batch).
fn rotate_history_if_over(path: &Path, threshold: u64) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size <= threshold {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("traces-history.jsonl");
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((s, e)) => (s, e),
        None => (file_name, "jsonl"),
    };
    let archived = path.with_file_name(format!("{stem}-{ts}.{ext}"));
    let _ = std::fs::rename(path, &archived);
    prune_history_archives(path, HISTORY_ARCHIVE_KEEP);
}

/// Delete rotated history archives beyond the newest `keep` (sorted by the
/// unix timestamp in their names, descending). Best-effort.
fn prune_history_archives(path: &Path, keep: usize) {
    let dir = match path.parent() {
        Some(d) => d,
        None => return,
    };
    let stem_prefix = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => format!("{s}-"),
        None => return,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut archives: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let ts = name
                .strip_prefix(&stem_prefix)?
                .strip_suffix(".jsonl")?
                .parse::<u64>()
                .ok()?;
            Some((ts, e.path()))
        })
        .collect();
    archives.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));
    for (_, p) in archives.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

/// Best-effort permission restriction for a log file: applies the same
/// user-only restriction as `keys.toml` (Unix `0600` / Windows user-only
/// DACL) and swallows any error as a warning — a restriction failure must
/// never break the request path (the data is already on disk).
fn restrict_log_file(path: &Path) {
    if let Err(e) = restrict_permissions(path) {
        eprintln!(
            "trace: warning — could not restrict permissions on '{}': {e}",
            path.display()
        );
    }
}

/// Recursively redact secret-shaped values from a JSON value before it is
/// persisted to a log file. Any object key named `api_key` or `authorization`
/// (case-insensitive) has its value replaced with `"[REDACTED]"`. The value is
/// not dropped (the key's presence is still visible) but its secret content is
/// scrubbed. Recurses into nested objects and arrays.
fn redact_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                let lower = k.to_ascii_lowercase();
                if lower == "api_key" || lower == "authorization" {
                    out.insert(k, serde_json::Value::String("[REDACTED]".into()));
                } else {
                    out.insert(k, redact_json(v));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(redact_json).collect())
        }
        other => other,
    }
}

/// Redact secret-shaped substrings from free-form text (raw response bodies,
/// error strings) before persistence. Two patterns:
/// - `Bearer <token>` → `Bearer [REDACTED]` (Authorization header values).
/// - JSON `"api_key":"<value>"` / `"authorization":"<value>"` (case-insensitive
///   key) → the value replaced with `[REDACTED]` (catches secrets embedded in
///   a raw JSON response that wasn't parsed as a structured value).
fn redact_text(s: &str) -> String {
    use std::sync::OnceLock;
    static BEARER: OnceLock<regex::Regex> = OnceLock::new();
    static JSON_KEY: OnceLock<regex::Regex> = OnceLock::new();
    let bearer = BEARER.get_or_init(|| {
        // `Bearer ` followed by non-whitespace token chars.
        regex::Regex::new(r"(?i)Bearer\s+[^\s]+").unwrap()
    });
    let json_key = JSON_KEY.get_or_init(|| {
        // A JSON string key "api_key" or "authorization" (case-insensitive)
        // followed by a colon and a quoted string value.
        regex::Regex::new(r#"(?i)"(api_key|authorization)"\s*:\s*"[^"]*""#).unwrap()
    });
    let s = bearer.replace_all(s, "Bearer [REDACTED]");
    let s = json_key.replace_all(&s, r#""$1":"[REDACTED]""#);
    s.into_owned()
}

impl Default for LlmRequestLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, Instant};

    fn json_body() -> serde_json::Value {
        serde_json::json!({ "model": "test-model", "messages": [{ "role": "user", "content": "hi" }] })
    }

    #[test]
    fn start_returns_monotonic_ids_and_records_the_request() {
        let log = LlmRequestLog::new();
        let first = log.start("a", "http://a/v1/", "p", json_body());
        let second = log.start("b", "http://b/v1/", "p", json_body());
        assert!(second > first, "ids must increase");

        let detail = log.get(first).expect("first record present");
        assert_eq!(detail.model, "a");
        assert_eq!(detail.base_url, "http://a/v1/");
        assert_eq!(detail.provider, "p");
        assert_eq!(
            detail.request_json["messages"][0]["content"],
            serde_json::json!("hi")
        );
        assert_eq!(detail.http_status, None);
        assert_eq!(detail.usage, None);
        assert_eq!(detail.error, None);
        assert!(!detail.response_truncated);

        // The provider attribution must survive onto the lightweight summary
        // row (the Trace tab's list renders provider next to model).
        let summary = log
            .list()
            .into_iter()
            .find(|s| s.id == first)
            .expect("summary for first record present");
        assert_eq!(summary.provider, "p");
    }

    #[test]
    fn list_and_get_and_clear_roundtrip() {
        let log = LlmRequestLog::new();
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.set_status(id, 200);

        let list = log.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].model, "m");
        assert_eq!(list[0].http_status, Some(200));
        // Summaries are the *light* shape — they carry no payload fields.
        let summary_json = serde_json::to_value(&list[0]).expect("serialize");
        assert!(summary_json.get("request_json").is_none());
        assert!(summary_json.get("response_raw").is_none());

        log.clear();
        assert!(log.list().is_empty());
        assert!(log.get(id).is_none(), "clear drops every record");
    }

    #[test]
    fn raw_budget_evicts_oldest_payload_first_and_keeps_rows() {
        // The global memory budget: when the sum of raw payloads exceeds it,
        // the OLDEST payload is dropped first — but its row (and the newest
        // record's full payload) survive.
        let log = LlmRequestLog::new();
        log.set_memory_budget(100); // tiny budget: two 80-byte payloads overflow it
        let old = log.start("old", "http://u/v1/", "p", json_body());
        log.append_response(old, &"a".repeat(80));
        let new = log.start("new", "http://u/v1/", "p", json_body());
        log.append_response(new, &"b".repeat(80));
        // 160 stored > 100 budget → oldest evicted.
        let old_detail = log.get(old).expect("row survives eviction");
        assert!(old_detail.response_raw.is_none(), "oldest payload evicted");
        assert!(old_detail.response_evicted, "eviction is flagged");
        let new_detail = log.get(new).expect("newest record present");
        assert_eq!(
            new_detail.response_raw.as_ref().map(|s| s.len()),
            Some(80),
            "newest payload kept intact"
        );
        assert!(!new_detail.response_evicted);
        // The list still shows both rows.
        assert_eq!(log.list().len(), 2);
        assert!(log.list()[0].response_evicted);
    }

    #[test]
    fn version_bumps_on_every_mutation_and_stays_stable_when_idle() {
        // The FE's detail-poll skip (perf review L5) relies on the mutation
        // counter: it must start at 0, be carried by BOTH the detail and the
        // cheap summary row, bump on every mutation, and stay stable while
        // the record is idle (repeated reads never bump it).
        let log = LlmRequestLog::new();
        let id = log.start("m", "http://u/v1/", "p", json_body());
        assert_eq!(log.get(id).expect("present").version, 0);
        assert_eq!(log.list()[0].version, 0, "summary carries the version");

        log.set_status(id, 200);
        assert_eq!(log.list()[0].version, 1);
        log.append_response(id, "chunk");
        assert_eq!(log.get(id).expect("present").version, 2);

        // Idle: reads never mutate — the version must not move, or the FE
        // would re-fetch the (up to ~2 MiB) detail on every poll.
        assert_eq!(log.list()[0].version, 2);
        assert_eq!(log.get(id).expect("present").version, 2);
    }

    #[test]
    fn version_bumps_on_payload_eviction() {
        // An eviction is a wire-visible change (response_raw → None +
        // response_evicted): the evicted record's version must bump, or the
        // FE's version-skip would keep showing the evicted body.
        let log = LlmRequestLog::new();
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.append_response(id, "payload");
        let before = log.get(id).expect("present").version;

        // Budget shrink evicts immediately (the set_memory_budget path).
        log.set_memory_budget(0);
        let detail = log.get(id).expect("row survives eviction");
        assert!(detail.response_evicted);
        assert!(detail.version > before, "shrink-eviction bumps the version");

        // Growth evicts the OLDEST payload (the with_record →
        // enforce_raw_budget path): the second record's append must bump the
        // FIRST record's version, not just its own.
        let log = LlmRequestLog::new();
        log.set_memory_budget(100); // two 80-byte payloads overflow it
        let old = log.start("old", "http://u/v1/", "p", json_body());
        log.append_response(old, &"a".repeat(80));
        let old_version = log.get(old).expect("present").version;
        let new = log.start("new", "http://u/v1/", "p", json_body());
        log.append_response(new, &"b".repeat(80));
        // 160 stored > 100 budget → the oldest payload is evicted.
        let old_detail = log.get(old).expect("row survives eviction");
        assert!(old_detail.response_evicted);
        assert!(
            old_detail.version > old_version,
            "growth-eviction bumps the evicted record's version"
        );
    }

    #[test]
    fn set_memory_budget_applies_immediately_to_stored_payloads() {
        let log = LlmRequestLog::new();
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.append_response(id, &"x".repeat(1000));
        assert!(log.get(id).unwrap().response_raw.is_some());
        // Shrinking the budget evicts the now-over-budget payload at once.
        log.set_memory_budget(100);
        let detail = log.get(id).unwrap();
        assert!(detail.response_raw.is_none());
        assert!(detail.response_evicted);
    }

    #[test]
    fn evicted_record_does_not_regrow_on_further_appends() {
        let log = LlmRequestLog::new();
        log.set_memory_budget(50);
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.append_response(id, &"x".repeat(80)); // evicts itself
        assert!(log.get(id).unwrap().response_evicted);
        log.append_response(id, "more"); // must not re-grow past the budget
        let detail = log.get(id).unwrap();
        assert!(
            detail.response_raw.is_none(),
            "evicted payload stays evicted"
        );
    }

    #[test]
    fn request_body_cap_truncates_longest_leaf_and_flags() {
        let log = LlmRequestLog::new();
        log.set_request_body_cap(512);
        let mut body = json_body();
        body["messages"][0]["content"] =
            serde_json::json!(format!("huge tool output: {}", "y".repeat(4096)));
        let id = log.start("m", "http://u/v1/", "p", body);
        let detail = log.get(id).unwrap();
        assert!(detail.request_truncated, "oversized body is flagged");
        // Structure preserved: keys survive, the long leaf is a stub.
        let content = detail.request_json["messages"][0]["content"]
            .as_str()
            .unwrap();
        assert!(content.starts_with("huge tool output: y"));
        assert!(content.contains("chars truncated"), "marker notes the cut");
        assert!(content.len() < 512, "leaf well under the cap");
        assert_eq!(
            detail.request_json["model"],
            serde_json::json!("test-model")
        );
        // The serialized whole fits the cap.
        let serialized = serde_json::to_string(&detail.request_json).unwrap();
        assert!(serialized.len() <= 512, "whole body fits the cap");
    }

    #[test]
    fn request_body_under_cap_stored_verbatim() {
        let log = LlmRequestLog::new();
        let body = json_body();
        let id = log.start("m", "http://u/v1/", "p", body.clone());
        let detail = log.get(id).unwrap();
        assert!(!detail.request_truncated);
        assert_eq!(detail.request_json, body);
    }

    #[test]
    fn is_complete_flips_on_finish_or_error_only() {
        let log = LlmRequestLog::new();
        let id = log.start("m", "http://u/v1/", "p", json_body());
        assert!(
            !log.get(id).unwrap().is_complete,
            "in-flight is not complete"
        );
        log.set_status(id, 200);
        log.append_response(id, "data");
        assert!(
            !log.get(id).unwrap().is_complete,
            "streaming is not complete"
        );
        log.finish(id, "stop");
        assert!(log.get(id).unwrap().is_complete, "finish completes");

        let other = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(other, 500, "boom");
        assert!(log.get(other).unwrap().is_complete, "error completes");
    }

    #[test]
    fn evicts_oldest_past_max_records() {
        let log = LlmRequestLog::new();
        let first_id = log.start("m", "http://u/v1/", "p", json_body());
        for _ in 0..MAX_RECORDS {
            log.start("m", "http://u/v1/", "p", json_body());
        }
        let list = log.list();
        assert_eq!(list.len(), MAX_RECORDS);
        assert!(log.get(first_id).is_none(), "oldest record evicted");
        assert_eq!(list[0].id, first_id + 1, "the rest survive in order");
    }

    #[test]
    fn raw_response_cap_truncates_and_flags() {
        let log = LlmRequestLog::new();
        let id = log.start("m", "http://u/v1/", "p", json_body());

        // Under the cap: everything is kept, no truncation.
        log.append_response(id, "hello ");
        log.append_response(id, "world");
        let detail = log.get(id).expect("present");
        assert_eq!(detail.response_raw.as_deref(), Some("hello world"));
        assert!(!detail.response_truncated);

        // Past the cap: only the prefix up to the cap is kept, flagged.
        let over = "x".repeat(MAX_RAW_RESPONSE_BYTES + 10);
        log.append_response(id, &over);
        let detail = log.get(id).expect("present");
        assert!(detail.response_truncated);
        let raw = detail.response_raw.expect("some raw stored");
        assert_eq!(raw.len(), MAX_RAW_RESPONSE_BYTES);
        assert!(raw.starts_with("hello worldxxxxx") || raw.starts_with("hello worldx"));

        // Once truncated, further appends are dropped (stays at the cap).
        log.append_response(id, "more");
        let detail = log.get(id).expect("present");
        assert_eq!(
            detail.response_raw.expect("raw").len(),
            MAX_RAW_RESPONSE_BYTES
        );

        // A cap that lands inside a multi-byte char must not panic: the slice
        // is backed up to a char boundary (the partial char is dropped, and
        // the truncation flag already signals the cut).
        let mid = log.start("m", "http://u/v1/", "p", json_body());
        log.append_response(mid, &"x".repeat(MAX_RAW_RESPONSE_BYTES - 1));
        log.append_response(mid, "é"); // 2-byte char, only 1 byte remains
        let detail = log.get(mid).expect("present");
        assert!(detail.response_truncated);
        assert_eq!(
            detail.response_raw.expect("raw").len(),
            MAX_RAW_RESPONSE_BYTES - 1
        );
    }

    #[test]
    fn usage_finish_fail_update_the_record() {
        let log = LlmRequestLog::new();

        let usage_id = log.start("m", "http://u/v1/", "p", json_body());
        log.set_usage(
            usage_id,
            LlmUsage {
                prompt: 100,
                completion: 40,
                reasoning: 10,
                cached: 80,
            },
            Some(150),
            Some(900),
            None,
            Some(25),
        );
        let d = log.get(usage_id).expect("present");
        let u = d.usage.expect("usage set");
        assert_eq!(
            (u.prompt, u.completion, u.reasoning, u.cached),
            (100, 40, 10, 80)
        );
        assert_eq!(d.ttft_ms, Some(150));
        assert_eq!(d.generation_ms, Some(900));
        assert_eq!(d.connect_ms, Some(25), "connect phase recorded with the usage");
        assert!(d.tools_ms.is_none(), "no tools phase yet");
        assert!((u.cache_hit_ratio() - 0.8).abs() < f64::EPSILON);

        let finish_id = log.start("m", "http://u/v1/", "p", json_body());
        log.finish(finish_id, "tool_calls");
        assert_eq!(
            log.get(finish_id)
                .expect("present")
                .finish_reason
                .as_deref(),
            Some("tool_calls")
        );

        let fail_id = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(fail_id, 429, "rate limited");
        let d = log.get(fail_id).expect("present");
        assert_eq!(d.http_status, Some(429));
        assert_eq!(d.error.as_deref(), Some("rate limited"));

        // Updates to an evicted id are silent no-ops.
        log.with_record(9_999, |_| panic!("must not run for a missing record"));
        log.set_status(9_999, 500);
        log.append_response(9_999, "ghost");
    }

    #[test]
    fn streaming_usage_overlay_lands_and_is_overwritten_by_final_usage() {
        // Live overlay while streaming (backlog b5503915): growing chars/4
        // estimates land on the in-flight record so the Trace tab shows
        // progress during a long reasoning phase, and the authoritative
        // set_usage at stream end overwrites them.
        let log = LlmRequestLog::new();
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.update_streaming_usage(id, 10, 25);
        log.update_streaming_usage(id, 40, 90);
        let d = log.get(id).expect("present");
        let u = d.usage.expect("overlay set");
        assert_eq!(
            (u.prompt, u.completion, u.reasoning, u.cached),
            (0, 40, 90, 0)
        );
        assert!(!d.is_complete, "an overlay must not complete the record");

        // The authoritative usage replaces the overlay at stream end…
        log.set_usage(
            id,
            LlmUsage {
                prompt: 100,
                completion: 42,
                reasoning: 7,
                cached: 80,
            },
            Some(150),
            Some(900),
            None,
            Some(25),
        );
        let d = log.get(id).expect("present");
        assert_eq!(
            d.usage,
            Some(LlmUsage {
                prompt: 100,
                completion: 42,
                reasoning: 7,
                cached: 80,
            })
        );

        // …and a terminal record ignores late pushes (never resurrected).
        log.finish(id, "stop");
        log.update_streaming_usage(id, 9_999, 9_999);
        let d = log.get(id).expect("present");
        assert_eq!(d.usage.unwrap().completion, 42, "terminal record untouched");
    }

    #[test]
    fn streaming_usage_overlay_is_a_no_op_for_unknown_ids() {
        let log = LlmRequestLog::new();
        log.update_streaming_usage(9_999, 10, 10); // must not panic
        assert!(log.get(9_999).is_none());
    }

    #[test]
    fn cache_hit_ratio_guards_zero_prompt() {
        let zero = LlmUsage::default();
        assert_eq!(zero.cache_hit_ratio(), 0.0);
        let half = LlmUsage {
            prompt: 200,
            cached: 100,
            ..Default::default()
        };
        assert!((half.cache_hit_ratio() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn set_tools_ms_targets_the_record_by_id() {
        let log = LlmRequestLog::new();

        // Unknown/evicted ids are silent no-ops (must not panic).
        log.set_tools_ms(9_999, 500);

        // Two requests from the SAME response's perspective: the tools phase
        // belongs to the FIRST (its response produced the tool calls), even
        // when a concurrent agent's request (the second record) was created
        // in between — attribution is by id, not "newest in the ring"
        // (review M1 regression).
        let first = log.start("m", "http://u/v1/", "p", json_body());
        let second = log.start("m", "http://u/v1/", "p", json_body());
        log.set_tools_ms(first, 1234);
        assert_eq!(log.get(first).expect("present").tools_ms, Some(1234));
        assert_eq!(log.get(second).expect("present").tools_ms, None);

        // The summary carries the phase field too.
        let summary: Vec<LlmRequestSummary> = log.list();
        let first_summary = summary.iter().find(|s| s.id == first).expect("listed");
        assert_eq!(first_summary.tools_ms, Some(1234));
        assert!(first_summary.connect_ms.is_none(), "no usage → no connect phase");

        // A later overwrite wins.
        log.set_tools_ms(first, 999);
        assert_eq!(log.get(first).expect("present").tools_ms, Some(999));
    }

    #[test]
    fn set_guard_cut_targets_the_record_by_id() {
        let log = LlmRequestLog::new();

        // Unknown/evicted ids are silent no-ops (must not panic).
        log.set_guard_cut(
            9_999,
            GuardCutDetails {
                boundary: "<custom_stop>".into(),
                byte_idx: 0,
                delta_len: 0,
                chars_before: 0,
                prefix_empty: true,
            },
        );

        let first = log.start("m", "http://u/v1/", "p", json_body());
        let second = log.start("m", "http://u/v1/", "p", json_body());
        log.set_guard_cut(
            first,
            GuardCutDetails {
                boundary: "<custom_stop>".into(),
                byte_idx: 3,
                delta_len: 20,
                chars_before: 42,
                prefix_empty: false,
            },
        );

        let cut = log
            .get(first)
            .expect("present")
            .guard_cut
            .expect("recorded");
        assert_eq!(cut.boundary, "<custom_stop>");
        assert_eq!(cut.byte_idx, 3);
        assert_eq!(cut.delta_len, 20);
        assert_eq!(cut.chars_before, 42);
        assert!(!cut.prefix_empty);
        // Other records untouched.
        assert!(log.get(second).expect("present").guard_cut.is_none());

        // The summary carries the cut too (list rows can badge it).
        let summary: Vec<LlmRequestSummary> = log.list();
        let first_summary = summary.iter().find(|s| s.id == first).expect("listed");
        assert!(first_summary.guard_cut.is_some());
    }

    #[test]
    fn set_raw_tool_calls_targets_the_record_by_id_and_caps() {
        // Backlog e8b39d72 H1: the generation-vs-harness discrimination tap —
        // the delivered args must land on the record of the request whose
        // response produced them (by id, never "newest in the ring"), survive
        // byte-verbatim, and respect the per-call/count caps.
        let log = LlmRequestLog::new();

        // Unknown/evicted ids are silent no-ops (must not panic).
        log.set_raw_tool_calls(9_999, vec![]);

        let first = log.start("m", "http://u/v1/", "p", json_body());
        let second = log.start("m", "http://u/v1/", "p", json_body());
        let long_args = "x".repeat(MAX_RAW_TOOL_CALL_BYTES + 10);
        log.set_raw_tool_calls(
            first,
            vec![
                ("call-1".into(), "file_edit".into(), "{\"command\":\"ls\"}".into()),
                ("call-2".into(), "shell".into(), long_args),
            ],
        );
        let rec = log.get(first).expect("present");
        let calls = rec.raw_tool_calls.as_ref().expect("delivered args");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call-1");
        assert_eq!(calls[0].name, "file_edit");
        assert_eq!(calls[0].arguments, "{\"command\":\"ls\"}");
        assert!(!calls[0].truncated);
        // The over-cap call is cut at the per-call cap, char-boundary safe,
        // and flagged.
        assert_eq!(calls[1].arguments.len(), MAX_RAW_TOOL_CALL_BYTES);
        assert!(calls[1].truncated);
        // Attribution by id: the concurrent record must not receive them.
        assert!(log.get(second).expect("present").raw_tool_calls.is_none());
        // The count cap: more calls than MAX_RAW_TOOL_CALLS are dropped.
        let many: Vec<(String, String, String)> = (0..MAX_RAW_TOOL_CALLS + 5)
            .map(|i| (format!("c{i}"), "shell".into(), "{}".into()))
            .collect();
        log.set_raw_tool_calls(first, many);
        let rec = log.get(first).expect("present");
        assert_eq!(
            rec.raw_tool_calls.as_ref().expect("delivered args").len(),
            MAX_RAW_TOOL_CALLS
        );
    }

    #[test]
    fn set_ttft_ms_stamps_waiting_bucket_on_error_record() {
        // A connect timeout or pre-first-byte stall must be attributed to the
        // waiting (ttft) bucket — not left null as before (user report
        // 2026-12-04: the 30s connect timeout was invisible in the trace).
        let log = LlmRequestLog::new();

        // Unknown/evicted ids are silent no-ops (must not panic).
        log.set_ttft_ms(9_999, 500);

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.set_ttft_ms(id, 30_000);
        assert_eq!(log.get(id).expect("present").ttft_ms, Some(30_000));

        // The summary carries the field too.
        let summary: Vec<LlmRequestSummary> = log.list();
        let s = summary.iter().find(|s| s.id == id).expect("listed");
        assert_eq!(s.ttft_ms, Some(30_000));
    }

    #[test]
    fn set_generation_ms_stamps_generation_bucket_on_error_record() {
        // A mid-stream stall (after data was flowing) must be attributed to
        // the generation bucket — not left null.
        let log = LlmRequestLog::new();

        // Unknown/evicted ids are silent no-ops (must not panic).
        log.set_generation_ms(9_999, 500);

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.set_generation_ms(id, 90_000);
        assert_eq!(log.get(id).expect("present").generation_ms, Some(90_000));

        // The summary carries the field too.
        let summary: Vec<LlmRequestSummary> = log.list();
        let s = summary.iter().find(|s| s.id == id).expect("listed");
        assert_eq!(s.generation_ms, Some(90_000));
    }

    #[test]
    fn set_prep_and_compact_ms_target_the_record_by_id() {
        let log = LlmRequestLog::new();

        // Unknown/evicted ids are silent no-ops (must not panic).
        log.set_prep_ms(9_999, 500);
        log.set_compact_ms(9_999, 600);

        // Attribution by id, exactly like the tools phase: the prep/compact
        // timings park on the client and land on the NEXT record it creates
        // — never on whatever record happens to be newest in the ring.
        let first = log.start("m", "http://u/v1/", "p", json_body());
        let second = log.start("m", "http://u/v1/", "p", json_body());
        log.set_prep_ms(first, 1234);
        log.set_compact_ms(first, 567);
        assert_eq!(log.get(first).expect("present").prep_ms, Some(1234));
        assert_eq!(log.get(first).expect("present").compact_ms, Some(567));
        assert_eq!(log.get(second).expect("present").prep_ms, None);
        assert_eq!(log.get(second).expect("present").compact_ms, None);

        // The summary carries the phase fields too.
        let summary: Vec<LlmRequestSummary> = log.list();
        let first_summary = summary.iter().find(|s| s.id == first).expect("listed");
        assert_eq!(first_summary.prep_ms, Some(1234));
        assert_eq!(first_summary.compact_ms, Some(567));
    }

    #[test]
    fn file_logging_mirrors_latest_state_to_one_jsonl_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());
        log.set_logging_enabled(true);
        assert!(log.logging_enabled());

        let id = log.start("m", "http://u/v1/", "p", json_body());
        // Mirrored on start (no update yet) so a crash after the writer drains
        // still leaves the request line — the flush makes the async write
        // observable before asserting.
        log.flush_file_writes();
        let after_start = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after_start.lines().filter(|l| !l.trim().is_empty()).count(),
            1
        );
        log.set_status(id, 200);
        log.append_response(id, "chunk1 ");
        log.append_response(id, "chunk2");
        log.set_usage(
            id,
            LlmUsage {
                prompt: 10,
                completion: 5,
                reasoning: 0,
                cached: 8,
            },
            Some(1),
            Some(2),
            None,
            Some(3),
        );
        log.finish(id, "stop");
        // The tools phase is attributed AFTER the stream ends — the file
        // mirror must see it too (review L1: the mutation must route through
        // the dirty-notification path).
        log.set_tools_ms(id, 7);
        log.flush_file_writes();

        // One line, holding the final state (not one line per update).
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "exactly one row for the single request");
        let row: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row["id"], serde_json::json!(id));
        assert_eq!(row["http_status"], serde_json::json!(200));
        assert_eq!(row["response_raw"], serde_json::json!("chunk1 chunk2"));
        assert_eq!(row["finish_reason"], serde_json::json!("stop"));
        assert_eq!(row["usage"]["cached"], serde_json::json!(8));
        assert_eq!(row["connect_ms"], serde_json::json!(3));
        assert_eq!(row["tools_ms"], serde_json::json!(7));
    }

    /// Read the history archive's ids in file order (test helper). A missing
    /// file reads as empty so red-state assertions fail on the comparison.
    fn history_ids(path: &Path) -> Vec<u64> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["id"]
                    .as_u64()
                    .unwrap()
            })
            .collect()
    }

    /// List the rotated history archives (test helper): sibling files named
    /// `traces-history-<unixts>.jsonl`, sorted by name.
    fn history_archives(dir: &Path) -> Vec<PathBuf> {
        let mut archives: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("traces-history-") && n.ends_with(".jsonl"))
                    .unwrap_or(false)
            })
            .collect();
        archives.sort();
        archives
    }

    /// F3 regression: terminal records archive append-only to
    /// `traces-history.jsonl`, so a turn making more than [`MAX_RECORDS`]
    /// requests cannot erase earlier records retrievably. The live mirror
    /// keeps its own semantics; the archive gains every terminal record
    /// exactly once (deduped across re-mirrors) and never an in-flight one,
    /// and it retains the full history even after the mirror's over-cap
    /// fresh-start drops rows.
    #[test]
    fn mirror_drops_a_crash_truncated_trailing_fragment() {
        // A kill during the mirror's rewrite leaves a half-written final line.
        // Before 2027-01-11 the merge loop copied that fragment forward into
        // every subsequent rewrite (it has no id, so it never matched), making
        // the corruption permanent — the 444 KB unterminated line found in
        // `.coding/logs/traces.jsonl` (cache-hit round 6, R6-3).
        let dir = tempfile::tempdir().unwrap();
        let traces_path = dir.path().join("traces.jsonl");
        let fragment = "{\"id\":2,\"request_json\":{\"messages\":[{\"content\":\"cut off he";
        std::fs::write(&traces_path, format!("{{\"id\":1}}
{fragment}")).unwrap();

        let log = LlmRequestLog::new();
        log.set_log_file_path(traces_path.clone());
        log.set_logging_enabled(true);
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.finish(id, "stop");
        log.flush_file_writes();

        let content = std::fs::read_to_string(&traces_path).unwrap();
        assert!(
            !content.contains("cut off he"),
            "the truncated trailing fragment must not survive the next write"
        );
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("every mirrored line must be valid JSON: {e}"));
        }
        // No temp file is left behind by the atomic replace.
        assert!(!dir.path().join(format!("traces.jsonl.tmp{}", std::process::id())).exists());
    }

    #[test]
    fn mirror_write_failure_leaves_the_previous_mirror_intact() {
        // The mirror REWRITES the whole file on every writer wakeup, so a failed
        // write must not destroy what is already there — which is exactly what a
        // truncate-first `std::fs::write` does: the target is emptied before the
        // new bytes land. The atomic replace writes a temp file first and
        // renames it over the target only on success.
        //
        // The transient half of the R6-3 defect — a concurrent reader observing
        // a half-written line — is guarded by construction (the rename) and not
        // by a race test: an earlier version of this test raced a reader thread
        // against 200 rewrites of a 300 KB body and never caught a
        // truncate-in-place implementation, so it could not fail without the fix
        // and was dropped rather than shipped as theater.
        let dir = tempfile::tempdir().unwrap();
        let traces_path = dir.path().join("traces.jsonl");
        let seeded = "{\"id\":1,\"finish_reason\":\"stop\"}\n";
        std::fs::write(&traces_path, seeded).unwrap();
        // Occupy the temp path with a directory so the temp write must fail.
        std::fs::create_dir(dir.path().join(format!("traces.jsonl.tmp{}", std::process::id())))
            .unwrap();

        let log = LlmRequestLog::new();
        log.set_log_file_path(traces_path.clone());
        log.set_logging_enabled(true);
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.finish(id, "stop");
        log.flush_file_writes();

        assert_eq!(
            std::fs::read_to_string(&traces_path).unwrap(),
            seeded,
            "a failed mirror write must leave the previous file untouched"
        );
    }

    #[test]
    fn terminal_records_archive_to_history_beyond_the_ring() {
        let dir = tempfile::tempdir().unwrap();
        let traces_path = dir.path().join("traces.jsonl");
        let history_path = dir.path().join("traces-history.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(traces_path.clone());
        log.set_history_log_path(history_path.clone());
        log.set_logging_enabled(true);

        // MAX_RECORDS + 8 terminal requests: the ring evicts the oldest 8,
        // the archive must hold every one of them in request order.
        let ids: Vec<u64> = (0..MAX_RECORDS + 8)
            .map(|_| {
                let id = log.start("m", "http://u/v1/", "p", json_body());
                log.finish(id, "stop");
                id
            })
            .collect();
        log.flush_file_writes();
        assert_eq!(
            history_ids(&history_path),
            ids,
            "every terminal record archived exactly once, in request order"
        );

        // The live mirror keeps its own semantics: one row per id ever
        // snapshotted by a drain — at least the final ring (MAX_RECORDS
        // rows), at most every id (rows below the cap are never dropped).
        // The exact count depends on writer drain timing, so pin the range.
        let mirror_rows = std::fs::read_to_string(&traces_path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        assert!(
            mirror_rows >= MAX_RECORDS && mirror_rows <= ids.len(),
            "the live mirror keeps its ring-snapshot semantics ({mirror_rows})"
        );

        // An in-flight (non-terminal) record is not archived...
        let inflight = log.start("m", "http://u/v1/", "p", json_body());
        log.flush_file_writes();
        assert_eq!(history_ids(&history_path), ids);

        // ...but archiving resumes the moment it turns terminal, without
        // re-appending anything already archived (the terminal-transition
        // enqueue fires exactly once — is_complete never flips back).
        log.finish(inflight, "stop");
        log.flush_file_writes();
        let mut archived = history_ids(&history_path);
        assert_eq!(archived.len(), ids.len() + 1);
        assert_eq!(archived.pop(), Some(inflight));

        // The acceptance scenario: once the mirror blows past its cap and
        // fresh-starts (dropping every row not in the current batch), the
        // archive still holds the full history.
        std::fs::write(
            &traces_path,
            "x".repeat(MAX_TRACE_FILE_BYTES as usize + 1024),
        )
        .unwrap();
        let last = log.start("m", "http://u/v1/", "p", json_body());
        log.finish(last, "stop");
        log.flush_file_writes();
        let mirror_rows_after = std::fs::read_to_string(&traces_path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        assert!(
            mirror_rows_after < ids.len(),
            "the mirror fresh-started (bounded live view)"
        );
        assert_eq!(
            history_ids(&history_path).len(),
            ids.len() + 2,
            "the archive retained everything across the mirror's fresh-start"
        );
    }

    /// F3 regression: the history archive rotates at
    /// [`HISTORY_ROTATE_BYTES`] (overridable for tests) and prunes to the
    /// newest [`HISTORY_ARCHIVE_KEEP`] archives, staying bounded while
    /// remaining retrievable. Part 1 exercises the rotation/prune mechanics
    /// directly (no writer-timing dependence); part 2 exercises the
    /// writer wiring end-to-end with a deterministic pre-oversized file.
    #[test]
    fn history_file_rotates_and_prunes_archives() {
        // Part 1 — the mechanics, called directly.
        let dir = tempfile::tempdir().unwrap();
        let history_path = dir.path().join("traces-history.jsonl");
        std::fs::write(&history_path, "x".repeat(200)).unwrap();
        rotate_history_if_over(&history_path, 64);
        assert!(!history_path.exists(), "the oversized file was renamed");
        assert_eq!(history_archives(dir.path()).len(), 1);
        std::fs::write(&history_path, "small").unwrap();
        rotate_history_if_over(&history_path, 64);
        assert!(history_path.exists(), "an under-threshold file is not rotated");
        assert_eq!(history_archives(dir.path()).len(), 1);
        for i in 0..6 {
            std::fs::write(
                dir.path().join(format!("traces-history-{}.jsonl", 1_000_000 + i)),
                "x\n",
            )
            .unwrap();
        }
        std::fs::write(&history_path, "x".repeat(200)).unwrap();
        rotate_history_if_over(&history_path, 64);
        assert_eq!(
            history_archives(dir.path()).len(),
            HISTORY_ARCHIVE_KEEP,
            "archives pruned to the keep window"
        );

        // Part 2 — end-to-end through the writer: an oversized history file
        // rotates on the next terminal record; the fresh file holds only
        // post-rotation records. Deterministic: the pre-written file is over
        // the threshold, so whichever batch carries the terminal record
        // rotates first.
        let dir = tempfile::tempdir().unwrap();
        let history_path = dir.path().join("traces-history.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(dir.path().join("traces.jsonl"));
        log.set_history_log_path(history_path.clone());
        log.set_history_rotate_bytes(64);
        log.set_logging_enabled(true);
        std::fs::write(&history_path, "x".repeat(200)).unwrap();
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.finish(id, "stop");
        log.flush_file_writes();
        assert_eq!(
            history_archives(dir.path()).len(),
            1,
            "the oversized file rotated on the next terminal record"
        );
        assert_eq!(
            history_ids(&history_path),
            vec![id],
            "the fresh file holds only post-rotation records"
        );
    }

    /// F2 regression (2026-04-19 freeze diagnosis): `traces.jsonl` used to be
    /// read + rewritten whole on every writer wakeup, growing forever. When
    /// the existing file exceeds [`MAX_TRACE_FILE_BYTES`] the merge is skipped
    /// and the file is rewritten from the batch alone, bounding it.
    #[test]
    fn trace_file_over_cap_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        // Pre-existing oversized junk (> cap).
        std::fs::write(&path, "x".repeat(MAX_TRACE_FILE_BYTES as usize + 1024)).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_TRACE_FILE_BYTES);

        // One record written on top: only that record's row remains.
        let record = LlmRequestRecord {
            id: 7,
            ts_ms: 1,
            model: "m".into(),
            base_url: "http://u/v1/".into(),
            provider: "p".into(),
            request_json: json_body(),
            response_raw: None,
            raw_tool_calls: None,
            guard_cut: None,
            http_status: Some(200),
            usage: None,
            finish_reason: Some("stop".into()),
            ttft_ms: None,
            generation_ms: None,
            reasoning_ms: None,
            connect_ms: None,
            tools_ms: None,
            prep_ms: None,
            compact_ms: None,
            backoff_ms: None,
            stall_ms: None,
            error: None,
            cancelled: false,
            response_truncated: false,
            request_truncated: false,
            response_evicted: false,
            is_complete: true,
            version: 0,
        };
        write_records_to_file(&path, &[record]);

        let meta = std::fs::metadata(&path).unwrap();
        assert!(
            meta.len() < MAX_TRACE_FILE_BYTES,
            "file shrinks below the cap (was > 8 MiB of junk)"
        );
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(lines.len(), 1, "only the fresh record's row remains");
        assert_eq!(lines[0]["id"], serde_json::json!(7));
        assert_eq!(lines[0]["http_status"], serde_json::json!(200));
    }

    #[test]
    fn file_logging_appends_distinct_requests_and_is_gated_by_toggle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());

        // Disabled by default: nothing is written.
        assert!(!log.logging_enabled());
        let off_id = log.start("m", "http://u/v1/", "p", json_body());
        log.finish(off_id, "stop");
        assert!(!path.exists(), "no file written while disabled");

        // Enable: two requests → two rows, each keyed by its own id.
        log.set_logging_enabled(true);
        let a = log.start("m", "http://u/v1/", "p", json_body());
        let b = log.start("m", "http://u/v1/", "p", json_body());
        log.finish(a, "stop");
        log.finish(b, "tool_calls");
        log.flush_file_writes();
        let content = std::fs::read_to_string(&path).unwrap();
        let ids: Vec<u64> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["id"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(ids, vec![a, b], "each request is its own row in order");

        // Disable again: further updates stop being mirrored. (A NON-error
        // update — an error-state update like set_status(500) would be
        // force-mirrored by design, see
        // failed_request_full_body_mirrored_even_when_logging_disabled.)
        log.set_logging_enabled(false);
        log.append_response(a, "more");
        let content_after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content_after, content, "writes stop when disabled");
    }

    #[test]
    fn file_logging_coalesces_many_updates_into_one_row() {
        // The asynchronous writer drains its queue in batches: many updates
        // to the same record (e.g. dozens of streamed SSE chunks) must land
        // as exactly ONE row holding the record's latest state — the row is
        // upserted per drain, not appended per update.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());
        log.set_logging_enabled(true);

        let id = log.start("m", "http://u/v1/", "p", json_body());
        for i in 0..50 {
            log.append_response(id, &format!("chunk{i} "));
        }
        log.finish(id, "stop");
        log.flush_file_writes();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "50 chunk updates → one row, not 50: {content}"
        );
        let row: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row["id"], serde_json::json!(id));
        let expected = (0..50).map(|i| format!("chunk{i} ")).collect::<String>();
        assert_eq!(row["response_raw"], serde_json::json!(expected));
        assert_eq!(row["finish_reason"], serde_json::json!("stop"));
    }

    /// Read the error-log file as (id, http_status, error) triples.
    fn read_error_log(path: &std::path::Path) -> Vec<(u64, Option<u16>, String)> {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                (
                    v["id"].as_u64().unwrap(),
                    v["http_status"].as_u64().map(|s| s as u16),
                    v["error"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn error_log_appends_on_fail_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("provider-errors.jsonl");
        let log = LlmRequestLog::new();
        log.set_error_log_path(path.clone());

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(id, 502, "bad gateway");
        log.flush_file_writes();
        let rows = read_error_log(&path);
        assert_eq!(rows.len(), 1, "one line after the first failure");
        assert_eq!(rows[0].0, id);
        assert_eq!(rows[0].1, Some(502));
        assert!(rows[0].2.contains("bad gateway"), "error text is logged");

        // A second fail() on the SAME request must not append another line.
        log.fail(id, 503, "again");
        log.flush_file_writes();
        let rows = read_error_log(&path);
        assert_eq!(rows.len(), 1, "deduped by request id");

        // A different request that fails → a second line.
        let id2 = log.start("m2", "http://u/v1/", "p", json_body());
        log.fail(id2, 429, "rate limited");
        log.flush_file_writes();
        let rows = read_error_log(&path);
        assert_eq!(rows.len(), 2, "one line per failed request");
        assert_eq!(rows[1].0, id2);
        assert_eq!(rows[1].1, Some(429));
    }

    #[test]
    fn error_log_over_cap_starts_fresh() {
        // N4 (2026-06-14): provider-errors.jsonl is always-on and used to
        // grow forever. Over MAX_ERROR_FILE_BYTES the next append truncates —
        // only the new failure's line remains.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-errors.jsonl");
        // Pre-existing oversized junk (> cap).
        std::fs::write(&path, "x".repeat(MAX_ERROR_FILE_BYTES as usize + 1024)).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_ERROR_FILE_BYTES);

        let log = LlmRequestLog::new();
        log.set_error_log_path(path.clone());
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(id, 500, "boom");
        log.flush_file_writes();

        let meta = std::fs::metadata(&path).unwrap();
        assert!(
            meta.len() < MAX_ERROR_FILE_BYTES,
            "file shrinks below the cap after the over-cap append"
        );
        let rows = read_error_log(&path);
        assert_eq!(rows.len(), 1, "only the fresh failure's line remains");
        assert_eq!(rows[0].0, id);
        assert_eq!(rows[0].1, Some(500));
    }

    #[test]
    fn error_log_dedup_set_is_bounded() {
        // N3 (2026-06-14): error_logged_ids grew one entry per failed request
        // for the process lifetime. Past 10k entries it clears; the only
        // observable effect is that an OLD id re-failing after the clear can
        // append one duplicate diagnostic line — harmless.
        let log = LlmRequestLog::new();
        // Fill the set past the bound directly (10_001 failures via the real
        // path would be slow); the cap check lives in maybe_log_error's
        // locked section, so exercising it via Shared is equivalent.
        {
            let mut seen = log
                .shared
                .error_logged_ids
                .lock()
                .expect("LlmRequestLog lock poisoned");
            for i in 0..=10_001 {
                seen.insert(i);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-errors.jsonl");
        log.set_error_log_path(path.clone());
        // This failure triggers the clear (> 10k entries) then re-inserts
        // its own id — the append must still happen exactly once.
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(id, 500, "boom");
        log.flush_file_writes();
        let rows = read_error_log(&path);
        assert_eq!(rows.len(), 1, "the failure still lands exactly once");
        let len = log
            .shared
            .error_logged_ids
            .lock()
            .expect("LlmRequestLog lock poisoned")
            .len();
        assert!(len <= 10_001, "set reset to a small size, got {len}");
    }

    #[test]
    fn error_log_skips_successful_requests() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("provider-errors.jsonl");
        let log = LlmRequestLog::new();
        log.set_error_log_path(path.clone());

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.set_status(id, 200);
        log.set_usage(
            id,
            LlmUsage {
                prompt: 100,
                completion: 40,
                reasoning: 10,
                cached: 80,
            },
            Some(150),
            Some(900),
            None,
            Some(25),
        );
        log.finish(id, "stop");
        log.flush_file_writes();
        assert!(
            !path.exists() || read_error_log(&path).is_empty(),
            "a successful request must not be logged as an error"
        );
    }

    #[test]
    fn error_log_is_always_on_without_trace_checkbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-errors.jsonl");
        let log = LlmRequestLog::new();
        // Set the error-log path ONLY — never enable the traces.jsonl opt-in.
        log.set_error_log_path(path.clone());
        assert!(!log.logging_enabled(), "traces.jsonl mirror is still off");

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(id, 401, "unauthorized");
        log.flush_file_writes();
        let rows = read_error_log(&path);
        assert_eq!(rows.len(), 1, "error log writes regardless of the checkbox");
        assert_eq!(rows[0].1, Some(401));
    }

    #[test]
    fn error_log_transport_error_status_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-errors.jsonl");
        let log = LlmRequestLog::new();
        log.set_error_log_path(path.clone());

        // Status 0 = no HTTP response at all (transport failure, e.g.
        // "failed to start stream: ..." — openai.rs logs it with status 0).
        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(id, 0, "failed to start stream: connection refused");
        log.flush_file_writes();
        let rows = read_error_log(&path);
        assert_eq!(rows.len(), 1, "transport failures are logged");
        assert_eq!(rows[0].1, Some(0));
        assert!(rows[0].2.contains("connection refused"));
    }

    #[test]
    fn traces_redact_api_key_in_request_body() {
        // A request body that embeds an api_key must be redacted before it is
        // persisted to traces.jsonl — the secret must never reach disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());
        log.set_logging_enabled(true);

        let secret_body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "api_key": "sk-super-secret-value"
        });
        let id = log.start("m", "http://u/v1/", "p", secret_body);
        log.finish(id, "stop");
        log.flush_file_writes();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("sk-super-secret-value"),
            "api_key leaked into traces.jsonl: {content}"
        );
        assert!(content.contains("[REDACTED]"), "expected redaction marker");
    }

    #[test]
    fn traces_redact_authorization_in_response_body() {
        // A raw response that echoes an Authorization header (Bearer token)
        // must be redacted before persistence.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());
        log.set_logging_enabled(true);

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.append_response(id, "Authorization: Bearer abc-123-secret-token");
        log.finish(id, "stop");
        log.flush_file_writes();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("abc-123-secret-token"),
            "Bearer token leaked into traces.jsonl: {content}"
        );
        assert!(content.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn traces_redact_json_key_in_response_case_insensitive() {
        // A raw JSON response with an "Authorization" key (mixed case) must be
        // redacted by the text regex.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());
        log.set_logging_enabled(true);

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.append_response(id, r#"{"Authorization":"sk-leaked","other":"keep"}"#);
        log.finish(id, "stop");
        log.flush_file_writes();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("sk-leaked"),
            "api_key leaked into traces.jsonl: {content}"
        );
        assert!(content.contains("keep"), "non-secret content must survive");
    }

    #[test]
    fn error_log_redacts_bearer_in_error_text() {
        // An error string that embeds a Bearer token must be redacted.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-errors.jsonl");
        let log = LlmRequestLog::new();
        log.set_error_log_path(path.clone());

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(id, 401, "401 Unauthorized: Bearer sk-leaked-token");
        log.flush_file_writes();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("sk-leaked-token"),
            "Bearer token leaked into error log: {content}"
        );
        assert!(content.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn error_log_file_is_user_only() {
        // H2: the always-on error log must be restricted to the current user
        // (Unix 0600; Windows user-only DACL) — it persists request bodies
        // that may embed secrets, so it gets the same treatment as keys.toml.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-errors.jsonl");
        let log = LlmRequestLog::new();
        log.set_error_log_path(path.clone());

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.fail(id, 500, "boom");
        log.flush_file_writes();

        assert!(path.exists(), "error log must exist after a failure");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "provider-errors.jsonl should be 0600, got {:o}",
                mode
            );
        }
        // On Windows we assert only that the file exists (the DACL is applied
        // best-effort and asserting the exact ACE set would couple the test to
        // the OS account layout).
    }

    #[test]
    fn traces_file_is_user_only() {
        // H2: the opt-in traces log must also be restricted to the current
        // user once it is written.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());
        log.set_logging_enabled(true);

        let id = log.start("m", "http://u/v1/", "p", json_body());
        log.finish(id, "stop");
        log.flush_file_writes();

        assert!(path.exists(), "traces log must exist after logging");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "traces.jsonl should be 0600, got {:o}",
                mode
            );
        }
    }

    #[test]
    fn redact_json_redacts_secret_keys() {
        let v = serde_json::json!({
            "api_key": "sk-secret",
            "Authorization": "Bearer xyz",
            "nested": {"API_KEY": "sk-inner", "safe": "keep"},
            "arr": [{"api_key": "sk-arr"}]
        });
        let redacted = redact_json(v);
        let s = serde_json::to_string(&redacted).unwrap();
        assert!(!s.contains("sk-secret"));
        assert!(!s.contains("sk-inner"));
        assert!(!s.contains("sk-arr"));
        assert!(!s.contains("Bearer xyz"));
        assert!(s.contains("[REDACTED]"));
        assert!(s.contains("keep"), "non-secret content must survive");
    }

    #[test]
    fn redact_text_redacts_bearer_and_json_keys() {
        let s = "header Authorization: Bearer abc123 and {\"api_key\":\"sk-x\"}";
        let out = redact_text(s);
        assert!(!out.contains("abc123"));
        assert!(!out.contains("sk-x"));
        assert!(out.contains("Bearer [REDACTED]"));
        assert!(out.contains("\"api_key\":\"[REDACTED]\""));
    }

    #[test]
    fn failed_request_full_body_mirrored_even_when_logging_disabled() {
        // A failed request must be fully diagnosable WITHOUT opting into file
        // logging: its full body (secrets redacted) + error text land in
        // traces.jsonl even when log_enabled is false. Both the traces path
        // and the always-on error path are set at startup in the app, so this
        // mirrors production.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());
        let error_path = dir.path().join("logs").join("provider-errors.jsonl");
        log.set_error_log_path(error_path.clone());
        assert!(!log.logging_enabled(), "trace mirror stays off");

        let body = serde_json::json!({
            "model": "glm-5.3",
            "api_key": "sk-super-secret",
            "messages": [{"role": "user", "content": "marker-12345"}]
        });
        let id = log.start("glm-5.3", "https://api.z.ai/v1/", "p", body);
        log.fail(id, 400, "boom: The messages parameter is illegal");
        log.flush_file_writes();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "exactly one row — the failed request: {content}"
        );
        let row: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(row["id"], serde_json::json!(id));
        assert_eq!(row["http_status"], serde_json::json!(400));
        assert!(
            row["error"].as_str().unwrap().contains("boom"),
            "error text is mirrored"
        );
        assert!(
            row["request_json"]["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("marker-12345"),
            "request body is mirrored"
        );
        // Redaction holds on the forced path.
        assert_eq!(
            row["request_json"]["api_key"],
            serde_json::json!("[REDACTED]")
        );
        assert!(
            !content.contains("sk-super-secret"),
            "secret leaked: {content}"
        );

        // A successful request under the same disabled logging still writes
        // nothing new (the forced mirror fires only for failures).
        let ok_id = log.start(
            "glm-5.3",
            "https://api.z.ai/v1/",
            "z.ai",
            serde_json::json!({"model": "glm-5.3", "messages": []}),
        );
        log.set_status(ok_id, 200);
        log.finish(ok_id, "stop");
        log.flush_file_writes();
        let content_after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            content_after, content,
            "successful requests still write nothing when disabled"
        );
    }

    #[test]
    fn writer_never_holds_log_path_while_waiting_for_records() {
        // Regression (recurring UI freeze, 2026-08-22): `writer_main` used
        // `if let Some(path) = shared.log_path.lock()...` — the edition-2021
        // scrutinee temporary kept the `log_path` guard alive across
        // `records.lock()` and `write_records_to_file`. Meanwhile
        // `with_record` holds `records` while `maybe_log_error` takes
        // `log_path` (the DirtyForced branch, logging off) — a classic AB-BA
        // lock-order inversion: writer holds log_path + waits on records,
        // with_record holds records + waits on log_path → deadlock (seen in
        // the app as ~10 s main-thread hangs while sync trace commands waited
        // on those locks). Post-fix the writer clones the path and drops the
        // guard BEFORE `records.lock()`, so the cycle cannot form.
        //
        // This orchestrates exactly that interleaving and asserts the whole
        // thing completes. Pre-fix it detects the inversion (log_path held
        // across the records wait) and the AB-side thread deadlocks; either
        // way the test FAILS. Post-fix it completes cleanly. No assertion
        // fires while a guard is held, so a failure never poisons the
        // mutexes (the knock-on writer panic that would otherwise follow).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traces.jsonl");
        let log = LlmRequestLog::new();
        log.set_log_file_path(path.clone());
        let error_path = dir.path().join("logs").join("provider-errors.jsonl");
        log.set_error_log_path(error_path.clone());
        assert!(
            !log.logging_enabled(),
            "logging off exercises the DirtyForced branch"
        );

        let id = log.start("m", "http://u/v1/", "p", json_body());

        // Hold `records` so the writer cannot finish its mirror early — any
        // writer reaching `records.lock()` parks here, and we can observe
        // whether it still holds `log_path` at that point.
        let records_guard = log
            .shared
            .records
            .lock()
            .expect("LlmRequestLog lock poisoned");

        // Wake the writer with a mirror request for this id.
        log.ensure_writer().send(Msg::DirtyForced(id)).unwrap();

        // Watch `log_path` while the writer is parked on `records`. A single
        // brief `Err` is the writer's momentary clone critical section
        // (lock + clone + unlock ≈ nanoseconds); only a *sustained* hold
        // (several consecutive polls) means the guard is alive across the
        // records wait. Post-fix: clean streak → proceed. Pre-fix: sustained
        // Err → inversion detected.
        let mut inversion = false;
        let mut clean_streak = 0;
        let mut err_streak = 0;
        let probe_deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < probe_deadline && clean_streak < 40 {
            match log.shared.log_path.try_lock() {
                Ok(g) => {
                    drop(g);
                    clean_streak += 1;
                    err_streak = 0;
                }
                Err(_) => {
                    err_streak += 1;
                    if err_streak >= 10 {
                        inversion = true;
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        // Drive the AB side: a thread that takes records then log_path, in
        // with_record's order. Post-fix it completes once we drop our guard;
        // pre-fix it deadlocks against the writer (which holds log_path and
        // waits on our records guard). The completion signal tells us which.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let shared = Arc::clone(&log.shared);
        std::thread::spawn(move || {
            let r = shared.records.lock().expect("LlmRequestLog lock poisoned");
            let p = shared.log_path.lock().expect("LlmRequestLog lock poisoned");
            drop(p);
            drop(r);
            let _ = done_tx.send(());
        });

        // Release records so the writer (post-fix) mirrors and the helper
        // (post-fix) proceeds. Pre-fix the helper is still wedged on log_path.
        drop(records_guard);

        let helper_done = done_rx.recv_timeout(Duration::from_secs(3)).is_ok();

        // Now that every guard we touched is released, assert the outcome.
        assert!(
            !inversion,
            "lock-order inversion: log_path was held while the writer waited on records \
             (the guard must be dropped before records.lock())"
        );
        assert!(
            helper_done,
            "deadlock: the records→log_path path (with_record's order) wedged against the writer"
        );
    }

    #[test]
    fn cancelled_is_a_noop_after_a_genuine_error() {
        // D1: a real stream error that lands just before the consumer exits
        // must not be masked by the consumer-drop cancellation stamp — the
        // error state (and its provider-errors.jsonl mirror) stands.
        let log = LlmRequestLog::new();
        let id = log.start("m", "http://u/v1/", "p", serde_json::json!({}));
        log.fail(id, 500, "boom");
        log.cancelled(id, 500);
        let s = &log.list()[0];
        assert_eq!(s.error.as_deref(), Some("boom"));
        assert!(!s.cancelled);
        assert!(s.is_complete);
        assert_eq!(s.http_status, Some(500));
    }
}
