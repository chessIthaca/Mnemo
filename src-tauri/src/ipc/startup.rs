// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Startup snapshot — a single IPC command returning everything the frontend
//! needs on mount (agents, context caps, ALL workflow states, backlog,
//! embedder + classifier status), replacing 5 sequential `invoke` round-trips.
//!
//! Closes the stale-non-active-workflowStates gap: the old startup fetched
//! only the ACTIVE agent's workflow state, so non-active tabs showed a stale
//! state until the user clicked them. The snapshot returns EVERY agent's
//! state.

use serde::Serialize;
use tauri::State;

use mnemo::memory::classifier::ClassifierStatus;
use mnemo::memory::embedder::EmbedderStatus;
use mnemo::runtime::AgentId;
use mnemo::workflow::WorkflowState;

use crate::ipc::agent::AgentInfo;
use crate::ipc::backlog_cmds::{resolve_item_view, BacklogItemView};
use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// The startup snapshot: everything the frontend needs on mount in one call.
#[derive(Debug, Clone, Serialize)]
pub struct StartupSnapshot {
    /// All agents (same as `list_agents`).
    pub agents: Vec<AgentInfo>,
    /// Per-agent context-window max (same as `context_caps`).
    pub context_caps: Vec<(AgentId, u32)>,
    /// EVERY agent's workflow state (closes the stale-non-active gap — the
    /// old startup fetched only the active agent's state).
    pub workflow_states: Vec<(AgentId, WorkflowState)>,
    /// The backlog (same as `backlog_list` — the frontend-facing views, with
    /// image paths resolved to data URLs and the run-all checkpoint sha
    /// parsed from each note).
    pub backlog: Vec<BacklogItemView>,
    /// Embedder status (same as `get_embedder_status`).
    pub embedder_status: EmbedderStatus,
    /// Laya classifier status (same as `get_classifier_status`): `disabled`
    /// (the default — Laya is opt-in) / `ready` / `failed`.
    pub classifier_status: ClassifierStatus,
    /// The same-project instance conflict resolved at startup: `Some` when
    /// ANOTHER live mnemo instance already holds this project (the frontend
    /// asks before opening it), `None` otherwise. The incumbent's marker is
    /// read before this instance's marker overwrites it (main.rs), so this
    /// reports the state that existed the moment this instance launched.
    pub instance_conflict: Option<InstanceConflict>,
}

/// The same-project instance conflict (a second instance opening a project
/// another LIVE instance already holds) — surfaced to the frontend as a
/// startup warning before it opens the project (2027-01-13: multiple
/// instances are now supported via per-instance WebView2 profiles, so the
/// shared project deserves an explicit ask).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct InstanceConflict {
    /// The pid of the incumbent instance (the one already on this project).
    pub pid: u32,
    /// Unix epoch seconds when the incumbent instance launched.
    pub started_at: u64,
}

/// Fetch the startup snapshot: agents, context caps, ALL workflow states,
/// backlog, and embedder + classifier status in one call.
///
/// Replaces the 5 sequential `invoke` round-trips the frontend used to make
/// on mount (`list_agents` + `context_caps` + `get_workflow_state(active)` +
/// `backlog_list` + `get_embedder_status`). The snapshot returns EVERY agent's
/// workflow state, closing the stale-non-active-workflowStates gap.
#[tauri::command]
pub async fn startup_snapshot(state: State<'_, IpcState>) -> Result<StartupSnapshot, IpcError> {
    // Snapshot the handle list under the manager lock, then DROP it before
    // acquiring agent_loops — mirrors `list_agents` (agent.rs) and respects
    // the documented lock-ordering invariant (manager first, then agent_loops;
    // never agent_loops then manager — state.rs:38-43).
    let handles: Vec<(AgentId, String, bool, Option<AgentId>)> = {
        let mgr = state.runtime.manager.lock().await;
        mgr.list()
            .iter()
            .map(|h| (h.id, h.name.clone(), h.is_running(), h.parent_id))
            .collect()
    };

    // Read models + context caps + workflow states under the agent_loops lock
    // (one acquisition). The per-agent workflow.lock().await nesting inside
    // agent_loops is fine — it matches get_workflow_state (agent.rs).
    let agent_loops = state.runtime.agent_loops.lock().await;

    // Agents (id, name, running, parent_id, model) — mirrors list_agents.
    let agents: Vec<AgentInfo> = handles
        .iter()
        .map(|(id, name, running, parent_id)| {
            let (model, provider, reasoning_effort) = agent_loops
                .get(id)
                .map(|l| {
                    (
                        Some(
                            l.resolved_model()
                                .unwrap_or_else(|| l.provider().model().to_string()),
                        ),
                        l.effective_provider_name(),
                        // The last turn's effort, else the factory-stamped
                        // default (no turn has run yet — backlog 51dab4da).
                        l.resolved_effort().or_else(|| l.default_display_effort()),
                    )
                })
                .unwrap_or((None, None, None));
            AgentInfo {
                id: *id,
                name: name.clone(),
                running: *running,
                parent_id: *parent_id,
                model,
                provider,
                reasoning_effort,
            }
        })
        .collect();

    // Context caps + workflow states for EVERY agent.
    let context_caps: Vec<(AgentId, u32)> = agent_loops
        .iter()
        .map(|(id, l)| (*id, l.context_manager().max_tokens() as u32))
        .collect();
    let mut workflow_states: Vec<(AgentId, WorkflowState)> = Vec::new();
    for (id, l) in agent_loops.iter() {
        let wf = l.workflow_handle();
        let wf = wf.lock().await;
        workflow_states.push((*id, wf.state()));
    }
    drop(agent_loops);

    // Backlog — the frontend-facing views (same as `backlog_list`): image
    // paths resolved to data URLs + the parsed checkpoint sha.
    let backlog: Vec<BacklogItemView> = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .map(|i| resolve_item_view(&store, i))
            .collect()
    };

    // Embedder status.
    let embedder_status = state.embedder_status()?;

    // Laya classifier status (opt-in; `disabled` unless enabled with an
    // endpoint).
    let classifier_status = state.classifier_status()?;

    // The same-project conflict was resolved at startup, before this
    // instance's marker overwrote the incumbent's (main.rs) — the snapshot
    // reports the state that existed the moment this instance launched.
    let instance_conflict = state.runtime.instance_conflict.clone();

    Ok(StartupSnapshot {
        agents,
        context_caps,
        workflow_states,
        backlog,
        embedder_status,
        classifier_status,
        instance_conflict,
    })
}

#[cfg(test)]
mod tests {
    use crate::ipc::contract_fixtures::normalize_lf;
    /// The snapshot's backlog must be the frontend-facing views (same as
    /// `backlog_list`) — image paths resolved + `checkpoint_sha` parsed —
    /// not the raw stored items (review LOW 1, plan 2929d340: the fourth
    /// backlog read path had been left on the raw `BacklogItem` shape).
    /// The command needs Tauri state, so the wiring is pinned as a source
    /// contract, following the backlog_cmds.rs pattern.
    #[test]
    fn startup_snapshot_backlog_routes_through_resolve_item_view() {
        let src = normalize_lf(include_str!("startup.rs"));
        let start = src
            .find("async fn startup_snapshot(")
            .expect("startup_snapshot command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("resolve_item_view("),
            "startup_snapshot's backlog must route through resolve_item_view \
             so checkpoint_sha + resolved images reach the startup payload"
        );
    }
}
