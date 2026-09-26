// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `spawn_agent` — start a background agent to run a task in parallel.
//!
//! The agent asks for a named background agent (e.g. "reviewer") plus a task
//! description; the tool delegates to an [`AgentSpawner`] (wired in by the IPC
//! layer) which builds the agent via the factory, spawns its task, and kicks
//! off its first turn with the task as the prompt.
//!
//! When a memory store is wired (the factory's `with_memory`), the task is
//! best-effort seeded with a passive RECALLED CONTEXT rider — prior knowledge
//! matched to the task — so the spawned agent starts with relevant memory in
//! its first prompt (see [`crate::tool::steering::recalled_context_block`]).
//! Reviewer spawns are exempt (backlog 1d0332ca): the reviewer protocol
//! hands the spawned reviewer a self-contained task, and the rider would
//! pollute it with parent-project memories — their task passes through
//! byte-identical.
//!
//! Goes through approval — spawning an agent consumes model tokens and starts
//! a concurrent actor, so it should be a deliberate, user-visible action.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::agent::review_scope::{render_review_preamble, ReviewScope, ReviewScoper};
use crate::memory::MemoryStoreTrait;
use crate::model_resolver::ModelResolver;
use crate::provider::ToolSchema;
use crate::runtime::AgentSpawner;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};
use crate::workflow::Workflow;

/// Arguments for `spawn_agent`.
#[derive(Debug, Deserialize)]
struct SpawnAgentArgs {
    /// A short display name for the background agent (e.g. "reviewer").
    name: String,
    /// The task for the agent to perform, sent as its first prompt. Should be
    /// self-contained — the new agent does not see this conversation.
    task: String,
    /// An optional model id (from `list_models`) to run the spawned agent on.
    /// Omit to use the default / subagent model. When set, the tool resolves
    /// the id to an endpoint + model via the model resolver and forces the
    /// spawned agent onto it for every turn.
    #[serde(default)]
    model: Option<String>,
    /// An optional role that constrains the spawned agent's tool surface.
    /// Currently the only supported value is `"reviewer"`: a read-only agent
    /// that can read code + run `git_read` + write its report via
    /// `write_review_report`, but cannot edit project files, run shell, commit,
    /// spawn, or finish. Omit for an unrestricted sub-agent (the default).
    #[serde(default)]
    role: Option<String>,
}

/// The `spawn_agent` tool.
///
/// Holds an `Arc<dyn AgentSpawner>` — the concrete spawn logic lives in the
/// IPC layer (it owns the `AgentManager`, the factory, and the async runtime
/// the agent task is spawned on). The tool just validates args and delegates.
///
/// When `parent_id` is `Some` (set by the factory to the owning agent's id),
/// the spawned agent records it as its parent so the completion-notification
/// feedback loop can route a "finished" message back to this agent.
///
/// When `model_resolver` is `Some`, the optional `model` argument can resolve
/// a bare model id (discovered via `list_models`) to an endpoint + model that
/// the spawned agent is forced onto. Without a resolver, a `model` arg errors.
///
/// A spawned **reviewer** (`role: "reviewer"`) is pinned at spawn time via
/// [`ModelResolver::resolve_reviewer_model`] to `[models.reviewing]` (falling
/// back to `[models.executing]` — back-compat with pre-reviewing-slot
/// configs), then `[models.subagent]`, then the default provider.
/// `[models.reviewing]` is **reviewer-spawn-only** (decision 2026-12-06): the
/// main agent keeps `[models.executing]` through the Reviewing state, so the
/// reviewing slot is consumed solely by this pin. The reviewer's own loop is
/// stamped `WorkflowState::Subagent` and never transitions, so without the
/// pin it would never consult `[models.reviewing]` — and with no
/// `[models.subagent]` configured it would silently run on the default
/// model. Pinning at spawn makes "the reviewing slot is the model that
/// reviews" true wherever the review runs.
///
/// Reviewer spawns must OMIT the `model` arg (backlog c8e48f81, 2026-01-03):
/// the configured reviewing model is authoritative, and an explicit model on
/// a reviewer spawn is DENIED at dispatch (`reviewer_spawn_gate`,
/// agent/dispatch.rs) unless it is the user-sanctioned failed-reviewer retry
/// (a reviewer failed without a report → ask_user → respawn on the model the
/// user picked — the ask_user interception opens a one-spawn retry sanction).
/// The 2026-12-30 session showed the agent inventing per-reviewer "model
/// diversity" (glm-5.2/glm-5.3-gcp/deepseek-v4-flash); the deepseek reviewer
/// failed with no report — exactly what the configured model prevents. The
/// pin applies only on the parent-aware spawn path (the plain path keeps
/// today's default-model behavior — no new error surface).
pub struct SpawnAgentTool {
    spawner: Arc<dyn AgentSpawner>,
    /// The id of the agent that owns this tool (its parent for any agents it
    /// spawns). `None` when the tool wasn't given an owning agent (e.g. tests).
    parent_id: Option<crate::runtime::AgentId>,
    /// The model resolver used to resolve the optional `model` argument to an
    /// endpoint + model. `None` when no per-context model resolution is
    /// configured (tests) — a `model` arg then errors.
    model_resolver: Option<Arc<dyn ModelResolver>>,
    /// The memory store, when one is wired. Seeds the spawned agent's task
    /// (its FIRST PROMPT) with the passive RECALLED CONTEXT rider — matched
    /// prior knowledge, `recall_peek` only. `None` in tests / store-less
    /// setups: the rider stays silent. Reviewer spawns skip the rider
    /// (backlog 1d0332ca): their task must stay clean — byte-identical to
    /// the `task` argument.
    memory: Option<Arc<dyn MemoryStoreTrait>>,
    /// The review-round scope wiring (backlog 85313a7e), when one is wired:
    /// the workflow holding the plan frame's round stamps, the project root
    /// the git reads run in, and the git-facing scoper. Reviewer spawns then
    /// get the harness-rendered preamble appended to their task and stamp
    /// their round on the plan frame after a successful spawn. `None` in
    /// tests — the spawn then behaves exactly as before.
    review_scope: Option<ReviewScopeWiring>,
}

/// The wiring that lets a REVIEWER spawn render the harness preamble and stamp
/// its round (backlog 85313a7e).
///
/// Wired by the factory; absent in tests / store-less setups, where a reviewer
/// spawn forwards the task verbatim exactly as it did before.
pub struct ReviewScopeWiring {
    /// The agent's workflow — its active plan frame carries the round stamps
    /// ([`Workflow::review_rounds`]) and receives the new one.
    workflow: Arc<Mutex<Workflow>>,
    /// The repository the git reads run in (the agent's project root).
    root: PathBuf,
    /// The git-facing scope reader — [`GitReviewScoper`] in production, a
    /// canned double in tests.
    ///
    /// [`GitReviewScoper`]: crate::agent::review_scope::GitReviewScoper
    scoper: Arc<dyn ReviewScoper>,
}

impl SpawnAgentTool {
    /// Create the tool with the spawner that starts agents.
    pub fn new(spawner: Arc<dyn AgentSpawner>) -> Self {
        Self {
            spawner,
            parent_id: None,
            model_resolver: None,
            memory: None,
            review_scope: None,
        }
    }

