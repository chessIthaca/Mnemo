// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Steering-effectiveness metrics — do the nudges actually change tool choice?
//!
//! The steering layer appends advisory markers to tool OUTPUT (the search
//! symbol nudge, the search literal-engine TIP, the shell grep TIP, the
//! graph miss hint, the RECALLED CONTEXT rider, the read_files SYMBOL
//! NUDGE, the file_edit drift-class fresh-read note) plus the two
//! C5-family gates that INTERCEPT (the symbol-shaped search redirect and
//! the file_edit stale-read redirect after ignored fresh-read nudges).
//! This module measures whether they work. Two observations, taken at the tool-dispatch choke point
//! (`AgentLoop::dispatch_with_interrupt` — see `dispatch.rs`):
//!
//! - [`SteeringStats::observe_result`]: the result is matched against the
//!   stable marker table below, scoped by the emitting TOOL — the
//!   `tool_name` taken from the parsed call at the choke point. Only the
//!   tool that can legitimately carry a marker may fire it (search emitters
//!   are additionally first-line-scoped); a content-bearing result from any
//!   OTHER tool never fires, no matter what its payload happens to quote
//!   (a `read_files` body can still contain "is an indexed symbol" — but
//!   `read_files` fires only its own read nudge). The matched kind's `fired`
//!   counter plus a per-agent "last fired" note are recorded.
//! - [`SteeringStats::observe_call`]: every tool call is checked against the
//!   fired marker of the PREVIOUS result from the same agent — when it names
//!   one of that marker's target tools, `switched` increments (the nudge
//!   changed the agent's next choice). The note is consumed either way: only
//!   the immediately-following call counts as a switch.
//!
//! Counters are advisory instrumentation only: nothing here can fail a tool
//! call. The mutex recovers from poisoning (metrics are a derived cache, like
//! the graph store), and non-matching results cost a handful of substring
//! scans against one short table.
//!
//! Measurement caveat (review 2026-09-14, tightened by the tool-scoping):
//! detection remains approximate, never exact — the tool scoping eliminates
//! the whole cross-tool misfire class (content-bearing payloads echo marker
//! text), and first-line scoping kills within-tool echoes for the search
//! tools, but the counts are still a steering effectiveness SIGNAL, not a
//! measurement; treat them as advisory.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::Serialize;

use crate::runtime::AgentId;

/// The kinds of steering markers, each with its stable output substring and
/// the tool set that "accepts" the nudge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkerKind {
    /// `search`/`search_read` symbol nudge: "'x' is an indexed symbol —
    /// graph_context(id=...) …".
    SearchNudge,
    /// `shell` TIP appended to grep-family results.
    ShellTip,
    /// `graph_*` tools' shared miss hint ("No symbols matched …"), serialized
    /// inside the pretty-JSON output (no arbitrary file content reaches it).
    GraphMiss,
    /// The passive RECALLED CONTEXT rider (create_plan result, spawn_agent
    /// task). Surfaced only — it is context, not an instruction, so there is
    /// no tool it "switches" to.
    RecallRider,
    /// `read_files` whole-file SYMBOL NUDGE.
    ReadNudge,
    /// `search`/`search_read` literal-engine TIP ("TIP: pattern has no
    /// regex metacharacters — …"). FIRED-ONLY: `observe_call` sees tool
    /// NAMES, and a re-call of `search` with `literal:true` is
    /// indistinguishable from any other `search` call — no switch is
    /// claimed, the fired count is the signal.
    LiteralTip,
    /// `search`/`search_read` known-memory-hit note ("known memory hit:
    /// '<title>' …", F9 — a uuid-shaped pattern a derived row knows).
    /// FIRED-ONLY: the note points at memory_search; a follow-up lookup is
    /// indistinguishable from any other memory_search call — the fired
    /// count is the signal.
    KnownMemoryHit,
    /// The consolidation-due note appended by the dispatch funnel (F11 —
    /// "NOTE: {n} working-memory events accumulated this session …").
    /// FIRED-ONLY, ANY tool: the note is dispatcher-owned (no tool emits
    /// it) and appended AFTER the result, so full-output detection applies;
    /// the phrasing is specific to the dispatcher, and the once-per-session
    /// gate bounds even a rare echo to noise in an advisory counter.
    ConsolidationDue,
    /// `shell` redirection warning ("TIP: output redirection detected …",
    /// C4 — prepended to commands redirecting output to a null sink).
    /// FIRED-ONLY: like the literal tip, a re-run of the same command
    /// unredirected is indistinguishable from any other `shell` call.
    ShellRedirect,
    /// `file_edit` drift-class failure note ("… Re-read the file
    /// (read_files, this exact path) …", backlog 714196da — the error text
    /// the tool returns when old_string/line-range no longer match the
    /// file, i.e. the content has drifted from the agent's last read).
    /// Actionable: the read tools (`read_files`/`search_read`) count as
    /// switches, and the C5-family gate (`should_gate_file_edit` + the
    /// dispatch funnel's `file_edit_redirect`) intercepts the next blind
    /// edit of the SAME path after a single failure (EDIT_GATE_THRESHOLD —
    /// backlog e8b39d72 H3's freshness contract: any drift failure forces a
    /// read). The gate's state is per-(agent, path) — [`Inner::edit_drift`],
    /// recorded directly by the dispatch funnel (plan be16ea36 step 3) —
    /// while this marker's fired/switched counters stay the global
    /// telemetry.
    EditStaleRead,
}

