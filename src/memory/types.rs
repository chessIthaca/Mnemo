// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Memory types — `MemoryTier`, `Memory`, `MemoryData`.
//!
//! The four-tier model after agent-memory.dev:
//! - **Working** — raw tool-use events (highest recency)
//! - **Episodic** — compressed session summaries
//! - **Semantic** — distilled cross-session facts
//! - **Procedural** — learned recurring workflows

use serde::{Deserialize, Serialize};

/// The four memory tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTier {
    /// Raw tool-use events from the current + recent sessions.
    Working,
    /// Compressed session summaries.
    Episodic,
    /// Distilled cross-session facts.
    Semantic,
    /// Learned recurring workflows.
    Procedural,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryTier::Working => "working",
            MemoryTier::Episodic => "episodic",
            MemoryTier::Semantic => "semantic",
            MemoryTier::Procedural => "procedural",
        }
    }

    /// Parse a tier from its string form.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "working" => Some(Self::Working),
            "episodic" => Some(Self::Episodic),
            "semantic" => Some(Self::Semantic),
            "procedural" => Some(Self::Procedural),
            _ => None,
        }
    }
}

impl std::fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The provenance class of a memory record.
///
/// - **Authored** — written by the agent or user via `memory_write` (facts,
///   decisions, workflows). Sacred: index rebuilds never touch these.
/// - **Derived** — regenerable digests built by the indexer from on-disk
///   truth (`.coding/plans/*`, `.coding/reviews/*`, backlog, git). A rebuild
///   drops and re-creates these freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryClass {
    /// Agent/user-authored knowledge (the default).
    #[default]
    Authored,
    /// Indexer-built digest pointing at an on-disk source of truth.
    Derived,
}

impl MemoryClass {
    /// The stable string form stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryClass::Authored => "authored",
            MemoryClass::Derived => "derived",
        }
    }

    /// Parse a class from its string form.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "authored" => Some(Self::Authored),
            "derived" => Some(Self::Derived),
            _ => None,
        }
    }
}

impl std::fmt::Display for MemoryClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The typed-record classification of a memory, parsed from its title prefix
/// (`SPEC:` / `DECISION:` / `BUG:` / `PLAN:` / `HOW:` / `REVIEW:`). Typed records
/// are pointer-first (the gist + a path/commit pointer; the file carries the
/// detail) and are filterable via `memory_list` and recall; `None` is an
/// ordinary untyped memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryRecordType {
    /// `SPEC:` — how a feature or subsystem is supposed to work.
    Spec,
    /// `DECISION:` — a choice plus its one-line rationale.
    Decision,
    /// `BUG:` — symptom → root cause → fix + regression test.
    Bug,
    /// `PLAN:` — a plan digest (goal, branch, commit, verdict, review path).
    Plan,
    /// `HOW:` — a recurring workflow recipe.
    How,
    /// `REVIEW:` — a review-report digest (verdict + findings count + path).
    /// Usually indexer-written (Phase 2 derived records).
    Review,
    /// No typed prefix — an ordinary memory (the default).
    #[default]
    None,
}

impl MemoryRecordType {
    /// The stable string form stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryRecordType::Spec => "spec",
            MemoryRecordType::Decision => "decision",
            MemoryRecordType::Bug => "bug",
            MemoryRecordType::Plan => "plan",
            MemoryRecordType::How => "how",
            MemoryRecordType::Review => "review",
            MemoryRecordType::None => "none",
        }
    }

    /// Parse a record type from its string form.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "spec" => Some(Self::Spec),
            "decision" => Some(Self::Decision),
            "bug" => Some(Self::Bug),
            "plan" => Some(Self::Plan),
            "how" => Some(Self::How),
            "review" => Some(Self::Review),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// Classify a title by its typed-record prefix. Case-sensitive, prefix at
    /// position 0 (`"BUG: crash"` → `Bug`; `"bug: crash"` / `"note: BUG:"` →
    /// `None`).
    pub fn from_title(title: &str) -> Self {
        const PREFIXES: &[(&str, MemoryRecordType)] = &[
            ("SPEC:", MemoryRecordType::Spec),
            ("DECISION:", MemoryRecordType::Decision),
            ("BUG:", MemoryRecordType::Bug),
            ("PLAN:", MemoryRecordType::Plan),
            ("HOW:", MemoryRecordType::How),
            ("REVIEW:", MemoryRecordType::Review),
        ];
        for (prefix, record_type) in PREFIXES {
            if title.starts_with(prefix) {
                return *record_type;
            }
        }
        MemoryRecordType::None
    }

    /// The default auto-truncation length (in chars) for derived/captured
    /// digests of this record type (configurable via MemorySearchConfig).
    /// Typed records are pointer-first — the memory carries the gist + the
    /// path/commit, the file carries the detail. `How`/`None` have no
    /// budget (their digests use the SPEC/REVIEW default of 500).
    pub fn content_budget(&self) -> Option<usize> {
        match self {
            MemoryRecordType::Plan => Some(400),
            MemoryRecordType::Bug => Some(600),
            MemoryRecordType::Spec => Some(500),
            MemoryRecordType::Decision => Some(300),
            MemoryRecordType::Review => Some(500),
            MemoryRecordType::How | MemoryRecordType::None => None,
        }
    }
}

