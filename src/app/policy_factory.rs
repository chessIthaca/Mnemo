// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The app-side policy factory (job 2b, step 2b.2): implements the kernel's
//! [`AgentPolicyFactory`](crate::agent::policy::AgentPolicyFactory) —
//! building each agent's [`WorkflowPolicy`] and registering the app-owned
//! tools (ask_user + the backlog trio) onto the loop's registry. The backlog
//! wiring (the store + the UI notifier) lives here as inherent setters so
//! the kernel never names the app's wiring types at the trait (master-spec
//! :255-258; the D5 register pattern: kernel-called, app-owned).

use tokio::sync::Mutex;
use std::sync::Arc;

use crate::agent::policy::{
    AgentPolicyFactory, PolicyBuildCtx, PolicyBuildResult, PolicyFactoryCtx,
};
use crate::app::backlog::BacklogStore;
use crate::app::tool::workflow::ask_user::AskUserTool;
use crate::app::tool::workflow::backlog::{BacklogAddTool, BacklogListTool, BacklogStatusTool};
use crate::app::workflow::Workflow;
use crate::app::tool::workflow::plan::{
    AbandonPlanTool, CompleteStepTool, CreatePlanTool, CurrentPlanTool, FinishTool,
    UpdatePlanTool,
};
use crate::app::tool::workflow::skill::{
    AbandonSkillTool, SkillCreateTool, SkillEndTool, SkillReloadTool, SkillStartTool,
};
use crate::app::workflow_policy::WorkflowPolicy;
use crate::tool::ToolRegistry;

/// The shared backlog wiring handed to every `backlog_add` tool: the store
/// handle (the SAME `Arc` the Tauri `backlog_*` commands lock) plus the
/// optional UI notifier the app layer injects so agent adds emit
/// `backlog://changed` live. Relocated here from the kernel factory (2b.2).
struct BacklogWiring {
    store: Arc<Mutex<BacklogStore>>,
    on_changed: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The app-side policy factory: builds [`WorkflowPolicy`] per agent and
/// registers the app-owned tools. Production wires it at startup
/// (`set_policy_factory` on the kernel factory); the kernel test helpers
/// wire it so factory-built agents keep the 2a-era semantics.
pub struct WorkflowPolicyFactory {
    backlog: std::sync::RwLock<Option<BacklogWiring>>,
    /// The shared wiring the plan/skill tools carry (2b.3: the registrations
    /// moved here from the kernel factory — the factory state they consumed
    /// moves with them).
    memory: Option<Arc<dyn crate::memory::MemoryStoreTrait>>,
    knowledge: Option<Arc<crate::memory::knowledge::KnowledgeStore>>,
    codegraph: Option<Arc<crate::codegraph::CodeGraph>>,
    reviews_dir: std::path::PathBuf,
    skills: Option<Arc<crate::app::skill::SkillLibrary>>,
}

impl WorkflowPolicyFactory {
    pub fn new(
        memory: Option<Arc<dyn crate::memory::MemoryStoreTrait>>,
        knowledge: Option<Arc<crate::memory::knowledge::KnowledgeStore>>,
        codegraph: Option<Arc<crate::codegraph::CodeGraph>>,
        reviews_dir: std::path::PathBuf,
        skills: Option<Arc<crate::app::skill::SkillLibrary>>,
    ) -> Self {
        Self {
            backlog: std::sync::RwLock::new(None),
            memory,
            knowledge,
            codegraph,
            reviews_dir,
            skills,
        }
    }

    /// Wire in the shared backlog store that backs the `backlog_add` tool.
    ///
    /// Called once by the IPC layer at startup with the SAME
    /// `Arc<tokio::sync::Mutex<BacklogStore>>` the `backlog_*` commands use,
    /// so agent adds and UI mutations serialize through one mutex and can
    /// never clobber each other. The wiring slot also carries an optional
    /// UI notifier injected later via
    /// [`set_backlog_notifier`](Self::set_backlog_notifier) so agent adds
    /// emit `backlog://changed` live. Only affects agents built **after**
    /// this call (the tool is read per `build`), so the IPC layer sets it
    /// before building the main agent. Also omitted in console mode
    /// (`-console`): no store is opened there, so the tool is not registered
    /// (review finding 7).
    /// The skill-library handle (2b.3): the IPC's `enter_skill` reads it
    /// app→app — the kernel factory's skills surface retired with the
    /// registration move.
    pub fn skills_handle(&self) -> Option<Arc<crate::app::skill::SkillLibrary>> {
        self.skills.clone()
    }

    pub fn set_backlog(&self, store: Arc<Mutex<BacklogStore>>) {
        *self.backlog.write().expect("backlog lock poisoned") = Some(BacklogWiring {
            store,
            on_changed: None,
        });
    }

