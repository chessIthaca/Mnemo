// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Trace backend: every model call the engine made — attributed, priced,
//! searched, and paged Rust-side, plus the two retention tiers and export.
//!
//! One [`LlmRequestLog`] lives on the engine's brain and holds the lead's and
//! every worker's requests — plan c3ba2b58 step A stamps each record with its
//! [`TraceAttribution`] (who made the call, why, for which worker/plan/turn).
//! This module (step B) turns that shared log into Mnemo2's trace surface:
//!
//! - **Live ring.** The kernel's in-memory ring, raised to
//!   [`TRACE_RING_CAPACITY`] records (the kernel default is 32; the decision
//!   2026-11-02 settled 128 for Mnemo2 — request bodies are capped per call,
//!   not in aggregate, and the metadata journal covers older history).
//! - **Metadata journal.** [`TraceJournal`] appends one terminal
//!   [`LlmRequestSummary`] per completed call to
//!   `.coding/logs/trace-index.jsonl`, rotated at [`TRACE_JOURNAL_ROTATE_BYTES`]
//!   keeping [`TRACE_JOURNAL_ARCHIVE_KEEP`] archives. It is on by default and
//!   contains NO prompt/response/reasoning text — only the metadata the
//!   summary carries (identity, model, status, usage, timings, flags), so
//!   retained calls still list after a restart without a privacy change.
//!   On open the journal is re-read and merged by `run_id:id`; rows that the
//!   live ring no longer holds list as `retained` metadata.
//! - **Full bodies — opt-in.** The kernel's `traces.jsonl` mirror and
//!   `traces-history.jsonl` archive stay exactly as Mnemo1 built them:
//!   off by default (`trace_set_log_bodies`), secrets always redacted, and
//!   failed requests always mirrored (redacted) — the most valuable calls to
//!   debug must survive regardless of the toggle (decision 2026-11-02).
//!
//! [`trace_list`] merges the two tiers, applies the Rust-side filters
//! (agent/model/status/purpose/time/turn, content search with scopes), pages
//! newest-first with an opaque cursor, and prices every row with the same
//! shared [`crate::mnemo2::pricing`] rule the Cost tab uses.
//!
//! [`trace_export`] bundles the chosen calls into
//! `.coding/logs/trace-exports/trace-<YYYYMMDD-HHMMSS>.jsonl` (UTC) behind a
//! v1 manifest, and [`trace_reveal_export`] opens the OS file manager — only
//! for canonicalized paths inside that folder.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::State;

use mnemo::config::PricingEntry;
use mnemo::headless::{Engine, TraceApi};
use mnemo::provider::trace::{
    LlmRequestRecord, LlmRequestSummary, TraceActorKind, TraceAttribution, TracePurpose,
};

use crate::mnemo2::ipc::Mnemo2State;
use crate::mnemo2::pricing::{cost_micros, resolve_rates, ModelRates};
use crate::mnemo2::runtime::LeadRuntime;

/// Mnemo2's live ring capacity in records (decision 2026-11-02: 128, not the
/// design draft's 256 — request bodies are capped per call but not in
/// aggregate; the metadata journal covers older history).
pub(crate) const TRACE_RING_CAPACITY: usize = 128;

/// The metadata journal's rotation threshold: 8 MiB per file.
pub(crate) const TRACE_JOURNAL_ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// Metadata-journal archives kept after rotation (four, like the kernel's
/// body-history rotation).
pub(crate) const TRACE_JOURNAL_ARCHIVE_KEEP: u32 = 4;

/// One list page's hard row cap (the frozen wire contract caps `limit` at 200).
pub(crate) const TRACE_PAGE_MAX: u32 = 200;

/// Content-search match cap: a searching query stops after this many matches
/// and reports `capped` so the UI can ask for a narrower time range.
pub(crate) const TRACE_SEARCH_MATCH_CAP: usize = 1000;

/// Safety bound on rows scanned by one SEARCHING query (the journal is
/// inherently bounded by its rotation, but a pathological search must not
/// scan without end). Exact, unsearched queries never hit this.
pub(crate) const TRACE_SCAN_ROW_CAP: usize = 50_000;

/// The metadata journal's active file (archives append `.1`..=`.4`).
pub(crate) const TRACE_INDEX_FILE: &str = "trace-index.jsonl";

/// The kernel's opt-in body-history file base name (active + `-<micros>`
/// archives) — sized, deleted, and detail-scanned here.
pub(crate) const TRACE_HISTORY_FILE: &str = "traces-history.jsonl";

/// Export bundles land in this folder under `.coding/logs/` (gitignored with
/// the rest of the logs directory).
pub(crate) const TRACE_EXPORTS_DIR: &str = "trace-exports";

/// How often the background journal writer polls the live ring for calls
/// that went terminal since the last pass.
const TRACE_JOURNAL_POLL: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Wire types (the frozen trace contract; mnemo2/src/lib/trace.ts mirrors them)
// ---------------------------------------------------------------------------

/// Status classes the list can filter on. Cancellation is never an error
/// (a user interrupt is not a provider failure), and a guard cut is its own
/// class (the response was cut by a stop-sequence guard, not failed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceListStatus {
    /// Finished with a provider finish reason and no error.
    Complete,
    /// Still streaming or waiting for the first byte.
    InProgress,
    /// Ended in an error (HTTP >= 400, transport, or stream failure).
    Failed,
    /// The consumer dropped the stream mid-flight (a user interrupt).
    Cancelled,
    /// A client-side stop-sequence guard cut the response.
    GuardCut,
}

/// Where a content search looks. `Reasoning` scans the streamed response body
/// (reasoning arrives as part of it — the same corpus as `Response`), and
/// `Metadata` scans identity/model/status text only, which is all a retained
/// journal-only call has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceSearchScope {
    /// Request + response + tool calls + metadata.
    All,
    /// The serialized request body.
    Request,
    /// The raw streamed response.
    Response,
    /// The reasoning inside the streamed response.
    Reasoning,
    /// Delivered tool-call names and arguments.
    Tools,
    /// Model/provider/error/finish-reason and attribution text.
    Metadata,
}

/// One filtered page query. All fields are optional; `limit` defaults to 100
/// and clamps to [`TRACE_PAGE_MAX`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TraceListQuery {
    /// `"lead"`, a worker id, an agent/session id, or `"unattributed"`.
    pub agent: Option<String>,
    /// Exact model id (as observed).
    pub model: Option<String>,
    /// Status class filter.
    pub status: Option<TraceListStatus>,
    /// Purpose filter (one of the kernel's `TracePurpose` wire strings).
    pub purpose: Option<String>,
    /// Inclusive lower bound on `ts_ms`.
    pub from_ms: Option<u64>,
    /// Inclusive upper bound on `ts_ms`.
    pub to_ms: Option<u64>,
    /// Only calls of this turn.
    pub turn_id: Option<String>,
    /// Case-insensitive content search text.
    pub search: Option<String>,
    /// Which corpora the search covers (default [`TraceSearchScope::All`]).
    pub search_scope: Option<TraceSearchScope>,
    /// The opaque cursor the previous page returned.
    pub cursor: Option<String>,
    /// Page size, defaulting to 100 and clamped to [`TRACE_PAGE_MAX`].
    #[serde(default)]
    pub limit: u32,
}

impl TraceListQuery {
    /// The effective page size: at least 1, at most [`TRACE_PAGE_MAX`].
    fn page_limit(&self) -> usize {
        if self.limit == 0 {
            100
        } else {
            self.limit.min(TRACE_PAGE_MAX) as usize
        }
    }
}

/// One row of the trace list: the kernel's summary priced and flagged for
/// retention. Flattened, so the wire shape is the summary's fields plus the
/// three extras — exactly the TS `TraceRow`.
#[derive(Debug, Clone, Serialize)]
pub struct TraceRow {
    #[serde(flatten)]
    pub summary: LlmRequestSummary,
    /// Estimated cost in micro-USD from the shared pricing rule; `None`
    /// when the model has no price (a dash, never a fake `$0`).
    pub cost_micros: Option<u64>,
    /// Whether the pricing rule knew this model.
    pub priced: bool,
    /// True when only journal metadata survived for this call — no body
    /// detail is available.
    pub retained: bool,
}

/// One page of [`trace_list`]: the rows plus honest totals.
#[derive(Debug, Clone, Serialize)]
pub struct TraceListPage {
    /// The page's rows, newest first.
    pub rows: Vec<TraceRow>,
    /// How many calls match the filters (a floor when `capped`).
    pub total: u64,
    /// How many matching calls have no configured price.
    pub unpriced: u64,
    /// Whether more matching rows exist past this page.
    pub has_more: bool,
    /// The cursor for the next page, when `has_more`.
    pub next_cursor: Option<String>,
    /// The backend capped the result set; narrow the filters to see the rest.
    pub capped: bool,
}

/// One call in full by its durable `run_id:id` key — the TS `TraceDetail`
/// (the record's flattened fields plus the pricing/retention extras).
#[derive(Debug, Clone, Serialize)]
pub struct TraceDetail {
    #[serde(flatten)]
    pub record: LlmRequestRecord,
    pub cost_micros: Option<u64>,
    pub priced: bool,
    /// Always false: a detail response implies bodies are available (live
    /// ring or logged history), never journal-only metadata.
    pub retained: bool,
}

/// [`trace_settings`]'s readout about on-disk trace logging.
#[derive(Debug, Clone, Serialize)]
pub struct TraceSettings {
    /// Whether full request/response bodies are mirrored to disk (opt-in).
    pub log_bodies: bool,
    /// The live ring's capacity in records.
    pub ring_capacity: u32,
    /// The metadata journal's size on disk (active + archives).
    pub journal_bytes: u64,
    /// The opt-in body history's size on disk (active + archives).
    pub history_bytes: u64,
}

/// [`trace_delete_history`]'s outcome, for the confirmation sheet.
#[derive(Debug, Clone, Serialize)]
pub struct TraceHistoryDeletion {
    /// How many files were removed.
    pub deleted_files: u32,
    /// How many bytes they held.
    pub bytes: u64,
}

/// [`trace_export`]'s scope: one call, a hand-picked set, or the filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceExportScope {
    /// One call, keyed by `run_id:id` in `keys`.
    Call,
    /// The `keys` list, verbatim.
    Selected,
    /// Every call matching `query`'s filters — `query.limit` and
    /// `query.cursor` are ignored by design (decision 2026-11-02).
    Filtered,
}

/// [`trace_export`]'s request.
#[derive(Debug, Clone, Deserialize)]
pub struct TraceExportRequest {
    pub scope: TraceExportScope,
    /// Durable `run_id:id` keys, for scope `call`/`selected`.
    pub keys: Option<Vec<String>>,
    /// The filters to re-run, for scope `filtered`.
    pub query: Option<TraceListQuery>,
}

/// [`trace_export`]'s outcome.
#[derive(Debug, Clone, Serialize)]
pub struct TraceExportResult {
    /// The bundle's absolute path (inside `.coding/logs/trace-exports/`).
    pub path: String,
    /// How many call lines the bundle carries.
    pub count: u32,
    /// Whether any line carries full request/response bodies.
    pub includes_bodies: bool,
}// ---------------------------------------------------------------------------
// Key, cursor, and classification helpers
// ---------------------------------------------------------------------------

/// The durable key type: (run_id, id) — ids restart with the process, so the
/// pair is what survives restarts and merges the journal.
type TraceKey = (String, u64);

/// The sort/cursor key: (ts_ms, run_id, id) — a total order over records.
type SortKey = (u64, String, u64);

/// A row's sort key. Newest first = descending over this triple.
fn sort_key(s: &LlmRequestSummary) -> SortKey {
    (s.ts_ms, s.run_id.clone(), s.id)
}

