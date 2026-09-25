// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The `AgentLoop` struct, its constructors, and its public handle accessors.
//!
//! The loop's *behavior* lives in sibling modules: [`super::turn`] (the
//! `run_turn` driver) and [`super::dispatch`] (tool-call dispatch, the
//! approval gate, and provider-retry). This module owns only the loop's
//! state (13 fields) and the small public surface callers use to build,
//! configure, and introspect it.

use std::sync::{Arc, RwLock};

use super::failure_triage;
use crate::config::SafetyMode;
use crate::error::Result;
use crate::memory::MemoryStoreTrait;
use crate::provider::{FinishReason, LlmClient};
use crate::safety_rules::SafetyRules;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::ToolRegistry;

/// Per-session state extracted from [`AgentLoop`] (A2: reduce the loop's field
/// count). Holds the memory session id + the prompt-token cache heuristic.
/// Both are behind `Mutex` so they can be mutated through a `&AgentLoop`.
#[derive(Debug)]
pub(crate) struct SessionState {
    /// The memory session id, set on the first turn (when a memory store is
    /// present). Exposed via [`AgentLoop::session_id`] so the IPC layer can
    /// query per-session stats without the AgentTask owning it.
    pub session_id: std::sync::Mutex<Option<String>>,
    /// The standing "project memory" primer, fetched once on the first turn
    /// (the top strongest semantic+procedural memories) and cached for the
    /// rest of the session. Injected into the stable head so the agent starts
    /// with project knowledge instead of re-discovering it each turn.
    ///
    /// Three states (so the fetch is not retried every turn when there are no
    /// distilled memories yet or the fetch errored):
    /// - `None` — never fetched (the first turn fetches).
    /// - `Some(None)` — fetched, but no primer (empty store / fetch error);
    ///   do not refetch.
    /// - `Some(Some(s))` — fetched, with primer string `s`.
    pub primer: std::sync::Mutex<Option<Option<String>>>,
    /// The plan id for which a "Completed plan" episodic memory has already
    /// been captured this session, so the Complete hook does not double-write
    /// when the workflow re-enters Complete for the same retained plan (e.g.
    /// a `merge_to_main` skill that starts and ends in Complete). `None` until
    /// the first capture.
    pub captured_complete_plan_id: std::sync::Mutex<Option<String>>,
}

impl SessionState {
    /// A fresh session state (no session yet, no primer fetched, no Complete
    /// capture yet).
    fn new() -> Self {
        Self {
            session_id: std::sync::Mutex::new(None),
            primer: std::sync::Mutex::new(None),
            captured_complete_plan_id: std::sync::Mutex::new(None),
        }
    }
}

/// The agent loop. Drives a conversation turn.
/// A deferred provider swap: stored when the user switches to a model with a
/// smaller context window than the current one. The turn loop completes the
/// swap after summarizing (if needed) using the OLD provider, so the
/// conversation fits the new (smaller) window.
pub struct PendingSwap {
    /// The new provider to swap to.
    pub provider: Arc<dyn LlmClient>,
    /// The new context manager (built from the new provider's caps).
    pub context_manager: super::context::ContextManager,
    /// The new model name (for the user-facing note emitted after the swap).
    pub model: String,
}

/// The result of a successful 429 fallback: the turn layer switched from one
/// endpoint to another (both serving the same model id) after a rate-limit.
/// Carries the from/to endpoint names + the model so the switching note is
/// informative.
pub(crate) struct FallbackInfo {
    /// The endpoint that returned the 429 (rate-limited / out of quota).
    pub from_endpoint: String,
    /// The alternate endpoint the agent switched to.
    pub to_endpoint: String,
    /// The model id (served by both endpoints).
    pub model: String,
}

pub struct AgentLoop {
    /// The LLM provider. Behind an `RwLock` so the model can be swapped at
    /// runtime (e.g. the status-bar model picker) without rebuilding the loop.
    /// Read once per turn into a local snapshot that serves as the turn's
    /// *default* (fallback) provider — a runtime swap takes effect on the next
    /// turn. A per-context resolver override may still switch the provider
    /// *between* requests within a turn (see
    /// [`resolve_turn_provider`](Self::resolve_turn_provider)); a single
    /// request never switches mid-flight.
    pub(crate) provider: Arc<RwLock<Arc<dyn LlmClient>>>,
    /// The context manager (token counting + summarization threshold). Swapped
    /// alongside the provider so the new model's context window takes effect.
    pub(crate) context_manager: Arc<RwLock<super::context::ContextManager>>,
    /// The live token count of the most recent request iteration — recorded
    /// at the top of every iteration in `run_turn` right after the
    /// token-accounting update, so [`try_429_fallback`](Self::try_429_fallback)
    /// can compare an alternate endpoint's window against the LIVE
    /// conversation size instead of the current endpoint's window. The
    /// basis is the harness's canonical live count (the same one the
    /// summarize trigger uses): messages + tools-schema overhead, EXCLUDING
    /// the per-request system scaffolding (the stable head on the first
    /// iteration, the volatile tail + CONTEXT_FOOTER on every iteration) —
    /// the viability headroom absorbs that scaffolding and the alternate's
    /// own preflight/compaction is the backstop. `0` = no count recorded
    /// yet (no request has been built for this loop).
    pub(crate) live_token_count: std::sync::atomic::AtomicUsize,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) workflow: Arc<tokio::sync::Mutex<crate::workflow::Workflow>>,
    /// The path sandbox — used by the approval gate to determine whether a
    /// tool call is project-scoped (for `AutoApproveProject` mode). Each
    /// agent shares the same sandbox (it's immutable after construction).
    pub(crate) sandbox: Arc<Sandbox>,
    /// The constitution. When a `ConstitutionSource` is available (production),
    /// it is re-read from disk each turn if the `agent.md` files' mtimes
    /// changed — so editing the constitution takes effect without restarting.
    /// The static fallback is used by tests that pass `Constitution::default()`.
    pub(crate) constitution: ConstitutionHolder,
    /// Safety mode is shared + mutable so the UI can toggle it at runtime
    /// (e.g. the "auto-approve" toolbar button). Read on every tool call,
    /// written rarely by a Tauri command.
    pub(crate) safety_mode: Arc<RwLock<SafetyMode>>,
    /// The memory store — used for automatic recall (injecting relevant
    /// memories into the system prompt) and working-memory capture (recording
    /// tool events). `None` in tests that don't exercise memory.
    pub(crate) memory: Option<Arc<dyn MemoryStoreTrait>>,
    /// Safety rules — regex-based auto-approve. When a tool call matches a
    /// rule in `.coding/safety.toml`, the approval prompt is skipped. `None`
    /// when no safety-rules file is configured (tests, or a project without
    /// `.coding/safety.toml`).
    pub(crate) safety_rules: Option<Arc<SafetyRules>>,
    /// An optional vision model for image-to-text when the main LLM is not
    /// multimodal. `None` when no vision model is configured (or when the main
    /// provider is already multimodal). Used by [`describe_image`](Self::describe_image).
    /// Behind a trait so tests can mock it without a live connection.
    pub(crate) vision: Option<Arc<dyn crate::provider::vision::ImageDescriber>>,
    /// Per-session state (session id + prompt-token cache heuristic).
    /// Extracted from the flat fields (A2).
    pub(crate) session: SessionState,
    /// This agent's id in the runtime (`AgentManager`), used so the agent's
    /// `spawn_agent` tool can record itself as the parent of any background
    /// agent it spawns (for the completion-notification feedback loop).
    /// `None` until the factory assigns it at build time (and for tests that
    /// construct a loop directly without an id).
    pub(crate) agent_id: std::sync::Mutex<Option<crate::runtime::AgentId>>,
    /// Whether plan-mutation tools are allowed on this agent's workflow.
    /// False for sub-agents spawned via the tool (Phase 4 policy). Used both
    /// to filter schemas (so the model is not offered the tools) and as a
    /// hard gate in dispatch.
    pub(crate) plan_mutations_allowed: std::sync::Mutex<bool>,
    /// An optional per-context model resolver. When present, the turn driver
    /// consults it at turn start to pick a model for *this turn* based on the
    /// workflow state + active skill + whether this is a subagent — overriding
    /// the default provider. `None` in tests / when no `[models]` section is
    /// configured (the default provider is always used).
    pub(crate) model_resolver: Option<Arc<dyn crate::model_resolver::ModelResolver>>,
    /// Whether this agent is a subagent (spawned via `spawn_agent` or the UI
    /// spawn button with a parent). Drives the `[models.subagent]` override
    /// in the resolver. `false` for the main agent. Behind a mutex so it can
    /// be set through a `&AgentLoop` reference (mirroring
    /// `plan_mutations_allowed`).
    pub(crate) is_subagent: std::sync::Mutex<bool>,
    /// Failed-reviewer protocol latch: set by the event forwarder when a
    /// `role: "reviewer"` child of this agent finishes with a final error and
    /// NO report (the review did not happen). While set, `spawn_agent` calls
    /// with `role: "reviewer"` are denied at dispatch — the parent must first
    /// ask the user (ask_user clears the latch) whether to retry on another
    /// model or abandon the review (the main agent can never self-review).
    /// Prevents the blind-respawn thrash loop (a failed reviewer almost
    /// always fails identically on respawn).
    pub(crate) reviewer_failure_pending: std::sync::Mutex<bool>,
    /// The failed-reviewer protocol's retry sanction (backlog c8e48f81): set
    /// when ask_user runs while `reviewer_failure_pending` was true (that
    /// ask_user IS the protocol's user consultation) — the retry it sanctions
    /// may carry the user-picked explicit `model` on the reviewer respawn.
    /// Consumed at dispatch by ANY reviewer spawn, so a lingering window
    /// authorizes at most ONE model-carrying reviewer spawn (the first after
    /// the consultation); closed by abandon_plan or an unrelated ask_user —
    /// full protocol in `reviewer_spawn_gate`'s doc (dispatch.rs).
    pub(crate) reviewer_retry_sanctioned: std::sync::Mutex<bool>,
    /// The context-manager fill rate (fraction of the context window at which
    /// summarization triggers). Stored so a per-context model override can
    /// build a correctly-sized `ContextManager` at turn time.
    pub(crate) fill_rate: f64,
    /// A forced model (endpoint + model id) the agent runs on for every turn,
    /// overriding the normal subagent/state/skill resolution chain. Set when a
    /// `spawn_agent` call passes a `model` argument. `None` for the main agent
    /// and any spawn that didn't request a specific model. Behind a mutex so
    /// it can be set through a `&AgentLoop` reference at spawn time.
    pub(crate) forced_model: std::sync::Mutex<Option<crate::config::ModelRef>>,
    /// An explicit provider pinned by the user's per-agent model picker
    /// (the `set_model` IPC command with an `agent_id` — the GUI status bar's
    /// per-agent switch). When set,
    /// [`resolve_turn_provider`](Self::resolve_turn_provider)
    /// returns it ahead of forced / subagent / workflow-state overrides —
    /// the picker is an explicit, live user choice for THIS agent, so those
    /// config slots must not silently discard it on the next turn (the
    /// 2026-08-22 bug: switching kimi → deepseek on the picker, then the next
    /// turn re-resolved the `[models.planning]` kimi override and ran kimi
    /// anyway). A configured `[models.skill.<name>]` override still beats the
    /// pin (2026-09-04): skill turns are deliberate context switches that
    /// carry their own model assignment.
    ///
    /// The provider here is the EXACT one the picker built (including the
    /// reasoning effort it resolved), so the turn uses it directly rather
    /// than rebuilding through the resolver's cache. Set only by the Tauri
    /// IPC layer's `swap_provider_into_loop`; never deleted by a later
    /// global factory swap (plain [`set_provider`](Self::set_provider) must
    /// not silently undo an explicit per-agent choice) — but see
    /// [`pinned_state`](Self::pinned_state) for where it yields.
    pub(crate) explicit_provider:
        Arc<RwLock<Option<(Arc<dyn LlmClient>, super::context::ContextManager)>>>,
    /// The workflow state the picker pin was first SERVED in — the pin's
    /// scope. Lazily stamped by [`resolve_turn_provider`](Self::resolve_turn_provider)
    /// (the pin exists the moment the picker acts; the state is recorded at
    /// first serve). The pin holds in that state; in any OTHER state it holds
    /// only while no configured `[models.*]` model resolves for the current
    /// context (2026-12-20 amendment of the 2026-08-22 "never yields" rule:
    /// on a state change the CONFIGURED model takes over, but with nothing
    /// configured the pin keeps serving instead of falling to the default
    /// model). Reset to `None` by
    /// [`set_explicit_provider`](Self::set_explicit_provider) so a fresh
    /// pick re-stamps on its first serve; the pending-swap completion path
    /// in `run_turn` writes `explicit_provider` directly and intentionally
    /// leaves the stamp alone (same pick, completing).
    pub(crate) pinned_state: std::sync::Mutex<Option<crate::workflow::WorkflowState>>,
    /// Endpoint stickiness recorded by the automatic 429 fallback
    /// ([`try_429_fallback`](Self::try_429_fallback)): model id →
    /// (the endpoint that 429'd, an alternate ModelRef serving the same model
    /// id). Consulted ONLY at provider-build time inside
    /// [`resolve_turn_provider`](Self::resolve_turn_provider), on every
    /// resolution path (skill override, the explicit picker pin, forced
    /// model, state/subagent override, and the default provider) — it reroutes a
    /// model to its alternate endpoint but never changes WHICH model the
    /// chain picks, so `[models.planning]` / `[models.executing]` /
    /// `[models.complete]` overrides keep firing after a 429 (the 2026-12-05
    /// bug: the fallback pinned itself via
    /// [`set_explicit_provider`](Self::set_explicit_provider) — the never-
    /// cleared picker pin outranks the whole resolver chain, so one 429
    /// permanently froze the agent on the fallback model). Entries store a
    /// ModelRef (not a built provider), so a rebuilt provider picks up config
    /// reloads. If the alternate endpoint itself later 429s, the next
    /// `try_429_fallback` overwrites the entry to exclude it (self-healing
    /// endpoint flip-flop, at most one fallback per turn).
    pub(crate) fallback_endpoints:
        Arc<RwLock<std::collections::HashMap<String, (String, crate::config::ModelRef)>>>,
    /// A deferred provider swap: set when the user switches to a model with a
    /// smaller context window. The turn loop takes it at the top of `run_turn`,
    /// summarizes using the OLD provider if the conversation is too large for
    /// the new window, then completes the swap. `None` when no swap is pending
    /// or the new context is >= the current one (immediate swap, no deferral).
    pub(crate) pending_swap: Arc<std::sync::Mutex<Option<PendingSwap>>>,
    /// The model id actually used for the most recent turn: the per-context
    /// override (workflow state / skill / subagent / forced) when one
    /// resolved, `None` when the turn ran on the default provider. Set by
    /// [`resolve_turn_provider`](Self::resolve_turn_provider) on every
    /// resolution path so it never goes stale; cleared by the IPC `set_model`
    /// command when the provider is swapped, so `list_agents` reports the new
    /// provider's model immediately. Read by the IPC layer to report
    /// the EFFECTIVE model in `AgentInfo` (the UI otherwise shows the default
    /// provider's model even while per-state overrides are in use).
    pub(crate) resolved_model: std::sync::Mutex<Option<String>>,
    /// The endpoint/provider NAME matching [`resolved_model`](Self::resolved_model):
    /// the `endpoints.toml` name of the endpoint that served the resolved
    /// override, `None` when the turn ran on the default provider. Mirrors
    /// the model mirror so the UI can label WHICH endpoint serves the model
    /// — first-match resolution over the endpoint list cannot disambiguate
    /// a model id listed under two endpoints (backlog 2980ca67: "model
    /// display shows the wrong provider when the name matches in two
    /// providers"). Read via [`effective_provider_name`](Self::effective_provider_name).
    pub(crate) resolved_provider: std::sync::Mutex<Option<String>>,
    /// The DISPLAY-space effective reasoning effort of the provider that
    /// served the most recent turn (`"off" | "low" | "medium" | "high" |
    /// "max"`), `None` when unknown. The effort mirror of
    /// [`resolved_model`](Self::resolved_model): set at the same resolution
    /// sites (the per-context `ModelRef`'s override, else the model's
    /// default chain — the same resolution the request builder uses, in UI
    /// vocabulary), cleared by the IPC `set_model` swap. Read by the IPC
    /// layer (`AgentInfo`) and the turn layer's `ModelChanged` emission so
    /// the status bar shows the effort of the model actually in use, never
    /// a stale toolbar echo or the endpoint-level default (backlog
    /// 51dab4da).
    pub(crate) resolved_effort: std::sync::Mutex<Option<String>>,
    /// The DISPLAY-space effort of the DEFAULT provider slot — set by the
    /// factory at construction (from the default `ModelRef`), by `set_model`
    /// swaps, and by Settings saves that rebuild the default. `None` when
    /// unknown (test loops) — the no-override resolution branch then reports
    /// no effort and the UI falls back.
    pub(crate) default_display_effort: std::sync::Mutex<Option<String>>,
    /// The DISPLAY-space effort the picker PIN was built with — set by the
    /// IPC `set_model` swap (the toolbar's requested effort, or the model's
    /// default when unset). `None` when unknown — the pin resolution branch
    /// then reports no effort and the UI falls back to the toolbar echo.
    pub(crate) pinned_display_effort: std::sync::Mutex<Option<String>>,
    /// An optional descendant tracker — the brain-side seam (like the
    /// spawner) that lets the dispatch layer check whether this agent has any
    /// running spawned subagents, so it can gate workflow-state transitions on
    /// "no descendants running" (the agent must wait for all subagents before
    /// changing state), and lets the runtime auto-continue logic
    /// (`run_turn_with_retry` in runtime/agent.rs) park a parent while its
    /// descendants run. `None` in tests / when no IPC spawner is wired.
    pub(crate) descendant_tracker: Option<Arc<dyn crate::runtime::DescendantTracker>>,
    /// The per-project code knowledge graph, wired by the factory so the
    /// dispatch layer's symbol-lookup redirect gate (C5) can check whether a
    /// `search`/`search_read` pattern names an indexed symbol before the tool
    /// runs — intercepting the call when the agent has ignored the symbol
    /// nudge >= ESCALATION_THRESHOLD times. `None` in tests / when indexing
    /// is disabled (`[general] codegraph = false`) — the gate is a no-op.
    pub(crate) graph: Option<Arc<crate::codegraph::CodeGraph>>,
    /// The path of the last review report this agent wrote via
    /// `write_review_report` (a reviewer subagent's single output channel).
    /// Captured by the turn layer on a successful `write_review_report` call
    /// so the event forwarder can include it in the completion notification
    /// sent to the parent agent — instead of a generic "read its report"
    /// message that forces the parent to search for the file (unreliable with
    /// multiple concurrent reviewers). `None` until the agent writes a report.
    /// Behind a mutex so it can be set through a `&AgentLoop` reference.
    pub(crate) last_review_report: std::sync::Mutex<Option<String>>,
    /// The project's plans dir (`.coding/plans`), wired by the factory so
    /// consolidation can digest the accumulated plan/review corpus at session
    /// end. Empty for directly-built test loops (no corpus — consolidation
    /// sees only the session's tool events, the historical behavior).
    pub(crate) plans_dir: std::path::PathBuf,
    /// The per-agent root override this loop was built with (parallel run-all
    /// worktree agents, plan ffd7a86f) — `None` for every factory-default
    /// build. Read by the IPC spawner so SUBAGENTS spawned by this agent
    /// inherit the same root (a worktree agent's reviewer must see the
    /// worktree's diff, not the main tree's).
    pub(crate) root_spec: Option<crate::agent::factory::AgentRootSpec>,
    /// The optional failure-triage gate (the Laya classifier's failure-
    /// handling consumer, `[general.laya] failure_triage`): the shared
    /// classifier slot plus the config-mirrored enable flag that the
    /// tool-dispatch and provider-retry sites read on every failure. `None`
    /// in tests / when the Laya foundation is not wired — those sites then
    /// keep their pre-classifier behavior byte-for-byte.
    pub(crate) failure_triage: Option<failure_triage::FailureTriageHandle>,
}