impl std::fmt::Display for MemoryRecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub tier: MemoryTier,
    pub title: String,
    pub content: String,
    pub strength: f64,
    pub access_count: u32,
    pub created_at: i64,
    pub last_accessed_at: i64,
    pub source_session_ids: Vec<String>,
    /// Tier-specific payload as JSON.
    pub data: serde_json::Value,
    /// Provenance class (authored vs indexer-derived).
    #[serde(default)]
    pub record_class: MemoryClass,
    /// Typed-record classification parsed from the title prefix.
    #[serde(default)]
    pub record_type: MemoryRecordType,
    /// Id of the memory that supersedes this one; `None` while the memory is
    /// live. Superseded memories are history — excluded from recall by
    /// default, retrievable via `include_superseded`.
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// Optional embedding vector (not serialized to JSON normally).
    #[serde(default, skip_serializing)]
    pub embedding: Vec<f32>,
}

impl Memory {
    /// Create a new memory with default strength and timestamps.
    pub fn new(
        tier: MemoryTier,
        title: impl Into<String>,
        content: impl Into<String>,
        now: i64,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            tier,
            title: title.into(),
            content: content.into(),
            strength: 1.0,
            access_count: 0,
            created_at: now,
            last_accessed_at: now,
            source_session_ids: Vec::new(),
            data: serde_json::Value::Null,
            record_class: MemoryClass::Authored,
            record_type: MemoryRecordType::None,
            superseded_by: None,
            embedding: Vec::new(),
        }
    }
}

/// One entry of the rolling memory-access log — surfaced in the Graph tab's
/// "Memory access" section so store activity (reads + writes) is visible at
/// a glance. Kept newest-last in a capped in-memory ring on
/// [`MemoryStore`](crate::memory::MemoryStore); a restart starts it empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAccessEntry {
    /// Unix timestamp (seconds) of the access, from the store's clock.
    pub at: i64,
    /// The operation kind: `"read"` (recall / session primer) or `"write"`.
    pub op: String,
    /// The tier a write targeted, or the tier filter a read used. `None`
    /// when a read had no explicit tier (the default filter) — writes
    /// always carry a tier.
    pub tier: Option<String>,
    /// What was searched for (a recall query / `"session primer"`) or
    /// written (the memory title), truncated to a bounded length.
    pub detail: String,
    /// How many memories a read returned. `None` for writes.
    pub hits: Option<usize>,
}

/// Tier-specific payloads (stored as JSON in the `data` column).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryData {
    /// Raw tool event: tool_name, tool_input, tool_output, error.
    Working(WorkingData),
    /// Session summary: narrative, key_decisions, files_modified, concepts.
    Episodic(EpisodicData),
    /// Distilled fact: fact, confidence (0-1, rises with reinforcement).
    Semantic(SemanticData),
    /// Learned workflow: name, steps[], trigger_condition, expected_outcome, frequency.
    Procedural(ProceduralData),
}

/// Working-memory payload — a raw tool event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkingData {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub tool_output: String,
    pub error: Option<String>,
}

/// Episodic-memory payload — a session summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpisodicData {
    pub narrative: String,
    pub key_decisions: Vec<String>,
    pub files_modified: Vec<String>,
    pub concepts: Vec<String>,
}

/// Semantic-memory payload — a distilled fact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticData {
    pub fact: String,
    /// 0.0–1.0; rises with reinforcement across sessions.
    pub confidence: f64,
}

/// Procedural-memory payload — a learned workflow.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProceduralData {
    pub name: String,
    pub steps: Vec<String>,
    pub trigger_condition: String,
    pub expected_outcome: String,
    pub frequency: u32,
}

