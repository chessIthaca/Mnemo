// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kernel-neutral workflow handle trait (job 2b, step 2b.3).
//!
//! The kernel owns [`crate::agent::loop_impl::AgentLoop`], whose per-agent
//! workflow is an app type — the loop must never name it (the boundary
//! contract, master-spec :255-258). This trait carries exactly the surfaces
//! the kernel reads today, every one of them an opaque representation:
//! the state token, the completion flag, the plan scope, the tool-filter
//! envelope, and an `Any` escape for the app side to recover the lockable
//! concrete. The workflow itself (its state machine, plans, skills) lives
//! app-side; job 2c's free agent runs on NoPolicy and the kernel-default
//! [`UnwiredWorkflow`].
//!
//! The trait is implemented on `Arc<tokio::sync::Mutex<Workflow>>` — the
//! loop's own handle shape — so the `Arc` level carries the lock that was
//! part of every pre-2b.3 read (`self.workflow.lock().await.state()`, the
//! pin stamps, the label), and `as_any_arc` hands the app side back the
//! same lockable `Arc`.

use std::sync::Arc;

use crate::tool::ToolFilter;

/// Capitalize a state token's first letter (2b.3): the Parked event's label
/// and the dispatch denial text keep the pre-2b.3 `Debug` shape
/// ("Reviewing"), while the wire event field uses the serde shape
/// ("reviewing") — two pinned contracts, one token, this bridge.
pub fn capitalize_token(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => token.to_string(),
    }
}

/// The kernel's neutral view of an agent's workflow. Async because the
/// pre-2b.3 reads all sat behind the workflow's tokio mutex; the
/// implementations lock and read exactly as the callers did.
#[async_trait::async_trait]
pub trait AgentWorkflowHandle: Send + Sync {
    /// The canonical state name (the state token). The pre-2b.3 reads used
    /// the state's `Debug` shape (e.g. `"Executing"`); the app-side impl
    /// keeps that exact formatting so the pinned-state tokens, the Parked
    /// event label, the denial text's state name and the wire-field
    /// representation are byte-stable.
    async fn state(&self) -> String;

    /// Whether the plan is complete (the pre-2b.3
    /// `wf_state == WorkflowState::Complete` comparisons — the finish-capture
    /// guard and the work-intent nudge).
    async fn is_complete(&self) -> bool;

    /// The live plan's `(id, current step index)` — the shape
    /// `AgentLoop::read_plan_scope` returned (the budget gate + the spend
    /// attribution), `None` when no plan is active.
    async fn plan_scope(&self) -> Option<(String, Option<usize>)>;

    /// The workflow's own allowed-tools filter — the dispatch gate's
    /// None-fallback (the raw read when a policy supplies no enforcement
    /// surface), which pre-2b.3 read `wf.allowed_tools()`.
    async fn allowed_tools(&self) -> ToolFilter;

    /// The plan-frozen schema filter — the advertisement fallback when a
    /// policy supplies no surface (the 2a.3 consult's raw read).
    async fn schema_filter(&self) -> ToolFilter;

    /// Whether completing the given step index would exit the Executing
    /// phase — the workflow's own state-machine question (the dispatch
    /// gate asks it on complete_step calls).
    async fn completing_step_exits_executing(&self, step_index: usize) -> bool;

    /// The plan-gate event snapshot: (the state token, the top plan id,
    /// the plan title, the plan goal) — the workflow-sourced fields of the
    /// plan-mutation gate's event payload (the completing step number
    /// stays kernel-side; it is parsed from the tool's own arguments).
    async fn plan_gate_snapshot(
        &self,
    ) -> (String, Option<String>, Option<String>, Option<String>);

    /// The active skill's name (the skill-routing probe needs the NAME as a
    /// plain string — the kernel never sees the skill overlay type).
    async fn active_skill_name(&self) -> Option<String>;

    /// The escalation-lane context (backlog ad56c7bd): the live plan's
    /// `(id, current step index, current step text)` — `None` planless or
    /// stepless. The lane memo keys on the plan's IDENTITY (its id, never
    /// its title).
    async fn lane_step_snapshot(&self) -> Option<(String, usize, String)>;