/// Holds either a live, mtime-checked constitution source or a static value.
#[derive(Debug)]
pub(crate) enum ConstitutionHolder {
    /// A file-backed source that re-reads on mtime change. Behind a mutex so
    /// the cache can be updated through a shared `&AgentLoop` reference.
    /// `cached_head` holds the last-built stable system-prompt head, rebuilt
    /// only when the constitution actually changes (L4).
    Source {
        source: std::sync::Mutex<crate::project::ConstitutionSource>,
        cached_head: std::sync::Mutex<Option<String>>,
    },
    /// A static value (used by tests / when no paths are known).
    Static(crate::project::Constitution),
}

impl ConstitutionHolder {
    /// Returns the stable system-prompt head, rebuilding it only when the
    /// constitution has changed since the last build (L4). The head is
    /// byte-stable across turns — it only changes when `agent.md` is edited —
    /// so caching it avoids a few-KB string concatenation every turn. For
    /// `Static` (tests), the head is rebuilt every call (tests don't need the
    /// cache and value the simplicity).
    pub(crate) fn stable_head(&self) -> String {
        match self {
            ConstitutionHolder::Source {
                source,
                cached_head,
            } => {
                let mut src = source.lock().expect("constitution source lock poisoned");
                let changed = src.reload_if_changed();
                let mut cache = cached_head.lock().expect("head cache lock poisoned");
                if changed || cache.is_none() {
                    let head = super::prompt::build_stable_head(src.constitution());
                    *cache = Some(head.clone());
                    head
                } else {
                    cache
                        .clone()
                        .expect("head cache invariant: Some after is_none check")
                }
            }
            ConstitutionHolder::Static(c) => super::prompt::build_stable_head(c),
        }
    }
}

/// Why a turn ended early — the unified "find a safe stopping point" signal.
///
/// Steer, Interrupt, Cancel, Compact, and Clear are all the same shape: the
/// user wants the agent to stop what it's doing at the next safe point and do
/// something else. The difference is only *what* happens after the stop:
///
/// - `Steer(text)` — the turn ends keeping partial output; `text` becomes a
///   regular user message that drives the next turn (mirrors the between-turn
///   steer path). The agent stays alive.
/// - `Interrupt` — the turn ends (partial output kept when it exists); the
///   agent stays alive and returns to idle, ready for the next prompt. Used by
///   the Stop button.
/// - `Cancel` — the turn ends and the agent task terminates (`Exited` fires,
///   the tab closes). Used by the subagent close-x.
/// - `Compact` — the turn ends (partial output kept); the agent stays alive
///   and compacts its context (summarizes old messages). Used by `/compact`
///   + the context-popup Compact button.
/// - `Clear` — the turn ends (partial output kept); the agent stays alive and
///   clears its conversation history entirely. Used by `/new`.
///
/// All are detected at the same safe points (streaming chunks, approval
/// / `ask_user` waits, summarization, between tool calls, and — for
/// Interrupt/Cancel — mid-tool-execution via a dropped future) and propagated
/// through `TurnOutcome.stop_reason` rather than being consumed at the
/// detection point. This is what makes the turn actually stop: the flag
/// travels back to the turn loop, which breaks and emits `Finished`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// One or more steers arrived mid-turn. The carried payloads (text +
    /// images) drive the next turn as user messages, in arrival order — a
    /// typed command is never dropped (previously only the first steer won
    /// and the rest were silently discarded).
    Steer(Vec<crate::runtime::SteerPayload>),
    /// The user interrupted (Stop button). The agent stays alive.
    Interrupt,
    /// The user interrupted with commands already queued mid-turn. This is a
    /// hard stop (like [`StopReason::Interrupt`]) that ALSO carries the queued
    /// steers: without it the Interrupt would overwrite the buffered Steer and
    /// silently drop the user's pending commands ("press Stop, then nothing
    /// runs"). `AgentTask` treats the carried payloads exactly like
    /// [`StopReason::Steer`]: pushed as user messages and run as the next
    /// turn, immediately after stopping.
    InterruptWithSteers(Vec<crate::runtime::SteerPayload>),
    /// The user cancelled (close-x). The agent task should exit.
    Cancel,
    /// The user requested a context compaction (`/compact`). The agent stays
    /// alive; `AgentTask::run` summarizes old messages after the turn ends.
    Compact,
    /// The user requested a context compaction with commands already queued
    /// mid-turn. Compaction runs on the quiesced conversation (as with
    /// [`StopReason::Compact`]), then the queued commands drive a fresh turn —
    /// without this the steer folded in the same batch window would be
    /// silently dropped (the same "pending command lost" bug class as Stop).
    CompactWithSteers(Vec<crate::runtime::SteerPayload>),
    /// The user requested a fresh conversation (`/new`). The agent stays
    /// alive; `AgentTask::run` clears the message history after the turn ends.
    /// Any queued steers are intentionally DROPPED here — `/new` wipes the
    /// conversation, so running the queued command against the wiped history
    /// would be wrong.
    Clear,
}

impl StopReason {
    /// Whether this is a HARD stop — the turn must end before any pending
    /// tool batch executes (Interrupt/Cancel/Compact/Clear, with or without
    /// carried steers). A bare [`StopReason::Steer`] is a SOFT stop: the turn
    /// drains the in-flight tool batch first and the steer drives the
    /// follow-up turn afterwards — a user message must never silently cancel
    /// tool calls the model already emitted (2026-12-30 bug: interrupted
    /// turns silently dropped in-flight tool calls; a steer arriving
    /// mid-stream used to end the turn before the batch ran, while the same
    /// steer arriving after stream-end let the batch run, so sub-second
    /// timing decided whether calls executed at all).
    pub fn is_hard_stop(&self) -> bool {
        !matches!(self, StopReason::Steer(_))
    }

