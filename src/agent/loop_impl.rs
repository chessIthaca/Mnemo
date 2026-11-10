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

use super::budget;
use super::failure_triage;
use super::model_routing;
use super::reflex;
use super::step_lanes;
use super::step_verify;
use crate::config::{BudgetConfig, SafetyMode};
use crate::error::Result;
use crate::memory::MemoryStoreTrait;
use crate::provider::{FinishReason, LlmClient};
use crate::safety_rules::SafetyRules;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::ToolRegistry;

/// The explicit picker pin (2a.6 G2 hot-reload): the exact `(client,
/// context)` pair an explicit pick installs, swappable in place so the
/// running loop adopts it without a rebuild. `None` = no pin installed.
pub(crate) type ExplicitProvider =
    Arc<RwLock<Option<(Arc<dyn LlmClient>, super::context::ContextManager)>>>;

/// The `(model, display effort, wire effort)` snapshot of the request
/// last actually SERVED — the trio `served_effective` holds behind one
/// mutex.
pub(crate) type ServedEffective =
    std::sync::Mutex<Option<(String, Option<String>, Option<String>)>>;

/// The once-per-session first-prompt routing lifecycle.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteOnce {
    /// No classifiable first prompt has been handled yet.
    Pending,
    /// The first prompt was classified; later prompts may only nudge.
    Settled {
        /// Laya's answer, when it returned a choice.
        label: Option<String>,
        /// The calibrated confidence of that answer.
        confidence: Option<f64>,
        /// The model spec installed by an enforced route, if any.
        model: Option<String>,
    },
    /// Routing is permanently off for this session incarnation.
    Off {
        /// `too_short`, `offline`, `resumed`, `manual`, or `ended`.
        reason: &'static str,
    },
}

/// All route-once lifecycle fields whose transitions must be atomic with a
/// manual pick, Clear, or a first-classification commit. One std mutex protects
/// the complete state; detached drift tasks clone only its `Arc` and lock for
/// the final nudge claim, never across an await.
#[derive(Debug)]
pub(crate) struct RouteSessionState {
    /// The first-prompt lifecycle.
    pub(crate) route_once: RouteOnce,
    /// Invalidates classification/drift answers from an earlier incarnation.
    pub(crate) epoch: u64,
    /// Whether the one permitted post-route suggestion has been emitted.
    pub(crate) nudge_sent: bool,
    /// The outstanding nudge's target, retained for `nudge_ignored`.
    pub(crate) nudge_to_label: Option<String>,
}

impl RouteSessionState {
    fn new() -> Self {
        Self {
            route_once: RouteOnce::Pending,
            epoch: 0,
            nudge_sent: false,
            nudge_to_label: None,
        }
    }
}

/// Everything needed to publish a claimed nudge synchronously while the
/// route-state lock is still held. Owned so a detached drift task can carry it.
pub(crate) struct NudgeNotice {
    /// Agent receiving the notice and owning the log row.
    pub(crate) agent_id: crate::runtime::AgentId,
    /// `drift`, `tool_failures`, or `repeat_failure`.
    pub(crate) reason: &'static str,
    /// Human-readable model suggestion.
    pub(crate) text: String,
    /// Session log destination, when a routing gate is attached.
    pub(crate) log_path: Option<std::path::PathBuf>,
    /// Bounded event channel; publication uses `try_send`, never `.await`.
    pub(crate) fanin_tx: tokio::sync::mpsc::Sender<(
        crate::runtime::AgentId,
        crate::runtime::AgentEvent,
    )>,
}

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
    /// The recall-delta cache (token-optimizer lever `recall_delta`, backlog
    /// 30bacfa2): what the volatile tail has already injected in this
    /// session, so a repeat recall of an unchanged memory renders as a
    /// compact reference instead of a fresh 160-char snippet. Cleared
    /// whenever compaction rewrites the conversation (the earlier injection
    /// is gone with it).
    pub recall_delta: std::sync::Mutex<super::recall_delta::RecallDeltaCache>,
}

impl SessionState {
    /// A fresh session state (no session yet, no primer fetched, no Complete
    /// capture yet).
    fn new() -> Self {
        Self {
            session_id: std::sync::Mutex::new(None),
            primer: std::sync::Mutex::new(None),
            captured_complete_plan_id: std::sync::Mutex::new(None),
            recall_delta: std::sync::Mutex::new(
                super::recall_delta::RecallDeltaCache::default(),
            ),
        }
    }
}

/// The agent loop. Drives a conversation turn.
/// A deferred provider swap: stored when the user switches to a model whose
/// context manager cannot hold the LIVE conversation (see
/// [`AgentLoop::swap_needs_compaction`]). The turn loop completes the swap
/// after summarizing with the OLD provider, so the conversation fits the new
/// window. A switch whose conversation already fits swaps at once — no
/// `PendingSwap`, no summarization.
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

/// The enforcement half of G7 (§5.9.7): the limits plus the counters
/// they check. Owned by [`AgentLoop`] behind a `Mutex` — `run_turn`
/// calls `begin_request` at the top of every provider-request iteration
/// (no await while held).
pub(crate) struct SessionLimiter {
    /// The limits in force (all `None` = unlimited).
    limits: crate::runtime::channels::SessionLimits,
    /// Requests served so far this session (incremented by
    /// `begin_request`; provider-error retries count — every retry is a
    /// provider request).
    requests_served: u64,
    /// When the limits were applied (the session's wall-clock epoch).
    started_at: Option<std::time::Instant>,
}

impl SessionLimiter {
    fn unlimited() -> Self {
        Self {
            limits: crate::runtime::channels::SessionLimits::default(),
            requests_served: 0,
            started_at: None,
        }
    }

    /// Apply a fresh limits set: resets the counters + starts the
    /// wall-clock epoch (the session's limits epoch = when they were
    /// applied, i.e. the engine's spawn).
    fn apply(&mut self, limits: crate::runtime::channels::SessionLimits) {
        self.limits = limits;
        self.requests_served = 0;
        self.started_at = Some(std::time::Instant::now());
    }