    /// Wire the UI notifier into the backlog wiring: after every successful
    /// agent-side `backlog_add`, the callback fires so the app layer can
    /// emit `backlog://changed` and the Backlog tab updates live. No-op when
    /// no store is wired (the IPC layer calls
    /// [`set_backlog`](Self::set_backlog) first, before this).
    pub fn set_backlog_notifier(&self, on_changed: Arc<dyn Fn() + Send + Sync>) {
        let mut slot = self.backlog.write().expect("backlog lock poisoned");
        if let Some(wiring) = slot.as_mut() {
            wiring.on_changed = Some(on_changed);
        }
    }
}

impl Default for WorkflowPolicyFactory {
    fn default() -> Self {
        Self::new(None, None, None, std::path::PathBuf::new(), None)
    }
}

impl AgentPolicyFactory for WorkflowPolicyFactory {
    fn create_workflow(
        &self,
        plans_dir: &std::path::Path,
        is_subagent: bool,
    ) -> Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle> {
        // 2b.3: the workflow construction moved kernel→app — the app owns
        // the type and every app-side fact from the point of construction.
        // The Subagent state stamp rides the app-side spawn path
        // (enter_subagent_state after load_latest, post-build — the spawner
        // downcasts app→app through as_any_arc); the build-time bool exists
        // for 2c's builder to stamp at construction and stays false for
        // today's build path.
        let mut workflow = crate::app::workflow::Workflow::new(plans_dir);
        // Fresh per-agent workflow — load the latest plan from disk so a
        // newly-spawned agent resumes an in-flight plan. The `let _ =` is
        // intentional: a fresh/empty plans dir legitimately has no plan to
        // load (the kernel's pre-2b.3 comment, carried verbatim).
        let _ = workflow.load_latest();
        if is_subagent {
            workflow.enter_subagent_state();
        }
        Arc::new(Arc::new(tokio::sync::Mutex::new(workflow)))
    }

    fn build_policy(&self, ctx: &PolicyFactoryCtx) -> PolicyBuildResult {
        // The kernel hands the loop's workflow handle back through the
        // neutral trait (2b.3); the app factory recovers its concrete
        // lockable Arc via as_any_arc — an app→app cast, the legal
        // direction. A mismatch means the kernel changed what it passes —
        // fail loudly rather than silently.
        let any_arc = ctx.workflow.as_any_arc();
        eprintln!(
            "DBG2b3: inner={} double={} unwired={}",
            any_arc.downcast_ref::<Arc<Mutex<Workflow>>>().is_some(),
            any_arc
                .downcast_ref::<Arc<Arc<Mutex<Workflow>>>>()
                .is_some(),
            any_arc.downcast_ref::<crate::agent::workflow_handle::UnwiredWorkflow>().is_some(),
        );
        let workflow = any_arc
            .downcast_ref::<Arc<Mutex<Workflow>>>()
            .expect("the kernel hands the policy factory the workflow it built")
            .clone();
        // The concrete policy stays app-side; the tail attach rides the
        // returned closure (the 2a.6 loop back-ref — idempotent OnceLock,
        // the first attach wins).
        let policy = Arc::new(WorkflowPolicy::new(workflow));
        let concrete: Arc<WorkflowPolicy> = Arc::clone(&policy);
        PolicyBuildResult {
            policy,
            attach_loop: Box::new(move |loop_weak| concrete.attach_loop(loop_weak)),
        }
    }

