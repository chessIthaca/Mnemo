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
}

impl WorkflowPolicyFactory {
    pub fn new() -> Self {
        Self {
            backlog: std::sync::RwLock::new(None),
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
        Self::new()
    }
}

impl AgentPolicyFactory for WorkflowPolicyFactory {
    fn build_policy(&self, ctx: &PolicyFactoryCtx) -> PolicyBuildResult {
        // The kernel hands the loop's workflow opaquely (2b.2's migration
        // seam — the kernel still owns the typed field until 2b.3); cast
        // app-side. A mismatch means the kernel changed what it passes —
        // fail loudly rather than silently.
        let workflow = ctx
            .workflow
            .downcast_ref::<Arc<Mutex<Workflow>>>()
            .expect("the kernel hands the policy factory an Arc<Mutex<Workflow>>")
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

    fn register_tools(&self, registry: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {
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
    }
}