    /// Record the owning agent's id, so background agents spawned by this tool
    /// are registered as its children (the completion-notification feedback
    /// loop notifies the parent when a child finishes). Returns `self` for
    /// chaining — the factory calls this right after construction.
    pub fn with_parent(mut self, parent_id: crate::runtime::AgentId) -> Self {
        self.parent_id = Some(parent_id);
        self
    }

    /// Wire in the model resolver so the optional `model` argument can be
    /// resolved to an endpoint + model. Returns `self` for chaining — the
    /// factory calls this right after construction (when a resolver is wired).
    pub fn with_model_resolver(mut self, resolver: Arc<dyn ModelResolver>) -> Self {
        self.model_resolver = Some(resolver);
        self
    }

    /// Wire in the memory store so the spawned agent's task is seeded with
    /// the passive RECALLED CONTEXT rider (best-effort: no store, no hits,
    /// or any store error leaves the task unchanged; reviewer spawns skip
    /// the rider — backlog 1d0332ca — their task stays byte-identical).
    /// Returns `self` for chaining — the factory calls this right after
    /// construction (when a store is wired).
    pub fn with_memory(mut self, memory: Arc<dyn MemoryStoreTrait>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Wire in the review-round scope (backlog 85313a7e): the workflow whose
    /// plan frame carries the round stamps, the project root the git reads run
    /// in, and the git-facing [`ReviewScoper`]. A REVIEWER spawn then gets the
    /// harness-rendered preamble appended to its task — the verdict contract,
    /// the one-line constitution checks, the `.coding/**` bookkeeping rule,
    /// and, for round >= 2, the delta scope (the delta since `<base>`, named
    /// via the reviewer's own `git_read` ops, plus the mechanically-derived
    /// changed file set) — and stamps its round once the
    /// spawn succeeds. Every step is best-effort: a git failure degrades the
    /// CHANGED-FILE SET to an instruction (the whole preamble to the contract
    /// block on a first round) and never blocks a spawn. Returns `self` for
    /// chaining — the factory calls this right after construction.
    pub fn with_review_scope(
        mut self,
        workflow: Arc<Mutex<Workflow>>,
        root: PathBuf,
        scoper: Arc<dyn ReviewScoper>,
    ) -> Self {
        self.review_scope = Some(ReviewScopeWiring {
            workflow,
            root,
            scoper,
        });
        self
    }
}

#[async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "spawn_agent",
            "Start a background agent on a task in parallel (code review, bug \
             investigation, drafting a plan) while you continue. It gets its own plan + \
             tool set and starts with `task` as its first prompt. It does NOT see this \
             conversation — make `task` self-contained. Requires approval.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Short display name (e.g. \"reviewer\")."
                    },
                    "task": {
                        "type": "string",
                        "description": "The self-contained task, sent as the agent's first prompt."
                    },
                    "model": {
                        "type": ["string", "null"],
                        "description": "Model id (from list_models); omit or pass null \
                                         for the default subagent model. For role:\"reviewer\" \
                                         spawns OMIT this — the configured reviewing model \
                                         is authoritative; an explicit model is denied \
                                         unless it is the user-sanctioned failed-reviewer \
                                         retry."
                    },
                    "role": {
                        "type": "string",
                        "description": "Optional role constraining the agent's tools. \"reviewer\" = \
                                         read-only: read tools, git_read (op diff/log/show/status), \
                                         web_fetch, graph tools, memory/backlog QUERIES and \
                                         write_review_report — it cannot ask questions, mutate \
                                         memory/backlog, run plans, or finish. A reviewer \
                                         ALWAYS runs on the configured reviewing model (→ \
                                         executing → subagent → default) — omit `model` (an \
                                         explicit model is denied unless it is the \
                                         user-sanctioned failed-reviewer retry). Omit for an \
                                         unrestricted sub-agent."
                    }
                },
                "required": ["name", "task"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: SpawnAgentArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };

        if args.name.trim().is_empty() {
            return ToolResult::error("spawn_agent requires a non-empty 'name'");
        }
        if args.task.trim().is_empty() {
            return ToolResult::error("spawn_agent requires a non-empty 'task'");
        }

        // Validate the optional `role`. "reviewer" constrains the spawned
        // agent to a read-only tool surface (read tools + git_read +
        // write_review_report); any other value is rejected so an unknown
        // role can't silently spawn an unrestricted agent.
        let role = match args.role.as_deref().map(str::trim) {
            Some(role) if !role.is_empty() => {
                if role != "reviewer" {
                    return ToolResult::error(format!(
                        "unknown role '{role}'; supported roles: reviewer"
                    ));
                }
                Some(role.to_string())
            }
            _ => None,
        };

        // Resolve the optional `model` argument to an endpoint + model. When
        // set, the spawned agent is forced onto this model for every turn
        // (overriding the subagent/state/skill resolution chain). A bare model
        // id (e.g. "deepseek-v4-flash-gcp") is resolved via the model resolver
        // to the endpoint that serves it.
        // The literal string "null" counts as unset here too (mirroring the
        // dispatch reviewer gate): a transport that stringifies JSON null
        // manufactures it, and no configured model is ever named "null"
        // (backlog 3e6f7887). The drop is never silent (review L1):
        // `null_model_note` makes the success output name it, so a user
        // who genuinely configured a model id "null" sees the override
        // was ignored. JSON null needs no note — it is an explicit
        // unset (the schema says "omit or pass null"), not a dropped pick.
        let mut null_model_note = false;
        let forced_model = match args.model.as_deref().map(str::trim) {
            Some(model_id) if !model_id.is_empty() && model_id != "null" => {
                let Some(resolver) = self.model_resolver.as_ref() else {
                    return ToolResult::error(
                        "model selection unavailable (no model resolver configured)",
                    );
                };
                let Some(model_ref) = resolver.resolve_model_id(model_id) else {
                    return ToolResult::error(format!(
                        "unknown model id '{model_id}' — call list_models to see the configured models"
                    ));
                };
                Some(model_ref)
            }
            // Empty string stays silently unset (long-standing
            // semantics); only the "null" artifact is surfaced (review L1).
            Some(model_id) => {
                null_model_note = model_id == "null";
                None
            }
            None => None,
        };

        // A spawned reviewer with no explicit model is pinned at spawn time
        // via resolve_reviewer_model: [models.reviewing] — reviewer-spawn-
        // only (decision 2026-12-06), so this pin is its SOLE consumer —
        // falling back to [models.executing] (back-compat), then the subagent
        // slot, then None (default provider). The main agent itself never
        // resolves the reviewing slot (it keeps the executing model through
        // the Reviewing state), and the reviewer's own loop is stamped
        // WorkflowState::Subagent and never transitions, so without this pin
        // nothing would ever consult [models.reviewing] — and with no
        // [models.subagent] it would silently run on the default model.
        // Skipped when an explicit `model` arg is set (for reviewer spawns
        // that path is dispatch-denied unless it is the user-sanctioned
        // failed-reviewer retry — see the struct doc), and when no resolver
        // is wired (the spawn proceeds on the default model).
        let reviewer_pin = if role.as_deref() == Some("reviewer") && forced_model.is_none() {
            self.model_resolver
                .as_ref()
                .and_then(|resolver| resolver.resolve_reviewer_model())
        } else {
            None
        };

        // A model override requires the parent-aware path (it's threaded
        // through `spawn_with_parent`); reject it BEFORE the rider's store
        // round-trip below — a rejected spawn must never pay for recall
        // (review 2026-09-14, finding 2).
        if forced_model.is_some()
            && (self.spawner.parent_aware().is_none() || self.parent_id.is_none())
        {
            return ToolResult::error(
                "model override requires the parent-aware spawn path (no owning agent id)",
            );
        }

        // The RECALLED CONTEXT rider: seed the task — the spawned agent's
        // FIRST PROMPT — with prior knowledge matched to it, PASSIVELY
        // (recall_peek, no access bump). Best-effort: no store, no hits, or
        // any store error leaves the task unchanged. Computed only after
        // EVERY rejection path (name/task/role/model + the parent-aware
        // check above) so a rejected spawn never pays for a store round-trip
        // — the child's memory_search-before-work rule is then structurally
        // enforced the same way create_plan's rider does for plans.
        // Reviewer spawns are EXEMPT (backlog 1d0332ca, user report
        // 2026-09-19): the reviewer protocol hands the spawned reviewer a
        // self-contained task, and the rider would pollute it with
        // parent-project memories — noise at best, review bias at worst
        // (the reviewer must judge the diff/plan, not be steered by
        // recalled context), plus prompt bloat on every review. The skip
        // is role-based ONLY, not an exposed spawn option: the reviewer
        // protocol is the one consumer that needs a clean prompt, and an
        // opt-out knob would grow the schema for no current consumer;
        // unrestricted sub-agents keep the rider (its original purpose:
        // seed a worker's task with prior knowledge).
        let task = if role.as_deref() == Some("reviewer") {
            args.task
        } else {
            match crate::tool::steering::recalled_context_block(
                self.memory.as_ref(),
                None,
                &args.task,
                None,
            )
            .await
            {
                Some(rider) => format!("{}{rider}", args.task),
                None => args.task,
            }
        };

        // The harness-rendered reviewer preamble (backlog 85313a7e). The
        // review contract — the verdict lines `finish` accepts, the one-line
        // constitution checks, the `.coding/**` bookkeeping rule — is appended
        // by the TOOL, so it holds on every reviewer spawn even when the
        // dispatching agent's task omits it. For round >= 2 it also carries the
        // DELTA SCOPE: the base revision the previous round verified, the
        // delta-since-`<base>` instruction (naming the reviewer's own
        // `git_read` ops), and the mechanically-derived changed file set — so a
        // re-review verifies the delta instead of re-reading
        // the whole tree (plan febcd6f5 burned three whole-tree rounds to
        // verify a 1-3 file fix). NON-reviewer spawns are untouched: their task
        // stays byte-identical (the 1d0332ca invariant above).
        //
        // Best-effort throughout: a git failure degrades the changed-file set to
        // an instruction (contract-only preamble on a first round) and never
        // blocks a spawn, and a missing stamp only ever means the NEXT round
        // reviews full scope again — never a falsely narrower one.
        let mut stamped_base: Option<String> = None;
        let task = match (role.as_deref(), self.review_scope.as_ref()) {
            (Some("reviewer"), Some(wiring)) => {
                let (round, previous_base) = {
                    // Snapshot-and-drop: the workflow lock is shared with every
                    // workflow tool, so it is released before any git
                    // round-trip (never held across I/O).
                    let wf = wiring.workflow.lock().await;
                    let rounds = wf.review_rounds();
                    let round = rounds.len() as u32 + 1;
                    let previous_base = if round >= 2 {
                        rounds.last().map(|s| s.base.clone())
                    } else {
                        None
                    };
                    (round, previous_base)
                };
                // HEAD is this round's base: the dispatcher suspends while the
                // reviewer works, so the tree it starts looking at is the tree
                // this sha names.
                let head = wiring.scoper.head(&wiring.root).await.ok();
                let (changed, changed_note) = match previous_base.as_deref() {
                    Some(base) => match wiring.scoper.changed_paths(&wiring.root, base).await {
                        Ok(paths) => (paths, None),
                        Err(e) => (Vec::new(), Some(e)),
                    },
                    None => (Vec::new(), None),
                };
                let preamble = render_review_preamble(&ReviewScope {
                    round,
                    base: previous_base,
                    changed,
                    changed_note,
                });
                stamped_base = head;
                format!("{task}\n\n{preamble}")
            }
            // Every other spawn (including a reviewer with no scope wiring
            // and the unrestricted sub-agents) forwards the task verbatim.
            _ => task,
        };

        // Prefer the parent-aware spawn when the concrete spawner supports it
        // (the IPC layer's `IpcSpawner`) AND we know our owning agent's id, so
        // the child is registered as this agent's child and the completion-
        // notification feedback loop fires. Fall back to the plain trait
        // `spawn` (no parent) otherwise. Both arms spawn — the only
        // rejection (model override without the parent-aware path) already
        // returned above.
        let spawned = match (self.spawner.parent_aware(), self.parent_id) {
            (Some(spawner), Some(parent_id)) => {
                spawner
                    .spawn_with_parent(
                        &args.name,
                        &task,
                        Some(parent_id),
                        // An explicit `model` arg takes precedence (for
                        // reviewer spawns it is dispatch-denied unless it is
                        // the user-sanctioned failed-reviewer retry); otherwise
                        // a spawned reviewer gets the reviewing-model pin
                        // (None for other roles / no resolver / nothing
                        // configured — the default subagent model path,
                        // unchanged).
                        forced_model.or(reviewer_pin),
                        role.clone(),
                    )
                    .await
            }
            _ => self.spawner.spawn(&args.name, &task, role.clone()).await,
        };

        match spawned {
            Ok(id) => {
                // Stamp the round ONLY after a successful spawn: a rejected or
                // failed spawn must never advance it, or a crashed reviewer
                // would narrow the next round's scope to a diff it never
                // verified. Best-effort — the agent is already spawned, and an
                // unstamped round only WIDENS the next review (fail-safe).
                let mut stamp_note: Option<String> = None;
                if let (Some(base), Some(wiring)) =
                    (stamped_base.as_deref(), self.review_scope.as_ref())
                {
                    let mut wf = wiring.workflow.lock().await;
                    if let Err(e) = wf.record_review_round(base) {
                        stamp_note = Some(e.to_string());
                    }
                }
                let mut output = format!(
                    "Spawned background agent '{}' (id {id}) and started it on the task. \
                     Its events appear under that agent in the sidebar; you'll be notified \
                     when it finishes.",
                    args.name
                );
                if null_model_note {
                    output.push_str(
                        " Note: model \"null\" ignored — reserved sentinel for a \
                         stringified JSON null (backlog 3e6f7887), never a \
                         configured model id; the spawn uses its default model \
                         resolution.",
                    );
                }
                // Never silent: a failed stamp means the NEXT reviewer spawn
                // will review full scope, which the agent should know before it
                // reports the round as delta-scoped.
                if let Some(note) = stamp_note {
                    output.push_str(&format!(
                        " Note: the review round was not stamped ({note}) — the next \
                         reviewer spawn reviews full scope."
                    ));
                }
                ToolResult::success(output)
            }
            Err(e) => ToolResult::error(format!("failed to spawn agent: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    /// A mock spawner that records calls and returns a canned id.
    struct MockSpawner {
        calls: Mutex<Vec<(String, String, Option<String>)>>,
        result: Result<u64, String>,
    }

    impl MockSpawner {
        fn ok(id: u64) -> Self {
            Self {
                calls: Mutex::new(vec![]),
                result: Ok(id),
            }
        }
    }

    #[async_trait]
    impl AgentSpawner for MockSpawner {
        async fn spawn(&self, name: &str, task: &str, role: Option<String>) -> Result<u64, String> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), task.to_string(), role));
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn invalid_args_error() {
        let tool = SpawnAgentTool::new(Arc::new(MockSpawner::ok(1)));
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        // Sanitized (plan 21118961): names the tool, no raw serde text.
        assert!(
            result
                .output
                .starts_with("Error: The tool 'spawn_agent' failed"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("missing field"));
    }

    #[tokio::test]
    async fn empty_name_or_task_error() {
        let tool = SpawnAgentTool::new(Arc::new(MockSpawner::ok(1)));
        let r = tool.execute(json!({"name": "  ", "task": "x"})).await;
        assert!(!r.success);
        assert!(r.output.contains("non-empty 'name'"));

        let r = tool.execute(json!({"name": "x", "task": ""})).await;
        assert!(!r.success);
        assert!(r.output.contains("non-empty 'task'"));
    }

    #[tokio::test]
    async fn delegates_to_spawner() {
        let spawner = Arc::new(MockSpawner::ok(7));
        let tool = SpawnAgentTool::new(spawner.clone());
        let result = tool
            .execute(json!({"name": "reviewer", "task": "review the last commit"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("id 7"));

        let calls = spawner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "reviewer");
        assert_eq!(calls[0].1, "review the last commit");
    }

    /// A canned [`ReviewScoper`] — no git subprocess, no temp repo.
    struct FakeScoper {
        head: Result<String, String>,
        changed: Result<Vec<String>, String>,
    }

    impl FakeScoper {
        /// A healthy repo: HEAD `head9999`, one changed file.
        fn ok() -> Self {
            Self {
                head: Ok("head9999".to_string()),
                changed: Ok(vec!["M src/a.rs".to_string()]),
            }
        }
    }

    #[async_trait]
    impl ReviewScoper for FakeScoper {
        async fn head(&self, _root: &Path) -> Result<String, String> {
            self.head.clone()
        }

        async fn changed_paths(&self, _root: &Path, _base: &str) -> Result<Vec<String>, String> {
            self.changed.clone()
        }
    }

    /// A workflow (tokio mutex — NOT the test module's `std` one) with one
    /// active plan in a throwaway plans dir; the TempDir is returned so it
    /// outlives the test.
    fn workflow_with_plan() -> (tempfile::TempDir, Arc<tokio::sync::Mutex<Workflow>>) {
        let dir = tempfile::tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        (dir, Arc::new(tokio::sync::Mutex::new(wf)))
    }

    #[tokio::test]
    async fn reviewer_spawn_gets_the_contract_without_the_dispatcher_supplying_it() {
        // Acceptance (b): the standard preamble rides every reviewer spawn even
        // though this task mentions none of it — and round 1 has no delta scope
        // (there is nothing to delta against yet).
        let (_dir, workflow) = workflow_with_plan();
        let spawner = Arc::new(MockSpawner::ok(1));
        let tool = SpawnAgentTool::new(spawner.clone()).with_review_scope(
            Arc::clone(&workflow),
            PathBuf::from("."),
            Arc::new(FakeScoper::ok()),
        );
        let result = tool
            .execute(json!({
                "name": "reviewer",
                "task": "review the uncommitted changes",
                "role": "reviewer"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        let task = spawner.calls.lock().unwrap()[0].1.clone();
        assert!(
            task.starts_with("review the uncommitted changes"),
            "the dispatching agent's task must stay the head of the prompt: {task}"
        );
        assert!(task.contains("## Reviewer contract"), "{task}");
        assert!(task.contains("## Verdict: PASS"), "{task}");
        assert!(task.contains("## Verdict: FINDINGS (n high, n low)"), "{task}");
        assert!(task.contains(".coding/**"), "{task}");
        assert!(!task.contains("Re-review scope"), "{task}");
        // The round is stamped once the spawn succeeded.
        let rounds = workflow.lock().await.review_rounds().to_vec();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].base, "head9999");
    }

    #[tokio::test]
    async fn second_reviewer_spawn_is_delta_scoped() {
        // Acceptance (a): a 2nd reviewer spawn for a plan automatically names
        // its base revision, the delta instruction and the changed file set —
        // all mechanically derived, none supplied by the dispatching agent.
        let (_dir, workflow) = workflow_with_plan();
        workflow
            .lock()
            .await
            .record_review_round("abc1234")
            .unwrap();
        let spawner = Arc::new(MockSpawner::ok(2));
        let tool = SpawnAgentTool::new(spawner.clone()).with_review_scope(
            Arc::clone(&workflow),
            PathBuf::from("."),
            Arc::new(FakeScoper::ok()),
        );
        let result = tool
            .execute(json!({
                "name": "reviewer",
                "task": "re-review after fixing the findings",
                "role": "reviewer"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        let task = spawner.calls.lock().unwrap()[0].1.clone();
        assert!(task.contains("Re-review scope (round 2 of this plan)"), "{task}");
        assert!(task.contains("Base revision: `abc1234`"), "{task}");
        assert!(task.contains("the delta since `abc1234`"), "{task}");
        assert!(task.contains("M src/a.rs"), "{task}");
        let rounds = workflow.lock().await.review_rounds().to_vec();
        assert_eq!(rounds.len(), 2);
        assert_eq!(rounds[1].round, 2);
        assert_eq!(rounds[1].base, "head9999");
    }

    #[tokio::test]
    async fn a_head_failure_still_spawns_and_stamps_nothing() {
        // Best-effort: an unreadable HEAD must never block a spawn, and must
        // never leave a stamp behind — an unstamped round only WIDENS the next
        // review (fail-safe), never narrows it. The delta block STILL renders:
        // its lower bound is the PREVIOUS round's recorded base, not this
        // round's HEAD.
        let (_dir, workflow) = workflow_with_plan();
        workflow
            .lock()
            .await
            .record_review_round("abc1234")
            .unwrap();
        let spawner = Arc::new(MockSpawner::ok(3));
        let tool = SpawnAgentTool::new(spawner.clone()).with_review_scope(
            Arc::clone(&workflow),
            PathBuf::from("."),
            Arc::new(FakeScoper {
                head: Err("not a git repository".to_string()),
                changed: Err("not a git repository".to_string()),
            }),
        );
        let result = tool
            .execute(json!({"name": "reviewer", "task": "t", "role": "reviewer"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let task = spawner.calls.lock().unwrap()[0].1.clone();
        assert!(task.contains("## Reviewer contract"), "{task}");
        assert!(task.contains("Re-review scope (round 2 of this plan)"), "{task}");
        assert!(task.contains("Base revision: `abc1234`"), "{task}");
        assert!(task.contains("Changed file set: unavailable"), "{task}");
        // No new stamp: the round was never reviewed against a known state.
        let rounds = workflow.lock().await.review_rounds().to_vec();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].base, "abc1234");
    }

    #[tokio::test]
    async fn a_changed_paths_failure_degrades_to_the_command_not_a_file_list() {
        // HEAD readable, diff not: the delta block still names the base and the
        // command, and the round IS stamped (its base was readable).
        let (_dir, workflow) = workflow_with_plan();
        workflow
            .lock()
            .await
            .record_review_round("abc1234")
            .unwrap();
        let spawner = Arc::new(MockSpawner::ok(4));
        let tool = SpawnAgentTool::new(spawner.clone()).with_review_scope(
            Arc::clone(&workflow),
            PathBuf::from("."),
            Arc::new(FakeScoper {
                head: Ok("head9999".to_string()),
                changed: Err("fatal: bad revision".to_string()),
            }),
        );
        let result = tool
            .execute(json!({"name": "reviewer", "task": "t", "role": "reviewer"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let task = spawner.calls.lock().unwrap()[0].1.clone();
        assert!(task.contains("Changed file set: unavailable"), "{task}");
        assert!(task.contains("fatal: bad revision"), "{task}");
        assert!(task.contains("the delta since `abc1234`"), "{task}");
        assert_eq!(workflow.lock().await.review_rounds().len(), 2);
    }

    #[tokio::test]
    async fn non_reviewer_spawn_task_is_byte_identical() {
        // The 1d0332ca invariant holds through this change: only reviewer
        // spawns get a preamble.
        let (_dir, workflow) = workflow_with_plan();
        let spawner = Arc::new(MockSpawner::ok(5));
        let tool = SpawnAgentTool::new(spawner.clone()).with_review_scope(
            Arc::clone(&workflow),
            PathBuf::from("."),
            Arc::new(FakeScoper::ok()),
        );
        let result = tool
            .execute(json!({"name": "worker", "task": "do the thing"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(spawner.calls.lock().unwrap()[0].1, "do the thing");
        assert!(workflow.lock().await.review_rounds().is_empty());
    }

    #[tokio::test]
    async fn reviewer_spawn_without_scope_wiring_forwards_the_task_verbatim() {
        // Backward compatibility: no wiring (tests / store-less setups) behaves
        // exactly as it did before this change.
        let spawner = Arc::new(MockSpawner::ok(6));
        let tool = SpawnAgentTool::new(spawner.clone());
        let result = tool
            .execute(json!({
                "name": "reviewer",
                "task": "review the diff",
                "role": "reviewer"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(spawner.calls.lock().unwrap()[0].1, "review the diff");
    }

    #[tokio::test]
    async fn a_failed_spawn_stamps_no_round() {
        // A failed spawn must never advance the round: a crashed reviewer
        // would otherwise narrow the next round's scope to a diff it never
        // verified.
        let (_dir, workflow) = workflow_with_plan();
        let spawner = Arc::new(MockSpawner {
            calls: Mutex::new(vec![]),
            result: Err("boom".to_string()),
        });
        let tool = SpawnAgentTool::new(spawner.clone()).with_review_scope(
            Arc::clone(&workflow),
            PathBuf::from("."),
            Arc::new(FakeScoper::ok()),
        );
        let result = tool
            .execute(json!({"name": "reviewer", "task": "t", "role": "reviewer"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("failed to spawn agent"),
            "{}",
            result.output
        );
        assert!(workflow.lock().await.review_rounds().is_empty());
    }

    #[tokio::test]
    async fn an_unstampable_round_is_reported_not_silent() {
        // A reviewer spawned with no plan in flight: the preamble still rides,
        // the spawn succeeds, and the failed stamp is surfaced to the agent
        // (never a silent no-op).
        let dir = tempfile::tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(dir.path().join("plans"))));
        let spawner = Arc::new(MockSpawner::ok(9));
        let tool = SpawnAgentTool::new(spawner.clone()).with_review_scope(
            Arc::clone(&workflow),
            PathBuf::from("."),
            Arc::new(FakeScoper::ok()),
        );
        let result = tool
            .execute(json!({"name": "reviewer", "task": "t", "role": "reviewer"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("was not stamped"), "{}", result.output);
        assert_eq!(spawner.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn null_model_string_counts_as_unset() {
        // Backlog 3e6f7887 (live incident 2027-01-24): a transport that
        // stringifies JSON null for non-nullable string properties delivers
        // model:"null" — the tool must treat it as unset (no configured
        // model is ever named "null"), not fail resolution. JSON null was
        // already unset via serde; this pins both shapes.
        let spawner = Arc::new(MockSpawner::ok(3));
        let tool = SpawnAgentTool::new(spawner);
        let result = tool
            .execute(json!({"name": "x", "task": "y", "model": "null"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("id 3"));
        // The dropped stringified artifact is surfaced (review L1) —
        // never silently ignored.
        assert!(
            result.output.contains("model \"null\" ignored"),
            "the drop must be named in the output: {}",
            result.output
        );

        let result = tool
            .execute(json!({"name": "x", "task": "y", "model": null}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("id 3"));
        // JSON null is an explicit unset (the schema says "omit or pass
        // null") — no note.
        assert!(
            !result.output.contains("ignored"),
            "an explicit unset needs no note: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn propagates_spawn_error() {
        let spawner = Arc::new(MockSpawner {
            calls: Mutex::new(vec![]),
            result: Err("runtime unavailable".into()),
        });
        let tool = SpawnAgentTool::new(spawner);
        let result = tool.execute(json!({"name": "x", "task": "y"})).await;
        assert!(!result.success);
        assert!(result.output.contains("runtime unavailable"));
    }

    /// A parent-aware mock spawner: records the parent id passed to
    /// `spawn_with_parent` so tests can assert the tool threads its owning
    /// agent's id through. Also records whether the plain `spawn` was used.
    struct ParentAwareMock {
        parent_calls: Mutex<Vec<Option<u64>>>,
        plain_calls: Mutex<usize>,
        role_seen: Mutex<Option<String>>,
    }

    impl ParentAwareMock {
        fn new() -> Self {
            Self {
                parent_calls: Mutex::new(vec![]),
                plain_calls: Mutex::new(0),
                role_seen: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl AgentSpawner for ParentAwareMock {
        async fn spawn(
            &self,
            _name: &str,
            _task: &str,
            role: Option<String>,
        ) -> Result<u64, String> {
            *self.plain_calls.lock().unwrap() += 1;
            *self.role_seen.lock().unwrap() = role;
            Ok(1)
        }
        fn parent_aware(&self) -> Option<&dyn crate::runtime::ParentAwareSpawner> {
            Some(self)
        }
    }

    #[async_trait]
    impl crate::runtime::ParentAwareSpawner for ParentAwareMock {
        async fn spawn_with_parent(
            &self,
            _name: &str,
            _task: &str,
            parent_id: Option<u64>,
            _model: Option<crate::config::ModelRef>,
            role: Option<String>,
        ) -> Result<u64, String> {
            self.parent_calls.lock().unwrap().push(parent_id);
            *self.role_seen.lock().unwrap() = role;
            Ok(9)
        }
    }

    #[tokio::test]
    async fn with_parent_routes_through_parent_aware_spawn() {
        // A tool that knows its owning agent's id AND has a parent-aware
        // spawner must use `spawn_with_parent` with that id (this is what
        // registers the child as the agent's child for the completion
        // notification). The plain `spawn` must NOT be used.
        let spawner = Arc::new(ParentAwareMock::new());
        let tool = SpawnAgentTool::new(spawner.clone()).with_parent(42);
        let result = tool
            .execute(json!({"name": "child", "task": "do a thing"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("id 9"));

        let parent_calls = spawner.parent_calls.lock().unwrap();
        assert_eq!(parent_calls.as_slice(), &[Some(42)]);
        assert_eq!(
            *spawner.plain_calls.lock().unwrap(),
            0,
            "parent-aware spawn must be used, not the plain spawn"
        );
    }

    #[tokio::test]
    async fn reviewer_role_is_forwarded_to_spawner() {
        // A `role: "reviewer"` arg must be threaded through to the parent-aware
        // spawner (which sets the read-only tool allow-list on the spawned
        // workflow). The mock records the role it received.
        let spawner = Arc::new(ParentAwareMock::new());
        let tool = SpawnAgentTool::new(spawner.clone()).with_parent(42);
        let result = tool
            .execute(json!({
                "name": "reviewer",
                "task": "review the diff",
                "role": "reviewer"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(
            spawner.role_seen.lock().unwrap().as_deref(),
            Some("reviewer"),
            "role:\"reviewer\" must be forwarded to the spawner"
        );
    }

    #[tokio::test]
    async fn unknown_role_errors_without_spawning() {
        // An unrecognized role must error before any spawn occurs — an
        // unknown role can't silently spawn an unrestricted agent.
        let spawner = Arc::new(ParentAwareMock::new());
        let tool = SpawnAgentTool::new(spawner.clone()).with_parent(42);
        let result = tool
            .execute(json!({
                "name": "x",
                "task": "t",
                "role": "admin"
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("unknown role"),
            "got: {}",
            result.output
        );
        assert!(
            spawner.parent_calls.lock().unwrap().is_empty(),
            "no spawn should occur for an unknown role"
        );
    }

    #[tokio::test]
    async fn no_role_arg_forwards_none() {
        // No `role` arg → None is forwarded (an unrestricted sub-agent).
        let spawner = Arc::new(ParentAwareMock::new());
        let tool = SpawnAgentTool::new(spawner.clone()).with_parent(42);
        let result = tool.execute(json!({"name": "r", "task": "t"})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            spawner.role_seen.lock().unwrap().is_none(),
            "no role should be forwarded when the arg is absent"
        );
    }

    #[tokio::test]
    async fn without_parent_id_falls_back_to_plain_spawn() {
        // A parent-aware spawner but a tool with NO owning agent id (e.g. a
        // test-built loop) must fall back to the plain `spawn` — there's no
        // parent to record, so no completion notification should fire.
        let spawner = Arc::new(ParentAwareMock::new());
        let tool = SpawnAgentTool::new(spawner.clone());
        let result = tool.execute(json!({"name": "c", "task": "t"})).await;
        assert!(result.success);
        assert_eq!(*spawner.plain_calls.lock().unwrap(), 1);
        assert!(
            spawner.parent_calls.lock().unwrap().is_empty(),
            "no parent id → plain spawn, not spawn_with_parent"
        );
    }

    #[tokio::test]
    async fn with_parent_but_plain_spawner_falls_back() {
        // A tool WITH an owning id but a spawner that ISN'T parent-aware
        // (parent_aware() returns None) must still spawn successfully via the
        // plain path — it just won't record a parent.
        let spawner = Arc::new(MockSpawner::ok(5));
        let tool = SpawnAgentTool::new(spawner.clone()).with_parent(7);
        let result = tool.execute(json!({"name": "c", "task": "t"})).await;
        assert!(result.success);
        assert!(result.output.contains("id 5"));
        assert_eq!(spawner.calls.lock().unwrap().len(), 1);
    }

    // --- model override -----------------------------------------------------

    /// A mock resolver that resolves a single canned model id.
    struct CannedResolver {
        model_id: String,
        model_ref: crate::config::ModelRef,
    }

    #[async_trait]
    impl crate::model_resolver::ModelResolver for CannedResolver {
        fn resolve(
            &self,
            _context: crate::model_resolver::ModelContext<'_>,
        ) -> Option<crate::config::ModelRef> {
            None
        }
        fn build_turn_provider(
            &self,
            _model: &crate::config::ModelRef,
            _fill_rate: f64,
        ) -> Option<(
            Arc<dyn crate::provider::LlmClient>,
            crate::agent::context::ContextManager,
        )> {
            None
        }
        fn resolve_model_id(&self, model_id: &str) -> Option<crate::config::ModelRef> {
            if model_id == self.model_id {
                Some(self.model_ref.clone())
            } else {
                None
            }
        }
    }

    /// A parent-aware mock that records the forced model passed to it.
    struct ModelRecordingSpawner {
        model_seen: Mutex<Option<crate::config::ModelRef>>,
    }

    impl ModelRecordingSpawner {
        fn new() -> Self {
            Self {
                model_seen: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl AgentSpawner for ModelRecordingSpawner {
        async fn spawn(
            &self,
            _name: &str,
            _task: &str,
            _role: Option<String>,
        ) -> Result<u64, String> {
            Ok(1)
        }
        fn parent_aware(&self) -> Option<&dyn crate::runtime::ParentAwareSpawner> {
            Some(self)
        }
    }

    #[async_trait]
    impl crate::runtime::ParentAwareSpawner for ModelRecordingSpawner {
        async fn spawn_with_parent(
            &self,
            _name: &str,
            _task: &str,
            _parent_id: Option<u64>,
            model: Option<crate::config::ModelRef>,
            _role: Option<String>,
        ) -> Result<u64, String> {
            *self.model_seen.lock().unwrap() = model;
            Ok(11)
        }
    }

    #[tokio::test]
    async fn model_arg_resolves_and_is_passed_to_spawner() {
        // A `model` arg with a resolver that knows the id → the resolved
        // ModelRef is forwarded to the parent-aware spawner.
        let resolver = Arc::new(CannedResolver {
            model_id: "deepseek-v4-flash-gcp".into(),
            model_ref: crate::config::ModelRef {
                endpoint: "openai".into(),
                model: "deepseek-v4-flash-gcp".into(),
                reasoning_effort: None,
            },
        });
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver);
        let result = tool
            .execute(json!({
                "name": "reviewer",
                "task": "review the diff",
                "model": "deepseek-v4-flash-gcp"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        let model_seen = spawner.model_seen.lock().unwrap().clone();
        let model = model_seen.expect("a model should have been forwarded");
        assert_eq!(model.endpoint, "openai");
        assert_eq!(model.model, "deepseek-v4-flash-gcp");
    }

    #[tokio::test]
    async fn unknown_model_id_errors() {
        // A `model` arg the resolver doesn't know → error, no spawn.
        let resolver = Arc::new(CannedResolver {
            model_id: "known".into(),
            model_ref: crate::config::ModelRef {
                endpoint: "openai".into(),
                model: "known".into(),
                reasoning_effort: None,
            },
        });
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver);
        let result = tool
            .execute(json!({"name": "r", "task": "t", "model": "does-not-exist"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("unknown model id"));
        assert!(
            spawner.model_seen.lock().unwrap().is_none(),
            "no spawn should have occurred"
        );
    }

    #[tokio::test]
    async fn model_arg_without_resolver_errors() {
        // A `model` arg but no resolver wired → error.
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone()).with_parent(42);
        let result = tool
            .execute(json!({"name": "r", "task": "t", "model": "anything"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("model selection unavailable"));
    }

    #[tokio::test]
    async fn no_model_arg_uses_plain_spawn_path() {
        // No `model` arg → None is forwarded (the normal subagent model path).
        let resolver = Arc::new(CannedResolver {
            model_id: "x".into(),
            model_ref: crate::config::ModelRef {
                endpoint: "e".into(),
                model: "x".into(),
                reasoning_effort: None,
            },
        });
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver);
        let result = tool.execute(json!({"name": "r", "task": "t"})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            spawner.model_seen.lock().unwrap().is_none(),
            "no model should be forwarded when the arg is absent"
        );
    }

    // --- reviewer model pin (spawn-time) ------------------------------------

    /// A resolver double mirroring the production arms the reviewer pin
    /// depends on (model_resolver.rs): `resolve(Reviewing, is_subagent=false)`
    /// → the reviewing slot falling back to executing; `resolve(Reviewing,
    /// is_subagent=true)` → the subagent slot. Records every context it was
    /// asked with as `(state == Reviewing, is_subagent)` — the pin must NOT
    /// consult the per-turn state chain anymore ([models.reviewing] is
    /// reviewer-spawn-only), so every pin test asserts `asked` stays EMPTY:
    /// `resolve` always returns None here, and a regressed pin would visibly
    /// fall through to the default model. `reviewer_asks` counts
    /// `resolve_reviewer_model` calls (the pin's sole model source).
    struct StateResolver {
        reviewing: Option<crate::config::ModelRef>,
        executing: Option<crate::config::ModelRef>,
        subagent: Option<crate::config::ModelRef>,
        /// Optional canned `resolve_model_id` mapping (for the explicit-arg
        /// precedence test).
        canned_id: Option<String>,
        canned_ref: Option<crate::config::ModelRef>,
        asked: Mutex<Vec<(bool, bool)>>,
        reviewer_asks: Mutex<usize>,
    }

    impl StateResolver {
        fn new(
            reviewing: Option<crate::config::ModelRef>,
            executing: Option<crate::config::ModelRef>,
            subagent: Option<crate::config::ModelRef>,
        ) -> Self {
            Self {
                reviewing,
                executing,
                subagent,
                canned_id: None,
                canned_ref: None,
                asked: Mutex::new(Vec::new()),
                reviewer_asks: Mutex::new(0),
            }
        }

        fn with_canned_model_id(mut self, id: &str, r: crate::config::ModelRef) -> Self {
            self.canned_id = Some(id.to_string());
            self.canned_ref = Some(r);
            self
        }
    }

    fn mref(model: &str) -> crate::config::ModelRef {
        crate::config::ModelRef {
            endpoint: "test-ep".into(),
            model: model.into(),
            reasoning_effort: None,
        }
    }

    #[async_trait]
    impl crate::model_resolver::ModelResolver for StateResolver {
        fn resolve(
            &self,
            context: crate::model_resolver::ModelContext<'_>,
        ) -> Option<crate::config::ModelRef> {
            self.asked.lock().unwrap().push((
                context.workflow_state == crate::workflow::WorkflowState::Reviewing,
                context.is_subagent,
            ));
            // The reviewer pin must NOT ride the per-turn state chain anymore
            // (decision 2026-12-06): [models.reviewing] is resolved solely by
            // resolve_reviewer_model below. Always return None here so a
            // regressed pin visibly falls through to the default model and
            // fails the forwarded-model assertions in the tests below.
            None
        }
        fn resolve_reviewer_model(&self) -> Option<crate::config::ModelRef> {
            *self.reviewer_asks.lock().unwrap() += 1;
            // Mirror the production chain: reviewing → executing → subagent.
            self.reviewing
                .clone()
                .or_else(|| self.executing.clone())
                .or_else(|| self.subagent.clone())
        }
        fn build_turn_provider(
            &self,
            _model: &crate::config::ModelRef,
            _fill_rate: f64,
        ) -> Option<(
            Arc<dyn crate::provider::LlmClient>,
            crate::agent::context::ContextManager,
        )> {
            None
        }
        fn resolve_model_id(&self, model_id: &str) -> Option<crate::config::ModelRef> {
            match (&self.canned_id, &self.canned_ref) {
                (Some(id), Some(r)) if id == model_id => Some(r.clone()),
                _ => None,
            }
        }
    }

    #[tokio::test]
    async fn reviewer_without_model_arg_pins_reviewing_model() {
        // role:"reviewer", no `model` arg, [models.reviewing] configured →
        // the spawned reviewer is forced onto the reviewing model.
        let resolver = Arc::new(StateResolver::new(Some(mref("review-model")), None, None));
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver.clone());
        let result = tool
            .execute(json!({"name": "rev", "task": "review it", "role": "reviewer"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let model = spawner
            .model_seen
            .lock()
            .unwrap()
            .clone()
            .expect("the reviewing model should be forwarded");
        assert_eq!(model.model, "review-model");
        // The pin resolved via resolve_reviewer_model — exactly once — and
        // NEVER consulted the per-turn state chain (resolve() stays unused:
        // a regressed two-step pin would fall through to the default model
        // and fail the forwarded-model assertion above).
        assert_eq!(*resolver.reviewer_asks.lock().unwrap(), 1);
        assert!(
            resolver.asked.lock().unwrap().is_empty(),
            "the pin must use resolve_reviewer_model, not the state chain"
        );
    }

    #[tokio::test]
    async fn reviewer_falls_back_to_executing_model() {
        // No [models.reviewing] but [models.executing] set → the reviewer
        // chain's back-compat link supplies the executing model. (The MAIN
        // agent in Reviewing also runs executing now — but via its own state
        // arm, not via this chain.)
        let resolver = Arc::new(StateResolver::new(None, Some(mref("exec-model")), None));
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver.clone());
        let result = tool
            .execute(json!({"name": "rev", "task": "review it", "role": "reviewer"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let model = spawner
            .model_seen
            .lock()
            .unwrap()
            .clone()
            .expect("the executing fallback should be forwarded");
        assert_eq!(model.model, "exec-model");
        assert_eq!(*resolver.reviewer_asks.lock().unwrap(), 1);
        assert!(resolver.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reviewer_falls_back_to_subagent_slot() {
        // Neither reviewing nor executing set, but [models.subagent] is →
        // the reviewer chain's last link supplies it.
        let resolver = Arc::new(StateResolver::new(None, None, Some(mref("sub-model"))));
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver.clone());
        let result = tool
            .execute(json!({"name": "rev", "task": "review it", "role": "reviewer"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let model = spawner
            .model_seen
            .lock()
            .unwrap()
            .clone()
            .expect("the subagent model should be forwarded");
        assert_eq!(model.model, "sub-model");
        assert_eq!(*resolver.reviewer_asks.lock().unwrap(), 1);
        assert!(resolver.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn explicit_model_arg_beats_reviewer_pin() {
        // An explicit `model` arg wins over the pin — and the pin must not
        // even be consulted (no resolver call recorded at all).
        let resolver = Arc::new(
            StateResolver::new(Some(mref("review-model")), None, None)
                .with_canned_model_id("explicit-model", mref("explicit-model")),
        );
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver.clone());
        let result = tool
            .execute(json!({
                "name": "rev",
                "task": "review it",
                "role": "reviewer",
                "model": "explicit-model"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        let model = spawner
            .model_seen
            .lock()
            .unwrap()
            .clone()
            .expect("the explicit model should be forwarded");
        assert_eq!(model.model, "explicit-model");
        assert!(
            resolver.asked.lock().unwrap().is_empty()
                && *resolver.reviewer_asks.lock().unwrap() == 0,
            "an explicit model arg must short-circuit the pin"
        );
    }

    #[tokio::test]
    async fn reviewer_without_any_configured_model_gets_default() {
        // Nothing configured anywhere → the chain returns None → no model is
        // forwarded; the spawned reviewer runs on the default provider
        // (unchanged behavior).
        let resolver = Arc::new(StateResolver::new(None, None, None));
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver.clone());
        let result = tool
            .execute(json!({"name": "rev", "task": "review it", "role": "reviewer"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            spawner.model_seen.lock().unwrap().is_none(),
            "no configured model → no pin, default provider"
        );
        assert_eq!(*resolver.reviewer_asks.lock().unwrap(), 1);
        assert!(resolver.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn non_reviewer_role_gets_no_pin() {
        // A plain (unrestricted) sub-agent is NOT pinned, even when
        // [models.reviewing] is configured — the pin is reviewer-only.
        let resolver = Arc::new(StateResolver::new(Some(mref("review-model")), None, None));
        let spawner = Arc::new(ModelRecordingSpawner::new());
        let tool = SpawnAgentTool::new(spawner.clone())
            .with_parent(42)
            .with_model_resolver(resolver.clone());
        let result = tool
            .execute(json!({"name": "worker", "task": "do things"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            spawner.model_seen.lock().unwrap().is_none(),
            "non-reviewer roles keep the default subagent model path"
        );
        assert!(
            resolver.asked.lock().unwrap().is_empty()
                && *resolver.reviewer_asks.lock().unwrap() == 0,
            "the pin must not consult the resolver for non-reviewer roles"
        );
    }

    // --- recall rider on the task (first prompt) ----------------------------

    #[tokio::test]
    async fn task_seeded_with_recalled_context_rider() {
        // A task text that matches a stored memory earns the passive
        // RECALLED CONTEXT rider INSIDE the task passed to the spawner — the
        // spawned agent's FIRST PROMPT — so its memory_search-before-work
        // rule is structurally enforced the same way create_plan's rider
        // does for plans.
        let embedder: Arc<dyn crate::memory::Embedder> =
            Arc::new(crate::memory::embedder::HashEmbedder::new());
        let store = Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap());
        // Seeded "now" (was 1_000_000_000): the rider's recency gate drops
        // broad hits older than 14 days — a 2001 SPEC seed would silently
        // stop riding (deliberate, documented change; bug records are the
        // age-exempt class, plain spec/plan seeds must be fresh).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        store
            .write(crate::memory::Memory::new(
                crate::memory::MemoryTier::Semantic,
                "SPEC: the kettle safety valve opens above 2 bar",
                "The kettle safety valve opens above 2 bar; boiler plate documents the spec.",
                now,
            ))
            .await
            .unwrap();
        let spawner = Arc::new(MockSpawner::ok(3));
        let tool = SpawnAgentTool::new(spawner.clone()).with_memory(store.clone());
        let result = tool
            .execute(json!({
                "name": "researcher",
                "task": "work on the kettle safety valve"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let calls = spawner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        // The TASK the spawner received carries the rider; the caller's
        // success message (built from the args) is unchanged.
        assert!(
            calls[0].1.contains("work on the kettle safety valve"),
            "original task text preserved: {}",
            calls[0].1
        );
        assert!(
            calls[0].1.contains("RECALLED CONTEXT"),
            "the rider rides the spawned FIRST PROMPT: {}",
            calls[0].1
        );
        assert!(
            calls[0].1.contains("SPEC: the kettle safety valve"),
            "the matched hit is surfaced in the task: {}",
            calls[0].1
        );
        // Passive by construction: recall_peek must NOT bump access counts.
        let rows = store
            .list_by_tier(crate::memory::MemoryTier::Semantic)
            .await
            .unwrap();
        assert_eq!(rows[0].access_count, 0, "rider is passive (no access bump)");
    }

    #[tokio::test]
    async fn reviewer_task_skips_the_recalled_context_rider() {
        // Backlog 1d0332ca (user report 2026-09-19): reviewer prompts must
        // be CLEAN — the reviewer protocol hands the spawned reviewer a
        // self-contained task, and the rider would pollute it with
        // parent-project memories (noise at best, review bias at worst:
        // the reviewer must judge the diff/plan, not be steered by recalled
        // context) plus prompt bloat on every review. With matching hits
        // in the store, a role:"reviewer" spawn's task reaches the spawner
        // BYTE-IDENTICAL to the task argument, while a normal spawn on
        // the same store still carries the rider.
        let embedder: Arc<dyn crate::memory::Embedder> =
            Arc::new(crate::memory::embedder::HashEmbedder::new());
        let store = Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        store
            .write(crate::memory::Memory::new(
                crate::memory::MemoryTier::Semantic,
                "SPEC: the kettle safety valve opens above 2 bar",
                "The kettle safety valve opens above 2 bar; boiler plate documents the spec.",
                now,
            ))
            .await
            .unwrap();
        let spawner = Arc::new(MockSpawner::ok(3));
        let tool = SpawnAgentTool::new(spawner.clone()).with_memory(store.clone());
        // The reviewer spawn: the task must pass through byte-identical.
        let result = tool
            .execute(json!({
                "name": "reviewer",
                "task": "work on the kettle safety valve",
                "role": "reviewer"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // A normal spawn on the same store: the rider still rides.
        let result = tool
            .execute(json!({
                "name": "researcher",
                "task": "work on the kettle safety valve"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let calls = spawner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].1, "work on the kettle safety valve",
            "reviewer task must be byte-identical (no rider): {:?}",
            calls[0].1
        );
        assert_eq!(calls[0].2.as_deref(), Some("reviewer"));
        assert!(
            calls[1].1.contains("RECALLED CONTEXT"),
            "normal spawns keep the rider: {:?}",
            calls[1].1
        );
        assert!(
            calls[1].1.contains("SPEC: the kettle safety valve"),
            "the matched hit rides the normal spawn's task: {:?}",
            calls[1].1
        );
    }

    #[tokio::test]
    async fn task_unchanged_without_store_or_hits() {
        // (a) No store wired → the task text reaches the spawner byte-identical.
        let spawner = Arc::new(MockSpawner::ok(4));
        let tool = SpawnAgentTool::new(spawner.clone());
        let result = tool
            .execute(json!({"name": "r", "task": "exact original task"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(spawner.calls.lock().unwrap()[0].1, "exact original task");

        // (b) A store whose only memory shares NO vocabulary with the task
        // (recall prefiters through FTS — a disjoint phrase can't match) →
        // no rider block. (Fixture note: "unrelated"-style negatives DO
        // match when they share tokens with the task; keep them disjoint.)
        let embedder: Arc<dyn crate::memory::Embedder> =
            Arc::new(crate::memory::embedder::HashEmbedder::new());
        let store = Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap());
        store
            .write(crate::memory::Memory::new(
                crate::memory::MemoryTier::Semantic,
                "SPEC: kitchen sink drains slowly",
                "Plumbing notes about drain flow, a different project entirely.",
                1_000_000_001,
            ))
            .await
            .unwrap();
        let spawner = Arc::new(MockSpawner::ok(5));
        let tool = SpawnAgentTool::new(spawner.clone()).with_memory(store.clone());
        let result = tool
            .execute(json!({"name": "r", "task": "assemble quantum widget catalogue"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            spawner.calls.lock().unwrap()[0].1,
            "assemble quantum widget catalogue",
            "no-store and no-hit cases both keep the task unchanged"
        );
    }
}