    fn register_tools(&self, registry: &mut ToolRegistry, ctx: &PolicyBuildCtx) {
        // 2b.3: the plan/skill registrations moved here from the kernel
        // factory (the kernel never names the app tool types). The concrete
        // workflow recovers app→app via as_any_arc — the tools hold the
        // concrete; every kernel consumer rides the trait's opaque
        // representations.
        let workflow = ctx
            .workflow
            .as_any_arc()
            .downcast_ref::<Arc<Mutex<Workflow>>>()
            .expect("the kernel hands the policy factory the workflow it built")
            .clone();
        let mut create = CreatePlanTool::new(workflow.clone());
        if let Some(store) = &self.memory {
            create = create.with_memory(store.clone());
        }
        registry.register(Box::new(create));
        // The harness-run verify handle rides the plan-step tick (the
        // cost-saving chain's verification item): a successful skeleton
        // complete_step runs `[general.verify]`'s command and appends the
        // compact evidence note — no model roundtrip spent deciding to run.
        let mut complete_step =
            CompleteStepTool::new(workflow.clone()).with_step_verify(ctx.step_verify.clone());
        // The budget layer's evidence row (backlog a25a5323): with the store
        // wired, a successful verification records its outcome in the
        // `spend_events` ledger for the per-plan cost report.
        if let Some(store) = &self.memory {
            complete_step = complete_step.with_memory(store.clone());
        }
        registry.register(Box::new(complete_step));
        registry.register(Box::new(UpdatePlanTool::new(workflow.clone())));
        // abandon_plan supersedes the abandoned plan's lingering crash
        // markers when the memory store is wired (Phase 4 hygiene — mirrors
        // finish's marker supersede; see AbandonPlanTool::with_memory).
        let mut abandon = AbandonPlanTool::new(workflow.clone());
        if let Some(store) = &self.memory {
            abandon = abandon.with_memory(store.clone());
        }
        registry.register(Box::new(abandon));
        // finish is the only path from Reviewing → Complete, gated on a review
        // report under .coding/reviews/ (the closing sequence's exit gate).
        // The reviews dir is the factory's main-derived path (NOT the per-agent
        // workflow's plans_dir) so a per-agent plans dir doesn't redirect it.
        // Phase 4: the finish hook also auto-captures the PLAN:/BUG: digests
        // (memory) and gates bug_fixing regression tests on the code graph.
        let mut finish = FinishTool::new(workflow.clone(), self.reviews_dir.clone());
        if let Some(store) = &self.memory {
            finish = finish.with_memory(store.clone());
        }
        // The knowledge-file backing: the BUG: capture lands in a knowledge
        // file (the truth) instead of an authored row — same store-derived
        // path as the memory tools' backing.
        if let Some(knowledge) = &self.knowledge {
            finish = finish.with_knowledge(knowledge.clone());
        }
        // A root-spec agent's finish gate validates regression tests against
        // the worktree's graph (the spec's), not the factory's.
        let finish_graph = match ctx.root.as_ref() {
            Some(spec) => spec.codegraph.clone(),
            None => self.codegraph.clone(),
        };
        if let Some(graph) = &finish_graph {
            finish = finish.with_codegraph(graph.clone());
        }
        registry.register(Box::new(finish));
        // current_plan is a read-only query (available in every state, like
        // ask_user + memory tools) so the agent can recover when disoriented
        // about which stacked plan is active.
        // current_plan reads the plan file for THIS agent's workflow stack —
        // the build's plans dir (main dir, per-agent dir, or worktree dir),
        // not the factory's main dir (a worktree agent must see its own
        // plans, not the main agent's).
        registry.register(Box::new(CurrentPlanTool::new(
            workflow.clone(),
            ctx.plans_dir.clone(),
        )));
        // ask_user is always available (not gated by ToolFilter — like memory
        // tools) so the agent can ask a question in any workflow state. The
        // pause is driven from dispatch, not the tool's execute().
        registry.register(Box::new(AskUserTool::new()));
        // backlog_add shares the app's one store handle (the same Arc the
        // IPC backlog commands lock) — the sanctioned agent write path to
        // the user's backlog (.coding/backlog.jsonl is protected from the
        // file tools). The optional on_changed notifier (wired by the IPC
        // layer via set_backlog_notifier) fires after each add so the
        // Backlog tab updates live. Omitted entirely when no store is
        // wired (tests / NeedsProject).
        if let Some(wiring) = self.backlog.read().expect("backlog lock poisoned").as_ref() {
            registry.register(Box::new(
                BacklogAddTool::new(Arc::clone(&wiring.store), wiring.on_changed.clone()),
            ));
            // backlog_status — same wiring; changes status through the same
            // guarded transition table the harness uses (see the tool docs).
            registry.register(Box::new(
                BacklogStatusTool::new(Arc::clone(&wiring.store), wiring.on_changed.clone()),
            ));
            // backlog_list — read-only query; needs no notifier (it never
            // mutates). Safe for read-only reviewers (their strict allow-list
            // names it): querying the backlog is allowed, modifying it is not.
            registry.register(Box::new(
                BacklogListTool::new(Arc::clone(&wiring.store)),
            ));
        }
        // The skill tools (2b.3: moved from the kernel factory). The
        // authoring tool gets the AGENT's sandbox (a root-spec agent gets
        // its worktree's), so skill_create obeys the same path policy the
        // file tools do: the link-free creation ladder + the `.coding/`
        // hardlink guard. A worktree agent therefore cannot author into
        // another tree's skills dir — by design, mirroring every other
        // write.
        if let Some(skills) = &self.skills {
            registry.register(Box::new(SkillStartTool::new(
                workflow.clone(),
                Arc::clone(skills),
            )));
            registry.register(Box::new(SkillEndTool::new(workflow.clone())));
            registry.register(Box::new(AbandonSkillTool::new(workflow.clone())));
            registry.register(Box::new(SkillReloadTool::new(Arc::clone(skills))));
            registry.register(Box::new(SkillCreateTool::new(
                Arc::clone(skills),
                ctx.sandbox.clone(),
            )));
        }
    }
}