/// A filter for memory recall.
#[derive(Debug, Clone)]
pub struct MemoryFilter {
    pub tier: Option<MemoryTier>,
    pub limit: Option<usize>,
    /// When `true` (the default), working-tier memories are excluded from
    /// recall. Working memories are raw tool-output snapshots meant as
    /// consolidation *input*, not recall *output* — left in, they crowd out
    /// distilled facts (they match common keywords and carry fresh strength).
    /// Set to `false` via [`MemoryFilter::include_working`] to recall raw
    /// events (rarely needed). Ignored when `tier` is explicitly set (an
    /// explicit tier always wins).
    pub exclude_working: bool,
    /// When `true` (the default), superseded memories are excluded from
    /// recall/listing — supersession is history, not live knowledge. Set to
    /// `false` via [`MemoryFilter::include_superseded`] to retrieve history.
    pub exclude_superseded: bool,
    /// Optional typed-record filter — restrict results to one record type
    /// (`SPEC:`/`DECISION:`/`BUG:`/`PLAN:`/`HOW:`/`REVIEW:`). `None` matches all.
    pub record_type: Option<MemoryRecordType>,
    /// Optional title prefix filter (case-sensitive, position 0 — e.g.
    /// `"BUG: login"`). Used by `memory_list` to browse a title namespace.
    /// `None` matches all.
    pub title_prefix: Option<String>,
}

impl Default for MemoryFilter {
    fn default() -> Self {
        Self {
            tier: None,
            limit: None,
            exclude_working: true,
            exclude_superseded: true,
            record_type: None,
            title_prefix: None,
        }
    }
}

impl MemoryFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tier(mut self, tier: MemoryTier) -> Self {
        self.tier = Some(tier);
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Opt into recalling working-tier (raw tool-event) memories. Off by
    /// default — working memories are consolidation input, not recall output.
    pub fn include_working(mut self) -> Self {
        self.exclude_working = false;
        self
    }

    /// Opt into recalling superseded memories (history). Off by default —
    /// superseded records are excluded entirely, not merely downranked.
    pub fn include_superseded(mut self) -> Self {
        self.exclude_superseded = false;
        self
    }

    /// Restrict results to one typed-record class.
    pub fn record_type(mut self, record_type: MemoryRecordType) -> Self {
        self.record_type = Some(record_type);
        self
    }

    /// Restrict results to memories whose title starts with `prefix`
    /// (case-sensitive, position 0 — e.g. `"BUG: login"`). A browse
    /// convenience for `memory_list`; recall rarely needs it (the query
    /// already covers keywords).
    pub fn title_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.title_prefix = Some(prefix.into());
        self
    }
}

/// The retrieval/indexing scale knobs (Phase 2), live-editable from
/// Settings → Memory. Persisted as the `[memory]` section of `config.toml`;
/// the store holds a snapshot that recall and the indexer read per call,
/// so a Settings save takes effect without a restart. Defaults equal the
/// historical hardcoded behavior — an untouched config changes nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemorySearchConfig {
    /// Recency half-life (days) for the Ebbinghaus strength decay used in
    /// recall/primer ranking. Default 7.0 — the historical constant.
    pub decay_half_life_days: f64,
    /// Default per-query result cap when the caller passes no explicit limit
    /// (an explicit filter limit always wins). Default 50 — the effective
    /// ceiling today (the FTS candidate pool is 50).
    pub per_query_cap: usize,
    /// Max DERIVED records of one record type in a single recall result
    /// (authored records are never capped). Default 5 — the scale backstop
    /// that keeps thousands of indexer digests from crowding authored
    /// knowledge out of a result set.
    pub derived_per_class_cap: usize,
    /// Digest auto-truncation lengths (chars) for derived/captured digests,
    /// read by the indexer and finish-capture. Defaults mirror
    /// [`MemoryRecordType::content_budget`].
    pub plan_budget: usize,
    /// See `plan_budget`.
    pub bug_budget: usize,
    /// See `plan_budget`.
    pub spec_budget: usize,
    /// See `plan_budget`.
    pub decision_budget: usize,
    /// See `plan_budget`.
    pub review_budget: usize,
}

impl Default for MemorySearchConfig {
    fn default() -> Self {
        // Budgets derive from content_budget() so the two can never drift.
        Self {
            decay_half_life_days: 7.0,
            per_query_cap: 50,
            derived_per_class_cap: 5,
            plan_budget: MemoryRecordType::Plan.content_budget().unwrap_or(400),
            bug_budget: MemoryRecordType::Bug.content_budget().unwrap_or(600),
            spec_budget: MemoryRecordType::Spec.content_budget().unwrap_or(500),
            decision_budget: MemoryRecordType::Decision.content_budget().unwrap_or(300),
            review_budget: MemoryRecordType::Review.content_budget().unwrap_or(500),
        }
    }
}