impl MarkerKind {
    /// The marker's stable substring (part of the public tool-output
    /// contract — changing one requires changing the line here in the same
    /// commit, or the metric silently drops that kind).
    fn marker(self) -> &'static str {
        match self {
            MarkerKind::SearchNudge => SEARCH_NUDGE_MARK,
            MarkerKind::ShellTip => "TIP: for file-content search",
            MarkerKind::GraphMiss => "No symbols matched",
            MarkerKind::RecallRider => "RECALLED CONTEXT",
            MarkerKind::ReadNudge => "SYMBOL NUDGE:",
            MarkerKind::LiteralTip => "TIP: pattern has no regex metacharacters",
            MarkerKind::KnownMemoryHit => "known memory hit:",
            MarkerKind::ConsolidationDue => "working-memory events accumulated this session",
            MarkerKind::ShellRedirect => "TIP: output redirection detected",
            MarkerKind::EditStaleRead => EDIT_STALE_READ_MARK,
        }
    }

    /// The tool names that count as "the nudge worked": the next call from
    /// the same agent naming one of these increments `switched`.
    fn targets(self) -> &'static [&'static str] {
        match self {
            MarkerKind::SearchNudge => &[
                "graph_search",
                "graph_context",
                "graph_impact",
                "graph_path",
            ],
            MarkerKind::ShellTip => &["search", "search_read", "graph_search", "graph_context"],
            MarkerKind::GraphMiss => &["search", "search_read"],
            MarkerKind::RecallRider => &[],
            MarkerKind::ReadNudge => &[
                "graph_search",
                "graph_context",
                "graph_impact",
                "graph_path",
            ],
            // Fired-only (like RecallRider): a "switch" to search with
            // literal:true is indistinguishable from a re-grep.
            MarkerKind::LiteralTip => &[],
            // Fired-only (like RecallRider): memory_search is a lookup
            // either way — there is no distinct "switched" tool to count.
            MarkerKind::KnownMemoryHit => &[],
            // Fired-only: the note suggests a memory_consolidate call, which
            // is indistinguishable from any other consolidation.
            MarkerKind::ConsolidationDue => &[],
            // Fired-only: an unredirected re-run is indistinguishable from
            // any other shell call.
            MarkerKind::ShellRedirect => &[],
            // The read tools are the "switch": a fresh read of the drifted
            // file before the next edit (backlog 714196da).
            MarkerKind::EditStaleRead => &["read_files", "search_read"],
        }
    }

    /// Stable lowercase-kebab label surfaced on the wire and in the UI.
    fn label(self) -> &'static str {
        match self {
            MarkerKind::SearchNudge => "search-nudge",
            MarkerKind::ShellTip => "shell-tip",
            MarkerKind::GraphMiss => "graph-miss",
            MarkerKind::RecallRider => "recall-rider",
            MarkerKind::ReadNudge => "read-nudge",
            MarkerKind::LiteralTip => "literal-tip",
            MarkerKind::KnownMemoryHit => "known-memory-hit",
            MarkerKind::ConsolidationDue => "consolidation-due",
            MarkerKind::ShellRedirect => "shell-redirect",
            MarkerKind::EditStaleRead => "edit-stale-read",
        }
    }

    /// Index into [`Inner::counts`] (declaration order of this enum).
    fn index(self) -> usize {
        match self {
            MarkerKind::SearchNudge => 0,
            MarkerKind::ShellTip => 1,
            MarkerKind::GraphMiss => 2,
            MarkerKind::RecallRider => 3,
            MarkerKind::ReadNudge => 4,
            MarkerKind::LiteralTip => 5,
            MarkerKind::KnownMemoryHit => 6,
            MarkerKind::ConsolidationDue => 7,
            MarkerKind::ShellRedirect => 8,
            MarkerKind::EditStaleRead => 9,
        }
    }

    /// Detect this kind in a result from `tool_name`, by the rule below.
    /// One rule per kind, each shaped after its actual nudge site:
    /// - SearchNudge: tools `search`/`search_read` only, and the marker must
    ///   be on the FIRST LINE — the note is prepended via
    ///   `with_note`/`merged_note` (search.rs), so a payload echo deeper in
    ///   the result can never fire. The auto-delegated answer block (backlog
    ///   b804012f) carries the marker on its first line too, so delegated
    ///   answers count as SearchNudge events — AND as switches (the
    ///   delegation IS the nudge, fulfilled in-place: observe_result
    ///   registers the switch when the first line carries "AUTO-DELEGATED"),
    ///   which keeps the C5 intercept gate from punishing delegation. The
    ///   escape repeat's advisory note counts as before (a fire, no
    ///   auto-switch). KnownMemoryHit stays uuid-only — memory delegation is
    ///   not counted there.
    /// - ReadNudge: tool `read_files` only, prefix-scoped — the note is the
    ///   single leading line (`output = format!("SYMBOL NUDGE: …")`,
    ///   read_files.rs).
    /// - ShellTip: tool `shell` only, first line — `GREP_NUDGE` is
    ///   `insert_str(0, …)`-prepended (shell.rs).
    /// - GraphMiss: tools `graph_search`/`graph_context`/`graph_impact`/
    ///   `graph_path` only, anywhere — these outputs are structured
    ///   pretty-JSON and never embed arbitrary file content.
    /// - RecallRider: tool `create_plan` only, anywhere — the rider is
    ///   appended to the plan summary by `recalled_context_block`.
    /// - LiteralTip: tools `search`/`search_read` only, first line — the
    ///   TIP rides the merged note (search.rs `merged_note`, prepended
    ///   via `with_note`), so a payload echo deeper in the result can
    ///   never fire.
    /// - KnownMemoryHit: tools `search`/`search_read` only, first line —
    ///   the note rides the same merged note, so a payload echo deeper
    ///   in the result can never fire.
    fn detect(self, tool_name: &str, output: &str) -> bool {
        let first_line = || output.lines().next().unwrap_or("");
        match self {
            MarkerKind::SearchNudge => {
                matches!(tool_name, "search" | "search_read")
                    && first_line().contains(SEARCH_NUDGE_MARK)
            }
            MarkerKind::ReadNudge => tool_name == "read_files" && output.starts_with(self.marker()),
            MarkerKind::ShellTip => tool_name == "shell" && first_line().contains(self.marker()),
            // The graph tools and create_plan cannot carry a kind other than
            // their own, so a full-output scan inside their gate is safe (but
            // the first-line scoping the search tools get is not available —
            // the miss hint is a JSON field value, the rider a trailing
            // appendix).
            MarkerKind::GraphMiss => {
                matches!(
                    tool_name,
                    "graph_search" | "graph_context" | "graph_impact" | "graph_path"
                ) && output.contains(self.marker())
            }
            MarkerKind::RecallRider => tool_name == "create_plan" && output.contains(self.marker()),
            MarkerKind::LiteralTip => {
                matches!(tool_name, "search" | "search_read")
                    && first_line().contains(self.marker())
            }
            MarkerKind::KnownMemoryHit => {
                matches!(tool_name, "search" | "search_read")
                    && first_line().contains(self.marker())
            }
            // Dispatch-owned (no tool emits it) and appended AFTER the
            // result, so full-output detection applies; the phrasing is
            // specific to the dispatcher.
            MarkerKind::ConsolidationDue => output.contains(self.marker()),
            MarkerKind::ShellRedirect => {
                tool_name == "shell" && first_line().contains(self.marker())
            }
            // The drift-class edit failure's error text IS the whole output
            // (one line), so first-line scoping applies — a read tool's file
            // content echoing the phrase can never fire it.
            MarkerKind::EditStaleRead => {
                tool_name == "file_edit" && first_line().contains(self.marker())
            }
        }
    }
}

/// The search-nudge marker substring, hoisted to a named const so
/// [`MarkerKind::marker`] and [`MarkerKind::detect`] read the same string
/// (part of the public tool-output contract — changing the emitted text
/// requires changing this line in the same commit, or the metric silently
/// drops that kind).
const SEARCH_NUDGE_MARK: &str = "is an indexed symbol";

/// The file_edit stale-read nudge marker substring (backlog 714196da):
/// drift-class edit failures ("old_string not found", line-range past EOF,
/// empty file) carry the fresh-read instruction in their error text. The
/// marker is emitted by `file_edit` (see its `with_fresh_read_nudge`) and
/// detected by [`MarkerKind::EditStaleRead`]. Part of the public tool-output
/// contract — changing the emitted text requires changing this line in the
/// same commit.
pub(crate) const EDIT_STALE_READ_MARK: &str = "Re-read the file";

/// Whether `output` is a drift-class `file_edit` failure — the
/// [`MarkerKind::EditStaleRead`] detect rule, exposed for the dispatch
/// funnel: it records per-path drift state ([`Inner::edit_drift`], plan
/// be16ea36 step 3) when a file_edit result carries the marker. Kept beside
/// [`MarkerKind::detect`] so the scoping rule has one home.
pub(crate) fn is_edit_drift_failure(tool_name: &str, output: &str) -> bool {
    MarkerKind::EditStaleRead.detect(tool_name, output)
}