    /// Fold a mid-turn command into the accumulating stop reason. Steers and
    /// Prompts accumulate into the pending-steer list (a stronger signal
    /// already in place wins — a Cancel/Interrupt is never downgraded). Hard
    /// signals preserve any already-buffered steers where the queued command
    /// must still run: Interrupt → [`StopReason::InterruptWithSteers`] and
    /// Compact → [`StopReason::CompactWithSteers`] (the commands run after the
    /// stop/compaction). Cancel and Clear intentionally DROP queued steers —
    /// Cancel exits the agent task (steers are moot) and Clear wipes the
    /// conversation (`/new`), so running a queued command afterwards would be
    /// wrong.
    pub fn fold(current: &mut Option<StopReason>, cmd: crate::runtime::AgentCommand) {
        use crate::runtime::AgentCommand as C;
        match cmd {
            // Both steer-carrying commands normalize to their payload — a
            // Prompt folded mid-stream rides the same steer pipeline, so its
            // images must ride along too (previously the fold discarded them
            // and the injected message was bare text — the
            // steered-images-dropped bug).
            C::Suggestion(p) => Self::fold_steer(current, p),
            C::Prompt { text, images } => Self::fold_steer(
                current,
                crate::runtime::SteerPayload { text, images },
            ),
            C::CancelSuggestion(s) => match current.take() {
                // Remove the matching text from any steer-carrying list. An
                // empty list collapses: Steer([]) -> None (the turn continues,
                // no soft-stop); InterruptWithSteers([]) -> Interrupt and
                // CompactWithSteers([]) -> Compact (the stop still happens,
                // just no follow-up command runs). A non-steer stop reason (or
                // None) is untouched — there's nothing queued to cancel.
                // Text-match is the documented limitation: two steers with
                // identical text cancel together even if their images differ.
                Some(StopReason::Steer(mut steers)) => {
                    steers.retain(|p| p.text != s);
                    *current = if steers.is_empty() {
                        None
                    } else {
                        Some(StopReason::Steer(steers))
                    };
                }
                Some(StopReason::InterruptWithSteers(mut steers)) => {
                    steers.retain(|p| p.text != s);
                    *current = if steers.is_empty() {
                        Some(StopReason::Interrupt)
                    } else {
                        Some(StopReason::InterruptWithSteers(steers))
                    };
                }
                Some(StopReason::CompactWithSteers(mut steers)) => {
                    steers.retain(|p| p.text != s);
                    *current = if steers.is_empty() {
                        Some(StopReason::Compact)
                    } else {
                        Some(StopReason::CompactWithSteers(steers))
                    };
                }
                other => *current = other,
            },
            C::Interrupt => {
                // Preserve queued steers across the interrupt so they run next
                // — from ANY steer-carrying state.
                let taken = current.take();
                *current = Some(match taken {
                    Some(StopReason::Steer(texts))
                    | Some(StopReason::InterruptWithSteers(texts))
                    | Some(StopReason::CompactWithSteers(texts)) => {
                        StopReason::InterruptWithSteers(texts)
                    }
                    _ => StopReason::Interrupt,
                });
            }
            C::Compact => {
                // Preserve queued steers across the compaction so they run
                // against the compacted conversation next — from ANY
                // steer-carrying state.
                let taken = current.take();
                *current = Some(match taken {
                    Some(StopReason::Steer(texts))
                    | Some(StopReason::InterruptWithSteers(texts))
                    | Some(StopReason::CompactWithSteers(texts)) => {
                        StopReason::CompactWithSteers(texts)
                    }
                    _ => StopReason::Compact,
                });
            }
            // Cancel exits the agent task; Clear wipes the conversation — both
            // intentionally drop any queued steers (see the doc comment).
            C::Cancel => *current = Some(StopReason::Cancel),
            C::Clear => *current = Some(StopReason::Clear),
        }
    }

    /// Fold a steer payload into the accumulating stop reason — the shared
    /// body of the `Suggestion`/`Prompt` arms of [`StopReason::fold`].
    /// Steers accumulate into the pending-steer list (a stronger signal
    /// already in place wins — a Cancel/Interrupt is never downgraded).
    fn fold_steer(current: &mut Option<StopReason>, p: crate::runtime::SteerPayload) {
        match current {
            Some(StopReason::Steer(steers))
            | Some(StopReason::InterruptWithSteers(steers))
            | Some(StopReason::CompactWithSteers(steers)) => steers.push(p),
            None => *current = Some(StopReason::Steer(vec![p])),
            // A steer arriving AFTER a hard stop is still the user's
            // message — it must drive the follow-up turn, not vanish.
            // Promote to the steer-carrying variant, mirroring the
            // Interrupt/Compact arms of `fold` (which preserve steers folded
            // BEFORE the signal). This is the grace-window seam (2026-12-30
            // review finding 1, plan edfff8d9): the user presses Stop
            // mid-tool-execution, then types a follow-up while the drain
            // window is open — the pre-inject drain folds the queued steer
            // here, and without this arm it was silently discarded.
            Some(StopReason::Interrupt) => {
                *current = Some(StopReason::InterruptWithSteers(vec![p]));
            }
            Some(StopReason::Compact) => {
                *current = Some(StopReason::CompactWithSteers(vec![p]));
            }
            _ => {} // Cancel/Clear: the task/tab is gone — documented drop
        }
    }
}