impl MemorySearchConfig {
    /// The content budget for a record type under this config — the
    /// configurable counterpart of [`MemoryRecordType::content_budget`].
    pub fn digest_budget(&self, record_type: MemoryRecordType) -> Option<usize> {
        match record_type {
            MemoryRecordType::Plan => Some(self.plan_budget),
            MemoryRecordType::Bug => Some(self.bug_budget),
            MemoryRecordType::Spec => Some(self.spec_budget),
            MemoryRecordType::Decision => Some(self.decision_budget),
            MemoryRecordType::Review => Some(self.review_budget),
            MemoryRecordType::How | MemoryRecordType::None => None,
        }
    }

    /// Clamp every knob into a sane range — a typo in `config.toml` must not
    /// disable recall or unbound the result set (mirrors
    /// `TraceConfig::clamped`). A non-finite half-life (TOML allows `nan`)
    /// falls back to the default rather than producing NaN scores.
    pub fn clamped(self) -> Self {
        Self {
            decay_half_life_days: if self.decay_half_life_days.is_finite() {
                self.decay_half_life_days.clamp(0.1, 365.0)
            } else {
                7.0
            },
            per_query_cap: self.per_query_cap.clamp(1, 500),
            derived_per_class_cap: self.derived_per_class_cap.clamp(1, 100),
            plan_budget: self.plan_budget.clamp(50, 5000),
            bug_budget: self.bug_budget.clamp(50, 5000),
            spec_budget: self.spec_budget.clamp(50, 5000),
            decision_budget: self.decision_budget.clamp(50, 5000),
            review_budget: self.review_budget.clamp(50, 5000),
        }
    }
}

/// A session record (for provenance + episodic summaries).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project: String,
    pub created_at: i64,
    pub ended_at: Option<i64>,
}

impl Session {
    pub fn new(project: impl Into<String>, now: i64) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            project: project.into(),
            created_at: now,
            ended_at: None,
        }
    }
}

/// A single LLM-request usage record (one row in `request_stats`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestStats {
    /// Unique row id (a UUID string).
    pub id: String,
    /// The memory session this request belongs to, when known.
    pub session_id: Option<String>,
    /// The model that served the request (from the provider snapshot).
    pub model: String,
    /// The endpoint/provider name from `endpoints.toml` that served the
    /// request. `None` for rows recorded before the endpoint dimension
    /// existed and for unnamed test mocks (empty `provider_name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Input tokens (includes cached + reasoning-prompt tokens).
    pub prompt_tokens: u32,
    /// Output tokens (includes reasoning tokens).
    pub completion_tokens: u32,
    /// Reasoning tokens (a subset of `completion_tokens`).
    pub reasoning_tokens: u32,
    /// Prompt tokens served from the provider's cache (a subset of
    /// `prompt_tokens` — already included, reported for pricing). `None`
    /// when the provider never reported usage for this request (an error
    /// row) — distinct from `Some(0)`, a real reported cache miss.
    pub cached_tokens: Option<u32>,
    /// Time-to-first-token (ms). `None` when timing wasn't captured.
    pub ttft_ms: Option<u32>,
    /// Generation time (ms): first chunk → usage event. `None` when timing
    /// wasn't captured.
    pub generation_ms: Option<u32>,
    /// Unix timestamp (seconds) of the request.
    pub created_at: i64,
    /// Outcome tag: `None` for a normal successful request, `Some("error")`
    /// when the request died before a usage report (the prompt token count
    /// is our estimate, `cached_tokens` is `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Purpose tag: `None` for main-loop requests, `Some("summarize")` for
    /// compaction's own call (its mega-prompt is invisible to the main
    /// loop's Usage arm otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
}