/// All marker kinds, in [`MarkerKind::index`] declaration order. Detection
/// is tool-scoped ([`MarkerKind::detect`]) and counts EVERY matching kind:
/// co-occurrence is possible by design — a result can carry its tool's own
/// marker plus a fired-only kind (the literal-engine TIP alongside the
/// known-memory-hit note on one search first line) or the dispatcher-
/// appended consolidation note (review 2026-09-17 finding 1). The array
/// order only fixes the kind ORDER (used by [`SteeringStats::snapshot`]
/// and `Inner::counts` indexing), not a matching priority.
const MARKERS: [MarkerKind; 10] = [
    MarkerKind::SearchNudge,
    MarkerKind::ShellTip,
    MarkerKind::GraphMiss,
    MarkerKind::RecallRider,
    MarkerKind::ReadNudge,
    MarkerKind::LiteralTip,
    MarkerKind::KnownMemoryHit,
    MarkerKind::ConsolidationDue,
    MarkerKind::ShellRedirect,
    MarkerKind::EditStaleRead,
];

/// `{fired, switched}` pair.
type Counts = (u64, u64);

/// C3: per-agent fired count at which an ACTIONABLE marker's escalation
/// note queues — once per (agent, kind), cancelled by any switch. Two
/// ignored fires in one session is a pattern, not noise — the second
/// repeat is already a habit, and catching it one round-trip sooner
/// (threshold 2, down from 3) stops the drift before it compounds.
const ESCALATION_THRESHOLD: u64 = 2;

/// The freshness-contract gate threshold for `file_edit` (backlog e8b39d72
/// H3, completing 714196da's stricter intent): after ANY drift-class failure
/// on a path (drift >= 1) the next edit of THAT path without an intervening
/// read or landed edit is intercepted —
/// not timeout-after-three, a machine-checkable contract. Deliberately
/// stricter than [`ESCALATION_THRESHOLD`]: that one governs the C3 advisory
/// escalation and the symbol-search gate (patience dials); the edit gate is
/// a contract — a re-read before the next edit, every time.
const EDIT_GATE_THRESHOLD: u64 = 1;

/// Interior state: per-kind counters + the per-agent "last fired" notes.
struct Inner {
    counts: [Counts; MARKERS.len()],
    last_fired: HashMap<AgentId, MarkerKind>,
    /// C3: per-agent per-kind fired counts — the escalation signal.
    agent_fired: HashMap<AgentId, [u64; MARKERS.len()]>,
    /// C3: per-agent per-kind switched counts — one switch ever cancels
    /// the escalation for that (agent, kind).
    agent_switched: HashMap<AgentId, [u64; MARKERS.len()]>,
    /// C5 telemetry (backlog e8b39d72 H4): per-agent per-kind count of gate
    /// INTERCEPTIONS — how often the C5-family gate actually refused the
    /// next attempt for this (agent, kind). Separate from `agent_fired` by
    /// design: the interception never executes the tool (so it never feeds
    /// the fired count), yet it is the strongest early emission-decay
    /// signal the Trace panel can show.
    agent_gated: HashMap<AgentId, [u64; MARKERS.len()]>,
    /// C3: (agent, kind) pairs already escalated — one-shot each.
    escalated: HashSet<(AgentId, usize)>,
    /// C3: queued escalation notes per agent, drained by the dispatch
    /// funnel via [`SteeringStats::take_escalations`].
    pending_escalations: HashMap<AgentId, Vec<String>>,
    /// The file_edit stale-read gate's per-(agent, path) drift state (plan
    /// be16ea36 step 3): how many drift-class failures each path has
    /// accumulated for this agent. Recorded directly by the dispatch funnel
    /// (`note_edit_drift` on a drift-class failure, `note_edit_success` on a
    /// landed edit, `note_file_read`/`clear_edit_drift` on reads) — NOT via
    /// the `last_fired` chain, whose consume-on-any-call semantics froze the
    /// gate whenever a non-read call interleaved between the failure and the
    /// read. Keyed by path so a fresh read of one file never lifts another
    /// file's contract.
    edit_drift: HashMap<(AgentId, PathBuf), u64>,
    /// Mutation-vehicle counters (plan be16ea36 step 9): file mutations via
    /// the file tools (successful file_edit/file_write/file_append calls)
    /// vs shell commands matching a mutation pattern — the Trace panel's
    /// file-tools-first observability.
    file_tool_mutations: u64,
    shell_mutations: u64,
}

/// Per-kind counts surfaced on the wire.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MarkerCounts {
    /// Stable kebab label (`search-nudge`, `shell-tip`, `graph-miss`,
    /// `recall-rider`, `read-nudge`, `literal-tip`).
    pub marker: &'static str,
    /// How many tool results carried this marker.
    pub fired: u64,
    /// How often the next call from the same agent named a target tool.
    pub switched: u64,
    /// How many next attempts the C5-family gate actually intercepted for
    /// this kind (backlog e8b39d72 H4 — the gate refuses the attempt without
    /// executing it, so this never feeds `fired`). Zero for kinds without a
    /// gate; nonzero is the early emission-decay signal the Trace panel
    /// surfaces.
    pub gated: u64,
}

/// A snapshot of the steering counters: totals plus the per-marker
/// breakdown. Serialized to the `get_steering_stats` IPC command.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SteeringStatsSnapshot {
    /// Total nudges observed (all marker kinds).
    pub fired: u64,
    /// Total switches — a marker fired and the next call named a target.
    pub switched: u64,
    /// Per-marker breakdown (all kinds, even at zero).
    pub by_marker: Vec<MarkerCounts>,
    /// File mutations via the file tools this session (plan be16ea36 step
    /// 9): successful `file_edit`/`file_write`/`file_append` calls.
    pub file_tool_mutations: u64,
    /// File mutations attempted via the shell this session (plan be16ea36
    /// step 9): shell commands matching a mutation pattern (Set-Content,
    /// Out-File, Add-Content, `>>`, `sed -i`, `tee`, `python*`).
    pub shell_mutations: u64,
}

/// The shared steering counters (Arc-shared, interior-mutable). One process
/// wide instance — the metric exists to answer "is steering working"
/// globally, and per-agent breakdowns are not surfaced.
pub struct SteeringStats {
    inner: Mutex<Inner>,
}