/// Encode a page cursor: the last returned row's sort key. Run ids contain
/// no `:` (they are `r<ms>-<pid>-<seq>`), so the triple round-trips through
/// `rsplitn` without escaping.
fn cursor_encode(key: &SortKey) -> String {
    format!("{}:{}:{}", key.0, key.1, key.2)
}

/// Decode a page cursor back into its sort key.
fn cursor_decode(cursor: &str) -> Option<SortKey> {
    let mut parts = cursor.rsplitn(3, ':');
    let id = parts.next()?.parse().ok()?;
    let run = parts.next()?;
    let ts = parts.next()?.parse().ok()?;
    Some((ts, run.to_string(), id))
}

/// Parse a `run_id:id` wire key (the export `keys` format).
fn parse_trace_key(key: &str) -> Option<TraceKey> {
    let (run, id) = key.split_once(':')?;
    Some((run.to_string(), id.parse().ok()?))
}

/// Parse the purpose wire string into the kernel enum.
fn parse_purpose(purpose: &str) -> Option<TracePurpose> {
    match purpose {
        "turn" => Some(TracePurpose::Turn),
        "compaction" => Some(TracePurpose::Compaction),
        "memory_consolidation" => Some(TracePurpose::MemoryConsolidation),
        "routing" => Some(TracePurpose::Routing),
        "other" => Some(TracePurpose::Other),
        _ => None,
    }
}

/// The purpose's wire string (serde's snake_case form, without serde).
fn purpose_str(purpose: TracePurpose) -> &'static str {
    match purpose {
        TracePurpose::Turn => "turn",
        TracePurpose::Compaction => "compaction",
        TracePurpose::MemoryConsolidation => "memory_consolidation",
        TracePurpose::Routing => "routing",
        TracePurpose::Other => "other",
    }
}

/// Classify one summary into a status class. Cancellation wins over errors
/// (an interrupt is not a failure); a guard cut is its own class.
fn row_status(s: &LlmRequestSummary) -> TraceListStatus {
    if s.cancelled {
        TraceListStatus::Cancelled
    } else if s.guard_cut.is_some() {
        TraceListStatus::GuardCut
    } else if s.is_complete && s.error.is_some() {
        TraceListStatus::Failed
    } else if s.is_complete {
        TraceListStatus::Complete
    } else {
        TraceListStatus::InProgress
    }
}

/// Whether the agent filter selects this attribution. `"lead"` matches lead
/// calls; `"unattributed"` matches legacy records with no attribution;
/// anything else matches that worker/agent/session id.
fn agent_matches(filter: &str, a: Option<&TraceAttribution>) -> bool {
    let Some(a) = a else {
        return filter == "unattributed";
    };
    match filter {
        "lead" => a.actor_kind == TraceActorKind::Lead,
        id => {
            a.worker_id.as_deref() == Some(id)
                || a.agent_id == id
                || a.session_id == id
        }
    }
}

/// Price one summary: `(cost_micros, priced)` from the shared rule. A priced
/// model with no usage costs an honest zero; an unpriced model is `None`
/// (the UI shows a dash, never `$0`).
fn price_summary(
    s: &LlmRequestSummary,
    pricing: &[PricingEntry],
    rate_cache: &mut HashMap<String, Option<ModelRates>>,
) -> (Option<u64>, bool) {
    let rates = *rate_cache
        .entry(s.model.clone())
        .or_insert_with(|| resolve_rates(&s.model, pricing));
    match rates {
        Some(rates) => {
            let cost = s.usage.map_or(0, |u| {
                cost_micros(&rates, u.prompt as u64, u.cached as u64, u.completion as u64)
            });
            (Some(cost), true)
        }
        None => (None, false),
    }
}

/// The shared cost rule over a model's usage: an honest zero for a priced
/// model with no usage, `None` (a dash) for an unpriced model.
fn price_model_usage(
    usage: Option<mnemo::provider::trace::LlmUsage>,
    rates: Option<ModelRates>,
) -> (Option<u64>, bool) {
    match rates {
        Some(rates) => (
            Some(usage.map_or(0, |u| {
                cost_micros(&rates, u.prompt as u64, u.cached as u64, u.completion as u64)
            })),
            true,
        ),
        None => (None, false),
    }
}

/// Price one full record (the detail/export path) — same rule as rows.
fn price_record(
    record: &LlmRequestRecord,
    pricing: &[PricingEntry],
) -> (Option<u64>, bool) {
    price_model_usage(record.usage, resolve_rates(&record.model, pricing))
}

/// Price one journaled summary (the export's retained lines) — same rule.
fn price_record_from_summary(
    summary: &LlmRequestSummary,
    pricing: &[PricingEntry],
) -> (Option<u64>, bool) {
    price_model_usage(summary.usage, resolve_rates(&summary.model, pricing))
}

/// The metadata haystack a search scans: model/provider/endpoint, error and
/// finish-reason text, and the attribution's identity strings. Retained
/// (journal-only) calls can match nothing else, and this is the corpus for
/// the `metadata` scope.
fn metadata_haystack(s: &LlmRequestSummary, out: &mut String) {
    out.clear();
    out.push_str(&s.model);
    out.push('\n');
    out.push_str(&s.provider);
    out.push('\n');
    out.push_str(&s.base_url);
    out.push('\n');
    if let Some(error) = &s.error {
        out.push_str(error);
        out.push('\n');
    }
    if let Some(finish) = &s.finish_reason {
        out.push_str(finish);
        out.push('\n');
    }
    if let Some(a) = &s.attribution {
        out.push_str(&a.agent_name);
        out.push('\n');
        out.push_str(&a.agent_id);
        out.push('\n');
        out.push_str(&a.session_id);
        out.push('\n');
        if let Some(worker) = &a.worker_id {
            out.push_str(worker);
            out.push('\n');
        }
        if let Some(plan) = &a.plan_id {
            out.push_str(plan);
            out.push('\n');
        }
        if let Some(turn) = &a.turn_id {
            out.push_str(turn);
            out.push('\n');
        }
        out.push_str(purpose_str(a.purpose));
    }
    out.make_ascii_lowercase();
}

/// Whether the metadata corpus contains the (already lowercased) needle.
fn metadata_match(s: &LlmRequestSummary, needle: &str) -> bool {
    let mut haystack = String::new();
    metadata_haystack(s, &mut haystack);
    haystack.contains(needle)
}

/// Whether one live call's detail bodies contain the needle in the scope's
/// corpora. Scopes: the serialized request body, the raw streamed response
/// (also the `reasoning` corpus — reasoning arrives inside the stream), and
/// delivered tool calls.
fn payload_match(
    detail: Option<&LlmRequestRecord>,
    needle: &str,
    scope: TraceSearchScope,
) -> bool {
    let Some(d) = detail else {
        return false;
    };
    let in_request = |needle: &str| {
        if d.request_json.is_null() {
            return false;
        }
        serde_json::to_string(&d.request_json)
            .map(|text| text.to_lowercase().contains(needle))
            .unwrap_or(false)
    };
    let in_response = |needle: &str| {
        d.response_raw
            .as_deref()
            .map(|raw| raw.to_lowercase().contains(needle))
            .unwrap_or(false)
    };
    let in_tools = |needle: &str| {
        d.raw_tool_calls
            .as_ref()
            .map(|calls| {
                calls.iter().any(|call| {
                    call.name.to_lowercase().contains(needle)
                        || call.arguments.to_lowercase().contains(needle)
                })
            })
            .unwrap_or(false)
    };
    match scope {
        TraceSearchScope::Request => in_request(needle),
        TraceSearchScope::Response | TraceSearchScope::Reasoning => in_response(needle),
        TraceSearchScope::Tools => in_tools(needle),
        // `row_matches` handles the metadata scope before any payload fetch;
        // reaching here with it means the metadata text did not match.
        TraceSearchScope::Metadata => false,
        TraceSearchScope::All => in_request(needle) || in_response(needle) || in_tools(needle),
    }
}

/// Whether the metadata filters (agent/model/status/purpose/time/turn)
/// select this summary. Search is applied separately.
fn filters_match(s: &LlmRequestSummary, q: &TraceListQuery, purpose: Option<TracePurpose>) -> bool {
    if let Some(agent) = q.agent.as_deref() {
        if !agent_matches(agent, s.attribution.as_ref()) {
            return false;
        }
    }
    if let Some(model) = q.model.as_deref() {
        if s.model != model {
            return false;
        }
    }
    if let Some(status) = q.status {
        if row_status(s) != status {
            return false;
        }
    }
    if let Some(purpose) = purpose {
        if s.attribution.as_ref().map(|a| a.purpose) != Some(purpose) {
            return false;
        }
    }
    if let Some(from) = q.from_ms {
        if s.ts_ms < from {
            return false;
        }
    }
    if let Some(to) = q.to_ms {
        if s.ts_ms > to {
            return false;
        }
    }
    if let Some(turn) = q.turn_id.as_deref() {
        if s.attribution.as_ref().and_then(|a| a.turn_id.as_deref()) != Some(turn) {
            return false;
        }
    }
    true
}

