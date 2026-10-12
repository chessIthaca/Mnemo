// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The workflow agent's policy: wraps today's `Workflow` so every kernel
//! coupling consults this wrapper (master-spec :200-202). Job 2b moves the
//! implementation under `mnemo::app`.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::policy::{
    AgentPolicy, Continuation, GateVerdict, KernelPromptBlocks, PolicyBuildCtx, PromptHook,
    RoleSpec, RouteKey, SpendTags, ToolSurface,
};
use crate::project::Constitution;
use crate::provider::Capabilities;
use crate::tool::{ToolRegistry, ToolResult};
use crate::workflow::Workflow;

/// The workflow agent's policy — byte-identical behavior to the pre-2a
/// direct reads (acceptance :785-788).
pub struct WorkflowPolicy {
    pub(crate) workflow: Arc<Mutex<Workflow>>,
}

impl WorkflowPolicy {
    /// Wrap the loop's workflow (factory build path, 2a.2).
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self { workflow }
    }
    /// The workflow handle (kernel helpers occasionally need the state map
    /// during migration; shrinks as sites flip).
    pub fn workflow(&self) -> Arc<Mutex<Workflow>> {
        Arc::clone(&self.workflow)
    }
}

#[async_trait]
impl AgentPolicy for WorkflowPolicy {
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String {
        crate::agent::policy::assemble_stable_head(k, c)
    }
    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {
        // D5: registration stays kernel-called in 2a (factory fns run as today).
    }
    async fn volatile_tail(&self, _caps: &Capabilities) -> String {
        // 2a.4 moves the workflow-section assembly here.
        String::new()
    }
    async fn tool_surface(&self) -> ToolSurface {
        // 2a.3: the real delegate — one lock, both filters, exactly today's
        // reads. advertise is the plan-frozen schema filter (byte-stable
        // while a plan is active — turn.rs :553-564's advertisement); gate
        // is the per-state enforcement filter (dispatch.rs :253).
        let wf = self.workflow.lock().await;
        ToolSurface {
            advertise: Some(wf.schema_filter()),
            gate: Some(wf.allowed_tools()),
        }
    }
    async fn gate_call(&self, _name: &str, _args: &Value) -> GateVerdict {
        // 2a: Allow — the surface filter above does all of today's
        // enforcement work; the plan-mutation gate and the research-write
        // verdict keep their tool-side homes in dispatch.rs until 2b/2c
        // (master-spec :215-216).
        GateVerdict::Allow
    }
    async fn route(&self) -> RouteKey {
        // 2a.6 delegates to today's slot-chain builder.
        RouteKey { slots: vec![] }
    }
    async fn on_prompt(&self, _text: &str) -> PromptHook {
        PromptHook::default()
    }
    async fn after_tool(&self, _name: &str, _result: &ToolResult) {}
    async fn on_turn_end(&self, _t: &crate::agent::policy::TurnSummary) -> Continuation {
        Continuation::Stop
    }
    async fn spend_tags(&self) -> SpendTags {
        SpendTags::default()
    }
    fn spawn_roles(&self) -> &[RoleSpec] {
        &[]
    }
    async fn on_session_end(&self) {}
}