impl SteeringStats {
    /// Create a fresh, zeroed counter set (tests).
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                counts: [(0, 0); MARKERS.len()],
                last_fired: HashMap::new(),
                agent_fired: HashMap::new(),
                agent_switched: HashMap::new(),
                agent_gated: HashMap::new(),
                escalated: HashSet::new(),
                pending_escalations: HashMap::new(),
                edit_drift: HashMap::new(),
                file_tool_mutations: 0,
                shell_mutations: 0,
            }),
        }
    }

    /// The process-wide shared instance, created on first use. Deliberately a
    /// static rather than an `IpcState`/factory field: the observers live
    /// inside the per-agent dispatch path, and the metric is process-scoped
    /// by design (agents are cheap to co-count).
    pub fn shared() -> &'static Arc<Self> {
        static SHARED: OnceLock<Arc<SteeringStats>> = OnceLock::new();
        SHARED.get_or_init(|| Arc::new(Self::new()))
    }

    /// Lock, recovering from poisoning — a poisoned metrics lock must never
    /// panic a tool call (this is advisory instrumentation, and the counters
    /// are a derived cache).
    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Observe one tool RESULT: detect EVERY marker carried by the emitting
    /// tool's output (`tool_name`, taken from the parsed call at the
    /// dispatch choke point) and increment each fired kind's counter.
    /// Co-occurrence is expected: a uuid-shaped regex search's first line
    /// carries both the literal-engine TIP and the known-memory-hit note,
    /// and the dispatcher-appended consolidation note rides any result
    /// (review 2026-09-17 finding 1 — first-match-wins under-counted
    /// exactly these). The per-agent "last fired" note keeps the most
    /// recently fired ACTIONABLE kind when one is present (fired-only kinds
    /// can never switch and must not evict a pending actionable note); an
    /// all-fired-only result still consumes the note. Detection is
    /// tool-scoped — see [`MarkerKind::detect`] — so a content-bearing
    /// result from a tool that emits no marker can never fire a kind
    /// spuriously.
    pub(crate) fn observe_result(&self, agent_id: AgentId, tool_name: &str, output: &str) {
        let detected: Vec<MarkerKind> = MARKERS
            .iter()
            .copied()
            .filter(|k| k.detect(tool_name, output))
            .collect();
        if detected.is_empty() {
            return;
        }
        let mut inner = self.lock();
        for kind in &detected {
            inner.counts[kind.index()].0 += 1;
        }
        // Backlog b804012f: an auto-delegated answer IS the steering acted
        // upon — the graph/memory answer arrived in-place (zero round-trips),
        // so it registers as a switch for the SearchNudge kind. Without
        // this, the C5 gate would punish delegation: two delegated answers
        // count as two "ignored" fires (the model rightly never calls the
        // graph tools — it doesn't need to) and the third symbol search
        // would be intercepted. This runs BEFORE the C3 escalation loop so
        // a delegation is never counted as an ignored fire at check time
        // (review 2026-12-28 L1).
        if detected.contains(&MarkerKind::SearchNudge)
            && output
                .lines()
                .next()
                .is_some_and(|l| l.contains("AUTO-DELEGATED"))
        {
            inner.counts[MarkerKind::SearchNudge.index()].1 += 1;
            inner
                .agent_switched
                .entry(agent_id)
                .or_insert([0; MARKERS.len()])[MarkerKind::SearchNudge.index()] += 1;
        }
        // C3: per-agent fired counts for ACTIONABLE kinds; the
        // ESCALATION_THRESHOLD-th fire with zero switches for this agent
        // queues a one-shot escalation note (drained via
        // [`SteeringStats::take_escalations`]). The phrasing deliberately
        // avoids every marker substring so the note itself never counts.
        for &kind in &detected {
            if kind.targets().is_empty() {
                continue;
            }
            let fired = {
                let per_agent = inner.agent_fired.entry(agent_id).or_insert([0; MARKERS.len()]);
                per_agent[kind.index()] += 1;
                per_agent[kind.index()]
            };
            let switched = inner
                .agent_switched
                .get(&agent_id)
                .map(|s| s[kind.index()])
                .unwrap_or(0);
            if fired >= ESCALATION_THRESHOLD
                && switched == 0
                && inner.escalated.insert((agent_id, kind.index()))
            {
                inner
                    .pending_escalations
                    .entry(agent_id)
                    .or_default()
                    .push(format!(
                        "NOTE: the '{label}' marker has fired {fired} times this session \
                         without a switch — follow it (its named target tools) on the next \
                         call; each repeat wastes a round-trip the target tools would answer \
                         directly",
                        label = kind.label(),
                    ));
            }
        }
        match detected.iter().rev().find(|k| !k.targets().is_empty()) {
            Some(kind) => {
                inner.last_fired.insert(agent_id, *kind);
            }
            None => {
                inner
                    .last_fired
                    .insert(agent_id, *detected.last().expect("detected is non-empty"));
            }
        }
    }

    /// Observe one tool CALL: when the agent's last fired marker (if any) is
    /// still pending and this call names one of its target tools, the nudge
    /// switched the agent's choice. The note is consumed either way — only
    /// the immediately-following call counts.
    pub(crate) fn observe_call(&self, agent_id: AgentId, tool: &str) {
        let mut inner = self.lock();
        let Some(kind) = inner.last_fired.remove(&agent_id) else {
            return;
        };
        if kind.targets().contains(&tool) {
            inner.counts[kind.index()].1 += 1;
            // C3: a switch cancels that (agent, kind)'s escalation for the
            // rest of the session.
            inner
            .agent_switched
            .entry(agent_id)
            .or_insert([0; MARKERS.len()])[kind.index()] += 1;
        }
    }

    /// C3: drain and return this agent's queued escalation notes — the
    /// dispatch funnel appends them to the successful result they were
    /// queued on. Empty when none are pending; a drain is final for the
    /// queued notes (the (agent, kind) stays escalated, one-shot).
    pub(crate) fn take_escalations(&self, agent_id: AgentId) -> Vec<String> {
        let mut inner = self.lock();
        inner
            .pending_escalations
            .remove(&agent_id)
            .unwrap_or_default()
    }

    /// C5: whether the symbol-lookup redirect gate should fire for this agent
    /// — the SearchNudge marker has fired >= ESCALATION_THRESHOLD times with
    /// zero switches. Returns the fired count when the gate should fire (for
    /// the redirect message), None otherwise. Read-only query on the existing
    /// C3 per-agent fired/switched state — the gate is the C3 escalation's
    /// teeth: after the advisory NOTE is ignored, the next symbol-shaped
    /// search is intercepted (not just nudged). The gate lifts the moment the
    /// agent switches to a graph tool once (agent_switched > 0 cancels
    /// escalation, same condition as the one-shot C3 NOTE).
    pub(crate) fn should_gate_symbol_search(&self, agent_id: AgentId) -> Option<u64> {
        let inner = self.lock();
        let idx = MarkerKind::SearchNudge.index();
        let fired = inner
            .agent_fired
            .get(&agent_id)
            .map(|f| f[idx])
            .unwrap_or(0);
        let switched = inner
            .agent_switched
            .get(&agent_id)
            .map(|s| s[idx])
            .unwrap_or(0);
        (fired >= ESCALATION_THRESHOLD && switched == 0).then_some(fired)
    }

    /// C5-family (backlog 714196da, threshold tightened by e8b39d72 H3): the
    /// file_edit stale-read gate — the drift count when path `path` has
    /// accumulated >= EDIT_GATE_THRESHOLD (1) drift-class failures for this
    /// agent without an intervening fresh read or landed edit, None
    /// otherwise. Per-(agent, path) state ([`Inner::edit_drift`], plan
    /// be16ea36 step 3): the dispatch funnel records the failures, the
    /// reads, and the landed edits directly, so the gate is immune to the
    /// `last_fired` chain's consume-on-any-call semantics (the session-long
    /// freeze: one interleaved non-read call between the failure and the
    /// read ate the pending marker and every later edit stayed intercepted).
    /// A fresh read of THAT path — or any successful edit on it — lifts the
    /// gate; other paths' contracts are untouched.
    pub(crate) fn should_gate_file_edit(&self, agent_id: AgentId, path: &str) -> Option<u64> {
        let inner = self.lock();
        let fired = *inner.edit_drift.get(&(agent_id, PathBuf::from(path)))?;
        (fired >= EDIT_GATE_THRESHOLD).then_some(fired)
    }

    /// Record a drift-class `file_edit` failure on `path` (the dispatch
    /// funnel, on a result carrying the [`MarkerKind::EditStaleRead`] marker
    /// — see [`is_edit_drift_failure`]). Arms
    /// [`Self::should_gate_file_edit`] for (agent, path) at
    /// [`EDIT_GATE_THRESHOLD`].
    pub(crate) fn note_edit_drift(&self, agent_id: AgentId, path: &str) {
        let mut inner = self.lock();
        *inner
            .edit_drift
            .entry((agent_id, PathBuf::from(path)))
            .or_insert(0) += 1;
    }

    /// Record a SUCCESSFUL `file_edit` on `path` (the dispatch funnel): the
    /// edit landed, so the path's drift state resets — the next edit must
    /// not be intercepted when nothing has failed since (plan be16ea36
    /// step 3).
    pub(crate) fn note_edit_success(&self, agent_id: AgentId, path: &str) {
        let mut inner = self.lock();
        inner.edit_drift.remove(&(agent_id, PathBuf::from(path)));
    }

    /// Record a fresh read of `paths` (the dispatch funnel, for `read_files`
    /// — the paths come from the call's args): each path's drift state
    /// clears, lifting that path's freshness contract (plan be16ea36
    /// step 3).
    pub(crate) fn note_file_read(&self, agent_id: AgentId, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let mut inner = self.lock();
        for path in paths {
            inner.edit_drift.remove(&(agent_id, PathBuf::from(path)));
        }
    }

    /// Clear ALL of this agent's per-path drift state (the dispatch funnel,
    /// for `search_read`: its matched paths are not in the call's args, and
    /// the result is a broad current-content survey — the whole freshness
    /// contract lifts, matching the old read-tools-count-as-switch behavior;
    /// plan be16ea36 step 3).
    pub(crate) fn clear_edit_drift(&self, agent_id: AgentId) {
        let mut inner = self.lock();
        inner.edit_drift.retain(|(agent, _), _| *agent != agent_id);
    }

    /// Clear ONE path's drift state for an agent (review L3): a successful
    /// `file_write`/`file_append` of P means the agent's knowledge of P is
    /// fresh by construction — the gate lifts for P without a read.
    pub(crate) fn clear_edit_drift_for(&self, agent_id: AgentId, path: &str) {
        self.lock()
            .edit_drift
            .remove(&(agent_id, PathBuf::from(path)));
    }

    /// A `fired`/`switched` snapshot for the IPC surface (all kinds, even at
    /// zero, so the UI can render a stable breakdown).
    pub fn snapshot(&self) -> SteeringStatsSnapshot {
        let inner = self.lock();
        let by_marker: Vec<MarkerCounts> = MARKERS
            .iter()
            .map(|&kind| MarkerCounts {
                marker: kind.label(),
                fired: inner.counts[kind.index()].0,
                switched: inner.counts[kind.index()].1,
                gated: inner
                    .agent_gated
                    .values()
                    .map(|g| g[kind.index()])
                    .sum(),
            })
            .collect();
        SteeringStatsSnapshot {
            fired: by_marker.iter().map(|m| m.fired).sum(),
            switched: by_marker.iter().map(|m| m.switched).sum(),
            by_marker,
            file_tool_mutations: inner.file_tool_mutations,
            shell_mutations: inner.shell_mutations,
        }
    }

    /// C5 telemetry (backlog e8b39d72 H4): record that the gate actually
    /// intercepted an attempt for `agent_id` (EditStaleRead kind — the only
    /// gate-bearing marker today). Called from the dispatch funnel's
    /// `file_edit_redirect` when it returns an interception, so the Trace
    /// panel can show how often the freshness contract fired. The
    /// interception itself still never feeds the fired count.
    pub(crate) fn note_edit_gated(&self, agent_id: AgentId) {
        let mut inner = self.lock();
        let idx = MarkerKind::EditStaleRead.index();
        *inner
            .agent_gated
            .entry(agent_id)
            .or_insert([0; MARKERS.len()])
            .get_mut(idx)
            .expect("marker index in bounds") += 1;
    }

    /// Mutation observability (plan be16ea36 step 9): record one file
    /// mutation via the file tools — a successful `file_edit`/`file_write`/
    /// `file_append` call.
    pub(crate) fn note_file_tool_mutation(&self) {
        self.lock().file_tool_mutations += 1;
    }

    /// Mutation observability (plan be16ea36 step 9): record one shell
    /// command matching a file-mutation pattern (Set-Content, Out-File,
    /// Add-Content, `>>`, `sed -i`, `tee`, any `python*` invocation).
    pub(crate) fn note_shell_mutation(&self) {
        self.lock().shell_mutations += 1;
    }
}