    /// Stamp the sub-agent role invariants (the spawn path's post-build
    /// stamp): deny plan mutations + replace the derived lifecycle state
    /// with the dedicated Subagent role state. The plan stack stays loaded
    /// (the read-only mirror that feeds `current_plan` for reviewers and
    /// the UI staircase); only the state is replaced.
    async fn stamp_subagent_invariants(&self);

    /// Stamp the spawn-time role allow-list (the parent's permission
    /// intersection): the reviewer arm enforces it STRICTLY (only the
    /// listed tools + current_plan are visible — no auto-granted memory/
    /// ask/backlog tools), the worker arm as the plain tool allow-list.
    async fn set_role_allowlist(&self, is_reviewer: bool, names: Vec<String>);

    /// The next review round's dispatch facts: (the round number being
    /// dispatched, 1-based; the base revision the previous round verified —
    /// the delta's lower bound, `None` on a first review). The opaque view
    /// of the plan frame's round stamps that the reviewer-preamble path
    /// needs (the stamp data itself stays app-side).
    async fn next_review_round(&self) -> (u32, Option<String>);

    /// Stamp a review round's base — the best-effort post-spawn write; an
    /// unstamped round only WIDENS the next review (fail-safe).
    async fn record_review_round(&self, base: &str) -> Result<(), String>;

    /// Whether the workflow is in a state where the plan loop expects the
    /// agent to keep making progress — Executing (an in-progress plan with
    /// steps still to complete) or Reviewing (the closing sequence: review
    /// → fix → commit → finish), or Planning while unattended (a synthetic
    /// continue may steer toward the plan). The pre-2b.3 kernel keeper fn,
    /// moved onto the handle: its one live caller is the subagent
    /// auto-continue parking test (production consults the policy).
    async fn expects_progress(&self, unattended: bool) -> bool;

    /// The app-side escape: the same handle as an untyped `Arc`. The kernel
    /// never downcasts; app-side consumers (the IPC handler, the policy
    /// factory's own build) recover the concrete lockable workflow app→app,
    /// which is the legal dependency direction.
    fn as_any_arc(&self) -> Arc<dyn std::any::Any + Send + Sync>;
}

/// The kernel's fallback workflow for unwired (NoPolicy) factories: every
/// consult reads the neutral defaults (state `Complete`-less, no plan, no
/// plan scope, the unfiltered tool envelope — the free agent).
#[derive(Debug)]
pub struct UnwiredWorkflow;

#[async_trait::async_trait]
impl AgentWorkflowHandle for UnwiredWorkflow {
    async fn state(&self) -> String {
        "complete".to_string()
    }

    async fn is_complete(&self) -> bool {
        // The free agent has no live plan to complete; the finish-capture
        // guard only fires on real plan-completion signals it never sees.
        false
    }

    async fn plan_scope(&self) -> Option<(String, Option<usize>)> {
        None
    }

    async fn allowed_tools(&self) -> ToolFilter {
        ToolFilter::Complete
    }

    async fn schema_filter(&self) -> ToolFilter {
        ToolFilter::Complete
    }

    async fn completing_step_exits_executing(&self, _step_index: usize) -> bool {
        // The free agent has no live plan whose step completion could exit
        // a phase.
        false
    }

    async fn plan_gate_snapshot(
        &self,
    ) -> (String, Option<String>, Option<String>, Option<String>) {
        ("complete".to_string(), None, None, None)
    }

    async fn active_skill_name(&self) -> Option<String> {
        None
    }

    async fn lane_step_snapshot(&self) -> Option<(String, usize, String)> {
        None
    }

    async fn stamp_subagent_invariants(&self) {
        // The free agent has no plan-lifecycle state to stamp.
    }

    async fn set_role_allowlist(&self, _is_reviewer: bool, _names: Vec<String>) {
        // The free agent accepts every tool its policy surfaces.
    }

    async fn next_review_round(&self) -> (u32, Option<String>) {
        // The free agent has no plan frame to carry round stamps.
        (1, None)
    }

    async fn record_review_round(&self, _base: &str) -> Result<(), String> {
        Ok(())
    }

    async fn expects_progress(&self, _unattended: bool) -> bool {
        // The free agent has no plan lifecycle that expects progress.
        false
    }

    fn as_any_arc(&self) -> Arc<dyn std::any::Any + Send + Sync> {
        Arc::new(UnwiredWorkflow)
    }
}