/// Format epoch milliseconds as a UTC `YYYYMMDD-HHMMSS` stamp for export file
/// names (the crate has no chrono dependency; the date half is Howard
/// Hinnant's `civil_from_days`, exact for the full day range, mirroring the
/// knowledge tools' `format_epoch_date`). The stamp is UTC — the manifest
/// carries the exact epoch for local rendering.
fn utc_stamp(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    let hour = time / 3600;
    let minute = (time % 3600) / 60;
    let second = time % 60;
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

/// The current epoch in milliseconds (0 before the epoch on an unset clock).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
// ---------------------------------------------------------------------------
// The metadata journal
// ---------------------------------------------------------------------------

/// The metadata journal's in-memory state: everything loaded from disk plus
/// everything appended this session.
#[derive(Debug, Default)]
struct JournalState {
    /// Terminal summaries on disk, in append order (queries sort keys
    /// themselves; the journal is bounded by its rotation).
    rows: Vec<LlmRequestSummary>,
    /// Every key on disk — bounds the poll's dedupe set.
    keys: HashSet<TraceKey>,
    /// After a history deletion, only calls CAPTURED after this moment are
    /// journaled again — the user deleted what existed, not what comes next.
    floor_ts_ms: u64,
}

/// The metadata journal: `.coding/logs/trace-index.jsonl` plus four archives.
///
/// One terminal summary per call, on by default, no prompt/response/reasoning
/// text — the journal's whole point is that retained calls still list after a
/// restart with zero privacy change. Reopened with [`TraceJournal::open`],
/// which merges all five files by `run_id:id` (newest version wins).
pub(crate) struct TraceJournal {
    /// The project's `.coding/logs` directory.
    logs_dir: PathBuf,
    state: Mutex<JournalState>,
}

impl TraceJournal {
    /// Open the journal at the logs dir, creating the directory when absent
    /// and reloading every archive (newest file first, newest line first, so
    /// the first-seen version of a duplicated key is the newest).
    pub(crate) fn open(logs_dir: PathBuf) -> Arc<Self> {
        let _ = fs::create_dir_all(&logs_dir);
        let mut state = JournalState::default();
        for path in journal_files(&logs_dir) {
            let Ok(file) = fs::File::open(&path) else {
                continue;
            };
            for line in BufReader::new(file).lines() {
                let Ok(line) = line else {
                    break;
                };
                let Ok(summary) = serde_json::from_str::<LlmRequestSummary>(&line) else {
                    continue; // a corrupt line is skipped, never fatal
                };
                let key = (summary.run_id.clone(), summary.id);
                if state.keys.insert(key) {
                    state.rows.push(summary);
                }
            }
        }
        Arc::new(Self {
            logs_dir,
            state: Mutex::new(state),
        })
    }

    /// The journal writer's one pass: journal every terminal record that is
    /// not on disk yet. Keys are committed only after the append succeeds,
    /// so a failed write retries on the next poll.
    pub(crate) fn poll(&self, trace: &TraceApi) {
        let summaries = trace.list();
        self.poll_summaries(&summaries);
    }

    /// The poll over already-fetched summaries (split from [`Self::poll`] so
    /// tests can drive the journal without a live engine).
    pub(crate) fn poll_summaries(&self, summaries: &[LlmRequestSummary]) {
        let mut state = self.lock_state();
        let fresh: Vec<&LlmRequestSummary> = summaries
            .iter()
            .filter(|s| s.is_complete)
            .filter(|s| s.ts_ms >= state.floor_ts_ms)
            .filter(|s| !state.keys.contains(&(s.run_id.clone(), s.id)))
            .collect();
        let fresh: Vec<LlmRequestSummary> = fresh.into_iter().cloned().collect();
        if fresh.is_empty() || self.append(&fresh).is_err() {
            return; // retried on the next poll — keys were not committed
        }
        for s in &fresh {
            state.keys.insert((s.run_id.clone(), s.id));
        }
        state.rows.extend(fresh);
    }

    /// The journal's size on disk: the active file plus every archive.
    pub(crate) fn journal_bytes(&self) -> u64 {
        journal_files(&self.logs_dir)
            .into_iter()
            .filter_map(|path| fs::metadata(path).ok())
            .map(|meta| meta.len())
            .sum()
    }

    /// Delete the journal files (active + archives) and forget everything
    /// loaded. Returns `(files, bytes)` for the confirmation sheet.
    pub(crate) fn delete(&self) -> (u32, u64) {
        let mut state = self.lock_state();
        state.rows.clear();
        state.keys.clear();
        state.floor_ts_ms = now_ms();
        remove_files(&journal_files(&self.logs_dir))
    }

    /// Look a set of keys up among the journaled summaries (one pass, for
    /// export assembly). Keys not journaled are simply absent from the map.
    pub(crate) fn summaries_for(&self, wanted: &HashSet<TraceKey>) -> HashMap<TraceKey, LlmRequestSummary> {
        let mut found = HashMap::new();
        if wanted.is_empty() {
            return found;
        }
        let state = self.lock_state();
        let mut wanted = wanted.clone();
        for row in &state.rows {
            let key = (row.run_id.clone(), row.id);
            if wanted.remove(&key) {
                found.insert(key, row.clone());
                if wanted.is_empty() {
                    break;
                }
            }
        }
        found
    }

    /// Poison-safe state lock.
    fn lock_state(&self) -> std::sync::MutexGuard<'_, JournalState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append one batch of terminal summaries to the active file, rotating
    /// first when it has reached the threshold.
    fn append(&self, fresh: &[LlmRequestSummary]) -> std::io::Result<()> {
        let active = self.logs_dir.join(TRACE_INDEX_FILE);
        let size = fs::metadata(&active).map(|meta| meta.len()).unwrap_or(0);
        if size >= TRACE_JOURNAL_ROTATE_BYTES {
            self.rotate(&active)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&active)?;
        for summary in fresh {
            let line = serde_json::to_string(summary).map_err(std::io::Error::other)?;
            writeln!(file, "{line}")?;
        }
        Ok(())
    }

    /// Rotate the journal: drop the oldest archive, shift `.1`..`.3` up, and
    /// move the active file to `.1` (the next append starts a fresh file).
    fn rotate(&self, active: &Path) -> std::io::Result<()> {
        let _ = fs::remove_file(
            self.logs_dir
                .join(format!("{TRACE_INDEX_FILE}.{TRACE_JOURNAL_ARCHIVE_KEEP}")),
        );
        for n in (1..TRACE_JOURNAL_ARCHIVE_KEEP).rev() {
            let _ = fs::rename(
                self.logs_dir.join(format!("{TRACE_INDEX_FILE}.{n}")),
                self.logs_dir.join(format!("{TRACE_INDEX_FILE}.{}", n + 1)),
            );
        }
        fs::rename(
            active,
            self.logs_dir.join(format!("{TRACE_INDEX_FILE}.1")),
        )
    }
}

/// The journal's files, newest first: the active file then `.1`..`.4`.
fn journal_files(logs_dir: &Path) -> Vec<PathBuf> {
    let mut paths = vec![logs_dir.join(TRACE_INDEX_FILE)];
    for n in 1..=TRACE_JOURNAL_ARCHIVE_KEEP {
        paths.push(logs_dir.join(format!("{TRACE_INDEX_FILE}.{n}")));
    }
    paths
}

/// Delete files that exist; return `(count, bytes)` of what was removed.
fn remove_files(paths: &[PathBuf]) -> (u32, u64) {
    let mut deleted = 0;
    let mut bytes = 0;
    for path in paths {
        if let Ok(meta) = fs::metadata(path) {
            bytes += meta.len();
        }
        if fs::remove_file(path).is_ok() {
            deleted += 1;
        }
    }
    (deleted, bytes)
}

/// The opt-in body history's files: `traces-history.jsonl` plus its rotated
/// `traces-history-<micros>.jsonl` archives, newest first.
pub(crate) fn history_files(logs_dir: &Path) -> Vec<PathBuf> {
    let mut paths = vec![logs_dir.join(TRACE_HISTORY_FILE)];
    let Ok(entries) = fs::read_dir(logs_dir) else {
        return paths;
    };
    let mut archives: Vec<(u128, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // The kernel's rotation names archives `traces-history-<micros>.jsonl`
        // (the active file's stem, a dash, the rotation timestamp).
        if let Some(stem) = name
            .strip_prefix("traces-history-")
            .and_then(|stem| stem.strip_suffix(".jsonl"))
        {
            if let Ok(micros) = stem.parse::<u128>() {
                archives.push((micros, entry.path()));
            }
        }
    }
    archives.sort_by_key(|(micros, _)| std::cmp::Reverse(*micros)); // newest rotation first
    paths.extend(archives.into_iter().map(|(_, path)| path));
    paths
}

/// The opt-in body history's size on disk (for `trace_settings`).
pub(crate) fn history_bytes(logs_dir: &Path) -> u64 {
    history_files(logs_dir)
        .into_iter()
        .filter_map(|path| fs::metadata(path).ok())
        .map(|meta| meta.len())
        .sum()
}

/// Delete the opt-in body history's files; returns `(files, bytes)` removed.
pub(crate) fn remove_history_files(logs_dir: &Path) -> (u32, u64) {
    remove_files(&history_files(logs_dir))
}

/// Load full records from the opt-in body history by key, one sequential pass
/// per file with a cheap `"run_id"` prefilter before parsing a line. Only
/// records that were actually logged are found here — enabling body logging
/// later never reconstructs bodies for calls made before it was on.
pub(crate) fn history_records(
    logs_dir: &Path,
    wanted: &HashSet<TraceKey>,
) -> HashMap<TraceKey, LlmRequestRecord> {
    let mut found = HashMap::new();
    if wanted.is_empty() {
        return found;
    }
    let runs: HashSet<&str> = wanted.iter().map(|(run, _)| run.as_str()).collect();
    let mut wanted = wanted.clone();
    for path in history_files(logs_dir) {
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                break;
            };
            let Some(run) = line_run_id(&line) else {
                continue;
            };
            if !runs.contains(run) {
                continue; // a different run's line: never parsed
            }
            let Ok(record) = serde_json::from_str::<LlmRequestRecord>(&line) else {
                continue;
            };
            let key = (record.run_id.clone(), record.id);
            if wanted.remove(&key) {
                found.insert(key, record);
                if wanted.is_empty() {
                    return found;
                }
            }
        }
    }
    found
}

/// A history line's run id, extracted without parsing the whole line: find
/// the `"run_id":"` marker and read to the closing quote (run ids contain no
/// escapes). `None` when the marker is absent or malformed.
fn line_run_id(line: &str) -> Option<&str> {
    const MARKER: &str = "\"run_id\":\"";
    let start = line.find(MARKER)? + MARKER.len();
    let rest = line.get(start..)?;
    let end = rest.find('"')?;
    rest.get(..end)
}// ---------------------------------------------------------------------------
// Query engine: filters, search, and paging over the merged pool
// ---------------------------------------------------------------------------

/// Everything one query evaluation matched: the newest-first sort keys of
/// every match, the unpriced count, and whether a cap stopped the scan.
struct MatchOutcome {
    /// Matched rows' sort keys, sorted newest first.
    keys: Vec<SortKey>,
    /// How many matched rows have no configured price.
    unpriced: u64,
    /// Whether the scan stopped early (search cap or scan bound).
    capped: bool,
}

/// Whether one row matches the query's metadata filters plus its content
/// search (if any). `is_live` rows may match payload scopes via
/// `fetch_detail`; journal rows can only match metadata text — that is all
/// a retained call has.
fn row_matches(
    row: &LlmRequestSummary,
    is_live: bool,
    q: &TraceListQuery,
    purpose: Option<TracePurpose>,
    needle: Option<&str>,
    fetch_detail: &dyn Fn(u64) -> Option<LlmRequestRecord>,
) -> bool {
    if !filters_match(row, q, purpose) {
        return false;
    }
    let Some(needle) = needle else {
        return true; // no search: the filters alone decide
    };
    if needle.is_empty() {
        return true; // an emptied search box is no-op
    }
    let scope = q.search_scope.unwrap_or(TraceSearchScope::All);
    // The metadata scope searches metadata text only — it never falls
    // through to bodies.
    if scope == TraceSearchScope::Metadata {
        return metadata_match(row, needle);
    }
    let metadata_hit =
        scope == TraceSearchScope::All && metadata_match(row, needle);
    if metadata_hit {
        return true;
    }
    if !is_live {
        return false; // retained rows carry no request/response/tool text
    }
    let detail = fetch_detail(row.id);
    payload_match(detail.as_ref(), needle, scope)
}

impl TraceJournal {
    /// The query's parsed purpose (an unknown purpose string is a wire
    /// error, surfaced as the command's error).
    fn purpose_of(q: &TraceListQuery) -> Result<Option<TracePurpose>, String> {
        match q.purpose.as_deref() {
            None => Ok(None),
            Some(text) => parse_purpose(text)
                .map(Some)
                .ok_or_else(|| format!("unknown trace purpose: {text}")),
        }
    }