impl Default for SteeringStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The detection matrix: each kind with its owning tool, a wrong tool
    /// for the cross-tool echo check, and the minimal marker output.
    const MATRIX: [(&str, MarkerKind, &str, &str); 7] = [
        (
            "search",
            MarkerKind::SearchNudge,
            "read_files",
            "'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") gives its \
             definition + callers in one call",
        ),
        (
            "shell",
            MarkerKind::ShellTip,
            "read_files",
            "TIP: for file-content search use the `search` tool\nsome stdout\n[exit code: 0]",
        ),
        (
            "graph_search",
            MarkerKind::GraphMiss,
            "read_files",
            "{\n  \"query\": \"zzz\",\n  \"hint\": \"No symbols matched 'zzz'.\"\n}",
        ),
        (
            "create_plan",
            MarkerKind::RecallRider,
            "spawn_agent",
            "# Plan: x\n\nRECALLED CONTEXT (1 hit(s)) — prior knowledge matched this plan",
        ),
        (
            "read_files",
            MarkerKind::ReadNudge,
            "search",
            "SYMBOL NUDGE: big.rs has 42 indexed symbols — graph_context(id) gives targeted \
             edges (definition/callers/blast radius); read line slices for surrounding detail\n\nfile body",
        ),
        (
            "search",
            MarkerKind::LiteralTip,
            "read_files",
            "TIP: pattern has no regex metacharacters — literal:true would use the \
             content-index engine (one indexed lookup instead of a tree walk)\n\na.txt:1: some phrase",
        ),
        (
            "shell",
            MarkerKind::ShellRedirect,
            "read_files",
            "TIP: output redirection detected — failure details may be hidden; the tool \
             caps output itself, prefer running the command unredirected.\nsome stdout\n[exit code: 0]",
        ),
    ];

    /// A marker's `fired`/`switched` counts from a snapshot.
    fn count_of(snap: &SteeringStatsSnapshot, label: &str) -> (u64, u64) {
        snap.by_marker
            .iter()
            .find(|m| m.marker == label)
            .map(|m| (m.fired, m.switched))
            .unwrap_or((u64::MAX, u64::MAX))
    }

    #[test]
    fn repeat_ignored_actionable_nudges_escalate_once() {
        // C3: an actionable marker firing for the 2nd time for the same
        // agent with zero switches queues exactly one escalation note;
        // an intervening switch cancels escalation for that agent+kind;
        // fired-only kinds never escalate.
        let nudge_out = "note: 'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") …\n\na.rs:1: hello";
        let stats = SteeringStats::new();
        stats.observe_result(1, "search", nudge_out);
        assert!(
            stats.take_escalations(1).is_empty(),
            "one fire: below threshold"
        );
        stats.observe_result(1, "search", nudge_out);
        let notes = stats.take_escalations(1);
        assert_eq!(notes.len(), 1, "second fire without a switch escalates");
        assert!(notes[0].contains("'search-nudge' marker"), "{}", notes[0]);
        assert!(notes[0].contains("2 times"), "{}", notes[0]);
        // One-shot: the third fire queues nothing more.
        stats.observe_result(1, "search", nudge_out);
        assert!(
            stats.take_escalations(1).is_empty(),
            "one-shot per agent+kind"
        );

        // A switch between fires cancels the escalation for that agent.
        let stats = SteeringStats::new();
        stats.observe_result(2, "search", nudge_out);
        stats.observe_call(2, "graph_context"); // the switch
        stats.observe_result(2, "search", nudge_out);
        assert!(
            stats.take_escalations(2).is_empty(),
            "a switch cancels the escalation"
        );

        // Fired-only kinds never escalate (no targets to switch to).
        let stats = SteeringStats::new();
        let tip_out = "TIP: pattern has no regex metacharacters — literal:true would use the \
                       content-index engine (one indexed lookup instead of a tree walk)\n\na.txt:1: x";
        for _ in 0..4 {
            stats.observe_result(3, "search", tip_out);
        }
        assert!(
            stats.take_escalations(3).is_empty(),
            "fired-only kinds never escalate"
        );
    }

    #[test]
    fn should_gate_symbol_search_fires_after_threshold_ignored_nudges() {
        // C5: the gate fires when SearchNudge has fired >= ESCALATION_THRESHOLD
        // (2) times for the same agent with zero switches — the advisory C3
        // NOTE was ignored, so the next symbol-shaped search is intercepted.
        let nudge_out = "'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") …";
        let stats = SteeringStats::new();
        // 0 fires → no gate.
        assert_eq!(stats.should_gate_symbol_search(1), None);
        // 1 fire → below threshold.
        stats.observe_result(1, "search", nudge_out);
        assert_eq!(stats.should_gate_symbol_search(1), None);
        // 2 fires, 0 switches → gate fires, returns the fired count.
        stats.observe_result(1, "search", nudge_out);
        assert_eq!(stats.should_gate_symbol_search(1), Some(2));
    }

    #[test]
    fn should_gate_symbol_search_lifts_after_a_switch() {
        // The gate lifts the moment the agent switches to a graph tool once
        // (agent_switched > 0 cancels escalation — existing C3 semantics).
        let nudge_out = "'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") …";
        let stats = SteeringStats::new();
        stats.observe_result(1, "search", nudge_out);
        stats.observe_result(1, "search", nudge_out);
        assert_eq!(stats.should_gate_symbol_search(1), Some(2));
        // Agent switches to graph_context → switch counted → gate lifts.
        stats.observe_call(1, "graph_context");
        assert_eq!(stats.should_gate_symbol_search(1), None);
    }

    #[test]
    fn should_gate_symbol_search_is_per_agent() {
        // The gate is per-agent: agent 1's ignored nudges don't gate agent 2.
        let nudge_out = "'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") …";
        let stats = SteeringStats::new();
        stats.observe_result(1, "search", nudge_out);
        stats.observe_result(1, "search", nudge_out);
        assert_eq!(stats.should_gate_symbol_search(1), Some(2));
        assert_eq!(stats.should_gate_symbol_search(2), None);
    }

    #[test]
    fn delegated_answer_registers_as_a_switch_and_never_gates() {
        // Backlog b804012f: an auto-delegated answer carries the SearchNudge
        // marker (metric coherence) but is NOT an ignored nudge — the graph
        // answer arrived in-place, so it registers as a switch. Two
        // delegations must never arm the C5 intercept gate.
        let delegated_out = "AUTO-DELEGATED to the code graph — 'hello' is an indexed symbol \
                             (re-issue this exact search to get the plain file search instead):";
        let stats = SteeringStats::new();
        stats.observe_result(1, "search", delegated_out);
        stats.observe_result(1, "search", delegated_out);
        // Two fires, but both were fulfilled in-place → the gate stays down.
        assert_eq!(stats.should_gate_symbol_search(1), None);
        // A third delegation keeps the gate down (switched > 0 for good).
        stats.observe_result(1, "search", delegated_out);
        assert_eq!(stats.should_gate_symbol_search(1), None);

        // L1 (review 2026-12-28): the auto-switch runs BEFORE the C3
        // escalation check — an ignored advisory nudge followed by a
        // delegated answer must not queue the C3 note (the delegation IS
        // the switch).
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "search",
            "'x' is an indexed symbol — graph_context(id=\"a.rs::x::1\") …",
        );
        stats.observe_result(1, "search", delegated_out);
        assert!(
            stats.take_escalations(1).is_empty(),
            "a delegation never counts as an ignored fire"
        );
    }

    // C5-family (backlog 714196da): the file_edit stale-read gate —
    // EditStaleRead fires on a failed file_edit's error text (tool-scoped:
    // read tools echoing the text never fire it), the read tools count as
    // switches, and the gate arms after ESCALATION_THRESHOLD fires with zero
    // switches, lifting the moment a read happens.
    const EDIT_STALE_ERR: &str =
        "old_string not found in file — the content has drifted from your last read. \
         Re-read the file (read_files, this exact path) and retry with the exact current \
         text; do not edit without a fresh read.";

    #[test]
    fn edit_stale_read_fires_on_failed_file_edit_only() {
        let stats = SteeringStats::new();
        // Tool-scoped: a read tool's output echoing the marker never fires it.
        stats.observe_result(1, "read_files", EDIT_STALE_ERR);
        assert_eq!(count_of(&stats.snapshot(), "edit-stale-read").0, 0);
        // The failed file_edit's error text fires it.
        stats.observe_result(1, "file_edit", EDIT_STALE_ERR);
        assert_eq!(count_of(&stats.snapshot(), "edit-stale-read").0, 1);
    }

    #[test]
    fn should_gate_file_edit_arms_after_threshold_and_lifts_after_a_read() {
        let stats = SteeringStats::new();
        assert_eq!(stats.should_gate_file_edit(1, "a.txt"), None);
        // observe_result only feeds the global telemetry — the per-path drift
        // state is recorded by the dispatch funnel (note_edit_drift).
        stats.observe_result(1, "file_edit", EDIT_STALE_ERR);
        assert_eq!(stats.should_gate_file_edit(1, "a.txt"), None);
        stats.note_edit_drift(1, "a.txt");
        // Freshness contract (backlog e8b39d72 H3): a SINGLE drift failure
        // arms the gate — the next edit of THAT path without an intervening
        // read or landed edit is intercepted.
        assert_eq!(stats.should_gate_file_edit(1, "a.txt"), Some(1));
        // A fresh read of the drifted path (what the funnel records for
        // read_files) lifts the gate...
        stats.note_file_read(1, &["a.txt".to_string()]);
        assert_eq!(stats.should_gate_file_edit(1, "a.txt"), None);
        // ...and only for THAT path: another drifted path stays armed.
        stats.note_edit_drift(1, "b.txt");
        stats.note_file_read(1, &["a.txt".to_string()]);
        assert_eq!(stats.should_gate_file_edit(1, "b.txt"), Some(1));
    }

    #[test]
    fn should_gate_file_edit_is_per_agent_and_per_path() {
        let stats = SteeringStats::new();
        stats.note_edit_drift(1, "a.txt");
        assert_eq!(stats.should_gate_file_edit(1, "a.txt"), Some(1));
        // A different agent's edit of the same path is not intercepted...
        assert_eq!(stats.should_gate_file_edit(2, "a.txt"), None);
        // ...and the same agent's edit of a different path is not either.
        assert_eq!(stats.should_gate_file_edit(1, "b.txt"), None);
    }

    // ---- Regression (plan be16ea36 step 3): the stale-read gate freeze ----
    // The gate no longer rides the `last_fired` chain (whose consume-on-any-
    // call semantics let one interleaved non-read call eat the pending
    // marker, so the read never registered and the gate stayed armed for the
    // rest of the session) — the funnel records the drift and the read
    // directly.

    /// The freeze regression: drift failure → one interleaved non-read call
    /// → the fresh read. The interleaved call must be irrelevant: the gate
    /// lifts the moment the funnel records the fresh read of the path.
    #[test]
    fn edit_gate_read_lifts_even_after_an_interleaved_non_read_call() {
        let stats = SteeringStats::new();
        // What the funnel does on a drift-class failure: telemetry + drift.
        stats.observe_result(1, "file_edit", EDIT_STALE_ERR);
        stats.note_edit_drift(1, "a.txt");
        stats.observe_call(1, "shell"); // any interleaved non-read call
        // What the funnel does on the fresh read: telemetry + per-path clear.
        stats.observe_call(1, "read_files");
        stats.note_file_read(1, &["a.txt".to_string()]);
        assert_eq!(
            stats.should_gate_file_edit(1, "a.txt"),
            None,
            "the fresh read must lift the gate even though a non-read call \
             interleaved between the drift failure and the read"
        );
    }

    /// Reset semantics: a SUCCESSFUL file_edit must clear the drift state —
    /// the edit landed, so there is nothing stale to guard against (the
    /// funnel records the success via note_edit_success).
    #[test]
    fn successful_file_edit_resets_the_drift_gate() {
        let stats = SteeringStats::new();
        stats.note_edit_drift(1, "a.txt");
        assert_eq!(stats.should_gate_file_edit(1, "a.txt"), Some(1));
        // A successful edit of the same path (what the funnel records on the
        // success) resets the drift state.
        stats.note_edit_success(1, "a.txt");
        assert_eq!(
            stats.should_gate_file_edit(1, "a.txt"),
            None,
            "a successful edit must reset the drift state — the next edit \
             must not be intercepted when nothing failed since"
        );
    }

    #[test]
    fn note_edit_gated_counts_interceptions_in_snapshot() {
        // H4 (backlog e8b39d72): gate interceptions surface in the snapshot —
        // per kind, isolated per agent by construction, and never feeding the
        // fired count.
        let stats = SteeringStats::new();
        stats.note_edit_gated(1);
        stats.note_edit_gated(1);
        stats.note_edit_gated(2);
        let snap = stats.snapshot();
        let edit = snap
            .by_marker
            .iter()
            .find(|m| m.marker == "edit-stale-read")
            .expect("edit marker present");
        assert_eq!(edit.gated, 3);
        assert_eq!(edit.fired, 0, "the interception never feeds the fired count");
        // Only gate-bearing kinds carry a nonzero gated count.
        let search = snap
            .by_marker
            .iter()
            .find(|m| m.marker == "search-nudge")
            .expect("search marker present");
        assert_eq!(search.gated, 0);
    }

    #[test]
    fn co_occurring_markers_each_count() {
        // Review 2026-09-17 finding 1: one result can carry MORE than one
        // kind — first-match-wins under-counted exactly these.
        // (a) A uuid-shaped REGEX-mode search first line: the literal TIP
        // joined with the known-memory note (merged_note, "; ").
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "search",
            "note: TIP: pattern has no regex metacharacters — literal:true would use the \
             content-index engine (one indexed lookup instead of a tree walk); known \
             memory hit: 'PLAN: backlog 70f5b248' — this id is a known backlog item; \
             memory_search it for detail\n\n.coding/backlog.jsonl:1: item",
        );
        let snap = stats.snapshot();
        assert_eq!(count_of(&snap, "literal-tip").0, 1);
        assert_eq!(count_of(&snap, "known-memory-hit").0, 1);

        // (b) The dispatcher-appended consolidation note riding a result
        // that ALSO carries the tool's own actionable marker: both count,
        // and the pending note stays the ACTIONABLE kind (search-nudge) —
        // the next graph call still measures the switch.
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "search",
            "note: 'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") …\n\n\
             a.rs:1: hello\nNOTE: 15 working-memory events accumulated this session — \
             consider memory_consolidate(session_id) to distill them",
        );
        let snap = stats.snapshot();
        assert_eq!(count_of(&snap, "search-nudge").0, 1);
        assert_eq!(count_of(&snap, "consolidation-due").0, 1);
        stats.observe_call(1, "graph_context");
        let snap = stats.snapshot();
        assert_eq!(
            count_of(&snap, "search-nudge").1,
            1,
            "the actionable note survived"
        );
    }

    #[test]
    fn each_kind_fires_only_from_its_own_tool() {
        // Matrix: the kind's own tool + marker text fires exactly that kind;
        // the SAME text from a different tool fires nothing (a
        // content-bearing payload quoting a marker can no longer misfire —
        // review 2026-09-14 finding 3's real fix); another kind's marker
        // text on this kind's tool fires nothing either (one tool emits at
        // most its own kind).
        for (tool, kind, wrong_tool, output) in MATRIX {
            let stats = SteeringStats::new();
            stats.observe_result(1, tool, output);
            let snap = stats.snapshot();
            for m in &snap.by_marker {
                if m.marker == kind.label() {
                    assert_eq!(m.fired, 1, "{}: fired for own tool {}", m.marker, tool);
                } else {
                    assert_eq!(m.fired, 0, "{}: must not fire alongside", m.marker);
                }
            }
            assert_eq!(snap.fired, 1);
            let stats = SteeringStats::new();
            stats.observe_result(1, wrong_tool, output);
            assert_eq!(
                stats.snapshot().fired,
                0,
                "{} echoed on {} must not fire",
                kind.label(),
                wrong_tool
            );
            // A kind whose detect() is gated OFF this kind's tool: its
            // marker text observed on `tool` must fire nothing. Kinds that
            // share the tool set (SearchNudge and LiteralTip both live on
            // search/search_read) are skipped here — their text
            // legitimately fires their own kind on this tool; the
            // guarantee that those two never co-occur on ONE result comes
            // from the emitters (the TIP is suppressed when the nudge
            // fires — pinned in search.rs's
            // symbol_nudge_suppresses_the_literal_tip).
            let (other_tool, _, _, other_output) = MATRIX
                .iter()
                .find(|(_, k2, _, out)| *k2 != kind && !k2.detect(tool, out))
                .expect("six kinds across more than one tool");
            let stats = SteeringStats::new();
            stats.observe_result(1, tool, other_output);
            assert_eq!(
                stats.snapshot().fired,
                0,
                "foreign marker text on {} (owning {}) must not fire",
                tool,
                other_tool
            );
        }
        // Plain non-marker output never fires anything.
        let stats = SteeringStats::new();
        stats.observe_result(1, "file_write", "plain tool result with no advisory line");
        assert_eq!(stats.snapshot().fired, 0);
    }

    #[test]
    fn search_nudge_is_first_line_scoped() {
        // The note is prepended (with_note/merged_note — always line 1), so
        // a marker echo DEEPER in a search result (e.g. a matched line
        // quoting this repo's own steering docs) must not fire.
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "search",
            "2 matches in 1 file (engine: index)\n\nsrc/spec.md:7: the string \"is an \
             indexed symbol\" appears in the steering docs",
        );
        assert_eq!(stats.snapshot().fired, 0);
        // Line-1 placement (how the emitters actually shape the output).
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "search",
            "'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") gives its \
             definition + callers in one call\n\nsrc/x.rs:40: hello\nsrc/x.rs:71: hello",
        );
        let snap = stats.snapshot();
        assert_eq!(
            snap.by_marker[MarkerKind::SearchNudge.index()].fired,
            1,
            "line-1 nudge fires"
        );
        assert_eq!(snap.fired, 1);
    }

    #[test]
    fn shell_tip_is_first_line_scoped() {
        // GREP_NUDGE is insert_str(0, …)-prepended; a shell result whose
        // stdout itself echoes the TIP on a later line must not fire.
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "shell",
            "grep done\nsrc/docs.md: the TIP: for file-content search string quoted from a file\n[exit code: 0]",
        );
        assert_eq!(stats.snapshot().fired, 0);
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "shell",
            "TIP: for file-content search use the `search` tool (sandboxed, content-indexed)\nmatch line\n[exit code: 1]",
        );
        let snap = stats.snapshot();
        assert_eq!(
            snap.by_marker[MarkerKind::ShellTip.index()].fired,
            1,
            "line-1 prepended TIP fires"
        );
        assert_eq!(snap.fired, 1);
    }

    #[test]
    fn read_nudge_is_prefix_scoped() {
        // The read_files note is the single leading line (single-prefix
        // join, read_files.rs) — the marker quoted deeper in a file body
        // must not fire.
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "read_files",
            "file body\n\nSYMBOL NUDGE: quoted in a doc comment",
        );
        assert_eq!(stats.snapshot().fired, 0);
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "read_files",
            "SYMBOL NUDGE: big.rs has 42 indexed symbols — graph_context(id) gives targeted \
             edges; read line slices for surrounding detail\n\nfile body",
        );
        let snap = stats.snapshot();
        assert_eq!(
            snap.by_marker[MarkerKind::ReadNudge.index()].fired,
            1,
            "leading prefix nudge fires"
        );
        assert_eq!(snap.fired, 1);
    }

    #[test]
    fn next_target_call_switches() {
        // fired → next call names a target → fired + switched both increment.
        let stats = SteeringStats::new();
        stats.observe_result(
            9,
            "search",
            "'hello' is an indexed symbol — graph_context(id=\"x\") gives its definition + \
             callers in one call",
        );
        stats.observe_call(9, "graph_context");
        let snap = stats.snapshot();
        assert_eq!(snap.fired, 1);
        assert_eq!(snap.switched, 1);
        assert_eq!(snap.by_marker[MarkerKind::SearchNudge.index()].switched, 1);
    }

    #[test]
    fn next_non_target_call_does_not_switch_and_clears() {
        // A non-target call consumes the fired note without counting a
        // switch — and a LATER target call does not retroactively switch.
        let stats = SteeringStats::new();
        stats.observe_result(
            9,
            "shell",
            "TIP: for file-content search use the index engine",
        );
        stats.observe_call(9, "file_write");
        stats.observe_call(9, "search");
        let snap = stats.snapshot();
        assert_eq!(snap.fired, 1);
        assert_eq!(snap.switched, 0);
    }

    #[test]
    fn agents_are_counted_independently() {
        // Agent 1's fired note must not make agent 2's next call a switch,
        // and a non-target call consumes the note without a switch.
        let stats = SteeringStats::new();
        stats.observe_result(
            1,
            "create_plan",
            "RECALLED CONTEXT (1 hit(s)) — prior knowledge",
        );
        stats.observe_call(2, "graph_context");
        assert_eq!(stats.snapshot().switched, 0);
        stats.observe_call(1, "file_write");
        assert_eq!(stats.snapshot().switched, 0);
        // A fresh marker on agent 3 DOES switch on its target call.
        stats.observe_result(
            3,
            "read_files",
            "SYMBOL NUDGE: big.rs has 42 indexed symbols",
        );
        stats.observe_call(3, "graph_impact");
        let snap = stats.snapshot();
        assert_eq!(snap.by_marker[MarkerKind::ReadNudge.index()].switched, 1);
    }

    #[test]
    fn recall_rider_fires_but_never_switches() {
        // The rider is surfaced-only (no targets): no next call can count.
        let stats = SteeringStats::new();
        stats.observe_result(
            4,
            "create_plan",
            "RECALLED CONTEXT (2 hit(s)) — prior knowledge",
        );
        stats.observe_call(4, "memory_search");
        let snap = stats.snapshot();
        assert_eq!(snap.fired, 1);
        assert_eq!(snap.switched, 0);
    }

    #[test]
    fn literal_tip_fires_but_never_switches() {
        // Fired-only: observe_call sees tool NAMES, and a re-call of
        // `search` with literal:true is indistinguishable from any other
        // search — no switch is claimed for this kind.
        let stats = SteeringStats::new();
        stats.observe_result(
            5,
            "search",
            "TIP: pattern has no regex metacharacters — literal:true would use the \
             content-index engine (one indexed lookup instead of a tree walk)\n\na.txt:1: x",
        );
        stats.observe_call(5, "search");
        stats.observe_call(5, "search_read");
        let snap = stats.snapshot();
        assert_eq!(snap.fired, 1);
        assert_eq!(snap.switched, 0);
        assert_eq!(snap.by_marker[MarkerKind::LiteralTip.index()].fired, 1);
    }
}