    /// Admit the next provider request, or report the limit it hits
    /// (G7): `max_turns` counts requests served, `wall_clock_ms` the
    /// elapsed time since the limits were applied. On a hit the turn
    /// ends with `FinishReason::LimitReached` and the session parks —
    /// every later call hits the same limit again.
    pub(crate) fn begin_request(&mut self) -> Option<crate::provider::LimitKind> {
        if let Some(max) = self.limits.max_turns {
            if self.requests_served >= max {
                return Some(crate::provider::LimitKind::MaxTurns);
            }
        }
        if let (Some(budget), Some(started)) = (self.limits.wall_clock_ms, self.started_at) {
            if started.elapsed() >= std::time::Duration::from_millis(budget) {
                return Some(crate::provider::LimitKind::WallClock);
            }
        }
        self.requests_served += 1;
        None
    }
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
    /// The SESSION's temporary compaction raise (backlog 11513ee5): the
    /// cumulative +10% ladder applied when a compaction pass still does not
    /// fit. Owned by the loop — NOT by the context manager — because the turn
    /// loop takes a fresh manager clone per iteration and the workflow-state
    /// resolver builds a fresh one per iteration, so a field on the manager
    /// would lose the ladder mid-turn; both hand-outs get this handle installed
    /// ([`context_manager`](Self::context_manager),
    /// `resolve_iteration_provider`). Created per loop, so every agent/session
    /// starts at 1.0 and `/new` resets it.
    pub(crate) compaction_raise: Arc<super::context::CompactionRaise>,
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
    /// The token count of the conversation its owning task last PUBLISHED —
    /// what every compact-before-switch decision measures (see
    /// [`swap_needs_compaction`](Self::swap_needs_compaction)). Published when
    /// a transcript is seeded (a resume, BEFORE any request runs), before and
    /// after every turn (the turn's final message counts), and at the top of
    /// every request iteration — the points where a bare request-iteration
    /// count would be `0` (a resumed session that has not issued a request)
    /// or stale (after a turn's final message). Only the COUNT is kept: the
    /// conversation itself is never copied into the loop.
    pub(crate) live_message_tokens: std::sync::atomic::AtomicUsize,
    /// The tool-schema overhead (tokens) measured for the request surface of
    /// the last publish — the second half of the inclusive count the turn
    /// loop's summarize trigger uses. Seeded at construction from the
    /// registry's advertised schema block, so even a resumed session's first
    /// switch decision is measured inclusively rather than as message-only.
    pub(crate) live_schema_tokens: std::sync::atomic::AtomicUsize,
    pub(crate) tools: Arc<ToolRegistry>,
    /// The steering counters this loop reports to (2c.4/G4): injected by
    /// the factory via `with_steering` so every loop the same factory
    /// builds shares one instance — the process-wide singleton is
    /// deleted. A loop built without wiring keeps its own fresh,
    /// isolated set.
    pub(crate) steering: Arc<crate::agent::steering_stats::SteeringStats>,
    /// The approval policy (2c.5, §5.4): per-loop. Defaults in
    /// `from_config` to today's semantics (`ApproveViaApi` over the
    /// config's initial mode) so directly-built loops keep approval
    /// gating; the factory wires it explicitly, and the free agent's
    /// `OverridePermissions` is set via `with_permission` (the 3a
    /// Engine's spawn path). Under Override, dispatch skips the approval
    /// branch entirely, emitting `AgentEvent::ApprovalBypassed` for
    /// calls that would have been gated. See
    /// [`crate::agent::policy::PermissionPolicy`].
    pub(crate) permission: std::sync::RwLock<crate::agent::policy::PermissionPolicy>,
    /// G7 (§5.9.7): the session limits + their enforcement counters —
    /// [`set_limits`](Self::set_limits) applies them post-build (the
    /// `set_permission` precedent). All-`None` (the default) makes
    /// every check a no-op.
    pub(crate) limiter: std::sync::Mutex<SessionLimiter>,
    /// The agent's own workflow — held as the kernel-neutral handle (2b.3):
    /// the loop never names the app workflow type, and every consult rides
    /// the trait's opaque representations (the state token, the completion
    /// flag, the plan scope, the tool-filter envelope). The app side
    /// recovers the concrete lockable Arc through `as_any_arc`.
    pub(crate) workflow: Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>,
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
    /// Live `[general.optimizer]` config (token-optimizer levers, backlog
    /// e4a50d22), shared with the factory's tool builds. The ingestion-time
    /// levers (progressive disclosure of oversized results, compaction
    /// survival) read it per turn. `None` in tests / directly-built loops —
    /// every lever then stays off.
    pub(crate) optimizer: Option<Arc<RwLock<crate::config::OptimizerConfig>>>,
    /// Session counters behind lever 6's S–F quality score (backlog e4a50d22):
    /// fed by [`record_savings_event`](AgentLoop::record_savings_event), read
    /// when a `ContextUsage` event is emitted. Atomics, so the grade can be
    /// computed from `&self` without a lock.
    pub(crate) optimizer_state: super::optimizer::OptimizerState,
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
    /// Session-scoped dismissal of auto-recalled rows (backlog c68a8c3b):
    /// the record ids the agent dismissed via the `recall_feedback` tool.
    /// Shared with that tool — the factory mints ONE Arc per built loop and
    /// hands clones to both — so the tool's writes and the auto-recall
    /// injector's filter see the same set, and a dismissed row never
    /// re-injects for this session's remainder. Fresh per loop (a new
    /// session starts with nothing dismissed); a directly-built loop
    /// without wiring keeps an empty set, so recall is unfiltered.
    pub(crate) dismissed_recall:
        Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// This loop's trace identity (plan c3ba2b58 step A): who makes the
    /// loop's model calls, so each trace record can be attributed to the
    /// lead/worker/subagent it belongs to. The factory mints ONE handle per
    /// built loop and hands clones to both the loop and the memory tools
    /// (the `dismissed_recall` pattern) — the spawn path stamps it
    /// post-build (pre-registration, before any turn can run), and a loop
    /// built without a host identity keeps it empty forever (Mnemo1:
    /// records stay unattributed).
    pub(crate) trace_identity:
        Arc<crate::provider::trace::TraceIdentityHandle>,
    /// This agent's id in the runtime (`AgentManager`), used so the agent's
    /// `spawn_agent` tool can record itself as the parent of any background
    /// agent it spawns (for the completion-notification feedback loop).
    /// `None` until the factory assigns it at build time (and for tests that
    /// construct a loop directly without an id).
    pub(crate) agent_id: std::sync::Mutex<Option<crate::runtime::AgentId>>,
    /// The live event fan-in route (§5.7, job 3b.4): a clone of the agent
    /// task's own fan-in sender, injected by `AgentTask::run` on entry — the
    /// ONE task body every spawn path (GUI, console, headless host, tests)
    /// funnels through. Lets the loop's SYNC persistence methods
    /// (`record_spend_row` / `record_savings_event` in turn.rs) fan the live
    /// `Spend` / `Savings` events from exactly the points where their ledger
    /// rows persist, without threading a sender parameter through every
    /// helper (the async turn paths carry `fanin_tx` as a parameter
    /// instead). `None` for directly-built test loops that no runtime task
    /// drives — the live events then simply don't fan (the store writes are
    /// unaffected). The routing id comes from
    /// [`agent_id`](Self::agent_id), so the emission always carries the
    /// loop's own id.
    pub(crate) event_fanin:
        std::sync::Mutex<Option<tokio::sync::mpsc::Sender<crate::runtime::channels::FaninItem>>>,
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
    /// A pinned compaction-summary provider. When set it wins over
    /// `[models.summarize]` in [`summarize_provider`](Self::summarize_provider),
    /// so a test can stream a summary without a network.
    pub(crate) summarize_override: Option<Arc<dyn LlmClient>>,
    /// Whether this agent is a subagent (spawned via `spawn_agent` or the UI
    /// spawn button with a parent). Drives the `[models.subagent]` override
    /// in the resolver. `false` for the main agent. Behind a mutex so it can
    /// be set through a `&AgentLoop` reference (mirroring
    /// `plan_mutations_allowed`).
    pub(crate) is_subagent: std::sync::Mutex<bool>,
    /// Stable root of this agent family, inherited at spawn and unaffected by
    /// intermediate agents exiting or being removed from the manager.
    pub(crate) family_root: std::sync::Mutex<Option<crate::runtime::AgentId>>,
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
    pub(crate) explicit_provider: ExplicitProvider,
    /// Whether `explicit_provider` / `pending_swap` belongs to Laya rather
    /// than the human picker. External provider changes clear ONLY this pin;
    /// a human pin keeps its historical global-swap dormancy semantics.
    routed_pin: std::sync::Mutex<bool>,
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
    /// The state the explicit picker was first served in (2a.6), held as
    /// the handle's state TOKEN since 2b.3 — the opaque representation the
    /// trait hands out; the comparison semantics are unchanged (hold in the
    /// stamped state, cede on a state change with a configured slot). Reset
    /// to `None` by [`set_explicit_provider`](Self::set_explicit_provider)
    /// so a fresh pick re-stamps on its first serve; the pending-swap
    /// completion path in `run_turn` writes `explicit_provider` directly and
    /// intentionally leaves the stamp alone (same pick, completing).
    pub(crate) pinned_state: std::sync::Mutex<Option<String>>,
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
    /// The effective (model, endpoint, effort) triple that served the most
    /// recent provider request — the last value announced to the UI as
    /// `ModelChanged`. Unlike [`resolved_model`](Self::resolved_model) this
    /// is never overwritten by a swap or a pick; it is written only when a
    /// request is actually served, so the turn loop can tell a change that
    /// just landed (2027-01-25: model/reasoning changes land before the next
    /// call, like steers) from one still in flight. Seeded once from the
    /// pre-serve display state
    /// ([`seed_served_effective_if_empty`](Self::seed_served_effective_if_empty))
    /// so the first request does not re-announce what `list_agents` already
    /// reports.
    pub(crate) served_effective: ServedEffective,
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
    /// The optional pre-prompt routing gate (the Laya classifier's
    /// task-complexity consumer, `[general.laya] routing`, backlog 091e694d):
    /// the SAME shared classifier slot the other Laya consumers read. `None`
    /// in tests / when the Laya foundation is not wired -- the turn then never
    /// classifies, and model selection is byte-identical to today. The opt-in
    /// flag and the `[general.routing]` threshold / `enforce` are read from the
    /// live config by the model resolver per turn, so no flag mirror is needed
    /// here.
    pub(crate) routing: Option<model_routing::RoutingGate>,
    /// The optional compound-reflex handle (the Laya classifier's compound
    /// decision layer, `[general.laya] reflex`, backlog a8495cc1) -- the
    /// escalation lane ladder's classifier (backlog ad56c7bd). Carries the
    /// SAME shared classifier slot + the config-mirrored enable flag, both read
    /// at every plan-step boundary; `None` in tests / unwired builds, where the
    /// ladder never classifies and model selection stays byte-identical.
    pub(crate) reflex: Option<reflex::ReflexHandle>,
    /// The escalation-lane memo for the plan step being worked (backlog
    /// ad56c7bd): the rung the reflex call selected, under which escalation
    /// epoch. Compared against the live step at every iteration boundary, so
    /// the classifier is asked once per step per attempt.
    pub(crate) lane_state: std::sync::Mutex<Option<step_lanes::LaneState>>,
    /// The escalation epoch (backlog ad56c7bd): bumped by a confident
    /// permanent/flaky_test failure-triage verdict, which is what re-arms the
    /// lane classification for the SAME step. Never bumped on a first
    /// attempt's behalf, so the ladder cannot route up before a real failure.
    pub(crate) escalation_epoch: std::sync::atomic::AtomicU64,
    /// The failure class that bumped the epoch most recently -- its label feeds
    /// the re-classification state text.
    pub(crate) lane_failure: std::sync::Mutex<Option<String>>,
    /// The harness-run verification handle (the cost-saving chain's
    /// verification item): a decided `action=verify` reflex call re-runs
    /// `[general.verify]`'s command harness-side and stashes the compact
    /// evidence note for the next request's volatile tail. `None` in tests /
    /// unwired builds -- the action then does nothing.
    pub(crate) step_verify: Option<step_verify::StepVerifyHandle>,
    /// The one-shot evidence note for the next request's volatile tail:
    /// stashed by the action=verify path, drained by the request seam (so it
    /// is delivered exactly once and never pollutes the cached prefix).
    pub(crate) verify_note: std::sync::Mutex<Option<String>>,
    /// The budget layer's live config (backlog a25a5323, the cost-saving
    /// chain's budget item): the plan-step gate reads it at every boundary.
    /// Disabled by default — the gate then no-ops byte-identically. Shared
    /// with the factory, so a Settings save lands on the next boundary with
    /// no rebuild.
    pub(crate) budget: Arc<RwLock<BudgetConfig>>,
    /// The live per-plan spend counters (backlog a25a5323): tokens, armed
    /// escalations, per-step failed-cycle retries, the human's bypass and any
    /// doubled caps. In-memory and live-session scoped — the durable
    /// `spend_events` ledger backs the per-plan cost report.
    pub(crate) plan_spend: std::sync::Mutex<budget::PlanSpend>,
    /// A failed-cycle retry flagged by `note_lane_escalation`: applied to the
    /// step that runs next by the budget gate — the flag bridges the sync
    /// triage sites, which cannot lock the async workflow to learn the live
    /// step.
    pub(crate) pending_lane_retry: std::sync::atomic::AtomicBool,
    /// The attribution stamped for the next spend row (backlog a25a5323):
    /// which plan/step/lane the coming request belongs to and why the model
    /// runs (route/retry/escalate/default). Consumed once by the usage site
    /// that writes the row; overwritten each iteration, so a request that
    /// never reports usage cannot mis-attribute the next one.
    pub(crate) spend_attribution: std::sync::Mutex<Option<budget::SpendAttribution>>,
    /// The kernel→app policy seam (2a.2): the free agent by default
    /// ([`NoPolicy`](crate::agent::policy::NoPolicy)); the factory attaches the
    /// app's WorkflowPolicy for the workflow agent. Nothing consults it until
    /// 2a.3+.
    pub(crate) policy: std::sync::RwLock<Arc<dyn crate::agent::policy::AgentPolicy>>,
    /// The session's pinned task (plan 40eef714 step 1): the spawn path's
    /// assignment, kept verbatim in the stable head of every request's
    /// system message for the whole session — compaction summarizes
    /// conversation messages only, and a model switch rebuilds the head,
    /// so the task survives both byte-for-byte. `None` = ordinary session.
    pub(crate) pinned_task: std::sync::RwLock<Option<String>>,
    /// Plan 2ba2168a step 1: a session opted out of the pre-prompt routing
    /// gate at spawn — its turns are never classified and no routing.jsonl
    /// rows are written for them (Mnemo2 routes once at dispatch instead).
    /// `false` = the per-turn gate (Mnemo1, unchanged).
    pub(crate) skip_turn_routing: std::sync::atomic::AtomicBool,
    /// Mnemo1's route lifecycle, epoch, and nudge claim under ONE lock.
    pub(crate) route_session: Arc<std::sync::Mutex<RouteSessionState>>,
    /// Consecutive failed-or-repeated tool calls behind a stuck nudge.
    pub(crate) strikes: std::sync::Mutex<super::route_question::Strikes>,
    /// The previous signature, used only to choose the stuck-nudge wording.
    pub(crate) last_route_tool_signature: std::sync::Mutex<Option<String>>,
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
    ///
    /// `policy` supplies the head (2a.4 + 2c.1/G1): the factory-attached
    /// `WorkflowPolicy` produces today's bytes
    /// (`assemble_stable_head(&KernelPromptBlocks::workflow(), …)`), while
    /// `NoPolicy` composes the free head from its caller's `PromptSpec`
    /// (whose constitution mode filters the loaded sources).
    pub(crate) fn stable_head(&self, policy: &dyn crate::agent::policy::AgentPolicy) -> String {
        match self {
            ConstitutionHolder::Source {
                source,
                cached_head,
            } => {
                let mut src = source.lock().expect("constitution source lock poisoned");
                let changed = src.reload_if_changed();
                let mut cache = cached_head.lock().expect("head cache lock poisoned");
                if changed || cache.is_none() {
                    let head = policy.stable_head(
                        &super::policy::KernelPromptBlocks::workflow(),
                        src.constitution(),
                    );
                    *cache = Some(head.clone());
                    head
                } else {
                    cache
                        .clone()
                        .expect("head cache invariant: Some after is_none check")
                }
            }
            ConstitutionHolder::Static(c) => {
                policy.stable_head(&super::policy::KernelPromptBlocks::workflow(), c)
            }
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
            // G5 (3a.6): the export is a pure side-channel read — never a
            // steer; the fold leaves any current stop reason untouched.
            C::ExportHistory { .. } => {}
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
    /// The workflow handle (kernel-neutral since 2b.3 — the state machine
    /// lives app-side behind the trait).
    pub workflow: Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>,
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

    /// Wire the FACTORY's steering counters (2c.4/G4) into this loop so
    /// every loop the same factory builds reports to one instance-scoped
    /// set — the replacement for the deleted process-wide
    /// `SteeringStats::shared()` singleton.
    pub fn with_steering(
        mut self,
        steering: Arc<crate::agent::steering_stats::SteeringStats>,
    ) -> Self {
        self.steering = steering;
        self
    }

    /// Set the approval policy (2c.5, §5.4). The factory wires
    /// `ApproveViaApi` for GUI/console loops (today's semantics); the
    /// kernel default (`OverridePermissions`) is the free agent's.
    /// 3a.5: the field is RwLock-backed so a BUILT loop can swap it
    /// ([`AgentLoop::set_permission`]) — the same live-swap shape as
    /// `set_provider`.
    pub fn with_permission(
        mut self,
        permission: crate::agent::policy::PermissionPolicy,
    ) -> Self {
        self.permission = std::sync::RwLock::new(permission);
        self
    }

    /// The current approval policy (2c.5 → 3a.5: read through the lock —
    /// the engine's per-session spawn may have swapped it post-build).
    pub fn permission(&self) -> crate::agent::policy::PermissionPolicy {
        self.permission
            .read()
            .expect("permission lock poisoned")
            .clone()
    }

    /// Set the approval policy on a built loop — the 3a.5 Engine's
    /// spawn path (`spawn_agent_shared`) applies the per-session
    /// override after the factory build.
    pub fn set_permission(&self, permission: crate::agent::policy::PermissionPolicy) {
        *self.permission.write().expect("permission lock poisoned") = permission;
    }

    /// Set the session limits on a built loop (G7, §5.9.7) — the 3a
    /// Engine's spawn path applies the per-session limits after the
    /// factory build (the `set_permission` precedent). Resets the
    /// counters + starts the wall-clock epoch; `Default` limits (all
    /// `None`) disable enforcement.
    pub fn set_limits(&self, limits: crate::runtime::channels::SessionLimits) {
        self.limiter
            .lock()
            .expect("limiter lock poisoned")
            .apply(limits);
    }

    /// Inject the live fan-in route (§5.7, job 3b.4) — called by the agent
    /// task on entry (`AgentTask::run`, runtime/agent.rs), the ONE task body
    /// every spawn path funnels through, so the sync persistence methods'
    /// live `Savings`/`Spend` emissions ride this same task's fan-in
    /// sender. Mirrors the other post-build setters (`set_limits`,
    /// `set_permission`): the sender is minted per spawn, so it cannot
    /// flow through the constructor.
    pub(crate) fn set_event_fanin(
        &self,
        tx: tokio::sync::mpsc::Sender<crate::runtime::channels::FaninItem>,
    ) {
        *self.event_fanin.lock().expect("event_fanin lock poisoned") = Some(tx);
    }

    /// The live fan-in sender (3c/G6 tier 3): the external policy's hook
    /// emission path — `None` only before the agent task runs (the
    /// sender is minted per spawn) or after the task is gone.
    pub(crate) fn event_fanin_sender(
        &self,
    ) -> Option<tokio::sync::mpsc::Sender<crate::runtime::channels::FaninItem>> {
        self.event_fanin
            .lock()
            .expect("event_fanin lock poisoned")
            .clone()
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
        // The session's compaction-raise ladder: ONE handle for the loop, shared
        // by the stored manager and by every manager the loop hands out (see
        // the field docs). Created here, so each agent/session starts at 1.0.
        let compaction_raise = Arc::new(super::context::CompactionRaise::default());
        Self {
            provider: Arc::new(RwLock::new(provider)),
            context_manager: Arc::new(RwLock::new(
                context_manager.with_compaction_raise(Arc::clone(&compaction_raise)),
            )),
            compaction_raise,
            live_token_count: std::sync::atomic::AtomicUsize::new(0),
            live_message_tokens: std::sync::atomic::AtomicUsize::new(0),
            // Baseline schema overhead for a session that has not issued a
            // request yet (a resumed transcript): the advertised surface of a
            // normal working state. Each request iteration replaces it with the
            // exact measured value.
            live_schema_tokens: std::sync::atomic::AtomicUsize::new(
                crate::provider::estimate_tools_tokens(
                    &tools.schemas(
                        &crate::provider::Capabilities::openai(),
                        &crate::tool::ToolFilter::Executing,
                    ),
                ),
            ),
            steering: Arc::new(crate::agent::steering_stats::SteeringStats::new()),
            permission: std::sync::RwLock::new(
                crate::agent::policy::PermissionPolicy::ApproveViaApi {
                    safety_mode,
                },
            ),
            limiter: std::sync::Mutex::new(SessionLimiter::unlimited()),
            tools,
            workflow,
            sandbox,
            constitution,
            safety_mode: Arc::new(RwLock::new(safety_mode)),
            memory,
            optimizer: None,
            optimizer_state: super::optimizer::OptimizerState::default(),
            safety_rules: None,
            vision,
            session: SessionState::new(),
            dismissed_recall: Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            trace_identity: Arc::new(crate::provider::trace::TraceIdentityHandle::new()),
            agent_id: std::sync::Mutex::new(None),
            event_fanin: std::sync::Mutex::new(None),
            plan_mutations_allowed: std::sync::Mutex::new(true),
            model_resolver: None,
            summarize_override: None,
            is_subagent: std::sync::Mutex::new(false),
            family_root: std::sync::Mutex::new(None),
            reviewer_failure_pending: std::sync::Mutex::new(false),
            reviewer_retry_sanctioned: std::sync::Mutex::new(false),
            fill_rate: 0.5,
            forced_model: std::sync::Mutex::new(None),
            explicit_provider: Arc::new(RwLock::new(None)),
            routed_pin: std::sync::Mutex::new(false),
            pinned_state: std::sync::Mutex::new(None),
            fallback_endpoints: Arc::new(RwLock::new(std::collections::HashMap::new())),
            pending_swap: Arc::new(std::sync::Mutex::new(None)),
            resolved_model: std::sync::Mutex::new(None),
            resolved_provider: std::sync::Mutex::new(None),
            resolved_effort: std::sync::Mutex::new(None),
            served_effective: std::sync::Mutex::new(None),
            default_display_effort: std::sync::Mutex::new(None),
            pinned_display_effort: std::sync::Mutex::new(None),
            descendant_tracker: None,
            graph: None,
            last_review_report: std::sync::Mutex::new(None),
            plans_dir: std::path::PathBuf::new(),
            root_spec: None,
            failure_triage: None,
            routing: None,
            reflex: None,
            lane_state: std::sync::Mutex::new(None),
            escalation_epoch: std::sync::atomic::AtomicU64::new(0),
            lane_failure: std::sync::Mutex::new(None),
            step_verify: None,
            verify_note: std::sync::Mutex::new(None),
            budget: Arc::new(RwLock::new(BudgetConfig::default())),
            plan_spend: std::sync::Mutex::new(budget::PlanSpend::default()),
            pending_lane_retry: std::sync::atomic::AtomicBool::new(false),
            spend_attribution: std::sync::Mutex::new(None),
            // 2c.1/G1: the bare-loop default carries the kernel's EXPLICIT
            // coding default (never a silent assembly; the factory may
            // overwrite via set_policy).
            policy: std::sync::RwLock::new(Arc::new(
                crate::agent::policy::NoPolicy::with_coding_default(),
            )),
            // Plan 40eef714 step 1: unset until the spawn path stamps the
            // session's task (`set_pinned_task`) — every constructor starts
            // pinned-free.
            pinned_task: std::sync::RwLock::new(None),
            // Plan 2ba2168a step 1: false until the spawn path opts the
            // session out (`set_skip_turn_routing`) — every constructor
            // starts with the per-turn gate in force.
            skip_turn_routing: std::sync::atomic::AtomicBool::new(false),
            route_session: Arc::new(std::sync::Mutex::new(RouteSessionState::new())),
            strikes: std::sync::Mutex::new(super::route_question::Strikes::new(3)),
            last_route_tool_signature: std::sync::Mutex::new(None),
        }
    }

    /// Wire the per-loop dismissal set (backlog c68a8c3b): the factory
    /// mints ONE `Arc<Mutex<HashSet>>` per built loop and shares it with
    /// the `recall_feedback` tool registered for the same loop, so the
    /// tool's writes and the auto-recall injector's filter see the same
    /// per-session set. Directly-built loops keep the fresh empty set
    /// from `from_config` (recall unfiltered).
    pub fn with_dismissed_recall(
        mut self,
        dismissed: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    ) -> Self {
        self.dismissed_recall = dismissed;
        self
    }

    /// Wire the per-loop trace identity handle (plan c3ba2b58 step A): the
    /// factory mints ONE handle per built loop and shares it with the
    /// memory tools registered for the same loop, so a tool can stamp its
    /// own LLM calls with the calling agent's identity. Directly-built
    /// loops keep the fresh empty handle from `from_config` (unattributed).
    pub fn with_trace_identity(
        mut self,
        identity: Arc<crate::provider::trace::TraceIdentityHandle>,
    ) -> Self {
        self.trace_identity = identity;
        self
    }

    /// Stamp the loop's trace identity — called ONCE by the spawn path
    /// post-build (pre-registration, before any turn can run, so no
    /// request can race the stamp) with the spawn options' identity (or
    /// the parent's, for a subagent) and the runtime-allocated agent and
    /// session ids filled in. The identity is snapshotted per request
    /// from the handle, never read back from live state.
    pub fn set_trace_identity(&self, identity: crate::provider::trace::TraceIdentity) {
        self.trace_identity.set(identity);
    }

    /// Snapshot the loop's trace identity (`None` = unattributed loop —
    /// Mnemo1 paths, or a spawn without host identity).
    pub fn trace_identity(&self) -> Option<crate::provider::trace::TraceIdentity> {
        self.trace_identity.get()
    }

    /// Stamp the session's pinned task (plan 40eef714 step 1) — called
    /// ONCE by the spawn path post-build (pre-registration, before any
    /// turn can run, so no request can race the stamp). A whitespace-only
    /// task is stored as `None` (unset); otherwise the text is stored with
    /// trailing whitespace trimmed, and the stable-head builder renders
    /// it verbatim under `## Your task (pinned)` on every request.
    pub fn set_pinned_task(&self, task: Option<String>) {
        let task = task.filter(|t| !t.trim().is_empty());
        *self.pinned_task.write().expect("pinned task lock poisoned") =
            task.map(|t| t.trim_end().to_string());
    }

    /// The session's pinned task (`None` = ordinary session — Mnemo1, or a
    /// spawn without a task).
    pub fn pinned_task(&self) -> Option<String> {
        self.pinned_task
            .read()
            .expect("pinned task lock poisoned")
            .clone()
    }

    /// Stamp the loop's per-turn-routing skip (plan 2ba2168a step 1) —
    /// called ONCE by the spawn path post-build (pre-registration, before
    /// any turn can run, so no request can race the stamp). `true` marks a
    /// session that routes once at dispatch (Mnemo2 workers and leads);
    /// `false` keeps the per-turn gate (Mnemo1).
    pub fn set_skip_turn_routing(&self, skip: bool) {
        self.skip_turn_routing
            .store(skip, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether this session's turns skip the pre-prompt routing gate
    /// (`false` = the per-turn gate Mnemo1 runs).
    pub fn skip_turn_routing(&self) -> bool {
        self.skip_turn_routing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Build the request-scoped trace context for a model call made by
    /// this loop's agent: identity + turn id + purpose, stamped at the
    /// call site (the turn request, compaction, consolidation...).
    /// Unattributed when no identity was stamped.
    pub fn llm_request_context(
        &self,
        turn_id: Option<&str>,
        purpose: crate::provider::trace::TracePurpose,
    ) -> crate::provider::trace::LlmRequestContext {
        self.trace_identity.context(turn_id, purpose)
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

    /// Attach the live `[general.optimizer]` config (token-optimizer levers,
    /// backlog e4a50d22). Returns `self` for chaining. Wired by
    /// [`AgentLoopFactory`]; `None` for directly-built test loops, where every
    /// lever stays off (byte-identical to the pre-lever build).
    pub fn with_optimizer(
        mut self,
        optimizer: Arc<RwLock<crate::config::OptimizerConfig>>,
    ) -> Self {
        self.optimizer = Some(optimizer);
        self
    }

    /// The live optimizer config, or the all-off default when none is wired
    /// (tests, bare loops) — so a lever's gate reads the same shape either
    /// way and a missing handle can never panic.
    pub(crate) fn optimizer_config(&self) -> crate::config::OptimizerConfig {
        self.optimizer
            .as_ref()
            .map(|c| c.read().expect("optimizer config lock poisoned").clone())
            .unwrap_or_default()
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

    /// Attach the pre-prompt routing gate (the Laya classifier's
    /// task-complexity consumer, `[general.laya] routing`, backlog 091e694d).
    /// The gate carries the shared classifier slot; the opt-in flag and the
    /// `[general.routing]` threshold / `enforce` ride the live config the
    /// model resolver reads per turn, so a Settings save needs no rebuild.
    /// Returns `self` for chaining. Wired by
    /// [`AgentLoopFactory`](crate::agent::factory::AgentLoopFactory); `None`
    /// in tests that don't exercise routing.
    pub fn with_routing_gate(mut self, gate: model_routing::RoutingGate) -> Self {
        self.routing = Some(gate);
        self
    }

    /// Return a snapshot of this session's first-prompt routing state.
    pub fn route_once(&self) -> RouteOnce {
        self.route_once_with_epoch().0
    }

    /// Snapshot the lifecycle and its epoch from the same critical section.
    pub(crate) fn route_once_with_epoch(&self) -> (RouteOnce, u64) {
        let state = self
            .route_session
            .lock()
            .expect("route-session lock poisoned");
        (state.route_once.clone(), state.epoch)
    }

    /// Whether the live `[general.laya] routing` opt-in currently exposes a
    /// first-prompt policy FOR THIS LOOP. Subagents and sessions that opted out
    /// of turn routing (Mnemo2 leads/workers route at dispatch) must not write
    /// Mnemo1 session rows from an external model change.
    pub fn session_routing_enabled(&self) -> bool {
        !self.is_subagent()
            && !self.skip_turn_routing()
            && self
                .model_resolver
                .as_ref()
                .and_then(|resolver| resolver.routing_policy())
                .is_some()
    }

    /// Transition to manual in one critical section. With `only_if_active`, an
    /// already Off/ended session is left untouched and returns `None`, which
    /// prevents duplicate reroutes when two external surfaces race.
    fn transition_to_manual_inner(
        &self,
        only_if_active: bool,
    ) -> Option<(Option<String>, bool)> {
        let (previous, nudged) = {
            let mut state = self
                .route_session
                .lock()
                .expect("route-session lock poisoned");
            if only_if_active
                && !matches!(
                    state.route_once,
                    RouteOnce::Pending | RouteOnce::Settled { .. }
                )
            {
                return None;
            }
            let previous = match &state.route_once {
                RouteOnce::Settled { model, .. } => model.clone(),
                _ => None,
            };
            let nudged = state.nudge_sent;
            state.route_once = RouteOnce::Off { reason: "manual" };
            state.epoch = state.epoch.wrapping_add(1);
            state.nudge_sent = false;
            state.nudge_to_label = None;
            (previous, nudged)
        };
        self.strikes.lock().expect("route strikes lock poisoned").reset();
        *self
            .last_route_tool_signature
            .lock()
            .expect("last tool signature lock poisoned") = None;
        Some((previous, nudged))
    }

    fn transition_to_manual(&self) -> (Option<String>, bool) {
        self.transition_to_manual_inner(false)
            .unwrap_or((None, false))
    }

    /// Record a human model pick without appending a reroute row. Used by the
    /// race tests and by callers that only need the previous routed model.
    pub fn note_manual_model_choice(&self, _to: &str) -> Option<String> {
        self.transition_to_manual().0
    }

    /// Mark a human pick and append its reroute row through the SAME kernel
    /// path used by IPC and tests. Routing-off or already-final picks remain
    /// byte-identical. Returns `(previously routed model, nudge outstanding)`.
    pub fn record_manual_model_choice(&self, to: &str) -> (Option<String>, bool) {
        if !self.session_routing_enabled() {
            return (None, false);
        }
        let Some((from, nudged)) = self.transition_to_manual_inner(true) else {
            return (None, false);
        };
        self.append_session_routing_row(&model_routing::SessionRoutingRow::Reroute {
            ts: model_routing::now_millis(),
            agent_id: self.agent_id().unwrap_or(0).to_string(),
            from: from.clone(),
            to: to.to_string(),
            nudged,
        });
        (from, nudged)
    }


    /// Whether a routing nudge is currently awaiting a human response.
    pub fn routing_nudged(&self) -> bool {
        self.route_session
            .lock()
            .expect("route-session lock poisoned")
            .nudge_sent
    }

    /// Append one route-once event to this loop's redirected session log.
    /// Logging is best-effort and never changes the model pick.
    pub fn append_session_routing_row(&self, row: &model_routing::SessionRoutingRow) {
        if let Some(gate) = &self.routing {
            model_routing::append_row(gate.session_log_path(), row);
        }
    }

    /// Atomically return any outstanding nudge and restore the initial route
    /// state. A drift claim either wins first and is returned for logging, or
    /// loses to the epoch/Pending transition and can never be lost between two
    /// separate locks.
    pub(crate) fn clear_route_session(&self) -> Option<String> {
        let outstanding = {
            let mut state = self
                .route_session
                .lock()
                .expect("route-session lock poisoned");
            let outstanding = if state.nudge_sent {
                state.nudge_to_label.take()
            } else {
                state.nudge_to_label = None;
                None
            };
            state.route_once = RouteOnce::Pending;
            state.epoch = state.epoch.wrapping_add(1);
            state.nudge_sent = false;
            outstanding
        };
        self.strikes.lock().expect("route strikes lock poisoned").reset();
        *self
            .last_route_tool_signature
            .lock()
            .expect("last tool signature lock poisoned") = None;
        outstanding
    }

    /// Atomically invalidate all in-flight drift work and return any nudge
    /// still outstanding at session termination. Off prevents a new claim;
    /// the epoch bump rejects a task that snapshotted the old Settled state.
    pub(crate) fn end_route_session(&self) -> Option<String> {
        let mut state = self
            .route_session
            .lock()
            .expect("route-session lock poisoned");
        let outstanding = if state.nudge_sent {
            state.nudge_to_label.take()
        } else {
            state.nudge_to_label = None;
            None
        };
        state.route_once = RouteOnce::Off { reason: "ended" };
        state.epoch = state.epoch.wrapping_add(1);
        state.nudge_sent = false;
        outstanding
    }

    /// Mark a resumed session as ineligible for first-prompt routing.
    pub(crate) fn mark_route_once_resumed(&self) {
        let mut state = self
            .route_session
            .lock()
            .expect("route-session lock poisoned");
        state.route_once = RouteOnce::Off { reason: "resumed" };
        state.epoch = state.epoch.wrapping_add(1);
        state.nudge_sent = false;
        state.nudge_to_label = None;
    }

    /// Validate and claim a nudge while the caller holds `route_session`.
    fn claim_nudge_locked(
        state: &mut RouteSessionState,
        to_label: &str,
        expected_epoch: Option<u64>,
    ) -> bool {
        if (super::route_question::tier_for_label(to_label).is_none()
            && to_label != model_routing::RouteTarget::Escalate.label())
            || !matches!(state.route_once, RouteOnce::Settled { .. })
            || expected_epoch.is_some_and(|epoch| epoch != state.epoch)
            || state.nudge_sent
        {
            return false;
        }
        state.nudge_sent = true;
        state.nudge_to_label = Some(to_label.to_string());
        true
    }

    /// Test-only bare claim used to race the state linearization point without
    /// constructing a notice. Production nudges use `claim_and_publish_nudge`.
    #[cfg(test)]
    pub(crate) fn claim_nudge(
        session: &Arc<std::sync::Mutex<RouteSessionState>>,
        to_label: &str,
        expected_epoch: Option<u64>,
    ) -> bool {
        let mut state = session.lock().expect("route-session lock poisoned");
        Self::claim_nudge_locked(&mut state, to_label, expected_epoch)
    }

    /// Claim and publish a nudge without ever consuming it unobservably.
    ///
    /// Capacity is reserved BEFORE the critical section, so a full channel
    /// waits instead of dropping the notice; a CLOSED channel publishes
    /// nothing, so the nudge is left unclaimed for a later attempt. Once the
    /// permit is held, the claim, the synchronous row append and the
    /// infallible `Permit::send` all happen inside one critical section — no
    /// await is ever held under the route-state lock.
    pub(crate) async fn claim_and_publish_nudge(
        session: &Arc<std::sync::Mutex<RouteSessionState>>,
        to_label: &str,
        expected_epoch: Option<u64>,
        publication: NudgeNotice,
    ) -> bool {
        let NudgeNotice {
            agent_id,
            reason,
            text,
            log_path,
            fanin_tx,
        } = publication;
        // Reserve capacity OUTSIDE the lock. A full channel therefore WAITS
        // instead of silencing the one nudge the session may ever send; a
        // CLOSED channel returns early so nothing is claimed or logged.
        let Ok(permit) = fanin_tx.reserve().await else {
            return false;
        };
        let mut state = session.lock().expect("route-session lock poisoned");
        if !Self::claim_nudge_locked(&mut state, to_label, expected_epoch) {
            drop(permit);
            return false;
        }
        if let Some(path) = log_path {
            model_routing::append_row(
                &path,
                &model_routing::SessionRoutingRow::Nudge {
                    ts: model_routing::now_millis(),
                    agent_id: agent_id.to_string(),
                    reason: reason.to_string(),
                    to_label: to_label.to_string(),
                },
            );
        }
        // The reserved permit cannot fail, so the claim just taken is always
        // observable. `send` is synchronous: no await under the lock.
        permit.send((
            agent_id,
            crate::runtime::AgentEvent::RoutingNotice {
                kind: "nudge".to_string(),
                text,
            },
        ));
        true
    }

    /// Append the nudge returned by [`clear_route_session`](Self::clear_route_session)
    /// or [`end_route_session`](Self::end_route_session). Those methods consume
    /// the outstanding claim atomically, so this shared row helper is called at
    /// most once for clear-then-end and repeated termination cleanup alike.
    pub(crate) fn log_nudge_ignored(
        &self,
        agent_id: crate::runtime::AgentId,
        outstanding: Option<String>,
    ) {
        if let Some(to_label) = outstanding {
            self.append_session_routing_row(
                &model_routing::SessionRoutingRow::NudgeIgnored {
                    ts: model_routing::now_millis(),
                    agent_id: agent_id.to_string(),
                    to_label,
                },
            );
        }
    }

    /// Attach the compound-reflex handle (the Laya classifier's compound
    /// decision layer, `[general.laya] reflex`; backlog a8495cc1) -- the
    /// escalation lane ladder's classifier (backlog ad56c7bd). The handle
    /// carries the shared classifier slot + the config-mirrored enable flag,
    /// both read at every plan-step boundary, so a Settings save lands on the
    /// next step with no rebuild. Returns `self` for chaining. Wired by
    /// [`AgentLoopFactory`](crate::agent::factory::AgentLoopFactory); `None`
    /// in tests that don't exercise the ladder.
    pub fn with_reflex(mut self, handle: reflex::ReflexHandle) -> Self {
        self.reflex = Some(handle);
        self
    }

    /// The compound-reflex handle this loop was built with (`None` when the
    /// Laya foundation is not wired). The IPC layer mirrors the config flag
    /// into the handle's live enable flag, so a Settings toggle needs no
    /// rebuild.
    pub fn reflex(&self) -> Option<&reflex::ReflexHandle> {
        self.reflex.as_ref()
    }

    /// Attach the harness-run verification handle (the cost-saving chain's
    /// verification item). With it wired, a decided `action=verify` reflex
    /// call re-runs the configured checks harness-side and rides the compact
    /// evidence note on the next request's volatile tail. Returns `self` for
    /// chaining. Wired by
    /// [`AgentLoopFactory`](crate::agent::factory::AgentLoopFactory); `None`
    /// in tests that don't exercise the path.
    pub fn with_step_verify(mut self, handle: step_verify::StepVerifyHandle) -> Self {
        self.step_verify = Some(handle);
        self
    }

    /// The harness-run verification handle this loop was built with (`None`
    /// when `[general.verify]` is not wired).
    pub fn step_verify(&self) -> Option<&step_verify::StepVerifyHandle> {
        self.step_verify.as_ref()
    }

    /// Stash a harness-verification evidence note for the next request's
    /// volatile tail (delivered exactly once — see [`take_verify_note`]).
    ///
    /// [`take_verify_note`]: Self::take_verify_note
    pub(crate) fn set_verify_note(&self, note: String) {
        if let Ok(mut slot) = self.verify_note.lock() {
            *slot = Some(note);
        }
    }

    /// Take the pending harness-verification evidence note, if any. One-shot
    /// by construction: the request seam drains it, so the note is delivered
    /// exactly once and the volatile tail stays byte-identical when empty.
    pub fn take_verify_note(&self) -> Option<String> {
        self.verify_note.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Attach the live `[general.budget]` config (backlog a25a5323). Wired by
    /// the factory from the shared mirror; the inert default (disabled) keeps
    /// the cap gate a no-op. Returns `self` for chaining.
    pub fn with_budget_config(mut self, cfg: Arc<RwLock<BudgetConfig>>) -> Self {
        self.budget = cfg;
        self
    }

    /// The live `[general.budget]` config — shared with the factory, so a
    /// Settings save is observed at the next plan-step gate.
    pub fn budget_config(&self) -> Arc<RwLock<BudgetConfig>> {
        Arc::clone(&self.budget)
    }

    /// Run `f` against the live per-plan spend counters (the budget gate's
    /// read/update path; `pub(crate)` so tests can pre-seed counters).
    pub(crate) fn with_plan_spend<R>(&self, f: impl FnOnce(&mut budget::PlanSpend) -> R) -> R {
        let mut guard = self.plan_spend.lock().expect("plan_spend lock poisoned");
        f(&mut guard)
    }

    /// Flag a failed-cycle retry for the next budget gate (see
    /// [`note_lane_escalation`](Self::note_lane_escalation)).
    pub(crate) fn set_pending_lane_retry(&self) {
        self.pending_lane_retry
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Consume the pending lane-retry flag (true exactly once per flag).
    pub(crate) fn take_pending_lane_retry(&self) -> bool {
        self.pending_lane_retry
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// Stamp the attribution for the next spend row (consumed once by the
    /// usage site, so every row carries the decision that priced it).
    pub(crate) fn set_spend_attribution(&self, attr: budget::SpendAttribution) {
        if let Ok(mut slot) = self.spend_attribution.lock() {
            *slot = Some(attr);
        }
    }

    /// Clear the stamped spend attribution — the planless path, so a stamp
    /// left by an earlier request that reported no usage can never be
    /// consumed by a later turn with no live plan (round-1 finding L1).
    pub(crate) fn clear_spend_attribution(&self) {
        if let Ok(mut slot) = self.spend_attribution.lock() {
            *slot = None;
        }
    }

    /// Take the stamped spend attribution, if any.
    pub(crate) fn take_spend_attribution(&self) -> Option<budget::SpendAttribution> {
        self.spend_attribution
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// Record a confident failure-triage verdict (backlog ad56c7bd). A
    /// `permanent` or `flaky_test` verdict means the current attempt failed a
    /// cycle: the escalation epoch moves, so the next plan-step boundary
    /// re-classifies the SAME step with the verdict named in the state text --
    /// the only way the ladder ever routes up. Other classes (transient,
    /// needs_user) change nothing here.
    pub fn note_lane_escalation(&self, class: failure_triage::FailureClass) {
        if !matches!(
            class,
            failure_triage::FailureClass::Permanent | failure_triage::FailureClass::FlakyTest
        ) {
            return;
        }
        if let Ok(mut failure) = self.lane_failure.lock() {
            *failure = Some(class.label().to_string());
        }
        self.escalation_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The budget layer (backlog a25a5323) counts this failed cycle as a
        // lane retry at the next gate: the flag bridges the sync triage sites
        // (which cannot lock the async workflow to learn the live step).
        self.set_pending_lane_retry();
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

    /// Set the stable family root during shared host spawn.
    pub fn set_family_root(&self, root: crate::runtime::AgentId) {
        *self.family_root.lock().expect("family_root lock poisoned") = Some(root);
    }

    /// The stable family root, if this loop was built through the shared host.
    pub fn family_root(&self) -> Option<crate::runtime::AgentId> {
        *self.family_root.lock().expect("family_root lock poisoned")
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

    /// Seed the last-served effective (model, endpoint, effort) triple from
    /// the current display state — the value `list_agents` would report —
    /// when no request has served this loop yet. Called at the top of every
    /// `run_turn`; idempotent (an already-seeded loop is left alone), so the
    /// loop's FIRST serve is compared against the pre-serve belief: a change
    /// that landed before the very first request (e.g. a deferred pick
    /// completed at `run_turn` entry) still announces itself, while an
    /// unchanged serve stays silent.
    pub(crate) fn seed_served_effective_if_empty(&self) {
        let mut slot = self
            .served_effective
            .lock()
            .expect("served_effective lock poisoned");
        if slot.is_some() {
            return;
        }
        *slot = Some((
            self.resolved_model()
                .unwrap_or_else(|| self.provider().model().to_string()),
            self.effective_provider_name(),
            self.resolved_effort().or_else(|| self.default_display_effort()),
        ));
    }

    /// Record the effective (model, endpoint, effort) triple serving the
    /// current provider request and report whether it differs from the
    /// previous one — the turn loop emits `ModelChanged` exactly when this
    /// returns `true`. Called from `resolve_iteration_provider` AFTER the
    /// per-request resolution, so the announcement is tied to the request
    /// actually served: a mid-run swap into the live default slot, a
    /// completed deferred pick, and a mid-turn override flip all land here
    /// (2027-01-25). An empty memory seeds silently and reports `false`.
    pub(crate) fn note_served_effective(
        &self,
        model: &str,
        endpoint: Option<&str>,
        effort: Option<&str>,
    ) -> bool {
        let mut slot = self
            .served_effective
            .lock()
            .expect("served_effective lock poisoned");
        let next = (
            model.to_string(),
            endpoint.map(str::to_string),
            effort.map(str::to_string),
        );
        match slot.as_ref() {
            None => {
                *slot = Some(next);
                false
            }
            Some(prev) if *prev == next => false,
            Some(_) => {
                *slot = Some(next);
                true
            }
        }
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

    /// Pin the compaction-summary provider, bypassing `[models.summarize]`.
    /// Tests use it for a summarizer that streams without a network.
    pub fn with_summarize_provider(mut self, provider: Arc<dyn LlmClient>) -> Self {
        self.summarize_override = Some(provider);
        self
    }

    /// The provider a compaction summary call runs on.
    ///
    /// `[models.summarize]` routes summaries to a dedicated — often cheaper —
    /// model; unset (or a dangling reference / build failure) falls back to
    /// the turn's provider, preserving the pre-slot behavior exactly. A
    /// pinned provider ([`Self::with_summarize_provider`]) wins over the
    /// slot. The resolved model is NOT stamped into the turn's display state
    /// ([`Self::set_resolved_model`] et al.) — a summary is a one-off
    /// internal call, not a turn model switch.
    pub(crate) fn summarize_provider(
        &self,
        turn_provider: &Arc<dyn LlmClient>,
    ) -> Arc<dyn LlmClient> {
        if let Some(provider) = &self.summarize_override {
            return provider.clone();
        }
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

    /// The model id the most recent turn ran on: the per-context override
    /// when one resolved, else the default provider's model.
    pub(crate) fn last_turn_model(&self) -> String {
        self.resolved_model()
            .unwrap_or_else(|| self.provider().model().to_string())
    }

    /// The model id of the dedicated compaction summarizer, or `None` when a
    /// summary would ride the agent's own model: no pinned summarizer and no
    /// `[models.summarize]` slot, a slot whose provider could not be built
    /// (a missing endpoint or key leaves [`Self::summarize_provider`] on the
    /// turn's provider), or a summarizer naming the model the last turn ran
    /// on. Idle compaction runs only with a dedicated summarizer — re-reading
    /// a cold context on the agent's own model costs what the next message
    /// would have paid anyway.
    pub(crate) fn dedicated_summarizer_model(&self) -> Option<String> {
        let model = match self.summarize_override.as_ref() {
            Some(provider) => provider.model().to_string(),
            None => {
                let slot = self.model_resolver.as_ref()?.resolve_summarize_model()?;
                let built = self.summarize_provider(&self.provider());
                // A slot the resolver accepts can still fail to build (no
                // endpoint / no key): `summarize_provider` then falls back to
                // the turn's provider, which is NOT a dedicated summarizer.
                if built.model() != slot.model {
                    return None;
                }
                built.model().to_string()
            }
        };
        if model == self.last_turn_model() {
            return None;
        }
        Some(model)
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
    ///    A human pin is never deleted by a global swap; it goes dormant.
    ///    Laya's routed pin is the exception: an external provider change
    ///    clears it in `note_external_model_change` so the human model serves.
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
    /// active). The resolver reads the live config each call, and the call
    /// runs per request, so a Settings save takes effect on the next request.
    ///
    /// Called at the top of EVERY `run_turn` loop iteration (i.e. per
    /// provider request, not once per turn) — so a mid-turn workflow change
    /// (`skill_start`/`skill_end`, or a state transition from any workflow
    /// tool) switches the next request within the same turn to the
    /// configured per-context model. See [`AgentLoop::run_turn`].
    ///
    /// This variant threads no routing decision; use
    /// [`resolve_turn_provider_routed`](Self::resolve_turn_provider_routed)
    /// for the turn's pre-prompt routing arm (backlog 091e694d).
    pub(crate) async fn resolve_turn_provider(
        &self,
        skill_name: Option<&str>,
    ) -> Option<(Arc<dyn LlmClient>, super::context::ContextManager)> {
        self.resolve_turn_provider_routed(skill_name, None).await
    }

    /// [`resolve_turn_provider`](Self::resolve_turn_provider) with the turn's
    /// pre-prompt ROUTING decision threaded in (backlog 091e694d).
    ///
    /// `route` is `Some` only when the turn's classification cleared the
    /// confidence gate AND enforcement is on. The arm sits BELOW the three
    /// explicit-pin arms above (a skill, the picker pin, a forced model keep
    /// their model) and ABOVE the state/subagent chain it replaces: a routed
    /// target that is unset, dangling, or refused by
    /// [`resolve_routed`](crate::model_resolver::ModelResolver::resolve_routed)
    /// falls through to that chain, unchanged.
    pub(crate) async fn resolve_turn_provider_routed(
        &self,
        skill_name: Option<&str>,
        route: Option<model_routing::RouteTarget>,
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
                // The skill arm is skill-slot-only: only the skill slot can
                // fire in this probe context. 2b.3: the probe key builds
                // kernel-side from plain strings — the slot encoding is
                // `skill:{name}` and the Skill state carries no state slot
                // (build_route_key's arm, mirrored here byte-exactly).
                let skill_only = crate::model_resolver::ModelContext {
                    route: crate::agent::policy::RouteKey {
                        slots: vec![format!("skill:{name}")],
                    },
                    is_subagent: false,
                };
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
                // 2b.3: the state rides the handle's token; the "is the
                // state configured?" check rides the policy's live route
                // consult (the state/skill/kind derivation lives app-side).
                let current_state_token = self.workflow.state().await;
                let live_route = self.policy().route().await;
                let mut stamp = self
                    .pinned_state
                    .lock()
                    .expect("pinned_state lock poisoned");
                let stamped_now = stamp.clone();
                match stamped_now.as_deref() {
                    None => {
                        // First serve of this pin — it belongs to the state
                        // of the turn that first serves it (normally the
                        // state the picker acted in; with a deferred swap,
                        // the state the turn loop completed the swap in).
                        // The pick can predate the stamp by several turns —
                        // e.g. pick in Planning, then `create_plan` flips to
                        // Executing before the next turn: the first serve
                        // stamps Executing, not the pick-time state.
                        *stamp = Some(current_state_token);
                        true
                    }
                    Some(stamped) if stamped == current_state_token => true,
                    Some(_) => self
                        .model_resolver
                        .as_ref()
                        .map(|resolver| {
                            resolver
                                .resolve(crate::model_resolver::ModelContext {
                                    route: live_route.clone(),
                                    is_subagent: self.is_subagent(),
                                })
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
        // Pre-prompt ROUTING (backlog 091e694d): a confident classification of
        // this turn's task text may run it on the configured cheap / capable
        // target instead of the state/subagent chain below. The explicit-pin
        // arms above already returned, and `resolve_routed` refuses the skill,
        // subagent and bug-fixing contexts itself -- so an arm that did not
        // fire cannot smuggle routing past an explicit choice. Unset, dangling
        // or refused -> `None` -> the chain below runs exactly as before.
        if let Some(target) = route {
            // 2b.3: the live route comes from the policy consult — the
            // state/skill/kind derivation lives app-side.
            let ctx = crate::model_resolver::ModelContext {
                route: self.policy().route().await,
                is_subagent,
            };
            if let Some(model_ref) = resolver.resolve_routed(target, ctx) {
                // Same endpoint stickiness as every other arm: a recorded 429
                // fallback reroutes the ENDPOINT of this model, never the
                // routed model choice itself.
                let model_ref = self.sticky_endpoint(&model_ref).unwrap_or(model_ref);
                let built = resolver.build_turn_provider(&model_ref, self.fill_rate);
                // Record the effective model so the UI reports what actually
                // ran (the routed target beats the state/subagent chain).
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
        let model_ref = resolver.resolve(crate::model_resolver::ModelContext {
            route: self.policy().route().await,
            is_subagent,
        });
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

    /// The live provider's `endpoint/model` spec (or its bare model for test
    /// doubles without an endpoint name).
    pub fn current_model_spec(&self) -> String {
        let provider = self.provider();
        if provider.provider_name().is_empty() {
            provider.model().to_string()
        } else {
            format!("{}/{}", provider.provider_name(), provider.model())
        }
    }

    /// A handle to the context manager, for manual compaction (`/compact`).
    /// Clones the `ContextManager` (cheap — two `usize`s) so the caller can
    /// call `summarize_with_interrupt` without holding the lock. The clone
    /// carries the SESSION's compaction-raise handle, so a raise applied on the
    /// manual path is the same ladder the turn loop reads.
    pub fn context_manager(&self) -> super::context::ContextManager {
        self.context_manager
            .read()
            .expect("context_manager lock poisoned")
            .clone()
            .with_compaction_raise(Arc::clone(&self.compaction_raise))
    }

    /// Clear the session's temporary compaction raise — a fresh session must
    /// start at 1.0 (`/new`).
    pub fn reset_compaction_raise(&self) {
        self.compaction_raise.reset();
    }

    /// Swap in a new provider + context manager (e.g. when the user switches
    /// models from the status bar). Takes effect on the NEXT REQUEST — the
    /// turn loop re-reads the live pair at the top of every iteration, so an
    /// in-flight run issues its next request against the new provider
    /// (2027-01-25: mid-run switches land before the next call, like steers;
    /// they no longer wait for the next turn). A single request still talks
    /// to one provider throughout.
    ///
    /// When this is an EXPLICIT per-agent picker switch (the
    /// [`set_explicit_provider`](Self::set_explicit_provider) variant), the
    /// new provider additionally pins THIS agent's turn resolution ahead of
    /// forced / subagent / workflow-state overrides, so the picker's choice
    /// lands on the next request and survives to the next turn (a configured
    /// skill override still beats the pin). The pin is STATE-SCOPED (2026-12-20) — see
    /// [`set_explicit_provider`] for where it yields to a configured
    /// `[models.*]` slot on a workflow-state change. Plain `set_provider`
    /// remains the *default* slot swap (factory rebuilds, global console
    /// switches) and never creates a pin; it clears Laya's prior routed pin so
    /// the external default can serve.
    pub fn set_provider(
        &self,
        provider: Arc<dyn LlmClient>,
        context_manager: super::context::ContextManager,
    ) {
        let new_spec = Self::provider_spec(provider.as_ref());
        let _ = self.note_external_model_change(&new_spec);
        self.set_provider_internal(provider, context_manager);
    }

    fn provider_spec(provider: &dyn LlmClient) -> String {
        if provider.provider_name().is_empty() {
            provider.model().to_string()
        } else {
            format!("{}/{}", provider.provider_name(), provider.model())
        }
    }

    /// External provider changes are human model choices only when routing is
    /// active and the spec actually changes. The reroute is recorded BEFORE
    /// the new provider is installed; every such change also clears Laya's
    /// explicit pin so the external model can really serve.
    fn note_external_model_change(&self, new_spec: &str) -> (Option<String>, bool) {
        let outcome = if self.current_model_spec() != new_spec && self.session_routing_enabled() {
            self.record_manual_model_choice(new_spec)
        } else {
            (None, false)
        };
        self.clear_routed_pin();
        outcome
    }

    fn clear_routed_pin(&self) {
        let routed = std::mem::take(
            &mut *self
                .routed_pin
                .lock()
                .expect("routed-pin lock poisoned"),
        );
        if routed {
            *self
                .explicit_provider
                .write()
                .expect("explicit_provider lock poisoned") = None;
            *self
                .pinned_state
                .lock()
                .expect("pinned_state lock poisoned") = None;
            *self
                .pending_swap
                .lock()
                .expect("pending_swap lock poisoned") = None;
        }
    }

    /// Internal default-provider swap: never a human routing decision.
    pub(crate) fn set_provider_internal(
        &self,
        provider: Arc<dyn LlmClient>,
        context_manager: super::context::ContextManager,
    ) {
        *self.provider.write().expect("provider lock poisoned") = provider;
        *self
            .context_manager
            .write()
            .expect("context_manager lock poisoned") = context_manager
            .with_compaction_raise(Arc::clone(&self.compaction_raise));
        self.compaction_raise.reset();
    }


    /// Publish the conversation's token COUNT to the loop — the measure every
    /// compact-before-switch decision uses. The conversation itself is never
    /// copied into the loop (it carries base64 images and this runs on every
    /// request iteration): [`ContextManager::count_tokens`] is computed right
    /// here, at publish time, and only the resulting number is kept.
    ///
    /// Called when a transcript is seeded (a resume, before the first
    /// request), before and after every turn, and at the top of every request
    /// iteration — the points where a request-iteration count is `0` (a resumed
    /// session that has not issued a request) or stale (after a turn's final
    /// message).
    pub fn publish_message_tokens(&self, messages: &[crate::provider::Message]) {
        self.live_message_tokens.store(
            super::context::ContextManager::count_tokens(messages),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Publish the tool-schema overhead (tokens) measured for the current
    /// request surface — the second half of the inclusive count the turn
    /// loop's summarize trigger compares.
    pub fn publish_schema_tokens(&self, tokens: usize) {
        self.live_schema_tokens
            .store(tokens, std::sync::atomic::Ordering::Relaxed);
    }

    /// THE compact-before-switch rule, shared by all three call sites — the pin
    /// ([`set_explicit_provider`](Self::set_explicit_provider)), the deferred
    /// completion's summary gate in `run_turn`, and
    /// [`AgentSession::set_model`](crate::headless::AgentSession::set_model)
    /// (via [`swap_needs_compaction`](Self::swap_needs_compaction)) — given the
    /// conversation's token count: do those tokens PLUS the tool-schema
    /// overhead reach `context_manager`'s own compaction trigger?
    ///
    /// This is the turn loop's trigger expression
    /// (`message_tokens + schema_tokens >= effective_limit`) applied to the
    /// target manager, so a switch summarizes exactly when the next request on
    /// the new model would itself summarize. `effective_limit` carries the raise
    /// ladder; the incoming manager is freshly built (no raise installed) and
    /// [`set_provider`](Self::set_provider) resets the ladder on every swap
    /// anyway, so this reads exactly the trigger the new model would apply.
    fn tokens_need_compaction(
        &self,
        message_tokens: usize,
        context_manager: &super::context::ContextManager,
    ) -> bool {
        let schema_tokens = self
            .live_schema_tokens
            .load(std::sync::atomic::Ordering::Relaxed);
        // An EMPTY conversation has nothing to summarize: compacting first would
        // only delay the swap and burn a summarization call that cannot reduce
        // anything (the irreducible system/schema block is measured separately
        // below and is not the conversation's to shrink).
        message_tokens > 0 && message_tokens + schema_tokens >= context_manager.effective_limit()
    }

    /// [`tokens_need_compaction`](Self::tokens_need_compaction) over an explicit
    /// conversation — the deferred completion's summary gate, which holds the
    /// real `messages` and must count them inclusively (tool overhead too).
    pub fn conversation_needs_compaction(
        &self,
        context_manager: &super::context::ContextManager,
        messages: &[crate::provider::Message],
    ) -> bool {
        self.tokens_need_compaction(
            super::context::ContextManager::count_tokens(messages),
            context_manager,
        )
    }

    /// [`tokens_need_compaction`](Self::tokens_need_compaction) over the count
    /// its task last [`published`](Self::publish_message_tokens).
    ///
    /// A model switch compacts first ONLY when this is `true`. A new window
    /// that is merely smaller than the old one is not, by itself, a reason to
    /// summarize a conversation that already fits: the 2027-11-02 bug (plan
    /// 1238e0de) summarized — and reported — a switch that needed no
    /// summarization, which made Mnemo2 mark a healthy worker failed.
    pub fn swap_needs_compaction(&self, context_manager: &super::context::ContextManager) -> bool {
        self.tokens_need_compaction(
            self.live_message_tokens
                .load(std::sync::atomic::Ordering::Relaxed),
            context_manager,
        )
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
    /// **Deferral:** when the LIVE conversation exceeds what the new
    /// context manager allows ([`swap_needs_compaction`](Self::swap_needs_compaction)
    /// — the turn loop's own compaction trigger measured on the new model),
    /// the swap is deferred — stored in [`pending_swap`](Self::pending_swap)
    /// — so the turn loop can summarize the conversation using the OLD
    /// provider before completing the swap. This prevents a too-large
    /// conversation from being sent to a smaller-context model (which would
    /// fail with a 400/502). A new window that is merely smaller is NOT
    /// enough: when the conversation fits, the swap lands immediately and
    /// nothing is summarized (2027-11-02 bug, plan 1238e0de).
    /// The turn loop completes it via [`take_pending_swap`](Self::take_pending_swap)
    /// both at the top of `run_turn` and at the top of every loop iteration —
    /// a deferred pick made mid-run therefore lands before the next request of
    /// the same run.
    pub fn set_explicit_provider(
        &self,
        provider: Arc<dyn LlmClient>,
        context_manager: super::context::ContextManager,
    ) {
        let new_spec = Self::provider_spec(provider.as_ref());
        let _ = self.note_external_model_change(&new_spec);
        self.set_explicit_provider_internal(provider, context_manager, false);
    }

    /// Install an explicit pin without treating it as an external model pick.
    /// `routed` distinguishes Laya's clearable pin from the human picker's.
    fn set_explicit_provider_internal(
        &self,
        provider: Arc<dyn LlmClient>,
        context_manager: super::context::ContextManager,
        routed: bool,
    ) {
        *self
            .routed_pin
            .lock()
            .expect("routed-pin lock poisoned") = routed;
        *self
            .pinned_state
            .lock()
            .expect("pinned_state lock poisoned") = None;
        if routed {
            // First-prompt routing NEVER defers: a pending routed swap could
            // complete after a later manual pick and reinstall Laya's model.
            debug_assert!(
                !self.swap_needs_compaction(&context_manager),
                "pin_routed_provider must not be called when compaction is needed"
            );
            self.set_provider_internal(provider.clone(), context_manager.clone());
            *self
                .pending_swap
                .lock()
                .expect("pending_swap lock poisoned") = None;
            *self
                .explicit_provider
                .write()
                .expect("explicit_provider lock poisoned") =
                Some((provider, context_manager));
            return;
        }
        if self.swap_needs_compaction(&context_manager) {
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
        self.set_provider_internal(provider.clone(), context_manager.clone());
        *self
            .pending_swap
            .lock()
            .expect("pending_swap lock poisoned") = None;
        *self
            .explicit_provider
            .write()
            .expect("explicit_provider lock poisoned") = Some((provider, context_manager));
    }

    /// The ONLY provider-pin path used by Laya's first-prompt commit. It must
    /// not count as an external manual choice or emit a reroute row.
    pub(crate) fn pin_routed_provider(
        &self,
        provider: Arc<dyn LlmClient>,
        context_manager: super::context::ContextManager,
    ) {
        self.set_explicit_provider_internal(provider, context_manager, true);
    }

    /// Whether a deferred provider swap is pending (set by
    /// [`set_explicit_provider`](Self::set_explicit_provider) when the live
    /// conversation exceeds what the new model's context manager allows —
    /// [`swap_needs_compaction`](Self::swap_needs_compaction); a smaller window
    /// alone no longer defers). Checked by the IPC layer so it can
    /// report the switch as pending instead of immediately serving.
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
    pub fn workflow_handle(
        &self,
    ) -> Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle> {
        Arc::clone(&self.workflow)
    }

    /// The agent's policy handle (2a.2) — the free agent
    /// ([`NoPolicy`](crate::agent::policy::NoPolicy)) by default; the
    /// factory attaches the workflow agent's WorkflowPolicy (app-side).
    /// Interior-mutable since 3c (mirroring `permission`): the engine
    /// layers policies post-build, before the first prompt.
    pub fn policy(&self) -> Arc<dyn crate::agent::policy::AgentPolicy> {
        Arc::clone(&self.policy.read().expect("policy lock poisoned"))
    }

    /// Attach (or replace) the agent's policy — used by the factory at
    /// build time (2a.2), later by the engine/session builders (3a), and
    /// since 3c by the engine's tier-3 wrap: the layer is swapped in
    /// POST-build, before the first prompt consults it (nothing reads the
    /// policy between the build and the first turn). `&self` +
    /// interior mutability (like `set_permission`) so an `Arc`-wrapped
    /// loop can be re-pointed.
    pub fn set_policy(&self, policy: Arc<dyn crate::agent::policy::AgentPolicy>) {
        *self.policy.write().expect("policy lock poisoned") = policy;
    }


    /// The workflow state as a short evidence label (e.g. "Reviewing") —
    /// carried by [`crate::runtime::channels::AgentEvent::Parked`] so the
    /// watchdog ring and the UI can see the pre-stall state without
    /// re-deriving it.
    pub async fn workflow_state_label(&self) -> String {
        // The Parked label keeps the pre-2b.3 Debug shape (first letter
        // capitalized); the token underneath is the serde shape.
        crate::agent::workflow_handle::capitalize_token(&self.workflow.state().await)
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
        let head1 =
            holder.stable_head(&crate::agent::policy::NoPolicy::with_coding_default());
        // Second call with no file change returns the cached head (identical).
        let head2 =
            holder.stable_head(&crate::agent::policy::NoPolicy::with_coding_default());
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
        let head3 =
            holder.stable_head(&crate::agent::policy::NoPolicy::with_coding_default());
        assert_ne!(head3, head1, "head must rebuild when constitution changes");
    }

    /// One session crossing its wall-clock budget: admitted before, refused
    /// after, and still refused on the next call. The epoch is back-dated, so
    /// no real time passes and machine load cannot change the outcome.
    #[test]
    fn wall_clock_budget_admits_requests_until_it_elapses() {
        let mut limiter = SessionLimiter::unlimited();
        limiter.apply(crate::runtime::channels::SessionLimits {
            wall_clock_ms: Some(60_000),
            ..Default::default()
        });
        assert_eq!(limiter.begin_request(), None);
        assert_eq!(limiter.requests_served, 1);

        limiter.started_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(61));
        let spent = Some(crate::provider::LimitKind::WallClock);
        assert_eq!(limiter.begin_request(), spent);
        assert_eq!(limiter.begin_request(), spent);
        assert_eq!(
            limiter.requests_served, 1,
            "a refused request is not served"
        );
    }
}