    /// Match a query over the merged pool: the live ring (newest first) plus
    /// every journaled row that the ring no longer shadows. Search caps at
    /// `match_cap` matches (interactive listing); the scan bound applies to
    /// searching queries only — unsearched queries return exact totals.
    fn match_query(
        &self,
        live: &[LlmRequestSummary],
        fetch_detail: &dyn Fn(u64) -> Option<LlmRequestRecord>,
        pricing: &[PricingEntry],
        q: &TraceListQuery,
        match_cap: Option<usize>,
    ) -> Result<MatchOutcome, String> {
        let purpose = Self::purpose_of(q)?;
        let needle: Option<String> = q.search.as_deref().map(str::to_lowercase);
        let needle = needle.as_deref();
        let state = self.lock_state();
        let live_keys: HashSet<TraceKey> = live
            .iter()
            .map(|s| (s.run_id.clone(), s.id))
            .collect();
        let mut rate_cache: HashMap<String, Option<ModelRates>> = HashMap::new();
        let mut keys: Vec<SortKey> = Vec::new();
        let mut unpriced: u64 = 0;
        let mut capped = false;
        let mut scanned = 0usize;

        let mut consider = |row: &LlmRequestSummary,
                            is_live: bool,
                            keys: &mut Vec<SortKey>|
         -> Control {
            if needle.is_some() {
                scanned += 1;
                if scanned > TRACE_SCAN_ROW_CAP {
                    return Control::StopCapped;
                }
            }
            if !row_matches(row, is_live, q, purpose, needle, fetch_detail) {
                return Control::Next;
            }
            let (_, priced) = price_summary(row, pricing, &mut rate_cache);
            if !priced {
                unpriced += 1;
            }
            keys.push(sort_key(row));
            if needle.is_some()
                && match_cap.is_some_and(|cap| keys.len() >= cap)
            {
                return Control::StopCapped;
            }
            Control::Next
        };

        for row in live.iter().rev() {
            if let Control::StopCapped = consider(row, true, &mut keys) {
                capped = true;
                break;
            }
        }
        if !capped {
            'journal: for row in &state.rows {
                let key = (row.run_id.clone(), row.id);
                if live_keys.contains(&key) {
                    continue; // the live ring shadows its journaled copy
                }
                match consider(row, false, &mut keys) {
                    Control::StopCapped => {
                        capped = true;
                        break 'journal;
                    }
                    Control::Next => {}
                }
            }
        }
        keys.sort_by(|a, b| b.cmp(a)); // newest first
        Ok(MatchOutcome {
            keys,
            unpriced,
            capped,
        })
    }

    /// One filtered, searched, and paged list pass — the backend of
    /// [`trace_list`]. Holds the journal state lock for the evaluation (a
    /// few milliseconds of sync in-memory work; the poll task contends
    /// briefly and in the same order, kernel locks never precede it).
    pub(crate) fn list_page(
        &self,
        live: &[LlmRequestSummary],
        live_run: &str,
        fetch_detail: &dyn Fn(u64) -> Option<LlmRequestRecord>,
        pricing: &[PricingEntry],
        q: &TraceListQuery,
    ) -> Result<TraceListPage, String> {
        let outcome = self.match_query(live, fetch_detail, pricing, q, Some(TRACE_SEARCH_MATCH_CAP))?;
        let limit = q.page_limit();
        // The cursor names the last returned row: the page starts strictly
        // below it. A cursor naming a key past the newest row (the filters
        // narrowed between pages, or a doctored value) has nothing left to
        // return — an empty page, never a wrap-around to page one.
        let skip = match q.cursor.as_deref().and_then(cursor_decode) {
            Some(cursor)
                if outcome.keys.first().is_some_and(|newest| *newest < cursor) =>
            {
                outcome.keys.len()
            }
            Some(cursor) => outcome.keys.partition_point(|key| *key >= cursor),
            None => 0,
        }
        .min(outcome.keys.len());
        let end = (skip + limit).min(outcome.keys.len());
        let page_keys = &outcome.keys[skip..end];
        let has_more = end < outcome.keys.len();
        let next_cursor = has_more
            .then(|| page_keys.last().map(cursor_encode))
            .flatten();

        let state = self.lock_state();
        let mut rate_cache: HashMap<String, Option<ModelRates>> = HashMap::new();
        let live_by_id: HashMap<u64, &LlmRequestSummary> =
            live.iter().map(|s| (s.id, s)).collect();
        // One pass over the journal rows picks up every page key the live
        // ring does not hold.
        let mut wanted: HashSet<TraceKey> = page_keys
            .iter()
            .filter(|key| !(key.1 == live_run && live_by_id.contains_key(&key.2)))
            .map(|key| (key.1.clone(), key.2))
            .collect();
        let mut journal_rows: HashMap<TraceKey, &LlmRequestSummary> = HashMap::new();
        for row in &state.rows {
            let key = (row.run_id.clone(), row.id);
            if wanted.remove(&key) {
                journal_rows.insert(key, row);
                if wanted.is_empty() {
                    break;
                }
            }
        }

        let mut rows = Vec::with_capacity(page_keys.len());
        for key in page_keys {
            let summary = if key.1 == live_run {
                live_by_id.get(&key.2).map(|s| (*s).clone())
            } else {
                None
            };
            let (summary, retained) = match summary {
                Some(summary) => (summary, false),
                None => match journal_rows.get(&(key.1.clone(), key.2)) {
                    Some(journaled) => ((*journaled).clone(), true),
                    None => continue, // unreachable: page keys come from the two tiers
                },
            };
            let (cost_micros, priced) = price_summary(&summary, pricing, &mut rate_cache);
            rows.push(TraceRow {
                summary,
                cost_micros,
                priced,
                retained,
            });
        }
        Ok(TraceListPage {
            rows,
            total: outcome.keys.len() as u64,
            unpriced: outcome.unpriced,
            has_more,
            next_cursor,
            capped: outcome.capped,
        })
    }

    /// The newest-first sort keys of every call matching a query — the
    /// export scope `filtered` (the decision: `query.limit` and
    /// `query.cursor` are ignored, every match exports; only the safety scan
    /// bound applies).
    pub(crate) fn matched_keys(
        &self,
        live: &[LlmRequestSummary],
        fetch_detail: &dyn Fn(u64) -> Option<LlmRequestRecord>,
        pricing: &[PricingEntry],
        q: &TraceListQuery,
    ) -> Result<Vec<SortKey>, String> {
        Ok(self
            .match_query(live, fetch_detail, pricing, q, None)?
            .keys)
    }
}

/// A `consider` walk's control flow.
enum Control {
    /// Keep scanning.
    Next,
    /// Stop the walk: a cap fired.
    StopCapped,
}// ---------------------------------------------------------------------------
// Export bundles
// ---------------------------------------------------------------------------

/// The export's v1 manifest — the first line of every bundle. Names the
/// pricing rule the per-line estimates used (the shared one from
/// [`crate::mnemo2::pricing`], explicit rows over vendor defaults) so later
/// analysis is reproducible.
#[derive(Debug, Clone, Serialize)]
struct TraceExportManifest {
    /// The format marker.
    format: &'static str,
    /// When the export ran (epoch ms).
    exported_at_ms: u64,
    /// The live run id that produced the export.
    run_id: String,
    /// `"call"`, `"selected"`, or `"filtered"`.
    scope: &'static str,
    /// How many call lines the bundle carries.
    count: u32,
    /// Whether any line carries full request/response bodies.
    includes_bodies: bool,
    /// The pricing rule used for the per-line estimates.
    pricing: TracePricingRule,
}

/// Which pricing computed the bundle's estimates.
#[derive(Debug, Clone, Serialize)]
struct TracePricingRule {
    /// Explicit `[[pricing]]` rows win; vendor defaults fall back.
    source: &'static str,
    /// The vendor-default table revision (bump when its rates change).
    vendor_table: u32,
}

/// One exported call. `kind` is `"record"` when full bodies are included
/// (live ring or logged history — never reconstructed), `"summary"` when
/// only the journal's metadata survived. Both carry `run_id:id`,
/// attribution, and the retention flags; the pricing object names the
/// estimate and the rates it used.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum TraceExportEntry {
    Record {
        record: LlmRequestRecord,
        retained: bool,
    },
    Summary {
        summary: LlmRequestSummary,
        retained: bool,
    },
}

/// One export line: the entry plus its pricing.
#[derive(Debug, Clone, Serialize)]
struct TraceExportLine {
    #[serde(flatten)]
    entry: TraceExportEntry,
    pricing: TraceExportPricing,
}

/// The per-line pricing object: the estimate and the rates behind it.
#[derive(Debug, Clone, Serialize)]
struct TraceExportPricing {
    /// Estimated cost in micro-USD; `None` when the model has no price.
    cost_micros: Option<u64>,
    /// Whether the model was priced (an unpriced call is not free).
    priced: bool,
    /// The per-1M-token rates used, when the model was priced.
    rates: Option<ModelRates>,
}

/// Write one bundle: the manifest line then one line per call, to a
/// temporary file renamed into place so a failure never leaves a partial
/// bundle presented as successful. The file is named
/// `trace-<YYYYMMDD-HHMMSS>.jsonl` (UTC) and numbered when a same-second
/// export already exists.
fn write_export(
    logs_dir: &Path,
    manifest: &TraceExportManifest,
    lines: &[TraceExportLine],
) -> Result<PathBuf, String> {
    let exports_dir = logs_dir.join(TRACE_EXPORTS_DIR);
    fs::create_dir_all(&exports_dir)
        .map_err(|error| format!("could not create the export folder: {error}"))?;
    let stamp = utc_stamp(now_ms());
    let mut name = format!("trace-{stamp}.jsonl");
    let mut suffix = 2;
    while exports_dir.join(&name).exists() {
        name = format!("trace-{stamp}-{suffix}.jsonl");
        suffix += 1;
    }
    let final_path = exports_dir.join(&name);
    let tmp_path = final_path.with_extension("jsonl.tmp");
    {
        let mut file = fs::File::create(&tmp_path)
            .map_err(|error| format!("could not create the export file: {error}"))?;
        let manifest = serde_json::to_string(manifest)
            .map_err(|error| format!("could not encode the manifest: {error}"))?;
        writeln!(file, "{manifest}")
            .map_err(|error| format!("could not write the manifest: {error}"))?;
        for line in lines {
            let line = serde_json::to_string(line)
                .map_err(|error| format!("could not encode a call line: {error}"))?;
            writeln!(file, "{line}")
                .map_err(|error| format!("could not write a call line: {error}"))?;
        }
    }
    fs::rename(&tmp_path, &final_path)
        .map_err(|error| format!("could not finish the export: {error}"))?;
    Ok(final_path)
}

/// Validate a reveal target: the canonicalized path must sit inside the
/// project's `.coding/logs/trace-exports/` folder, so a stale or doctored
/// result string can never point the file manager at an arbitrary file.
/// Returns the canonicalized path to open.
fn validate_reveal_path(logs_dir: &Path, path: &str) -> Result<PathBuf, String> {
    let exports_dir = fs::canonicalize(logs_dir.join(TRACE_EXPORTS_DIR))
        .map_err(|_| "the export folder does not exist yet".to_string())?;
    let target = fs::canonicalize(path)
        .map_err(|_| "the exported file no longer exists".to_string())?;
    if !target.starts_with(&exports_dir) {
        return Err("refusing to reveal a path outside the export folder".to_string());
    }
    Ok(target)
}

/// Open the OS file manager with `path` selected (Windows) or revealed
/// (macOS); on Linux the containing folder is opened (there is no portable
/// file-selection switch).
fn open_in_file_manager(path: &Path) -> Result<(), String> {
    let status = if cfg!(target_os = "windows") {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
    } else {
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .spawn()
    };
    match status {
        // Fire-and-forget: `explorer` reports a nonzero exit for successful
        // reveals, so the child is never waited on — spawning IS the result.
        Ok(_) => Ok(()),
        Err(error) => Err(format!("could not open the file manager: {error}")),
    }
}// ---------------------------------------------------------------------------
// Per-engine trace support
// ---------------------------------------------------------------------------