/// Aggregated token usage + timing for a single session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStats {
    /// The session's id (matches `sessions.id`).
    pub session_id: String,
    /// Sum of prompt tokens across the session's requests.
    pub prompt_tokens: u64,
    /// Sum of completion tokens across the session's requests.
    pub completion_tokens: u64,
    /// Sum of reasoning tokens (subset of `completion_tokens`).
    pub reasoning_tokens: u64,
    /// Sum of cached prompt tokens (subset of `prompt_tokens`).
    pub cached_tokens: u64,
    /// Total TTFT across requests that reported it (for averages).
    pub ttft_ms_total: u64,
    /// Total generation time across requests that reported it.
    pub generation_ms_total: u64,
    /// Number of requests that reported TTFT (for averages).
    pub timed_requests: u64,
    /// Total number of requests in the session.
    pub request_count: u64,
    /// Per-model × endpoint breakdown: one row per (model, endpoint) — the
    /// same model on different endpoints is separate rows.
    pub per_model: Vec<ModelBreakdown>,
    /// Unix timestamp (seconds) the session started.
    pub created_at: i64,
    /// Unix timestamp (seconds) the session ended, when it has.
    pub ended_at: Option<i64>,
}

/// Token usage for a single model within a session or project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelBreakdown {
    /// The model name.
    pub model: String,
    /// The endpoint that served these requests; `None` groups pre-endpoint
    /// rows (and unnamed test mocks) — same model on different endpoints
    /// renders as separate rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Sum of prompt tokens for this model.
    pub prompt_tokens: u64,
    /// Sum of completion tokens for this model.
    pub completion_tokens: u64,
    /// Sum of reasoning tokens (subset of `completion_tokens`).
    pub reasoning_tokens: u64,
    /// Sum of cached prompt tokens (subset of `prompt_tokens`).
    pub cached_tokens: u64,
    /// Total TTFT across this model's timed requests.
    pub ttft_ms_total: u64,
    /// Total generation time across this model's timed requests.
    pub generation_ms_total: u64,
    /// Number of requests that reported TTFT/generation timing.
    pub timed_requests: u64,
    /// Total number of requests for this model.
    pub request_count: u64,
}

/// Aggregated token usage + timing across the whole project (all sessions).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectStats {
    /// Sum of prompt tokens across all sessions.
    pub prompt_tokens: u64,
    /// Sum of completion tokens across all sessions.
    pub completion_tokens: u64,
    /// Sum of reasoning tokens (subset of `completion_tokens`).
    pub reasoning_tokens: u64,
    /// Sum of cached prompt tokens (subset of `prompt_tokens`).
    pub cached_tokens: u64,
    /// Total TTFT across all timed requests.
    pub ttft_ms_total: u64,
    /// Total generation time across all timed requests.
    pub generation_ms_total: u64,
    /// Number of requests that reported TTFT/generation timing.
    pub timed_requests: u64,
    /// Total number of requests across all sessions.
    pub request_count: u64,
    /// Number of sessions in the project.
    pub session_count: u64,
    /// Per-model × endpoint breakdown across the whole project.
    pub per_model: Vec<ModelBreakdown>,
    /// Tokens per day (epoch-day → totals). For the time-series view.
    pub per_day: Vec<DayBreakdown>,
}

/// One day's worth of token usage (for the project time series).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DayBreakdown {
    /// Unix timestamp of the start of the day (UTC midnight, in seconds).
    pub day: i64,
    /// Sum of prompt tokens on that day.
    pub prompt_tokens: u64,
    /// Sum of completion tokens on that day.
    pub completion_tokens: u64,
    /// Sum of reasoning tokens (subset of `completion_tokens`).
    pub reasoning_tokens: u64,
    /// Sum of cached prompt tokens (subset of `prompt_tokens`).
    pub cached_tokens: u64,
    /// Number of requests on that day.
    pub request_count: u64,
}