/// Drop cancelled steers from a buffered-command list (the summarization
/// re-injection paths buffer commands received during the LLM call, then
/// re-inject them). A [`AgentCommand::CancelSuggestion`] cancels every
/// matching [`AgentCommand::Suggestion`] / [`AgentCommand::Prompt`] by text
/// (order-independent — the cancel may have arrived before or after the
/// steer it cancels), and is itself removed. Non-steer commands are kept.
/// `Prompt` is included for consistency with `StopReason::fold`, which
/// accumulates `Suggestion` and `Prompt` into the same steer list — so a
/// cancel must drop either kind by the same text.
///
/// Called before each summarization re-injection loop so a steer the user
/// dismissed (the "x" on a pending steer) is not re-injected as a system
/// message during/after compaction or a model switch.
pub fn drop_cancelled_steers(buffered: &mut Vec<crate::runtime::AgentCommand>) {
    use crate::runtime::AgentCommand as C;
    use std::collections::HashSet;

    let cancelled: HashSet<String> = buffered
        .iter()
        .filter_map(|c| match c {
            C::CancelSuggestion(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    if cancelled.is_empty() {
        return;
    }
    buffered.retain(|c| match c {
        C::CancelSuggestion(_) => false,
        C::Suggestion(p) => !cancelled.contains(&p.text),
        C::Prompt { text: s, .. } => !cancelled.contains(s),
        _ => true,
    });
}

/// The outcome of running a turn.
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub finish_reason: FinishReason,
    pub text: String,
    pub tool_calls_made: usize,
    /// Why the turn ended early, when it did. `Some(StopReason::Steer(texts))`
    /// means one or more steers arrived mid-turn and every `texts` entry
    /// should drive the next turn as a regular user message (the `AgentTask`
    /// reads this and runs a follow-up turn over all of them, in order).
    /// `Some(StopReason::Interrupt)` means the user pressed Stop — the
    /// agent stays alive but the turn is over. `Some(StopReason::Cancel)`
    /// means the agent task should terminate (`Exited` fires). `None` for a
    /// normal turn end.
    pub stop_reason: Option<StopReason>,
}

/// Construction parameters for [`AgentLoop`].
///
/// Groups the eight shared constructor arguments so the constructors stay
/// under clippy's `too_many_arguments` threshold without an `#[allow]`. The
/// constitution is passed separately to [`AgentLoop::new`] or
/// [`AgentLoop::with_constitution_source`] because the two differ only in
/// that one field. Build with a struct literal.
pub struct AgentLoopConfig {
    /// The LLM client (swappable at runtime via the provider lock).
    pub provider: Arc<dyn LlmClient>,
    /// The tool registry (shared, immutable after construction).
    pub tools: Arc<ToolRegistry>,
    /// The workflow state machine (shared across agents).
    pub workflow: Arc<tokio::sync::Mutex<crate::workflow::Workflow>>,
    /// The project sandbox (root dir + path validation).
    pub sandbox: Arc<Sandbox>,
    /// The initial safety mode (also wired as a shared handle).
    pub safety_mode: SafetyMode,
    /// The context-window manager (token counting + truncation).
    pub context_manager: super::context::ContextManager,
    /// Optional persistent memory store.
    pub memory: Option<Arc<dyn MemoryStoreTrait>>,
    /// Optional vision/image-describer (swappable at runtime).
    pub vision: Option<Arc<dyn crate::provider::vision::ImageDescriber>>,
}

impl AgentLoop {
    /// Create a new agent loop with a static (in-memory) constitution.
    ///
    /// Use [`with_constitution_source`](Self::with_constitution_source) in
    /// production so edits to `agent.md` take effect without a restart.
    pub fn new(config: AgentLoopConfig, constitution: crate::project::Constitution) -> Self {
        Self::from_config(config, ConstitutionHolder::Static(constitution))
    }

    /// Like [`new`](Self::new) but with a file-backed constitution source that
    /// re-reads `agent.md` from disk when its mtime changes — so edits to the
    /// constitution take effect on the next turn without restarting.
    pub fn with_constitution_source(
        config: AgentLoopConfig,
        constitution_source: crate::project::ConstitutionSource,
    ) -> Self {
        Self::from_config(
            config,
            ConstitutionHolder::Source {
                source: std::sync::Mutex::new(constitution_source),
                cached_head: std::sync::Mutex::new(None),
            },
        )
    }

    /// Shared constructor body for [`new`] and [`with_constitution_source`].
    fn from_config(config: AgentLoopConfig, constitution: ConstitutionHolder) -> Self {
        let AgentLoopConfig {
            provider,
            tools,
            workflow,
            sandbox,
            safety_mode,
            context_manager,
            memory,
            vision,
        } = config;
        Self {
            provider: Arc::new(RwLock::new(provider)),
            context_manager: Arc::new(RwLock::new(context_manager)),
            live_token_count: std::sync::atomic::AtomicUsize::new(0),
            tools,
            workflow,
            sandbox,
            constitution,
            safety_mode: Arc::new(RwLock::new(safety_mode)),
            memory,
            safety_rules: None,
            vision,
            session: SessionState::new(),
            agent_id: std::sync::Mutex::new(None),
            plan_mutations_allowed: std::sync::Mutex::new(true),
            model_resolver: None,
            is_subagent: std::sync::Mutex::new(false),
            reviewer_failure_pending: std::sync::Mutex::new(false),
            reviewer_retry_sanctioned: std::sync::Mutex::new(false),
            fill_rate: 0.5,
            forced_model: std::sync::Mutex::new(None),
            explicit_provider: Arc::new(RwLock::new(None)),
            pinned_state: std::sync::Mutex::new(None),
            fallback_endpoints: Arc::new(RwLock::new(std::collections::HashMap::new())),
            pending_swap: Arc::new(std::sync::Mutex::new(None)),
            resolved_model: std::sync::Mutex::new(None),
            resolved_provider: std::sync::Mutex::new(None),
            resolved_effort: std::sync::Mutex::new(None),
            default_display_effort: std::sync::Mutex::new(None),
            pinned_display_effort: std::sync::Mutex::new(None),
            descendant_tracker: None,
            graph: None,
            last_review_report: std::sync::Mutex::new(None),
            plans_dir: std::path::PathBuf::new(),
            root_spec: None,
            failure_triage: None,
        }
    }

    /// Attach the project's plans dir so consolidation can digest the
    /// accumulated plan/review corpus at session end (see
    /// [`corpus_digest`](crate::memory::consolidation::corpus_digest)).
    /// Returns `self` for chaining. Wired by [`AgentLoopFactory`]; empty for
    /// directly-built test loops (no corpus).
    pub fn with_plans_dir(mut self, plans_dir: impl Into<std::path::PathBuf>) -> Self {
        self.plans_dir = plans_dir.into();
        self
    }

    /// The project's plans dir wired by the factory (empty for test loops
    /// built without one). Used by session-end consolidation to locate the
    /// plan/review corpus.
    pub fn plans_dir(&self) -> &std::path::Path {
        &self.plans_dir
    }

    /// Store the per-agent root override (parallel run-all worktree agents,
    /// plan ffd7a86f). Returns `self` for chaining. Wired by
    /// [`AgentLoopFactory::build_with_root_spec`]; read by the IPC spawner so
    /// subagents inherit the same root.
    pub fn with_root_spec(mut self, root: Option<crate::agent::factory::AgentRootSpec>) -> Self {
        self.root_spec = root;
        self
    }

    /// The per-agent root override this loop was built with (`None` for
    /// factory-default builds). The IPC spawner reads it so subagents
    /// spawned by this agent inherit the same root.
    pub fn root_spec(&self) -> Option<&crate::agent::factory::AgentRootSpec> {
        self.root_spec.as_ref()
    }

    /// Attach the failure-triage gate (the Laya classifier's failure-handling
    /// consumer, `[general.laya] failure_triage`). The gate carries the shared
    /// classifier slot + the config-mirrored enable flag; BOTH are read at
    /// failure time, so a Settings save takes effect on the next failure and
    /// the flag can stay off (keeping the pre-classifier behavior) while the
    /// slot itself is live. Returns `self` for chaining. Wired by
    /// [`AgentLoopFactory`](crate::agent::factory::AgentLoopFactory); `None`
    /// in tests that don't exercise triage.
    pub fn with_failure_triage(mut self, handle: failure_triage::FailureTriageHandle) -> Self {
        self.failure_triage = Some(handle);
        self
    }

    /// The failure-triage gate this loop was built with (`None` when triage is
    /// unwired). The IPC layer mirrors the config flag into the gate's live
    /// enable flag, so a Settings toggle needs no rebuild.
    pub fn failure_triage(&self) -> Option<&failure_triage::FailureTriageHandle> {
        self.failure_triage.as_ref()
    }

    /// The context-manager fill rate this loop was built with — the LIVE
    /// value the engine's fill-rate summarization path uses (the fraction
    /// of the window at which summarization triggers). The run-all
    /// between-items auto-compact gate (backlog ffd4bac3) reads it so its
    /// threshold tracks the same dial the user tuned, even if the config
    /// changed after the loop was built.
    pub fn fill_rate(&self) -> f64 {
        self.fill_rate
    }

    /// Attach a safety-rules store so matching tool calls are auto-approved.
    ///
    /// Returns `self` for chaining. The store is shared behind an `Arc` so the
    /// UI layer (Tauri commands) can add rules / edit the file through the
    /// same handle the agent loop reads from.
    pub fn with_safety_rules(mut self, rules: Arc<SafetyRules>) -> Self {
        self.safety_rules = Some(rules);
        self
    }

    /// Attach a descendant tracker so the dispatch layer can gate workflow-
    /// state transitions on "no spawned subagents running" (the agent must
    /// wait for all descendants before changing state) and the runtime
    /// auto-continue logic can park a parent while its descendants run.
    /// Returns `self` for chaining. Wired by [`AgentLoopFactory`] once the
    /// IPC-layer spawner exists; `None` in tests (no gate is enforced).
    pub fn with_descendant_tracker(
        mut self,
        tracker: Arc<dyn crate::runtime::DescendantTracker>,
    ) -> Self {
        self.descendant_tracker = Some(tracker);
        self
    }

    /// Wire the per-project code knowledge graph so the dispatch-layer
    /// symbol-lookup redirect gate (C5) can resolve symbol-shaped search
    /// patterns to their graph ids before the tool runs. `None` when indexing
    /// is disabled — the gate is a no-op. Returns `self` for chaining. Wired
    /// by [`AgentLoopFactory`](crate::agent::factory::AgentLoopFactory).
    pub fn with_codegraph(mut self, graph: Option<Arc<crate::codegraph::CodeGraph>>) -> Self {
        self.graph = graph;
        self
    }

    /// Replace the internal safety-mode handle with a shared one, so the UI
    /// toggle (or another agent's factory build) affects this loop too.
    ///
    /// Returns `self` for chaining. Used by [`AgentLoopFactory`](crate::agent::factory::AgentLoopFactory)
    /// so all agents built from the same factory share one safety-mode handle —
    /// a runtime toggle is a global setting, not per-agent.
    pub fn with_safety_mode_handle(mut self, handle: Arc<RwLock<SafetyMode>>) -> Self {
        self.safety_mode = handle;
        self
    }

    /// Attach a per-context model resolver. When present, the turn driver
    /// consults it at turn start to pick a model for *this turn* based on the
    /// workflow state + active skill + whether this is a subagent — overriding
    /// the default provider. Returns `self` for chaining.
    pub fn with_model_resolver(
        mut self,
        resolver: Arc<dyn crate::model_resolver::ModelResolver>,
    ) -> Self {
        self.model_resolver = Some(resolver);
        self
    }

    /// Set whether this agent is a subagent. Used by the IPC spawner
    /// immediately after building a child agent (when `parent_id.is_some()`)
    /// so the per-context model resolver considers the `[models.subagent]`
    /// override. Mirrors [`set_plan_mutations_allowed`](Self::set_plan_mutations_allowed).
    pub fn set_is_subagent(&self, is_subagent: bool) {
        *self.is_subagent.lock().expect("is_subagent lock poisoned") = is_subagent;
    }

    /// Whether this agent is a subagent (spawned via `spawn_agent` or the UI
    /// spawn button with a parent).
    pub fn is_subagent(&self) -> bool {
        *self.is_subagent.lock().expect("is_subagent lock poisoned")
    }

    /// Force this agent onto a specific model (endpoint + model id) for every
    /// turn, overriding the normal subagent/state/skill resolution chain. Set
    /// by the IPC spawner when a `spawn_agent` call passes a `model` argument.
    ///
    /// Stays in effect for the agent's lifetime *while the referenced endpoint
    /// remains configured* — if the endpoint is later deleted from config,
    /// `resolve_turn_provider` can no longer build a provider for it and the
    /// turn falls back to the default provider (consistent with the state-
    /// override path). Once set, the forced model is not re-evaluated per turn
    /// (it's a property of how the agent was spawned, not a per-turn toggle).
    pub fn set_forced_model(&self, model: crate::config::ModelRef) {
        *self
            .forced_model
            .lock()
            .expect("forced_model lock poisoned") = Some(model);
    }

    /// The forced model for this agent, if one was set at spawn time. `None`
    /// for the main agent and any spawn that didn't request a specific model.
    /// Checked first in [`resolve_turn_provider`](Self::resolve_turn_provider),
    /// before the normal resolution chain.
    pub fn forced_model(&self) -> Option<crate::config::ModelRef> {
        self.forced_model
            .lock()
            .expect("forced_model lock poisoned")
            .clone()
    }

    /// Record the model id actually used for the most recent turn. Called by
    /// [`resolve_turn_provider`](Self::resolve_turn_provider) on every
    /// resolution path: `Some(model)` when a per-context override (or forced
    /// model) was used, `None` when the turn fell back to the default provider.
    /// Also called by the IPC `set_model` command with `None` when the provider
    /// is swapped, so `list_agents` reports the new provider's model instead of
    /// the previous turn's override.
    pub fn set_resolved_model(&self, model: Option<String>) {
        *self
            .resolved_model
            .lock()
            .expect("resolved_model lock poisoned") = model;
    }

    /// The model id actually used for the most recent turn: the per-context
    /// override when one resolved, `None` when the turn ran on the default
    /// provider. Read by the IPC layer to report the effective model in
    /// `AgentInfo` (the Tauri-side agent-info struct).
    pub fn resolved_model(&self) -> Option<String> {
        self.resolved_model
            .lock()
            .expect("resolved_model lock poisoned")
            .clone()
    }

    /// Record the endpoint name that served the most recent turn's resolved
    /// model — the mirror of [`set_resolved_model`](Self::set_resolved_model),
    /// called at the same resolution sites. `None` when the turn ran on the
    /// default provider. Empty names (test mocks) are stored as-is and
    /// skipped by [`effective_provider_name`](Self::effective_provider_name).
    pub fn set_resolved_provider(&self, provider: Option<String>) {
        *self
            .resolved_provider
            .lock()
            .expect("resolved_provider lock poisoned") = provider;
    }

    /// The endpoint name the IPC layer should report for this agent: the
    /// resolved override's endpoint when one ran (non-empty), else the
    /// DEFAULT provider's endpoint name. `None` when neither carries a
    /// non-empty name (test mocks) — the frontend then falls back to
    /// resolving the model id against `endpoints.toml` (the pre-wire
    /// behavior), so mock-backed agents are unaffected.
    pub fn effective_provider_name(&self) -> Option<String> {
        if let Some(name) = self
            .resolved_provider
            .lock()
            .expect("resolved_provider lock poisoned")
            .clone()
        {
            if !name.is_empty() {
                return Some(name);
            }
        }
        let name = self.provider().provider_name().to_string();
        (!name.is_empty()).then_some(name)
    }

    /// Record the DISPLAY-space effective reasoning effort of the provider
    /// that served the most recent turn — the mirror of
    /// [`set_resolved_model`](Self::set_resolved_model), called at the same
    /// resolution sites. `None` when the effort is unknown (mock-backed
    /// loops, a failed override build) — the UI then falls back to the
    /// toolbar echo / endpoint default. Also cleared by the IPC `set_model`
    /// swap (alongside `set_resolved_model(None)`).
    pub fn set_resolved_effort(&self, effort: Option<String>) {
        *self
            .resolved_effort
            .lock()
            .expect("resolved_effort lock poisoned") = effort;
    }

    /// The DISPLAY-space effective reasoning effort of the provider that
    /// served the most recent turn (`"off" | "low" | "medium" | "high" |
    /// "max"`), or `None` when unknown. Read by the IPC layer to report the
    /// effective effort in `AgentInfo` and by the turn layer's
    /// `ModelChanged` emission — the same resolution the request builder
    /// uses, in UI vocabulary.
    pub fn resolved_effort(&self) -> Option<String> {
        self.resolved_effort
            .lock()
            .expect("resolved_effort lock poisoned")
            .clone()
    }

    /// Record the DISPLAY-space effort of the DEFAULT provider slot — set
    /// by the factory at construction (from the default `ModelRef`), by
    /// `set_model` swaps, and by Settings saves that rebuild the default.
    /// `None` when unknown (test loops) — the no-override resolution branch
    /// then reports no effort and the UI falls back.
    pub fn set_default_display_effort(&self, effort: Option<String>) {
        *self
            .default_display_effort
            .lock()
            .expect("default_display_effort lock poisoned") = effort;
    }

    /// The DISPLAY-space effort of the DEFAULT provider slot (see
    /// [`set_default_display_effort`](Self::set_default_display_effort)).
    pub fn default_display_effort(&self) -> Option<String> {
        self.default_display_effort
            .lock()
            .expect("default_display_effort lock poisoned")
            .clone()
    }

    /// Record the DISPLAY-space effort the picker PIN was built with — set
    /// by the IPC `set_model` swap (the toolbar's requested effort, or the
    /// model's default when unset). `None` when the pin predates the field
    /// or was set by a path that doesn't know it — the pin resolution branch
    /// then reports no effort and the UI falls back to the toolbar echo.
    pub fn set_pinned_display_effort(&self, effort: Option<String>) {
        *self
            .pinned_display_effort
            .lock()
            .expect("pinned_display_effort lock poisoned") = effort;
    }

    /// The DISPLAY-space effort the picker pin was built with (see
    /// [`set_pinned_display_effort`](Self::set_pinned_display_effort)).
    pub fn pinned_display_effort(&self) -> Option<String> {
        self.pinned_display_effort
            .lock()
            .expect("pinned_display_effort lock poisoned")
            .clone()
    }

    /// Record the path of the last review report this agent wrote via
    /// `write_review_report`. Called by the turn layer on a successful
    /// `write_review_report` tool call so the event forwarder can include the
    /// path in the completion notification sent to the parent agent — instead
    /// of a generic "read its report" message that forces the parent to search
    /// for the file (unreliable with multiple concurrent reviewers).
    pub fn set_last_review_report(&self, path: String) {
        *self
            .last_review_report
            .lock()
            .expect("last_review_report lock poisoned") = Some(path);
    }

    /// The path of the last review report this agent wrote, if any. `None`
    /// until the agent calls `write_review_report` successfully. Read by the
    /// event forwarder when notifying the parent of this child's completion.
    pub fn last_review_report(&self) -> Option<String> {
        self.last_review_report
            .lock()
            .expect("last_review_report lock poisoned")
            .clone()
    }

    /// Set the context-manager fill rate (fraction of the context window at
    /// which summarization triggers). Stored so a per-context model override
    /// can build a correctly-sized `ContextManager` at turn time. Returns
    /// `self` for chaining.
    pub fn with_fill_rate(mut self, fill_rate: f64) -> Self {
        self.fill_rate = fill_rate;
        self
    }

    /// The provider a compaction summary call runs on.
    ///
    /// `[models.summarize]` routes summaries to a dedicated — often cheaper —
    /// model; unset (or a dangling reference / build failure) falls back to
    /// the turn's provider, preserving the pre-slot behavior exactly. The
    /// resolved model is NOT stamped into the turn's display state
    /// ([`Self::set_resolved_model`] et al.) — a summary is a one-off
    /// internal call, not a turn model switch.
    pub(crate) fn summarize_provider(
        &self,
        turn_provider: &Arc<dyn LlmClient>,
    ) -> Arc<dyn LlmClient> {
        let Some(resolver) = self.model_resolver.as_ref() else {
            return turn_provider.clone();
        };
        let Some(model_ref) = resolver.resolve_summarize_model() else {
            return turn_provider.clone();
        };
        // 429 endpoint stickiness reroutes the same model id to its recorded
        // alternate endpoint (routing, not a model choice).
        let model_ref = self.sticky_endpoint(&model_ref).unwrap_or(model_ref);
        resolver
            .build_turn_provider(&model_ref, self.fill_rate)
            .map(|(provider, _)| provider)
            .unwrap_or_else(|| turn_provider.clone())
    }

    /// Resolve the provider + context manager to use for a turn given the
    /// current workflow state + active skill.
    ///
    /// Consults the per-context [`ModelResolver`] (if any). When the resolver
    /// returns a [`ModelRef`](crate::config::ModelRef), builds a throwaway
    /// provider + context manager for that model (sized to its context window
    /// via the loop's fill rate). Returns `None` when no resolver is attached
    /// or the resolver returns `None` — the caller then uses the default
    /// provider snapshot.
    ///
    /// **Resolution priority (first match wins):**
    /// 1. **Skill override** — when `skill_name` is `Some` and the resolver
    ///    returns a configured `[models.skill.<name>]` model, that model is
    ///    used. Skill turns are deliberate context switches with their own
    ///    model assignment, so they beat the picker pin (2026-09-04).
    /// 2. **Explicit provider** — pinned by the per-agent model picker
    ///    (`set_model` with an `agent_id`; see
    ///    [`set_explicit_provider`](Self::set_explicit_provider)),
    ///    STATE-SCOPED: it beats forced / subagent / workflow-state
    ///    overrides in the workflow state it was first served in (2026-08-22:
    ///    a live picker choice must not be discarded by that state's
    ///    `[models.planning]` etc.), while in any OTHER state it holds only
    ///    until a configured `[models.*]` model takes over (2026-12-20) —
    ///    with nothing configured for the new state the pin keeps serving.
    ///    The pin is never deleted by a swap; it goes dormant instead.
    /// 3. **Forced model** — when [`forced_model`](Self::forced_model) is set
    ///    (a `spawn_agent` call passed a `model` argument), it's used
    ///    directly — the subagent/state resolution chain is skipped.
    ///    This makes the spawned agent run on the requested model for every
    ///    turn, regardless of workflow state changes, *while the endpoint
    ///    remains configured* (a deleted endpoint falls back to the default
    ///    provider, same as a dangling state override).
    /// 4. **Resolver chain** — subagent / workflow-state overrides from the
    ///    live config `[models]` section (skill already handled in step 1).
    ///
    /// `skill_name` is the active skill's name (`Some` only while a skill is
    /// active). The resolver reads the live config each call, so a Settings
    /// save takes effect on the next turn.
    ///
    /// Called at the top of EVERY `run_turn` loop iteration (i.e. per
    /// provider request, not once per turn) — so a mid-turn workflow change
    /// (`skill_start`/`skill_end`, or a state transition from any workflow
    /// tool) switches the next request within the same turn to the
    /// configured per-context model. See [`AgentLoop::run_turn`].
    pub(crate) fn resolve_turn_provider(
        &self,
        workflow_state: crate::workflow::WorkflowState,
        skill_name: Option<&str>,
        plan_kind: Option<crate::workflow::PlanKind>,
    ) -> Option<(Arc<dyn LlmClient>, super::context::ContextManager)> {
        // 1. Configured skill override beats everything — including the
        // picker pin. Skill runs are deliberate context switches that carry
        // their own `[models.skill.<name>]` assignment; the 2026-08-22 pin
        // fix overcorrected by short-circuiting the pin before any resolver
        // lookup, which made skill models unreachable once the user had
        // ever used the per-agent picker (2026-09-04 regression).
        //
        // Detect a true skill-map hit (not a subagent/state fallback) by
        // resolving with Skill state + is_subagent=false: only the skill
        // slot can fire in that context. That keeps pin > subagent/state.
        if let Some(name) = skill_name {
            if let Some(resolver) = self.model_resolver.as_ref() {
                let skill_only = crate::model_resolver::ModelContext::new(
                    crate::workflow::WorkflowState::Skill,
                    Some(name),
                    false,
                    // The skill arm is skill-slot-only: only the skill slot
                    // can fire in this probe context, so the plan kind is
                    // deliberately not engaged here.
                    None,
                );
                if let Some(model_ref) = resolver.resolve(skill_only) {
                    // 429 endpoint stickiness reroutes the same model id to
                    // its recorded alternate endpoint; the skill model
                    // choice itself still wins.
                    let model_ref = self.sticky_endpoint(&model_ref).unwrap_or(model_ref);
                    let built = resolver.build_turn_provider(&model_ref, self.fill_rate);
                    self.set_resolved_model(built.as_ref().map(|(p, _)| p.model().to_string()));
                    self.set_resolved_provider(
                        built.as_ref().map(|(p, _)| p.provider_name().to_string()),
                    );
                    self.set_resolved_effort(
                        built.as_ref().and_then(|_| resolver.display_effort_for(&model_ref)),
                    );
                    return built;
                }
            }
        }

        // 2. Explicit picker pin (2026-08-22), STATE-SCOPED (2026-12-20
        // amendment): the user's live per-agent choice holds in the workflow
        // state it was first served in (lazily stamped into `pinned_state`)
        // and in any other state only while NO configured `[models.*]` model
        // resolves for that context. On a state change with a configured slot
        // (e.g. create_plan flipping Planning → Executing with
        // `[models.executing]` set), the CONFIGURED model takes over: the
        // code falls through to the normal chain below WITHOUT deleting the
        // pin — it stays dormant and resumes if the workflow returns to its
        // state (e.g. abandon_plan back to Planning). With nothing configured
        // for the new state the pin keeps serving: falling to the default
        // model would discard the user's choice for no configured gain.
        // Within the pin's own state it still beats forced / subagent / state
        // overrides — checked BEFORE the forced-model branch, and it carries
        // the exact provider (with its resolved reasoning effort) so no
        // rebuild happens. A recorded 429-fallback endpoint for the pinned
        // model still reroutes the ENDPOINT (routing, not a model choice —
        // see [`sticky_endpoint`](Self::sticky_endpoint)): the pin keeps its
        // model, the retried turn keeps running.
        let pinned = self
            .explicit_provider
            .read()
            .expect("explicit_provider lock poisoned")
            .clone();
        if let Some((p, cm)) = pinned {
            // Does the pin hold in the CURRENT workflow state? Stamped on
            // first serve; holds in its own state, and in any other state
            // only while the resolver chain has nothing configured to serve
            // there (the skill slot already had its chance in step 1 — an
            // unconfigured skill name misses here too, and the Skill state
            // itself resolves None, so a pin survives unconfigured skill
            // turns exactly as before).
            let pin_holds = {
                let mut stamp = self
                    .pinned_state
                    .lock()
                    .expect("pinned_state lock poisoned");
                match *stamp {
                    None => {
                        // First serve of this pin — it belongs to the state
                        // of the turn that first serves it (normally the
                        // state the picker acted in; with a deferred swap,
                        // the state the turn loop completed the swap in).
                        // The pick can predate the stamp by several turns —
                        // e.g. pick in Planning, then `create_plan` flips to
                        // Executing before the next turn: the first serve
                        // stamps Executing, not the pick-time state.
                        *stamp = Some(workflow_state);
                        true
                    }
                    Some(stamped) if stamped == workflow_state => true,
                    Some(_) => self
                        .model_resolver
                        .as_ref()
                        .map(|resolver| {
                            resolver
                                .resolve(crate::model_resolver::ModelContext::new(
                                    workflow_state,
                                    skill_name,
                                    self.is_subagent(),
                                    plan_kind,
                                ))
                                .is_none()
                        })
                        // No resolver attached → nothing is ever configured
                        // → the pin is the only override there is; it holds.
                        .unwrap_or(true),
                }
            };
            if pin_holds {
                let pinned_ref = crate::config::ModelRef {
                    endpoint: p.provider_name().to_string(),
                    model: p.model().to_string(),
                    // Reconstructed from the live provider for the 429-sticky
                    // lookup only — a reroute build inherits the model's own
                    // effort default.
                    reasoning_effort: None,
                };
                if let Some(alt) = self.sticky_endpoint(&pinned_ref) {
                    if let Some(resolver) = self.model_resolver.as_ref() {
                        if let Some((provider, context_manager)) =
                            resolver.build_turn_provider(&alt, self.fill_rate)
                        {
                            self.set_resolved_model(Some(provider.model().to_string()));
                            self.set_resolved_provider(Some(provider.provider_name().to_string()));
                            self.set_resolved_effort(resolver.display_effort_for(&alt));
                            return Some((provider, context_manager));
                        }
                    }
                }
                self.set_resolved_model(Some(p.model().to_string()));
                self.set_resolved_provider(Some(p.provider_name().to_string()));
                // The pin's build-time effort (recorded by the set_model
                // swap) — None when unknown, the UI falls back to the
                // toolbar echo.
                self.set_resolved_effort(self.pinned_display_effort());
                return Some((p, cm));
            }
            // A configured model takes over in this state — fall through to
            // forced / subagent / state resolution. The pin stays in
            // `explicit_provider` (dormant), resuming if the workflow ever
            // returns to its stamped state.
        }
        // 3. A forced model (from spawn_agent's `model` arg) overrides the
        // normal resolution chain entirely. Checked BEFORE the
        // `model_resolver.as_ref()?` early-return so a forced model is never
        // silently dropped by a missing resolver — if a forced model is set
        // but no resolver can build it, we fall through to None (default
        // provider) rather than honoring it partially. In practice the
        // factory wires the resolver whenever a forced model can be set, so
        // this is defensive.
        if let Some(forced) = self.forced_model() {
            if let Some(resolver) = self.model_resolver.as_ref() {
                // 429 endpoint stickiness reroutes the same model id to its
                // recorded alternate endpoint; the forced model choice
                // itself still wins.
                let forced = self.sticky_endpoint(&forced).unwrap_or(forced);
                let built = resolver.build_turn_provider(&forced, self.fill_rate);
                // Record the effective model so the UI reports what actually
                // ran (forced model beats the state/subagent chain).
                self.set_resolved_model(built.as_ref().map(|(p, _)| p.model().to_string()));
                self.set_resolved_provider(
                    built.as_ref().map(|(p, _)| p.provider_name().to_string()),
                );
                self.set_resolved_effort(
                    built.as_ref().and_then(|_| resolver.display_effort_for(&forced)),
                );
                return built;
            }
            // Forced model set but no resolver — inconsistent state. Fall
            // through to None (default provider) rather than silently honoring
            // a forced model we can't build.
        }
        let Some(resolver) = self.model_resolver.as_ref() else {
            // No resolver attached — every turn runs on the default provider.
            self.set_resolved_model(None);
            self.set_resolved_provider(None);
            self.set_resolved_effort(None);
            return None;
        };
        let is_subagent = self.is_subagent();
        let model_ref = resolver.resolve(crate::model_resolver::ModelContext::new(
            workflow_state,
            skill_name,
            is_subagent,
            plan_kind,
        ));
        let Some(model_ref) = model_ref else {
            // No override resolved for this context — default provider,
            // unless 429 endpoint stickiness reroutes the default model to
            // its recorded alternate endpoint (same model id — routing, not
            // a model choice, so the default path stays on the default
            // MODEL, just not on the endpoint that just 429'd).
            let default = self.provider();
            let default_ref = crate::config::ModelRef {
                endpoint: default.provider_name().to_string(),
                model: default.model().to_string(),
                // Reconstructed from the live default provider for the
                // 429-sticky lookup only — a reroute build inherits the
                // model's own effort default.
                reasoning_effort: None,
            };
            if let Some(alt) = self.sticky_endpoint(&default_ref) {
                let built = resolver.build_turn_provider(&alt, self.fill_rate);
                self.set_resolved_model(built.as_ref().map(|(p, _)| p.model().to_string()));
                self.set_resolved_provider(
                    built.as_ref().map(|(p, _)| p.provider_name().to_string()),
                );
                self.set_resolved_effort(
                    built.as_ref().and_then(|_| resolver.display_effort_for(&alt)),
                );
                return built;
            }
            self.set_resolved_model(None);
            self.set_resolved_provider(None);
            // The default provider's own effort (per-model override →
            // endpoint default → "max") — recorded by the factory / set_model
            // / Settings rebuild. None when unknown (test loops) — the UI
            // falls back to the endpoint default.
            self.set_resolved_effort(self.default_display_effort());
            return None;
        };
        // 429 endpoint stickiness: reroute to the recorded alternate
        // endpoint serving this same model id. Never changes WHICH model
        // the chain picked — so [models.planning] → [models.executing]
        // switches still fire after a 429.
        let model_ref = self.sticky_endpoint(&model_ref).unwrap_or(model_ref);
        // The resolver is the ConfigModelResolver, which holds the live config.
        // Build the provider + context manager from that config. We reach the
        // config via a downcast-free path: the ConfigModelResolver exposes it
        // through a dedicated method on the trait below.
        let built = resolver.build_turn_provider(&model_ref, self.fill_rate);
        // Record the effective model (the per-context override that resolved)
        // so the UI reports what actually ran this turn.
        self.set_resolved_model(built.as_ref().map(|(p, _)| p.model().to_string()));
        self.set_resolved_provider(built.as_ref().map(|(p, _)| p.provider_name().to_string()));
        self.set_resolved_effort(
            built.as_ref().and_then(|_| resolver.display_effort_for(&model_ref)),
        );
        built
    }

    /// Consult the 429-fallback endpoint stickiness for a model ref: when a
    /// previous turn 429'd while serving `model_ref.model` from
    /// `model_ref.endpoint`, return the recorded alternate ModelRef (same
    /// model id, different endpoint). `None` when no stickiness applies —
    /// the caller uses its own ModelRef unchanged. This reroutes ENDPOINTS,
    /// never models: which model the chain picks is decided before this is
    /// consulted, so per-state overrides survive a 429.
    fn sticky_endpoint(
        &self,
        model_ref: &crate::config::ModelRef,
    ) -> Option<crate::config::ModelRef> {
        let map = self
            .fallback_endpoints
            .read()
            .expect("fallback_endpoints lock poisoned");
        let (failed, alt) = map.get(&model_ref.model)?;
        (failed == &model_ref.endpoint && alt.endpoint != model_ref.endpoint).then(|| alt.clone())
    }

    /// A handle to the safety-rules store (if any), so the UI layer can add
    /// rules ("Mark Safe") or edit the file through the same shared store the
    /// agent loop reads from.
    pub fn safety_rules_handle(&self) -> Option<Arc<SafetyRules>> {
        self.safety_rules.clone()
    }

    /// A handle to the shared safety mode, so the UI layer (Tauri commands)
    /// can toggle it at runtime without rebuilding the agent loop.
    pub fn safety_mode_handle(&self) -> Arc<RwLock<SafetyMode>> {
        Arc::clone(&self.safety_mode)
    }

    /// A handle to the memory store (if any), so the agent task can manage
    /// session lifecycle (start/end/consolidate).
    pub fn memory_handle(&self) -> Option<Arc<dyn MemoryStoreTrait>> {
        self.memory.clone()
    }

    /// The memory session id, if one has been started (set on the first turn
    /// with a memory store). The IPC layer uses this to query per-session
    /// stats. The AgentTask still owns starting the session — this just
    /// reflects what was started.
    pub fn session_id(&self) -> Option<String> {
        self.session
            .session_id
            .lock()
            .expect("session_id lock poisoned")
            .clone()
    }

    /// Set the session id (called by AgentTask when it starts a session, so
    /// the loop and the task agree on the same id).
    pub fn set_session_id(&self, id: String) {
        *self
            .session
            .session_id
            .lock()
            .expect("session_id lock poisoned") = Some(id);
    }

    /// The standing project-memory primer, if one has been fetched this session.
    /// Returns `Some(Some(s))` when a primer string is cached, `Some(None)` when
    /// a fetch completed but found no primer (do not refetch), and `None` when no
    /// fetch has happened yet (the first turn fetches). Read by the turn driver
    /// to inject the primer into the stable head so the agent starts with
    /// project knowledge.
    pub fn session_primer(&self) -> Option<Option<String>> {
        self.session
            .primer
            .lock()
            .expect("primer lock poisoned")
            .clone()
    }

    /// Cache the project-memory primer for the rest of this session (called
    /// once on the first turn after fetching the strongest memories). Pass
    /// `Some(None)` when the fetch found no memories (so the fetch is not
    /// retried every turn), or `Some(Some(s))` with the primer string.
    pub fn set_session_primer(&self, primer: Option<Option<String>>) {
        *self.session.primer.lock().expect("primer lock poisoned") =
            primer.map(|p| p.filter(|s| !s.trim().is_empty()));
    }

    /// The plan id for which a "Completed plan" episodic memory has already
    /// been captured this session, if any. Used by the Complete hook to avoid
    /// double-writing when the workflow re-enters Complete for the same
    /// retained plan (e.g. a skill that starts and ends in Complete).
    pub fn captured_complete_plan_id(&self) -> Option<String> {
        self.session
            .captured_complete_plan_id
            .lock()
            .expect("captured_complete_plan_id lock poisoned")
            .clone()
    }

    /// Record that a "Completed plan" episodic memory has been captured for
    /// the given plan id, so the Complete hook does not capture it again.
    pub fn set_captured_complete_plan_id(&self, plan_id: String) {
        *self
            .session
            .captured_complete_plan_id
            .lock()
            .expect("captured_complete_plan_id lock poisoned") = Some(plan_id);
    }

    /// This agent's id in the runtime, if assigned (the factory sets it at
    /// build time). `None` for tests that construct a loop directly.
    pub fn agent_id(&self) -> Option<crate::runtime::AgentId> {
        *self.agent_id.lock().expect("agent_id lock poisoned")
    }

    /// Assign this agent's runtime id. Called by the factory once the id is
    /// known, so the agent's `spawn_agent` tool can record itself as the
    /// parent of background agents it spawns. Returns `self` for chaining.
    pub fn with_agent_id(self, id: crate::runtime::AgentId) -> Self {
        *self.agent_id.lock().expect("agent_id lock poisoned") = Some(id);
        self
    }

    /// Whether plan-mutation workflow tools (`create_plan`, `update_plan`,
    /// `complete_step`, `abandon_plan`) are allowed for this agent.
    ///
    /// Under the main-agent-only policy, tool-spawned sub-agents have this
    /// set to false at construction time; they may read workflow state via
    /// `get_workflow_state` but cannot mutate plans.
    pub fn plan_mutations_allowed(&self) -> bool {
        *self
            .plan_mutations_allowed
            .lock()
            .expect("plan_mutations_allowed lock poisoned")
    }

    /// Set plan-mutation allowance. Used by the IPC spawner immediately after
    /// building a child agent so that only the main (parentless, smallest-id)
    /// agent drives the authoritative plan.
    pub fn set_plan_mutations_allowed(&self, allowed: bool) {
        *self
            .plan_mutations_allowed
            .lock()
            .expect("plan_mutations_allowed lock poisoned") = allowed;
    }

    /// Whether a `role: "reviewer"` child of this agent failed with no report
    /// (the failed-reviewer protocol latch). While true, spawning another
    /// reviewer is denied at dispatch until the agent asks the user.
    pub fn reviewer_failure_pending(&self) -> bool {
        *self
            .reviewer_failure_pending
            .lock()
            .expect("reviewer_failure_pending lock poisoned")
    }

    /// Set the failed-reviewer protocol latch. Set by the event forwarder
    /// when a reviewer child fails without a report; cleared when the agent
    /// asks the user (ask_user interception in dispatch).
    pub fn set_reviewer_failure_pending(&self, pending: bool) {
        *self
            .reviewer_failure_pending
            .lock()
            .expect("reviewer_failure_pending lock poisoned") = pending;
    }

    /// Whether a failed-reviewer retry is sanctioned (see
    /// [`Self::reviewer_retry_sanctioned`]). Opened by the ask_user
    /// interception when a reviewer failure was pending; consumed by any
    /// reviewer spawn (checked in `reviewer_spawn_gate`, dispatch.rs).
    pub fn reviewer_retry_sanctioned(&self) -> bool {
        *self
            .reviewer_retry_sanctioned
            .lock()
            .expect("reviewer_retry_sanctioned lock poisoned")
    }

    /// Set the failed-reviewer retry sanction.
    pub fn set_reviewer_retry_sanctioned(&self, sanctioned: bool) {
        *self
            .reviewer_retry_sanctioned
            .lock()
            .expect("reviewer_retry_sanctioned lock poisoned") = sanctioned;
    }

    /// A handle to the provider, for consolidation's LLM call.
    pub fn provider(&self) -> Arc<dyn LlmClient> {
        self.provider
            .read()
            .expect("provider lock poisoned")
            .clone()
    }

    /// A handle to the context manager, for manual compaction (`/compact`).
    /// Clones the `ContextManager` (cheap — two `usize`s) so the caller can
    /// call `summarize_with_interrupt` without holding the lock.
    pub fn context_manager(&self) -> super::context::ContextManager {
        self.context_manager
            .read()
            .expect("context_manager lock poisoned")
            .clone()
    }

    /// Swap in a new provider + context manager (e.g. when the user switches
    /// models from the status bar). Takes effect on the next turn — a turn
    /// already in flight finishes against the provider it started with (each
    /// turn snapshots the provider into a local at the start).
    ///
    /// When this is an EXPLICIT per-agent picker switch (the
    /// [`set_explicit_provider`](Self::set_explicit_provider) variant), the
    /// new provider additionally pins THIS agent's turn resolution ahead of
    /// forced / subagent / workflow-state overrides, so the picker's choice
    /// survives to the next turn (a configured skill override still beats
    /// the pin). The pin is STATE-SCOPED (2026-12-20) — see
    /// [`set_explicit_provider`] for where it yields to a configured
    /// `[models.*]` slot on a workflow-state change. Plain `set_provider`
    /// remains the *default* slot swap (factory rebuilds, global console
    /// switches) and never pins anything.
    pub fn set_provider(
        &self,
        provider: Arc<dyn LlmClient>,
        context_manager: super::context::ContextManager,
    ) {
        *self.provider.write().expect("provider lock poisoned") = provider;
        *self
            .context_manager
            .write()
            .expect("context_manager lock poisoned") = context_manager;
    }

    /// Pin an explicit provider + context manager for THIS agent — the
    /// per-agent model picker's live switch (`set_model` with an
    /// `agent_id`). Also swaps the default slot (like [`set_provider`]) so
    /// `provider()` reports the new model and non-resolving paths see it;
    /// the pinned provider is what [`resolve_turn_provider`](Self::resolve_turn_provider)
    /// returns ahead of forced / subagent / workflow-state overrides. A
    /// configured `[models.skill.<name>]` override still beats the pin.
    ///
    /// STATE-SCOPED (2026-12-20 amendment of the original "never cleared"
    /// rule): the pin serves in the workflow state it was first served in
    /// (tracked as `pinned_state`) and in any other state only while no
    /// configured `[models.*]` model would take over there — on a state
    /// change with a configured slot, the configured model takes over (the
    /// pin stays dormant, resuming if the workflow returns to its state).
    /// Never deleted by a global factory/console swap (which uses plain
    /// [`set_provider`]) — an explicit per-agent choice is not silently
    /// undone.
    ///
    /// **Deferral:** when the new provider's `max_context` is SMALLER than the
    /// current one, the swap is deferred — stored in [`pending_swap`](Self::pending_swap)
    /// — so the turn loop can summarize the conversation using the OLD provider
    /// before completing the swap. This prevents a too-large conversation from
    /// being sent to a smaller-context model (which would fail with a 400/502).
    /// The turn loop calls [`take_pending_swap`](Self::take_pending_swap) at the
    /// top of `run_turn` to complete the swap after summarization.
    pub fn set_explicit_provider(
        &self,
        provider: Arc<dyn LlmClient>,
        context_manager: super::context::ContextManager,
    ) {
        // A fresh pick discards the old stamp: the new pin is re-stamped
        // with the state of its first serve (see `pinned_state`). Placed at
        // the top so it covers both the immediate path and the deferred
        // path — the deferred swap completes by writing `explicit_provider`
        // directly in `run_turn` (same pick), and its first serve re-stamps.
        *self
            .pinned_state
            .lock()
            .expect("pinned_state lock poisoned") = None;
        let current_max = self
            .context_manager
            .read()
            .expect("context_manager lock poisoned")
            .max_tokens();
        let new_max = provider.capabilities().max_context;
        if new_max < current_max {
            // Smaller context — defer the swap so the turn loop can summarize
            // using the OLD provider first.
            *self
                .pending_swap
                .lock()
                .expect("pending_swap lock poisoned") = Some(PendingSwap {
                model: provider.model().to_string(),
                provider,
                context_manager,
            });
            return;
        }
        self.set_provider(provider.clone(), context_manager.clone());
        // Clear any stale deferred swap — a later immediate swap must not be
        // overridden by a previously-deferred one the user superseded.
        *self
            .pending_swap
            .lock()
            .expect("pending_swap lock poisoned") = None;
        *self
            .explicit_provider
            .write()
            .expect("explicit_provider lock poisoned") = Some((provider, context_manager));
    }

    /// Whether a deferred provider swap is pending (set by
    /// [`set_explicit_provider`](Self::set_explicit_provider) when the new
    /// model has a smaller context window). Checked by the IPC layer so it can
    /// emit a user-facing note instead of `ModelChanged`.
    pub fn has_pending_swap(&self) -> bool {
        self.pending_swap
            .lock()
            .expect("pending_swap lock poisoned")
            .is_some()
    }

    /// Take (and clear) a pending deferred swap. Called at the top of
    /// `run_turn` — if `Some`, the turn loop summarizes (if needed) using the
    /// OLD provider, then completes the swap via `set_provider` +
    /// `set_explicit_provider`.
    pub fn take_pending_swap(&self) -> Option<PendingSwap> {
        self.pending_swap
            .lock()
            .expect("pending_swap lock poisoned")
            .take()
    }

    /// On a 429 (rate limit), try to switch to the same model on a different
    /// endpoint — the automatic cross-provider fallback.
    ///
    /// Reads the model id + endpoint name that the most recent turn resolved
    /// (via [`resolved_model`](Self::resolved_model) /
    /// [`effective_provider_name`](Self::effective_provider_name)), asks the
    /// resolver for an alternate endpoint serving the same model id (excluding
    /// the one that just 429'd), validates that it can be built, and records
    /// it in the `fallback_endpoints` map — endpoint
    /// stickiness consulted at provider-build time by
    /// [`resolve_turn_provider`](Self::resolve_turn_provider), on every
    /// resolution path (skill, the explicit picker pin, state/subagent
    /// overrides, forced models, and the default provider), so the retry
    /// iteration reroutes to the alternate endpoint
    /// for the SAME model id. It must NOT pin via
    /// [`set_explicit_provider`](Self::set_explicit_provider): a picker pin
    /// outranks the resolver chain in its own state (and in every state
    /// without a configured slot), so one 429 froze the agent on the
    /// fallback model across state changes and phase model switching
    /// stopped working (2026-12-05 bug).
    ///
    /// Returns the from/to endpoint names + model when a fallback was found
    /// and recorded (the caller emits an informative switching note). Returns
    /// `None` when no resolver is attached, no model/endpoint was resolved,
    /// no alternate endpoint serves the model, the provider couldn't be
    /// built, or the alternate's context window can neither hold the live
    /// conversation plus headroom (its own summarize threshold) nor match
    /// the current endpoint's window (no sticky entry is recorded in that
    /// case; see the viability check in the body) — the caller surfaces an
    /// actionable error / asks the user.
    ///
    /// At most one fallback per `run_turn_attempt` call (the caller tracks a
    /// `tried_fallback` flag). If the fallback endpoint also 429s, the caller
    /// goes straight to final failure rather than cascading through every
    /// endpoint. A LATER turn may fall back again — the sticky entry is
    /// overwritten to exclude the endpoint that just failed, which lets the
    /// routing flip back once the rate limit clears.
    pub(crate) fn try_429_fallback(&self) -> Option<FallbackInfo> {
        let resolver = self.model_resolver.as_ref()?;
        // The model id that just served (and 429'd). When a per-context
        // override resolved this turn, `resolved_model()` carries it. When the
        // turn ran on the DEFAULT provider (no override — the most common
        // configuration), `resolved_model()` is `None`, so fall back to the
        // default-slot provider's model id — mirroring `effective_provider_name()`,
        // which already falls back to `provider().provider_name()` on the next
        // line. Without this, a 429 on the default provider would silently
        // no-op (return None) even when a backup endpoint serving the same
        // model is configured.
        let model = self.resolved_model().or_else(|| {
            let m = self.provider().model().to_string();
            (!m.is_empty()).then_some(m)
        })?;
        let current = self.effective_provider_name()?;
        let alt = resolver.find_alternate_endpoint(&model, &current)?;
        // The alternate must actually be buildable AND able to hold the live
        // conversation — if not, treat it as "no viable alternate" (the
        // caller fails the turn with a clear message instead of retrying
        // into the same 429). Viability is decided against the LIVE token
        // count recorded at the top of the request iteration (backlog
        // a117e827): the alternate's window must hold the live conversation
        // plus headroom ≈ the alternate's OWN effective summarize threshold,
        // so the immediate serve plus one growth step fits before the
        // alternate's compaction would fire (its preflight hard-ceiling
        // check + compaction remain the last line of defense for further
        // growth). The OLD check compared context WINDOWS only — a 429 on a
        // 200k-window endpoint never failed over to a 128k alternate
        // serving the same model id even when the live conversation was a
        // handful of tokens, silently disabling failover for mixed-window
        // configurations. The headroom rule alone is NOT enough: with
        // fill_rate > 0.5 its bar (live ≤ alt×(1−fill)) sits BELOW the
        // incumbent's own operating band, so it would reject equal- and
        // larger-window alternates the window comparison always served —
        // the OR keeps every pre-fix acceptance (strictly dominant). When
        // no live count exists yet (0 — the 429 escaped before any request
        // was built), only the conservative window comparison applies.
        // Treating a not-viable alternate this way records no sticky
        // entry: the turn fails once with the actionable message and later
        // turns keep the original endpoint.
        let alt_cm = resolver.build_turn_provider(&alt, self.fill_rate)?.1;
        let cur_window = self
            .context_manager
            .read()
            .expect("context_manager lock poisoned")
            .max_tokens();
        let live = self.live_token_count.load(std::sync::atomic::Ordering::Relaxed);
        let viable = if live > 0 {
            alt_cm.max_tokens() >= live + alt_cm.effective_summarize_at()
                || alt_cm.max_tokens() >= cur_window
        } else {
            alt_cm.max_tokens() >= cur_window
        };
        if !viable {
            return None;
        }
        // Record ENDPOINT stickiness — never a provider pin. The map only
        // reroutes this model id away from the endpoint that just 429'd;
        // resolve_turn_provider applies it at build time, so
        // [models.planning]/[models.executing]/[models.complete]
        // overrides keep switching models after a 429 (the reviewing slot is
        // reviewer-spawn-only — ModelResolver::resolve_reviewer_model — so
        // it never rides this per-turn chain). A
        // stored ModelRef (not a built provider) picks up config reloads.
        self.fallback_endpoints
            .write()
            .expect("fallback_endpoints lock poisoned")
            .insert(model.clone(), (current.clone(), alt.clone()));
        Some(FallbackInfo {
            from_endpoint: current,
            to_endpoint: alt.endpoint,
            model,
        })
    }

    /// A handle to the vision model (if any), for image-to-text when the main
    /// LLM is not multimodal. In production this is the shared
    /// [`SwappableVision`](crate::provider::vision::SwappableVision) so
    /// Settings rewires take effect without rebuilding the loop.
    pub fn vision_handle(&self) -> Option<Arc<dyn crate::provider::vision::ImageDescriber>> {
        self.vision.clone()
    }

    /// Whether the main provider is multimodal (accepts image inputs).
    pub fn is_multimodal(&self) -> bool {
        self.provider
            .read()
            .expect("provider lock poisoned")
            .capabilities()
            .multimodal
    }

    /// Build the user-message content for a text + images payload — the ONE
    /// image-handling path, shared by the normal prompt arm and every steer
    /// injection site so a steered image rides the exact same handling as a
    /// prompt's:
    /// - No images → plain text.
    /// - Multimodal provider (or no vision client) → multipart message (text
    ///   block + image_url blocks); a provider that can't use image blocks
    ///   strips them itself.
    /// - Text-only provider with a vision client → describe each image via
    ///   the vision model and fold the descriptions into the text, so the
    ///   model "sees" the images. Each round-trip is announced via
    ///   VisionDescribe / VisionDescribed so the transcript shows an "image
    ///   parsing" card (with the query + response) instead of pausing
    ///   silently.
    pub(crate) async fn build_user_content(
        &self,
        agent_id: crate::runtime::AgentId,
        fanin_tx: &tokio::sync::mpsc::Sender<(crate::runtime::AgentId, crate::runtime::AgentEvent)>,
        text: String,
        images: &[String],
    ) -> crate::provider::MessageContent {
        let vision = self.vision_handle();
        if images.is_empty() {
            crate::provider::MessageContent::text(text)
        } else if self.is_multimodal() || vision.is_none() {
            let mut parts = vec![crate::provider::ContentPart::Text { text }];
            for data_url in images {
                parts.push(crate::provider::ContentPart::ImageUrl {
                    image_url: crate::provider::ImageUrl {
                        url: data_url.clone(),
                    },
                });
            }
            crate::provider::MessageContent::Parts(parts)
        } else {
            // Text-only provider + vision fallback: describe each image and
            // fold the descriptions into the user text.
            let vision = vision.expect("checked above");
            let total = images.len();
            let query = crate::tool::agent::image_tools::DEFAULT_DESCRIBE_PROMPT;
            let mut full_text = text;
            for (i, data_url) in images.iter().enumerate() {
                // Announce the round-trip BEFORE it starts so the UI can
                // show "image parsing" instead of silence.
                let _ = fanin_tx
                    .send((
                        agent_id,
                        crate::runtime::AgentEvent::VisionDescribe {
                            index: i + 1,
                            total,
                            query: query.to_string(),
                        },
                    ))
                    .await;
                let outcome = crate::tool::agent::image_tools::describe_image_data_url(
                    vision.as_ref(),
                    data_url,
                    None,
                )
                .await;
                let note = match &outcome {
                    Ok(desc) => {
                        format!("\n\n[image {} description: {}]", i + 1, desc)
                    }
                    Err(e) => {
                        format!("\n\n[image {}: description unavailable: {}]", i + 1, e)
                    }
                };
                // Pair the announcement with the answer (success or failure)
                // so the transcript card completes.
                let _ = fanin_tx
                    .send((
                        agent_id,
                        crate::runtime::AgentEvent::VisionDescribed {
                            index: i + 1,
                            total,
                            success: outcome.is_ok(),
                            description: match &outcome {
                                Ok(desc) => desc.clone(),
                                Err(e) => e.to_string(),
                            },
                        },
                    ))
                    .await;
                full_text.push_str(&note);
            }
            crate::provider::MessageContent::text(full_text)
        }
    }

    /// Describe an image using the vision model, for image-to-text when the
    /// main LLM is not multimodal. Delegates to the configured vision client.
    ///
    /// Returns an error if no vision client is configured. The caller should
    /// check [`is_multimodal`](Self::is_multimodal) first — if the main
    /// provider is multimodal, image blocks are sent directly and this method
    /// is unnecessary.
    pub async fn describe_image(&self, image_url: &str, prompt: &str) -> Result<String> {
        let vision = self.vision.as_ref().ok_or_else(|| {
            crate::error::Error::InvalidInput(
                "no vision client configured — cannot describe image".into(),
            )
        })?;
        vision.describe_image(image_url, prompt).await
    }

    /// A handle to this agent's workflow (plan state), so the IPC layer can
    /// read a specific agent's plan via `get_workflow_state(agent_id)`. Each
    /// agent owns its own `Workflow` (built by the `AgentLoopFactory`), so
    /// this is per-agent, not shared.
    pub fn workflow_handle(&self) -> Arc<tokio::sync::Mutex<crate::workflow::Workflow>> {
        Arc::clone(&self.workflow)
    }

    /// Whether the workflow is in a state where the plan loop expects the
    /// agent to keep making progress — Executing (an in-progress plan with
    /// steps still to complete) or Reviewing (the closing sequence: review →
    /// fix → commit → finish). Used by the auto-continue logic in
    /// `run_turn_with_retry` to decide whether to push a synthetic "continue"
    /// message after a normal turn end (Finish::Stop) instead of parking for
    /// user input. Reviewing must self-resume too: a turn that ends
    /// mid-closing-sequence there used to park until the user typed "c"
    /// (observed live 2026-12-31, twice — the agent stalled after a garbled
    /// tool result and waited for a manual nudge). Planning additionally
    /// expects progress when `unattended` is true (a run-all dispatched
    /// agent — its Prompt embedded the unattended preamble): a premature
    /// turn end mid-exploration would otherwise park with nobody watching
    /// and halt the whole run (backlog a6a7727a, live 2027-01-07).
    /// Interactive Planning still parks — a deliberate turn end there is
    /// usually a question-wait for the user, and a synthetic continue
    /// would self-answer it. Any other state (Complete, Skill, Subagent)
    /// is terminal/overlay/role — no synthetic turns there. Subagent in
    /// particular is a parented sub-agent's role state, never a
    /// plan-lifecycle phase, so it can never expect progress on its own.
    pub async fn workflow_expects_progress(&self, unattended: bool) -> bool {
        let wf = self.workflow.lock().await;
        let state = wf.state();
        matches!(
            state,
            crate::workflow::WorkflowState::Executing | crate::workflow::WorkflowState::Reviewing
        ) || (unattended && state == crate::workflow::WorkflowState::Planning)
    }

    /// The workflow state as a short evidence label (e.g. "Reviewing") —
    /// carried by [`crate::runtime::channels::AgentEvent::Parked`] so the
    /// watchdog ring and the UI can see the pre-stall state without
    /// re-deriving it.
    pub async fn workflow_state_label(&self) -> String {
        let wf = self.workflow.lock().await;
        format!("{:?}", wf.state())
    }

    /// Whether this agent has spawned background agents (reviewers, parallel
    /// workers) that are still running. Consulted by the auto-continue logic
    /// in `run_turn_with_retry` so a parent whose turn ends while its
    /// descendants run PARKS instead of synthesizing "continue" turns — the
    /// state machine refuses every workflow transition while subagents are
    /// running, so those turns could make no plan progress; the
    /// child-completion Suggestion is the designed resume. Mirrors the
    /// dispatch state-transition gate (same field, same semantics). `false`
    /// when no tracker is wired (tests and pre-wiring loops enforce no gate,
    /// exactly like dispatch).
    pub async fn has_running_descendants(&self) -> bool {
        if let Some(tracker) = &self.descendant_tracker {
            if let Some(id) = self.agent_id() {
                return tracker.has_running_descendants(id).await;
            }
        }
        false
    }

    /// A reference to this agent's tool registry, so the IPC spawner can
    /// compute a subagent's tool allow-list as a subset of this agent's
    /// currently-registered tools (intersected with the workflow filter at
    /// spawn time). The registry is per-agent (built by the factory).
    pub fn tools(&self) -> &crate::tool::ToolRegistry {
        &self.tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::AgentCommand as C;

    fn steer(s: &str) -> crate::runtime::AgentCommand {
        C::Suggestion(s.into())
    }

    /// A steer carrying one image attachment (base64 data URL).
    fn steer_with_images(s: &str, images: &[&str]) -> crate::runtime::AgentCommand {
        C::Suggestion(crate::runtime::SteerPayload {
            text: s.into(),
            images: images.iter().map(|i| i.to_string()).collect(),
        })
    }

    fn cancel(s: &str) -> crate::runtime::AgentCommand {
        C::CancelSuggestion(s.into())
    }

    #[test]
    fn fold_accumulates_steers_in_order() {
        // The core never-drop rule: successive steers accumulate into one list.
        let mut reason: Option<StopReason> = None;
        StopReason::fold(&mut reason, steer("one"));
        StopReason::fold(&mut reason, steer("two"));
        StopReason::fold(&mut reason, steer("three"));
        assert_eq!(
            reason,
            Some(StopReason::Steer(vec![
                "one".into(),
                "two".into(),
                "three".into()
            ]))
        );
    }

    #[test]
    fn fold_carries_images_with_steers_in_order() {
        // REGRESSION (steered images dropped, 2027-01-07): a steer's images
        // must ride the fold — the payload (text + images) accumulates, not
        // a bare String, so the injection site can build image blocks.
        let mut reason: Option<StopReason> = None;
        StopReason::fold(&mut reason, steer("text only"));
        StopReason::fold(
            &mut reason,
            steer_with_images("with image", &["data:image/png;base64,AAA"]),
        );
        StopReason::fold(
            &mut reason,
            steer_with_images("with two", &["data:image/png;base64,BBB", "data:image/png;base64,CCC"]),
        );
        assert_eq!(
            reason,
            Some(StopReason::Steer(vec![
                crate::runtime::SteerPayload {
                    text: "text only".into(),
                    images: vec![]
                },
                crate::runtime::SteerPayload {
                    text: "with image".into(),
                    images: vec!["data:image/png;base64,AAA".into()]
                },
                crate::runtime::SteerPayload {
                    text: "with two".into(),
                    images: vec![
                        "data:image/png;base64,BBB".into(),
                        "data:image/png;base64,CCC".into()
                    ]
                },
            ]))
        );
    }

    #[test]
    fn fold_prompt_mid_stream_carries_images() {
        // REGRESSION (steered images dropped, 2027-01-07): a Prompt folded
        // mid-stream rides the steer pipeline — its images must ride along
        // too (previously the fold discarded them via `..`).
        let mut reason: Option<StopReason> = None;
        StopReason::fold(
            &mut reason,
            C::Prompt {
                text: "look at this".into(),
                images: vec!["data:image/png;base64,AAA".into()],
            },
        );
        assert_eq!(
            reason,
            Some(StopReason::Steer(vec![crate::runtime::SteerPayload {
                text: "look at this".into(),
                images: vec!["data:image/png;base64,AAA".into()]
            }]))
        );
    }

    #[test]
    fn fold_cancel_by_text_still_drops_image_bearing_steer() {
        // CancelSuggestion stays text-match based (the documented
        // limitation): an image-bearing steer is cancelled by its text.
        let mut reason = Some(StopReason::Steer(vec![
            crate::runtime::SteerPayload {
                text: "keep me".into(),
                images: vec!["data:image/png;base64,AAA".into()],
            },
            crate::runtime::SteerPayload {
                text: "drop me".into(),
                images: vec!["data:image/png;base64,BBB".into()],
            },
        ]));
        StopReason::fold(&mut reason, cancel("drop me"));
        assert_eq!(
            reason,
            Some(StopReason::Steer(vec![crate::runtime::SteerPayload {
                text: "keep me".into(),
                images: vec!["data:image/png;base64,AAA".into()]
            }]))
        );
    }

    #[test]
    fn fold_interrupt_preserves_queued_steers() {
        // Stop pressed with commands queued: Interrupt becomes
        // InterruptWithSteers, keeping the queued commands (the user's core
        // complaint — Stop must not drop pending commands).
        let mut reason = Some(StopReason::Steer(vec!["queued".into()]));
        StopReason::fold(&mut reason, C::Interrupt);
        assert_eq!(
            reason,
            Some(StopReason::InterruptWithSteers(vec!["queued".into()]))
        );
    }

    #[test]
    fn fold_interrupt_with_no_steers_is_plain_interrupt() {
        let mut reason: Option<StopReason> = None;
        StopReason::fold(&mut reason, C::Interrupt);
        assert_eq!(reason, Some(StopReason::Interrupt));
    }

    #[test]
    fn fold_compact_preserves_queued_steers() {
        // `/compact` with commands queued mid-turn: becomes CompactWithSteers
        // so the commands run on the compacted conversation (Low 3).
        let mut reason = Some(StopReason::Steer(vec!["queued".into()]));
        StopReason::fold(&mut reason, C::Compact);
        assert_eq!(
            reason,
            Some(StopReason::CompactWithSteers(vec!["queued".into()]))
        );
    }

    #[test]
    fn fold_compact_with_no_steers_is_plain_compact() {
        let mut reason: Option<StopReason> = None;
        StopReason::fold(&mut reason, C::Compact);
        assert_eq!(reason, Some(StopReason::Compact));
    }

    #[test]
    fn fold_clear_drops_queued_steers() {
        // `/new` wipes the conversation — queued commands are intentionally
        // dropped (running them against wiped history would be wrong).
        let mut reason = Some(StopReason::Steer(vec!["queued".into()]));
        StopReason::fold(&mut reason, C::Clear);
        assert_eq!(reason, Some(StopReason::Clear));
    }

    #[test]
    fn fold_cancel_drops_queued_steers() {
        // Cancel exits the agent task — queued commands are moot.
        let mut reason = Some(StopReason::Steer(vec!["queued".into()]));
        StopReason::fold(&mut reason, C::Cancel);
        assert_eq!(reason, Some(StopReason::Cancel));
    }

    #[test]
    fn fold_steer_into_hard_signal_preserves_the_carrying_list() {
        // A steer arriving AFTER an InterruptWithSteers appends to its list
        // (the hard signal is not downgraded, but the command is kept).
        let mut reason = Some(StopReason::InterruptWithSteers(vec!["first".into()]));
        StopReason::fold(&mut reason, steer("second"));
        assert_eq!(
            reason,
            Some(StopReason::InterruptWithSteers(vec![
                "first".into(),
                "second".into()
            ]))
        );
    }

    #[test]
    fn fold_suggestion_into_interrupt_promotes_to_interrupt_with_steers() {
        // The grace-window seam (2026-12-30 review finding 1, plan edfff8d9):
        // the user presses Stop mid-tool-execution, then types a follow-up
        // while the drain window is open — the pre-inject drain folds the
        // queued steer into Some(Interrupt). Without the promotion the steer
        // was silently discarded (the old `_ => {}` arm); now it becomes
        // InterruptWithSteers and drives the follow-up turn.
        let mut reason = Some(StopReason::Interrupt);
        StopReason::fold(&mut reason, steer("actually do X instead"));
        assert_eq!(
            reason,
            Some(StopReason::InterruptWithSteers(vec![
                "actually do X instead".into()
            ]))
        );
    }

    #[test]
    fn fold_suggestion_into_compact_promotes_to_compact_with_steers() {
        // Same seam for /compact: the steer runs against the compacted
        // conversation instead of vanishing.
        let mut reason = Some(StopReason::Compact);
        StopReason::fold(&mut reason, steer("queued"));
        assert_eq!(
            reason,
            Some(StopReason::CompactWithSteers(vec!["queued".into()]))
        );
    }

    #[test]
    fn fold_suggestion_into_cancel_still_drops() {
        // Cancel exits the agent task — a steer folded into it stays dropped
        // (documented intent, mirrors fold_cancel_drops_queued_steers).
        let mut reason = Some(StopReason::Cancel);
        StopReason::fold(&mut reason, steer("queued"));
        assert_eq!(reason, Some(StopReason::Cancel));
    }

    #[test]
    fn fold_cancel_suggestion_drops_the_matching_steer() {
        // The "x" on a pending steer: cancelling the only queued steer
        // collapses Steer([]) -> None so the turn continues (no soft-stop,
        // no follow-up turn, no SuggestionInjected).
        let mut reason = Some(StopReason::Steer(vec!["do this".into()]));
        StopReason::fold(&mut reason, cancel("do this"));
        assert_eq!(reason, None);
    }

    #[test]
    fn fold_cancel_suggestion_preserves_other_steers() {
        // Only the matching text is removed; the rest still drive the turn.
        let mut reason = Some(StopReason::Steer(vec!["keep me".into(), "drop me".into()]));
        StopReason::fold(&mut reason, cancel("drop me"));
        assert_eq!(reason, Some(StopReason::Steer(vec!["keep me".into()])));
    }

    #[test]
    fn fold_cancel_suggestion_collapses_interrupt_with_steers_to_interrupt() {
        // Stop was pressed with a queued steer, then the steer was cancelled:
        // the stop still happens (Interrupt), just no command runs after.
        let mut reason = Some(StopReason::InterruptWithSteers(vec!["queued".into()]));
        StopReason::fold(&mut reason, cancel("queued"));
        assert_eq!(reason, Some(StopReason::Interrupt));
    }

    #[test]
    fn fold_cancel_suggestion_collapses_compact_with_steers_to_compact() {
        // `/compact` was pressed with a queued steer, then the steer was
        // cancelled: the compaction still happens (Compact), just no command
        // runs after.
        let mut reason = Some(StopReason::CompactWithSteers(vec!["queued".into()]));
        StopReason::fold(&mut reason, cancel("queued"));
        assert_eq!(reason, Some(StopReason::Compact));
    }

    #[test]
    fn fold_cancel_suggestion_unknown_text_is_noop() {
        // Cancelling a text that isn't queued changes nothing.
        let mut reason = Some(StopReason::Steer(vec!["do this".into()]));
        StopReason::fold(&mut reason, cancel("something else"));
        assert_eq!(reason, Some(StopReason::Steer(vec!["do this".into()])));
    }

    #[test]
    fn stable_head_caches_across_calls_until_constitution_changes() {
        // L4: the stable system-prompt head is byte-stable across turns — it
        // only changes when agent.md is edited. stable_head() caches it and
        // rebuilds only when the constitution reloads. This test verifies the
        // observable contract: two calls with no file change return identical
        // output, and after editing agent.md the output changes.
        use crate::project::ConstitutionSource;
        use tempfile::tempdir;

        let g = tempdir().unwrap();
        let p = tempdir().unwrap();
        let gpath = g.path().join("agent.md");
        let ppath = p.path().join("agent.md");
        std::fs::write(&gpath, "GLOBAL v1").unwrap();
        std::fs::write(&ppath, "PROJECT v1").unwrap();

        let src = ConstitutionSource::new(&gpath, &ppath).unwrap();
        let holder = ConstitutionHolder::Source {
            source: std::sync::Mutex::new(src),
            cached_head: std::sync::Mutex::new(None),
        };

        // First call builds and caches the head.
        let head1 = holder.stable_head();
        // Second call with no file change returns the cached head (identical).
        let head2 = holder.stable_head();
        assert_eq!(
            head1, head2,
            "head must be cached across calls when constitution unchanged"
        );

        // Edit the project constitution and bump the mtime explicitly
        // (deterministic on every filesystem — see source_rereads_after_mtime_change).
        std::fs::write(&ppath, "PROJECT v2").unwrap();
        {
            let f = std::fs::File::options().write(true).open(&ppath).unwrap();
            let times = std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60));
            f.set_times(times).unwrap();
        }

        // Third call rebuilds because the constitution changed.
        let head3 = holder.stable_head();
        assert_ne!(head3, head1, "head must rebuild when constitution changes");
    }
}