/// The trace backend for one engine: the raised ring, the metadata journal,
/// and the background writer keeping the journal current. Owned by the lead
/// runtime's slot — a re-started engine gets a fresh support whose Drop (or
/// [`Self::stop`]) aborts the previous writer.
pub struct TraceSupport {
    journal: Arc<TraceJournal>,
    logs_dir: PathBuf,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TraceSupport {
    /// Raise the live ring to [`TRACE_RING_CAPACITY`], open the metadata
    /// journal at `<coding_dir>/logs/`, and start the background journal
    /// writer. Called once per engine start (the lead runtime's
    /// `start_engine`).
    pub(crate) fn start(engine: &Engine) -> TraceSupport {
        let trace = engine.trace();
        trace.set_ring_capacity(TRACE_RING_CAPACITY);
        let logs_dir = engine
            .reviews_dir()
            .parent()
            .map(|coding| coding.join("logs"))
            .unwrap_or_default();
        let journal = TraceJournal::open(logs_dir.clone());
        let writer = {
            let journal = Arc::clone(&journal);
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(TRACE_JOURNAL_POLL);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    journal.poll(&trace);
                }
            })
        };
        TraceSupport {
            journal,
            logs_dir,
            task: Some(writer),
        }
    }

    /// The metadata journal (shared with the background writer).
    pub(crate) fn journal(&self) -> &Arc<TraceJournal> {
        &self.journal
    }

    /// The project's `.coding/logs` directory.
    pub(crate) fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    /// Stop the background writer (idempotent; Drop stops it too).
    pub(crate) fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for TraceSupport {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// The open project's lead runtime and shared engine.
async fn open_trace(
    state: &Mnemo2State,
) -> Result<(Arc<LeadRuntime>, Arc<Engine>), String> {
    let lead = state
        .open_lead()
        .await
        .ok_or_else(|| "Mnemo2 has no open lead; call lead_open first".to_string())?;
    let engine = lead.engine_handle().await?;
    Ok((lead, engine))
}

/// The lead's trace support — the ring configuration, journal, and writer.
fn trace_support_of(lead: &LeadRuntime) -> Result<Arc<TraceSupport>, String> {
    lead.trace_support()
        .ok_or_else(|| "the trace journal is not running".to_string())
}

/// One page of traced model calls: the live ring merged with the retained
/// metadata journal, filtered/searched/paged Rust-side, newest first.
#[tauri::command]
pub async fn trace_list(
    state: State<'_, Mnemo2State>,
    query: TraceListQuery,
) -> Result<TraceListPage, String> {
    let (lead, engine) = open_trace(&state).await?;
    let support = trace_support_of(&lead)?;
    let trace = engine.trace();
    let pricing = lead.pricing()?;
    let live_run = trace.run_id();
    tokio::task::spawn_blocking(move || {
        let live = trace.list();
        let fetch = |id: u64| trace.get(id);
        support
            .journal()
            .list_page(&live, &live_run, &fetch, &pricing, &query)
    })
    .await
    .map_err(|error| format!("the trace list task failed: {error}"))?
}

/// One call in full by its durable `run_id:id` key: the live ring first,
/// then the opt-in body history's files. `None` when only journal metadata
/// survived — the UI explains that state, it is not an error.
#[tauri::command]
pub async fn trace_detail(
    state: State<'_, Mnemo2State>,
    run_id: String,
    id: u64,
) -> Result<Option<TraceDetail>, String> {
    let (lead, engine) = open_trace(&state).await?;
    let support = trace_support_of(&lead)?;
    let trace = engine.trace();
    let pricing = lead.pricing()?;
    tokio::task::spawn_blocking(move || {
        let record = if run_id == trace.run_id() {
            trace.get(id)
        } else {
            None
        };
        let record = match record {
            Some(record) => Some(record),
            // Ring-evicted or a previous run: the logged history is the only
            // place bodies survive, and only when body logging was on.
            None => {
                let mut wanted = HashSet::new();
                wanted.insert((run_id.clone(), id));
                history_records(support.logs_dir(), &wanted)
                    .into_values()
                    .next()
            }
        };
        record.map(|record| {
            let (cost_micros, priced) = price_record(&record, &pricing);
            TraceDetail {
                record,
                cost_micros,
                priced,
                retained: false,
            }
        })
    })
    .await
    .map_err(|error| format!("the trace detail task failed: {error}"))
}

/// The settings readout: body-logging toggle, ring capacity, and both
/// retention tiers' sizes on disk.
#[tauri::command]
pub async fn trace_settings(state: State<'_, Mnemo2State>) -> Result<TraceSettings, String> {
    let (lead, engine) = open_trace(&state).await?;
    let support = trace_support_of(&lead)?;
    let trace = engine.trace();
    Ok(TraceSettings {
        log_bodies: trace.logging_enabled(),
        ring_capacity: trace.ring_capacity() as u32,
        journal_bytes: support.journal().journal_bytes(),
        history_bytes: history_bytes(support.logs_dir()),
    })
}

/// Toggle mirroring full request/response bodies to disk (opt-in — bodies
/// may contain sensitive content; secrets stay redacted either way, and
/// failed requests are always mirrored redacted). Returns the new settings.
#[tauri::command]
pub async fn trace_set_log_bodies(
    state: State<'_, Mnemo2State>,
    on: bool,
) -> Result<TraceSettings, String> {
    let (lead, engine) = open_trace(&state).await?;
    let support = trace_support_of(&lead)?;
    let trace = engine.trace();
    trace.set_logging_enabled(on);
    Ok(TraceSettings {
        log_bodies: trace.logging_enabled(),
        ring_capacity: trace.ring_capacity() as u32,
        journal_bytes: support.journal().journal_bytes(),
        history_bytes: history_bytes(support.logs_dir()),
    })
}

/// Drop every live ring record (the retained journal and logged history are
/// untouched — that is [`trace_delete_history`]). Ids keep counting up; no
/// id is ever reused.
#[tauri::command]
pub async fn trace_clear_live(state: State<'_, Mnemo2State>) -> Result<(), String> {
    let (_, engine) = open_trace(&state).await?;
    let trace = engine.trace();
    trace.clear();
    Ok(())
}

/// Drop the retained history: the metadata journal (active + archives) and
/// the opt-in body history's files. The live ring is untouched. Returns
/// what went for the confirmation sheet's count and size.
#[tauri::command]
pub async fn trace_delete_history(
    state: State<'_, Mnemo2State>,
) -> Result<TraceHistoryDeletion, String> {
    let (lead, _engine) = open_trace(&state).await?;
    let support = trace_support_of(&lead)?;
    tokio::task::spawn_blocking(move || {
        let (journal_files, journal_bytes) = support.journal().delete();
        let (history_files, history_bytes) = remove_history_files(support.logs_dir());
        TraceHistoryDeletion {
            deleted_files: journal_files + history_files,
            bytes: journal_bytes + history_bytes,
        }
    })
    .await
    .map_err(|error| format!("the trace history deletion task failed: {error}"))
}

/// Bundle calls into `.coding/logs/trace-exports/trace-<UTC>.jsonl`: the v1
/// manifest, then one full record per call when bodies are available (the
/// live ring or the logged history — never reconstructed) or the summary
/// line for journal-only calls. Scope `filtered` ignores `query.limit` and
/// `query.cursor` and exports every match (decision 2026-11-02); the result
/// states the exported count.
#[tauri::command]
pub async fn trace_export(
    state: State<'_, Mnemo2State>,
    request: TraceExportRequest,
) -> Result<TraceExportResult, String> {
    let (lead, engine) = open_trace(&state).await?;
    let support = trace_support_of(&lead)?;
    let trace = engine.trace();
    let pricing = lead.pricing()?;
    tokio::task::spawn_blocking(move || {
        export_bundle(&trace, &support, &pricing, request)
    })
    .await
    .map_err(|error| format!("the trace export task failed: {error}"))?
}

/// Assemble and write one export bundle over the engine's live log (the
/// sync body of [`trace_export`]).
fn export_bundle(
    trace: &TraceApi,
    support: &TraceSupport,
    pricing: &[PricingEntry],
    request: TraceExportRequest,
) -> Result<TraceExportResult, String> {
    let live_run = trace.run_id();
    let live = trace.list();
    let fetch = |id: u64| trace.get(id);
    assemble_export(
        support.journal(),
        support.logs_dir(),
        &live,
        &live_run,
        &fetch,
        pricing,
        request,
    )
}

/// Assemble and write one export bundle from the merged pool — engine-free,
/// so the scopes and the manifest are testable without a live log.
fn assemble_export(
    journal: &TraceJournal,
    logs_dir: &Path,
    live: &[LlmRequestSummary],
    live_run: &str,
    fetch_detail: &dyn Fn(u64) -> Option<LlmRequestRecord>,
    pricing: &[PricingEntry],
    request: TraceExportRequest,
) -> Result<TraceExportResult, String> {

    // 1. The target keys by scope.
    let keys: Vec<TraceKey> = match request.scope {
        TraceExportScope::Call => {
            let raw = request
                .keys
                .as_ref()
                .and_then(|keys| keys.first())
                .ok_or_else(|| "trace_export: scope \"call\" needs the call's key".to_string())?;
            vec![parse_trace_key(raw)
                .ok_or_else(|| format!("trace_export: malformed key {raw:?}"))?]
        }
        TraceExportScope::Selected => {
            let raw = request
                .keys
                .as_ref()
                .ok_or_else(|| "trace_export: scope \"selected\" needs keys".to_string())?;
            raw.iter()
                .filter_map(|key| parse_trace_key(key))
                .collect()
        }
        TraceExportScope::Filtered => {
            let query = request
                .query
                .as_ref()
                .ok_or_else(|| "trace_export: scope \"filtered\" needs the query".to_string())?;
            journal.matched_keys(live, fetch_detail, pricing, query)?
                .into_iter()
                .map(|key| (key.1, key.2))
                .collect()
        }
    };
    if keys.is_empty() {
        return Err("trace_export: no calls match the export's keys or filters".to_string());
    }

    // 2. Resolve each key: live ring, then logged history, then journal
    //    metadata. Unresolvable keys (rotated away) are skipped — the count
    //    reflects the bundle.
    let wanted: HashSet<TraceKey> = keys.iter().cloned().collect();
    let mut records: HashMap<TraceKey, LlmRequestRecord> = HashMap::new();
    for (run, id) in &keys {
        if run != live_run {
            continue;
        }
        if let Some(record) = fetch_detail(*id) {
            records.insert((run.clone(), *id), record);
        }
    }
    let remaining: HashSet<TraceKey> = wanted
        .iter()
        .filter(|key| !records.contains_key(key))
        .cloned()
        .collect();
    records.extend(history_records(logs_dir, &remaining));
    let summaries = journal.summaries_for(&wanted);

    let mut lines: Vec<TraceExportLine> = Vec::with_capacity(keys.len());
    let mut includes_bodies = false;
    for key in &keys {
        if let Some(record) = records.get(key) {
            includes_bodies |= !record.request_json.is_null() || record.response_raw.is_some();
            let (cost_micros, priced) = price_record(record, pricing);
            let rates = resolve_rates(&record.model, pricing);
            lines.push(TraceExportLine {
                entry: TraceExportEntry::Record {
                    record: record.clone(),
                    retained: false,
                },
                pricing: TraceExportPricing {
                    cost_micros,
                    priced,
                    rates,
                },
            });
        } else if let Some(summary) = summaries.get(key) {
            let (cost_micros, priced) = price_record_from_summary(summary, pricing);
            let rates = resolve_rates(&summary.model, pricing);
            lines.push(TraceExportLine {
                entry: TraceExportEntry::Summary {
                    summary: summary.clone(),
                    retained: true,
                },
                pricing: TraceExportPricing {
                    cost_micros,
                    priced,
                    rates,
                },
            });
        }
    }
    if lines.is_empty() {
        return Err(
            "trace_export: no calls matched — the keys may have rotated away".to_string(),
        );
    }

    let scope = match request.scope {
        TraceExportScope::Call => "call",
        TraceExportScope::Selected => "selected",
        TraceExportScope::Filtered => "filtered",
    };
    let manifest = TraceExportManifest {
        format: "mnemo-trace-v1",
        exported_at_ms: now_ms(),
        run_id: live_run.to_string(),
        scope,
        count: lines.len() as u32,
        includes_bodies,
        pricing: TracePricingRule {
            source: "endpoints.toml + vendor defaults",
            vendor_table: 1,
        },
    };
    let path = write_export(logs_dir, &manifest, &lines)?;
    Ok(TraceExportResult {
        path: path.display().to_string(),
        count: manifest.count,
        includes_bodies,
    })
}

/// Show one exported bundle in the OS file manager. Only canonicalized
/// paths inside `.coding/logs/trace-exports/` are accepted, so a stale or
/// doctored result can never point the app at an arbitrary file.
#[tauri::command]
pub async fn trace_reveal_export(
    state: State<'_, Mnemo2State>,
    path: String,
) -> Result<(), String> {
    let (lead, _engine) = open_trace(&state).await?;
    let support = trace_support_of(&lead)?;
    let target = validate_reveal_path(support.logs_dir(), &path)?;
    open_in_file_manager(&target)
}// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::config::PricingEntry;
    use mnemo::provider::trace::{GuardCutDetails, LlmUsage};

    /// A priced test model's explicit entry (the Cost-tab rule's shape).
    fn pricing() -> Vec<PricingEntry> {
        vec![PricingEntry {
            model: "priced-model".to_string(),
            input_per_1m: 1.5,
            cached_per_1m: 0.3,
            output_per_1m: 6.0,
        }]
    }

    /// A worker attribution with everything set.
    fn worker_attribution(worker: Option<&str>, plan: Option<(&str, u32)>) -> TraceAttribution {
        TraceAttribution {
            actor_kind: TraceActorKind::Worker,
            agent_id: "agent-7".to_string(),
            session_id: "session-7".to_string(),
            agent_name: "Worker 7".to_string(),
            worker_id: worker.map(str::to_string),
            plan_id: plan.map(|(id, _)| id.to_string()),
            plan_step: plan.map(|(_, step)| step),
            turn_id: Some("t-42".to_string()),
            purpose: TracePurpose::Turn,
        }
    }

    /// A lead attribution.
    fn lead_attribution() -> TraceAttribution {
        TraceAttribution {
            actor_kind: TraceActorKind::Lead,
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            agent_name: "Lead".to_string(),
            worker_id: None,
            plan_id: None,
            plan_step: None,
            turn_id: Some("t-1".to_string()),
            purpose: TracePurpose::Turn,
        }
    }

    /// One terminal summary; the knobs the filters read are parameters.
    fn summary(id: u64, ts_ms: u64, model: &str, attribution: Option<TraceAttribution>) -> LlmRequestSummary {
        LlmRequestSummary {
            id,
            ts_ms,
            model: model.to_string(),
            base_url: "https://api.invalid/v1".to_string(),
            provider: "test".to_string(),
            http_status: Some(200),
            usage: Some(LlmUsage {
                prompt: 1_000,
                completion: 200,
                reasoning: 50,
                cached: 800,
            }),
            finish_reason: Some("stop".to_string()),
            is_complete: true,
            version: 1,
            ttft_ms: Some(120),
            generation_ms: Some(800),
            reasoning_ms: Some(300),
            connect_ms: Some(40),
            upload_ms: None,
            server_ms: None,
            tools_ms: Some(0),
            prep_ms: Some(5),
            compact_ms: Some(0),
            backoff_ms: Some(0),
            stall_ms: Some(0),
            chunks: None,
            error: None,
            cancelled: false,
            response_truncated: false,
            response_evicted: false,
            guard_cut: None,
            run_id: "r-run-1".to_string(),
            attribution,
        }
    }

    /// One full record for a live call (the search/export payload fixture).
    fn record(
        summary: &LlmRequestSummary,
        request: serde_json::Value,
        response: Option<&str>,
    ) -> LlmRequestRecord {
        LlmRequestRecord {
            id: summary.id,
            ts_ms: summary.ts_ms,
            model: summary.model.clone(),
            base_url: summary.base_url.clone(),
            provider: summary.provider.clone(),
            http_status: summary.http_status,
            usage: summary.usage,
            finish_reason: summary.finish_reason.clone(),
            is_complete: summary.is_complete,
            version: summary.version,
            ttft_ms: summary.ttft_ms,
            generation_ms: summary.generation_ms,
            reasoning_ms: summary.reasoning_ms,
            connect_ms: summary.connect_ms,
            upload_ms: summary.upload_ms,
            server_ms: summary.server_ms,
            tools_ms: summary.tools_ms,
            prep_ms: summary.prep_ms,
            compact_ms: summary.compact_ms,
            backoff_ms: summary.backoff_ms,
            stall_ms: summary.stall_ms,
            chunks: summary.chunks,
            error: summary.error.clone(),
            cancelled: summary.cancelled,
            response_truncated: summary.response_truncated,
            response_evicted: summary.response_evicted,
            guard_cut: summary.guard_cut.clone(),
            run_id: summary.run_id.clone(),
            attribution: summary.attribution.clone(),
            request_json: request,
            response_raw: response.map(str::to_string),
            raw_tool_calls: None,
            request_truncated: false,
        }
    }

    /// A journal over a fresh temp dir (empty: no rows on disk).
    fn empty_journal() -> (tempfile::TempDir, Arc<TraceJournal>) {
        let dir = tempfile::tempdir().expect("logs dir");
        let journal = TraceJournal::open(dir.path().to_path_buf());
        (dir, journal)
    }

    const NO_FETCH: fn(u64) -> Option<LlmRequestRecord> = |_| None;

    #[test]
    fn status_classifies_cancel_guard_failure_and_completion() {
        let base = summary(1, 1_000, "priced-model", None);
        assert_eq!(row_status(&base), TraceListStatus::Complete);

        let mut cancelled = base.clone();
        cancelled.cancelled = true;
        cancelled.error = Some("dropped".to_string());
        assert_eq!(
            row_status(&cancelled),
            TraceListStatus::Cancelled,
            "cancellation is never an error"
        );

        let mut guard = base.clone();
        guard.guard_cut = Some(GuardCutDetails {
            boundary: "</assistant>".to_string(),
            byte_idx: 8,
            delta_len: 40,
            chars_before: 8,
            prefix_empty: false,
        });
        assert_eq!(row_status(&guard), TraceListStatus::GuardCut);

        let mut failed = base.clone();
        failed.error = Some("http 500".to_string());
        failed.finish_reason = None;
        assert_eq!(row_status(&failed), TraceListStatus::Failed);

        let mut in_flight = base.clone();
        in_flight.is_complete = false;
        in_flight.finish_reason = None;
        assert_eq!(row_status(&in_flight), TraceListStatus::InProgress);
    }

    #[test]
    fn list_page_filters_agent_model_status_purpose_and_time() {
        let (_, journal) = empty_journal();
        let live = vec![
            summary(4, 4_000, "priced-model", Some(lead_attribution())),
            summary(
                3,
                3_000,
                "other-model",
                Some(worker_attribution(Some("w-77"), Some(("plan-2", 3)))),
            ),
            summary(2, 2_000, "priced-model", None),
            summary(1, 1_000, "unpriced-model", Some(lead_attribution())),
        ];
        let q = |agent: Option<&str>| TraceListQuery {
            agent: agent.map(str::to_string),
            ..Default::default()
        };

        let page = journal
            .list_page(&live, "r-run-1", &NO_FETCH, &pricing(), &q(Some("lead")))
            .expect("lead filter");
        assert_eq!(page.total, 2, "rows 4 and 1 are the lead's");

        let page = journal
            .list_page(&live, "r-run-1", &NO_FETCH, &pricing(), &q(Some("w-77")))
            .expect("worker filter");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].summary.id, 3);

        let page = journal
            .list_page(&live, "r-run-1", &NO_FETCH, &pricing(), &q(Some("unattributed")))
            .expect("unattributed filter");
        assert_eq!(page.total, 1, "only the legacy record has no attribution");
        assert_eq!(page.rows[0].summary.id, 2);

        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &TraceListQuery {
                    model: Some("priced-model".to_string()),
                    from_ms: Some(1_500),
                    to_ms: Some(4_500),
                    ..Default::default()
                },
            )
            .expect("model+time filter");
        assert_eq!(page.total, 2, "rows 4 and 2 match the model in the window");

        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &TraceListQuery {
                    status: Some(TraceListStatus::Failed),
                    ..Default::default()
                },
            )
            .expect("status filter");
        assert_eq!(page.total, 0, "no failed rows in the fixture");

        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &TraceListQuery {
                    purpose: Some("turn".to_string()),
                    ..Default::default()
                },
            )
            .expect("purpose filter");
        assert_eq!(page.total, 3, "the lead and worker turns");

        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &TraceListQuery {
                    turn_id: Some("t-42".to_string()),
                    ..Default::default()
                },
            )
            .expect("turn filter");
        assert_eq!(page.total, 1, "only the worker's row carries t-42");
        assert_eq!(page.rows[0].summary.id, 3);

        assert!(
            journal
                .list_page(
                    &live,
                    "r-run-1",
                    &NO_FETCH,
                    &pricing(),
                    &TraceListQuery {
                        purpose: Some("not-a-purpose".to_string()),
                        ..Default::default()
                    }
                )
                .is_err(),
            "an unknown purpose string is a wire error"
        );
    }

    #[test]
    fn pricing_matches_the_cost_tab_rule_and_shows_a_dash_for_unpriced() {
        let rates = resolve_rates("priced-model", &pricing()).expect("explicit entry");
        // 200 uncached * 1.5 + 800 cached * 0.3 + 200 output * 6.0 = 1740 micros
        assert_eq!(cost_micros(&rates, 1_000, 800, 200), 1_740);

        let priced = summary(1, 1_000, "priced-model", None);
        let mut rates_cache = HashMap::new();
        let (cost, was_priced) = price_summary(&priced, &pricing(), &mut rates_cache);
        assert_eq!(cost, Some(1_740));
        assert!(was_priced);

        // A vendor-default model prices the same way the Cost tab does.
        let glm = summary(2, 2_000, "glm-5.3", None);
        let (cost, was_priced) = price_summary(&glm, &pricing(), &mut rates_cache);
        // 200 * 1.40 + 800 * 0.26 + 200 * 4.40 = 1368
        assert_eq!(cost, Some(1_368));
        assert!(was_priced);

        // An unpriced model is a dash, never a fake zero.
        let unpriced = summary(3, 3_000, "mystery-model", None);
        let (cost, was_priced) = price_summary(&unpriced, &pricing(), &mut rates_cache);
        assert_eq!(cost, None);
        assert!(!was_priced);

        // A priced model with no usage costs an honest zero, not a dash.
        let mut usageless = summary(4, 4_000, "priced-model", None);
        usageless.usage = None;
        let (cost, was_priced) = price_summary(&usageless, &pricing(), &mut rates_cache);
        assert_eq!(cost, Some(0));
        assert!(was_priced);
    }

    #[test]
    fn cursor_round_trips_the_sort_key() {
        let key = (1_728_000_000_000u64, "r-12-34-5".to_string(), 42);
        let encoded = cursor_encode(&key);
        assert_eq!(cursor_decode(&encoded).as_ref(), Some(&key));
        assert!(cursor_decode("not-a-cursor").is_none());
        assert!(cursor_decode("a:b:c").is_none());
    }

    #[test]
    fn list_page_pages_newest_first_with_a_stable_cursor() {
        let (_, journal) = empty_journal();
        let live: Vec<LlmRequestSummary> = (1..=5)
            .map(|id| summary(id, id * 1_000, "priced-model", None))
            .collect();
        let q = |limit: u32, cursor: Option<String>| TraceListQuery {
            limit,
            cursor,
            ..Default::default()
        };

        let page1 = journal
            .list_page(&live, "r-run-1", &NO_FETCH, &pricing(), &q(2, None))
            .expect("page one");
        assert_eq!(
            page1.rows.iter().map(|row| row.summary.id).collect::<Vec<_>>(),
            vec![5, 4],
            "newest first"
        );
        assert_eq!(page1.total, 5);
        assert_eq!(page1.unpriced, 0);
        assert!(page1.has_more);
        assert!(page1.next_cursor.is_some());

        let page2 = journal
            .list_page(
                &live,
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &q(2, page1.next_cursor.clone()),
            )
            .expect("page two");
        assert_eq!(
            page2.rows.iter().map(|row| row.summary.id).collect::<Vec<_>>(),
            vec![3, 2],
            "the cursor continues exactly where page one ended"
        );

        let page3 = journal
            .list_page(
                &live,
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &q(2, page2.next_cursor.clone()),
            )
            .expect("page three");
        assert_eq!(page3.rows.iter().map(|row| row.summary.id).collect::<Vec<_>>(), vec![1]);
        assert!(!page3.has_more);
        assert_eq!(page3.next_cursor, None);

        // A cursor past the end (filters narrowed between pages) yields an
        // empty page, not an error or a wrap-around.
        let stale = journal
            .list_page(
                &live,
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &q(2, Some("9999999999:zzz:999".to_string())),
            )
            .expect("stale cursor");
        assert!(stale.rows.is_empty());
        assert!(!stale.has_more);
    }

    #[test]
    fn the_page_limit_clamps_to_the_contract_maximum() {
        let q = |limit: u32| TraceListQuery {
            limit,
            ..Default::default()
        };
        assert_eq!(q(0).page_limit(), 100, "the default page is 100 rows");
        assert_eq!(q(1).page_limit(), 1);
        assert_eq!(q(500).page_limit(), TRACE_PAGE_MAX as usize);
    }

    #[test]
    fn search_matches_by_scope_over_live_payloads_only() {
        let (_, journal) = empty_journal();
        let mut requester = summary(1, 1_000, "priced-model", None);
        requester.attribution = Some(lead_attribution());
        let mut responder = summary(2, 2_000, "priced-model", None);
        responder.attribution = Some(worker_attribution(Some("w-1"), None));
        let live = vec![requester.clone(), responder.clone()];
        let mut records = HashMap::new();
        records.insert(
            1u64,
            record(
                &requester,
                serde_json::json!({ "model": "priced-model", "messages": [
                    { "role": "system", "content": "plan the ALPHA release" }
                ] }),
                Some("data: {\"delta\":{\"content\":\"alpha done\"}}\n"),
            ),
        );
        records.insert(
            2u64,
            record(
                &responder,
                serde_json::json!({ "messages": [{ "role": "user", "content": "plain" }] }),
                Some("data: {\"delta\":{\"reasoning_content\":\"think BETA\"}}\n"),
            ),
        );
        let fetch = move |id: u64| records.get(&id).cloned();

        let search = |text: &str, scope: Option<TraceSearchScope>| TraceListQuery {
            search: Some(text.to_string()),
            search_scope: scope,
            ..Default::default()
        };

        // Case-insensitive request-body search.
        let page = journal
            .list_page(&live, "r-run-1", &fetch, &pricing(), &search("alpha release", None))
            .expect("search all");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].summary.id, 1);

        // The response scope finds text in the streamed response.
        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &fetch,
                &pricing(),
                &search("beta", Some(TraceSearchScope::Response)),
            )
            .expect("search response");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].summary.id, 2);

        // The request scope does not see response text.
        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &fetch,
                &pricing(),
                &search("beta", Some(TraceSearchScope::Request)),
            )
            .expect("search request");
        assert_eq!(page.total, 0);

        // Reasoning rides in the streamed response — same corpus, so the
        // reasoning scope finds the streamed thinking text.
        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &fetch,
                &pricing(),
                &search("think", Some(TraceSearchScope::Reasoning)),
            )
            .expect("search reasoning");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].summary.id, 2);

        // Metadata search matches the agent name; payload text never does
        // under the metadata scope.
        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &fetch,
                &pricing(),
                &search("worker 7", Some(TraceSearchScope::Metadata)),
            )
            .expect("search metadata");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].summary.id, 2);

        let page = journal
            .list_page(
                &live,
                "r-run-1",
                &fetch,
                &pricing(),
                &search("alpha", Some(TraceSearchScope::Metadata)),
            )
            .expect("metadata scope ignores payloads");
        assert_eq!(page.total, 0);
    }

    #[test]
    fn retained_rows_match_metadata_search_but_never_payload_scopes() {
        let (dir, journal) = empty_journal();
        let old = summary(1, 1_000, "priced-model", Some(lead_attribution()));
        journal.poll_summaries(std::slice::from_ref(&old));

        // The simulated restart: a fresh journal over the same files.
        let reloaded = TraceJournal::open(dir.path().to_path_buf());
        let search = |text: &str, scope: TraceSearchScope| TraceListQuery {
            search: Some(text.to_string()),
            search_scope: Some(scope),
            ..Default::default()
        };

        let page = reloaded
            .list_page(
                &[],
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &search("lead", TraceSearchScope::Metadata),
            )
            .expect("retained metadata search");
        assert_eq!(page.total, 1);
        assert!(page.rows[0].retained, "the row lists from the journal");

        let page = reloaded
            .list_page(
                &[],
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &search("lead", TraceSearchScope::Request),
            )
            .expect("retained payload search");
        assert_eq!(
            page.total, 0,
            "a retained call has no request body to search"
        );

        let page = reloaded
            .list_page(
                &[],
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &search("lead", TraceSearchScope::All),
            )
            .expect("retained all-scope search");
        assert_eq!(
            page.total, 1,
            "the all scope still finds the metadata text"
        );
    }

    #[test]
    fn the_live_ring_shadows_its_journaled_copy_without_double_counting() {
        let (dir, journal) = empty_journal();
        let call = summary(1, 1_000, "priced-model", None);
        journal.poll_summaries(std::slice::from_ref(&call));
        let reloaded = TraceJournal::open(dir.path().to_path_buf());

        let page = reloaded
            .list_page(
                std::slice::from_ref(&call),
                "r-run-1",
                &NO_FETCH,
                &pricing(),
                &TraceListQuery::default(),
            )
            .expect("merged pool");
        assert_eq!(page.total, 1, "the call counts once, live");
        assert!(!page.rows[0].retained, "the live copy wins");

        let page = reloaded
            .list_page(&[], "r-run-1", &NO_FETCH, &pricing(), &TraceListQuery::default())
            .expect("journal-only pool");
        assert_eq!(page.total, 1);
        assert!(page.rows[0].retained, "without the ring, the journal copy lists");
    }

    #[test]
    fn the_journal_survives_a_restart_and_never_duplicates_a_key() {
        let (dir, journal) = empty_journal();
        let first = summary(1, 1_000, "priced-model", None);
        let second = summary(2, 2_000, "other-model", None);
        journal.poll_summaries(&[first.clone(), second.clone()]);

        // Simulated restart: everything re-read from disk, merged by key.
        let reloaded = TraceJournal::open(dir.path().to_path_buf());
        let page = reloaded
            .list_page(&[], "r-run-1", &NO_FETCH, &pricing(), &TraceListQuery::default())
            .expect("reload");
        assert_eq!(page.total, 2, "both terminal calls reload");
        assert!(page.rows.iter().all(|row| row.retained));

        // The same rows poll again (the ring still holds them): no dupes.
        reloaded.poll_summaries(&[first.clone(), second.clone()]);
        let page = reloaded
            .list_page(&[], "r-run-1", &NO_FETCH, &pricing(), &TraceListQuery::default())
            .expect("re-polled");
        assert_eq!(page.total, 2, "a key is journaled exactly once");

        // In-flight calls are not journaled yet.
        let mut streaming = summary(3, 3_000, "priced-model", None);
        streaming.is_complete = false;
        reloaded.poll_summaries(&[streaming]);
        let page = reloaded
            .list_page(&[], "r-run-1", &NO_FETCH, &pricing(), &TraceListQuery::default())
            .expect("in-flight filtered");
        assert_eq!(page.total, 2, "only terminal calls journal");

        // A new run's call appends alongside the old one.
        let mut other_run = summary(1, 4_000, "priced-model", None);
        other_run.run_id = "r-run-2".to_string();
        reloaded.poll_summaries(&[other_run.clone()]);
        let page = reloaded
            .list_page(&[], "r-run-1", &NO_FETCH, &pricing(), &TraceListQuery::default())
            .expect("cross-run");
        assert_eq!(page.total, 3, "ids are namespaced by run — no collision");

        // And another restart still sees all three.
        let again = TraceJournal::open(dir.path().to_path_buf());
        let page = again
            .list_page(&[], "r-run-1", &NO_FETCH, &pricing(), &TraceListQuery::default())
            .expect("second reload");
        assert_eq!(page.total, 3);
    }

    #[test]
    fn journal_rotation_keeps_four_archives_and_reloads_the_newest() {
        let (dir, journal) = empty_journal();
        // Fat rows (a long agent name) keep the batch count that fills the
        // 8 MiB threshold small; the threshold itself is the frozen const.
        let fat_batch = |run: &str, from: u64, count: u64| -> Vec<LlmRequestSummary> {
            (0..count)
                .map(|n| {
                    let mut row = summary(from + n, 1_000 + from + n, "priced-model", None);
                    row.run_id = run.to_string();
                    row.attribution = Some(TraceAttribution {
                        actor_kind: TraceActorKind::Worker,
                        agent_id: "agent-rotation".to_string(),
                        session_id: "session-rotation".to_string(),
                        agent_name: "W".repeat(1400),
                        worker_id: Some("w-rotation".to_string()),
                        plan_id: None,
                        plan_step: None,
                        turn_id: None,
                        purpose: TracePurpose::Turn,
                    });
                    row
                })
                .collect()
        };

        // Batch A fills past the threshold in one append (the rotate check
        // runs per append, not per line).
        let batch_a = fat_batch("r-a", 1, 6_000);
        journal.poll_summaries(&batch_a);
        assert!(journal.journal_bytes() >= TRACE_JOURNAL_ROTATE_BYTES);
        assert!(
            !dir.path().join(format!("{TRACE_INDEX_FILE}.1")).exists(),
            "no rotation before a second append"
        );

        // Batch B: the pre-append size check rotates A to .1 first.
        let batch_b = fat_batch("r-b", 10_001, 6_000);
        journal.poll_summaries(&batch_b);
        assert!(dir.path().join(format!("{TRACE_INDEX_FILE}.1")).exists());
        assert!(
            !dir.path().join(format!("{TRACE_INDEX_FILE}.2")).exists(),
            "one archive so far"
        );

        // Batch C rotates again: .1 -> .2, active -> .1.
        let batch_c = fat_batch("r-c", 20_001, 6_000);
        journal.poll_summaries(&batch_c);
        assert!(dir.path().join(format!("{TRACE_INDEX_FILE}.2")).exists());
        assert!(
            !dir.path().join(format!("{TRACE_INDEX_FILE}.3")).exists(),
            "at most the kept archives exist"
        );

        // The reload (a simulated restart) merges all three files: the
        // newest batch's rows and the oldest batch's rows are all listed.
        let reloaded = TraceJournal::open(dir.path().to_path_buf());
        let page = reloaded
            .list_page(&[], "r-run-1", &NO_FETCH, &pricing(), &TraceListQuery::default())
            .expect("reload after rotation");
        assert_eq!(page.total, 18_000, "every archived row reloads, deduped");
        assert!(!page.capped, "the unsearched scan stays exact at 18k rows");
        assert!(page.has_more, "18k retained rows page 100 at a time");
        let models: HashSet<String> = page
            .rows
            .iter()
            .map(|row| row.summary.run_id.clone())
            .collect();
        assert_eq!(
            models,
            ["r-c".to_string()].into_iter().collect(),
            "the newest rows page first (18k rows, limit 100)"
        );
    }

    #[test]
    fn deleting_history_removes_both_tiers_and_floors_the_journal() {
        let (dir, journal) = empty_journal();
        let old = summary(1, 1_000, "priced-model", None);
        journal.poll_summaries(std::slice::from_ref(&old));
        // The opt-in body history files, as the kernel would have written them.
        let history_record = record(&old, serde_json::json!({"messages": []}), Some("raw\n"));
        std::fs::write(
            dir.path().join(TRACE_HISTORY_FILE),
            format!("{}\n", serde_json::to_string(&history_record).expect("line")),
        )
        .expect("history file");
        std::fs::write(
            dir.path().join("traces-history-12345.jsonl"),
            format!("{}\n", serde_json::to_string(&history_record).expect("line")),
        )
        .expect("history archive");
        assert!(journal.journal_bytes() > 0);
        assert!(history_bytes(dir.path()) > 0);

        let (journal_files, journal_bytes) = journal.delete();
        assert!(journal_files >= 1);
        assert!(journal_bytes > 0);
        let (history_files, history_bytes) = remove_history_files(dir.path());
        assert_eq!(history_files, 2, "the active history file and its archive");
        assert!(history_bytes > 0);
        assert!(!dir.path().join(TRACE_INDEX_FILE).exists());
        assert!(!dir.path().join(TRACE_HISTORY_FILE).exists());
        assert!(!dir.path().join("traces-history-12345.jsonl").exists());
        assert_eq!(journal.journal_bytes(), 0);

        // The deletion floors the journal: calls captured BEFORE it are not
        // re-journaled from the ring, calls after it are.
        journal.poll_summaries(&[old]);
        assert_eq!(journal.journal_bytes(), 0, "the pre-delete call stays deleted");
        let fresh = summary(9, now_ms() + 60_000, "priced-model", None);
        journal.poll_summaries(&[fresh]);
        assert!(journal.journal_bytes() > 0, "new calls journal again");

        let reloaded = TraceJournal::open(dir.path().to_path_buf());
        let page = reloaded
            .list_page(&[], "r-run-1", &NO_FETCH, &pricing(), &TraceListQuery::default())
            .expect("post-delete reload");
        assert_eq!(page.total, 1, "only the post-delete call survived");
    }

    #[test]
    fn history_records_load_by_key_from_the_archives() {
        let (dir, _journal) = empty_journal();
        let mut old = summary(9, 9_000, "priced-model", None);
        old.run_id = "r-old".to_string();
        let with_bodies = record(
            &old,
            serde_json::json!({ "messages": [{ "role": "user", "content": "hello history" }] }),
            Some("data: {}\n"),
        );
        std::fs::write(
            dir.path().join(TRACE_HISTORY_FILE),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&with_bodies).expect("line"),
                "{\"unparseable\": true}"
            ),
        )
        .expect("history file");

        let mut wanted = HashSet::new();
        wanted.insert(("r-old".to_string(), 9u64));
        let found = history_records(dir.path(), &wanted);
        assert_eq!(found.len(), 1, "corrupt lines are skipped, not fatal");
        let loaded = &found[&("r-old".to_string(), 9u64)];
        assert_eq!(loaded.run_id, "r-old");
        assert!(loaded.response_raw.is_some(), "the logged body survives");
        assert_eq!(
            loaded.request_json["messages"][0]["content"],
            "hello history"
        );

        let mut miss = HashSet::new();
        miss.insert(("r-other".to_string(), 1u64));
        assert!(history_records(dir.path(), &miss).is_empty());
    }

    #[test]
    fn utc_stamp_formats_epoch_zero() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1_001), "19700101-000001");
    }

    /// The export fixture: two live calls (one priced, one unpriced), two
    /// journal-only calls from an earlier run, and a body-logged history
    /// record for a fifth key.
    fn export_fixture() -> (
        tempfile::TempDir,
        Arc<TraceJournal>,
        Vec<LlmRequestSummary>,
        HashMap<u64, LlmRequestRecord>,
    ) {
        let (dir, journal) = empty_journal();
        let live1 = summary(1, 4_000, "priced-model", Some(lead_attribution()));
        let live2 = summary(2, 3_000, "mystery-model", None);
        let mut retained1 = summary(3, 2_000, "priced-model", None);
        retained1.run_id = "r-run-0".to_string();
        let mut retained2 = summary(4, 1_000, "glm-5.3", None);
        retained2.run_id = "r-run-0".to_string();
        journal.poll_summaries(&[retained1.clone(), retained2.clone()]);
        let journal = TraceJournal::open(dir.path().to_path_buf()); // a restart

        let mut logged = summary(9, 500, "priced-model", None);
        logged.run_id = "r-old".to_string();
        let logged = record(
            &logged,
            serde_json::json!({ "messages": [{ "role": "user", "content": "from history" }] }),
            Some("data: {}\n"),
        );
        std::fs::write(
            dir.path().join(TRACE_HISTORY_FILE),
            format!("{}\n", serde_json::to_string(&logged).expect("line")),
        )
        .expect("history file");

        let live = vec![live1.clone(), live2.clone()];
        let mut records = HashMap::new();
        records.insert(
            1u64,
            record(
                &live1,
                serde_json::json!({ "messages": [{ "role": "user", "content": "hello export" }] }),
                Some("data: {\"delta\":{}}\n"),
            ),
        );
        records.insert(2u64, record(&live2, serde_json::json!({ "messages": [] }), None));
        (dir, journal, live, records)
    }

    /// Read an export bundle back as parsed JSON values, manifest first.
    fn read_export(path: &str) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(path).expect("the export file exists");
        text.lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).expect("one JSON value per line"))
            .collect()
    }

    #[test]
    fn export_filtered_ignores_limit_and_cursor_and_states_the_count() {
        let (dir, journal, live, records) = export_fixture();
        let fetch = move |id: u64| records.get(&id).cloned();
        let result = assemble_export(
            &journal,
            dir.path(),
            &live,
            "r-run-1",
            &fetch,
            &pricing(),
            TraceExportRequest {
                scope: TraceExportScope::Filtered,
                keys: None,
                query: Some(TraceListQuery {
                    limit: 2,                // ignored by design
                    cursor: Some("9:zzz:9".to_string()), // also ignored
                    ..Default::default()
                }),
            },
        )
        .expect("the filtered export");
        assert_eq!(result.count, 4, "every match exports — four calls total");

        let lines = read_export(&result.path);
        assert_eq!(lines.len(), 5, "the manifest plus four call lines");
        let manifest = &lines[0];
        assert_eq!(manifest["format"], "mnemo-trace-v1");
        assert_eq!(manifest["scope"], "filtered");
        assert_eq!(manifest["count"], 4);
        assert_eq!(manifest["includes_bodies"], true);
        assert_eq!(manifest["pricing"]["source"], "endpoints.toml + vendor defaults");
        assert!(manifest["run_id"].is_string(), "the producing run is named");

        let kinds: Vec<&str> = lines[1..]
            .iter()
            .map(|line| line["kind"].as_str().expect("kind"))
            .collect();
        assert_eq!(kinds, vec!["record", "record", "summary", "summary"]);
        assert!(
            lines[1..]
                .iter()
                .any(|line| line["record"]["model"] == "mystery-model"
                    && line["pricing"]["priced"] == false),
            "the unpriced live call carries its dash, not a fake zero"
        );
        assert!(lines[4]["summary"]["id"] == 4, "journal rows export as metadata");
        assert!(lines[4]["retained"] == true);
        assert!(
            lines[1]["pricing"]["cost_micros"].is_number(),
            "priced records carry their estimate"
        );
        assert!(result.path.contains(TRACE_EXPORTS_DIR), "the bundle lands in the exports folder");
        assert!(result.includes_bodies);
    }

    #[test]
    fn export_call_and_selected_resolve_live_history_and_retained() {
        let (dir, journal, live, records) = export_fixture();
        let fetch = move |id: u64| records.get(&id).cloned();

        // Scope call: exactly one record line.
        let call = assemble_export(
            &journal,
            dir.path(),
            &live,
            "r-run-1",
            &fetch,
            &pricing(),
            TraceExportRequest {
                scope: TraceExportScope::Call,
                keys: Some(vec!["r-run-1:1".to_string()]),
                query: None,
            },
        )
        .expect("the call export");
        assert_eq!(call.count, 1);
        let lines = read_export(&call.path);
        assert_eq!(lines[1]["kind"], "record");
        assert_eq!(lines[1]["record"]["id"], 1);

        // Scope selected: live, history-logged, retained, and a key that no
        // longer resolves anywhere — the count states what really exported.
        let selected = assemble_export(
            &journal,
            dir.path(),
            &live,
            "r-run-1",
            &fetch,
            &pricing(),
            TraceExportRequest {
                scope: TraceExportScope::Selected,
                keys: Some(vec![
                    "r-run-1:1".to_string(),
                    "r-old:9".to_string(),
                    "r-run-0:4".to_string(),
                    "r-ghost:1".to_string(),
                ]),
                query: None,
            },
        )
        .expect("the selected export");
        assert_eq!(selected.count, 3, "the ghost key is skipped, not faked");
        let lines = read_export(&selected.path);
        let mut saw_history = false;
        let mut saw_retained = false;
        for line in &lines[1..] {
            if line["kind"] == "record" {
                assert!(line["record"]["run_id"] == "r-run-1" || line["record"]["run_id"] == "r-old");
                if line["record"]["run_id"] == "r-old" {
                    saw_history = true;
                    assert!(line["record"]["response_raw"].is_string(), "the logged body exports");
                }
            } else {
                saw_retained = true;
                assert_eq!(line["summary"]["run_id"], "r-run-0");
                assert_eq!(line["retained"], true);
                assert!(line["pricing"]["rates"].is_object(), "the vendor-default rates are named");
            }
        }
        assert!(saw_history, "the history-logged call resolved with bodies");
        assert!(saw_retained, "the journal-only call exported as metadata");

        // An empty selected scope is an error, not an empty bundle.
        let empty = assemble_export(
            &journal,
            dir.path(),
            &live,
            "r-run-1",
            &fetch,
            &pricing(),
            TraceExportRequest {
                scope: TraceExportScope::Selected,
                keys: Some(vec!["r-ghost:1".to_string()]),
                query: None,
            },
        );
        assert!(empty.is_err(), "a bundle with no calls is refused");
    }

    #[test]
    fn reveal_accepts_only_canonical_paths_inside_the_exports_folder() {
        let (dir, _journal) = empty_journal();
        let exports = dir.path().join(TRACE_EXPORTS_DIR);
        std::fs::create_dir_all(&exports).expect("exports dir");
        let inside = exports.join("trace-20260101-000000.jsonl");
        std::fs::write(&inside, "{}\n").expect("an exported bundle");
        let outside = dir.path().join("journal-adjacent.jsonl");
        std::fs::write(&outside, "{}\n").expect("a file outside");

        let revealed = validate_reveal_path(dir.path(), &inside.display().to_string());
        assert!(revealed.is_ok(), "the bundle inside the folder reveals");

        assert!(
            validate_reveal_path(dir.path(), &outside.display().to_string()).is_err(),
            "a sibling file is refused"
        );
        assert!(
            validate_reveal_path(
                dir.path(),
                &outside
                    .parent()
                    .unwrap()
                    .join(format!("..\\{}", outside.file_name().unwrap().to_string_lossy()))
                    .display()
                    .to_string()
            )
            .is_err(),
            "a traversal resolves outside the folder and is refused"
        );
        assert!(
            validate_reveal_path(dir.path(), &exports.join("missing.jsonl").display().to_string())
                .is_err(),
            "a nonexistent file cannot reveal"
        );
    }
}