/// A session summary row (for listing sessions in the Stats tab).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// The session's id (matches `sessions.id`).
    pub session_id: String,
    /// Unix timestamp (seconds) the session started.
    pub created_at: i64,
    /// Unix timestamp (seconds) the session ended, when it has.
    pub ended_at: Option<i64>,
    /// Number of requests in the session.
    pub request_count: u64,
    /// Sum of prompt tokens in the session.
    pub prompt_tokens: u64,
    /// Sum of completion tokens in the session.
    pub completion_tokens: u64,
    /// Sum of reasoning tokens (subset of `completion_tokens`).
    pub reasoning_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_roundtrip() {
        for tier in [
            MemoryTier::Working,
            MemoryTier::Episodic,
            MemoryTier::Semantic,
            MemoryTier::Procedural,
        ] {
            let s = tier.as_str();
            assert_eq!(MemoryTier::from_str(s), Some(tier));
        }
        assert_eq!(MemoryTier::from_str("bogus"), None);
    }

    #[test]
    fn memory_new_defaults() {
        let m = Memory::new(MemoryTier::Working, "title", "content", 1000);
        assert_eq!(m.tier, MemoryTier::Working);
        assert_eq!(m.title, "title");
        assert_eq!(m.strength, 1.0);
        assert_eq!(m.access_count, 0);
        assert!(!m.id.is_empty());
    }

    #[test]
    fn memory_data_serde() {
        let d = MemoryData::Semantic(SemanticData {
            fact: "uses jose for JWT".into(),
            confidence: 0.8,
        });
        let json = serde_json::to_string(&d).unwrap();
        let back: MemoryData = serde_json::from_str(&json).unwrap();
        match back {
            MemoryData::Semantic(s) => {
                assert_eq!(s.fact, "uses jose for JWT");
                assert!((s.confidence - 0.8).abs() < 1e-9);
            }
            _ => panic!("expected semantic"),
        }
    }

    #[test]
    fn filter_builder() {
        let f = MemoryFilter::new().tier(MemoryTier::Episodic).limit(10);
        assert_eq!(f.tier, Some(MemoryTier::Episodic));
        assert_eq!(f.limit, Some(10));
    }

    #[test]
    fn record_enums_roundtrip() {
        for class in [MemoryClass::Authored, MemoryClass::Derived] {
            assert_eq!(MemoryClass::from_str(class.as_str()), Some(class));
        }
        assert_eq!(MemoryClass::from_str("bogus"), None);
        for rt in [
            MemoryRecordType::Spec,
            MemoryRecordType::Decision,
            MemoryRecordType::Bug,
            MemoryRecordType::Plan,
            MemoryRecordType::How,
            MemoryRecordType::Review,
            MemoryRecordType::None,
        ] {
            assert_eq!(MemoryRecordType::from_str(rt.as_str()), Some(rt));
        }
        assert_eq!(MemoryRecordType::from_str("bogus"), None);
    }

    #[test]
    fn record_type_from_title() {
        assert_eq!(
            MemoryRecordType::from_title("SPEC: browser tab"),
            MemoryRecordType::Spec
        );
        assert_eq!(
            MemoryRecordType::from_title("DECISION: use sqlite"),
            MemoryRecordType::Decision
        );
        assert_eq!(
            MemoryRecordType::from_title("BUG: crash on open"),
            MemoryRecordType::Bug
        );
        assert_eq!(
            MemoryRecordType::from_title("PLAN: 1c9ba6c8"),
            MemoryRecordType::Plan
        );
        assert_eq!(
            MemoryRecordType::from_title("HOW: run tests"),
            MemoryRecordType::How
        );
        assert_eq!(
            MemoryRecordType::from_title("REVIEW: phase-1 report"),
            MemoryRecordType::Review
        );
        // No prefix / lowercase / mid-string prefix → None.
        assert_eq!(
            MemoryRecordType::from_title("a plain title"),
            MemoryRecordType::None
        );
        assert_eq!(
            MemoryRecordType::from_title("bug: lowercase prefix"),
            MemoryRecordType::None
        );
        assert_eq!(
            MemoryRecordType::from_title("note SPEC: mid-string"),
            MemoryRecordType::None
        );
    }

    #[test]
    fn record_type_content_budgets() {
        assert_eq!(MemoryRecordType::Plan.content_budget(), Some(400));
        assert_eq!(MemoryRecordType::Bug.content_budget(), Some(600));
        assert_eq!(MemoryRecordType::Spec.content_budget(), Some(500));
        assert_eq!(MemoryRecordType::Decision.content_budget(), Some(300));
        assert_eq!(MemoryRecordType::Review.content_budget(), Some(500));
        assert_eq!(MemoryRecordType::How.content_budget(), None);
        assert_eq!(MemoryRecordType::None.content_budget(), None);
    }

    #[test]
    fn memory_new_record_defaults() {
        let m = Memory::new(MemoryTier::Semantic, "t", "c", 1000);
        assert_eq!(m.record_class, MemoryClass::Authored);
        assert_eq!(m.record_type, MemoryRecordType::None);
        assert_eq!(m.superseded_by, None);
    }

    #[test]
    fn filter_superseded_and_record_type_builders() {
        let f = MemoryFilter::new();
        assert!(f.exclude_superseded);
        assert_eq!(f.record_type, None);
        let f = f
            .include_superseded()
            .record_type(MemoryRecordType::Bug)
            .title_prefix("BUG: login");
        assert!(!f.exclude_superseded);
        assert_eq!(f.record_type, Some(MemoryRecordType::Bug));
        assert_eq!(f.title_prefix.as_deref(), Some("BUG: login"));
    }
}
