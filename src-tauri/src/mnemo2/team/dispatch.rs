// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Dispatch: the one entry point that turns a lead task into a worker
//! session - the board-step check, dispatch-time routing, provisioning
//! and idempotent reuse.

use std::sync::atomic::Ordering;

use mnemo::app::worktrees::agent_branch;
use mnemo::config::endpoints::TierLadder;
use mnemo::tool::ToolResult;

use crate::mnemo2::dispatch_routing::{
    threshold_for, DispatchRoutingDecision, DispatchRoutingRow, EnabledTiers, RoutingMode,
    RoutingSettings,
};
use crate::mnemo2::ledger;

use serde_json::{json, Value};

use super::lead_tools::{team_tool_error, worker_status_view};
use super::record::{validate_worker_id, WorkerRecord, WorkerRouting, WorkerStatus};
use super::routing::RoutedDispatch;
use super::TeamRuntime;

/// A plan and step a task is dispatched under (`step` is 0-based).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PlanRef {
    pub(super) plan_id: String,
    pub(super) step: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DispatchSource {
    LeadTool,
    Ipc,
}

/// One dispatch request's knobs, threaded through the dispatch path together.
#[derive(Clone, Debug)]
pub(super) struct DispatchRequest {
    /// The lead's task-specific instructions.
    pub(super) brief: String,
    /// The requested tier, including `auto` where the mode offers it.
    pub(super) tier: Option<String>,
    /// Whether this implementation dispatch uses dispatch-time routing.
    pub(super) dispatch_routing: bool,
    /// Which public entry point supplied the dispatch.
    pub(super) source: DispatchSource,
    /// The exact model spec, when the lead chose one.
    pub(super) model: Option<String>,
    /// The chosen reasoning effort, when the lead chose one.
    pub(super) reasoning_effort: Option<String>,
    /// The stable worker id, when the lead named one.
    pub(super) worker_id: Option<String>,
    /// The plan step the task belongs to, when it is on the board.
    pub(super) plan: Option<PlanRef>,
    /// The lead-assigned review risk.
    pub(super) review_risk: Option<String>,
    /// Whether the first turn reads the spec back and asks questions.
    pub(super) read_back: bool,
}

/// The training-corpus `Route` row for one dispatch classification: the
/// single construction shared by the inline path and the deferred
/// explicit-choice classification, so both write identical rows.
#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_route_row(
    ts: u64,
    worker_id: &str,
    plan_id: &str,
    step: usize,
    mode: RoutingMode,
    decision: Option<&DispatchRoutingDecision>,
    skip: Option<&str>,
    laya_tier: Option<&str>,
    cleared: bool,
    lead_tier: Option<&str>,
    ran_tier: Option<&str>,
    ran_model: Option<&str>,
    text: &str,
    enabled: EnabledTiers,
    settings: &RoutingSettings,
) -> DispatchRoutingRow {
    use sha2::Digest as _;
    let text_sha256 = sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    DispatchRoutingRow::Route {
        ts,
        worker_id: worker_id.to_string(),
        plan_id: plan_id.to_string(),
        step,
        mode,
        text_sha256,
        label: decision
            .map(|decision| decision.label.clone())
            .unwrap_or_default(),
        confidence: decision.map_or(0.0, |decision| decision.confidence),
        probabilities: decision
            .map(|decision| decision.probabilities.clone())
            .unwrap_or_default(),
        tier: laya_tier.map(str::to_string),
        threshold: decision.map_or_else(
            || threshold_for(enabled.offered().count(), settings),
            |decision| {
                if decision.source == "knn" {
                    settings.knn_min_share
                } else {
                    threshold_for(enabled.offered().count(), settings)
                }
            },
        ),
        cleared,
        lead_tier: lead_tier.map(str::to_string),
        ran_tier: ran_tier.map(str::to_string),
        ran_model: ran_model.map(str::to_string),
        text: text.to_string(),
        skip: skip.map(str::to_string),
        source: decision.map(|decision| decision.source.to_string()),
        knn_label: decision.and_then(|decision| decision.knn_label.clone()),
        knn_share: decision.and_then(|decision| decision.knn_share),
        knn_k: decision.and_then(|decision| decision.knn_k),
    }
}

/// The `LeadOverride` corpus row an explicit choice logs when it overrode a
/// tier Laya cleared: shared by the inline path and the deferred
/// classification so both write identical rows. `lead_tier` is the tier
/// the choice resolved to (`custom` when it has none) and
/// `requested_tier` the tier the lead asked for when that differs.
#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_lead_override_row(
    ts: u64,
    worker_id: &str,
    plan_id: &str,
    step: usize,
    laya_tier: &str,
    lead_tier: Option<&str>,
    requested_tier: Option<&str>,
    confidence: f64,
) -> DispatchRoutingRow {
    DispatchRoutingRow::LeadOverride {
        ts,
        worker_id: worker_id.to_string(),
        plan_id: plan_id.to_string(),
        step,
        laya_tier: Some(laya_tier.to_string()),
        lead_tier: lead_tier.unwrap_or("custom").to_string(),
        requested_tier: requested_tier.map(str::to_string),
        confidence,
    }
}

/// A dispatch answer: the compact worker entry next to the idempotent-reuse
/// flag. `data` keeps the whole record, as the single-worker answers do.
pub(super) fn dispatch_result(
    result: Result<(WorkerRecord, bool, bool), String>,
    action: &str,
    mode: RoutingMode,
    fold_request: Option<String>,
    explicit_model: bool,
) -> ToolResult {
    match result {
        Ok((worker, reused, laya_pending)) => {
            let routing = worker.routing.as_ref();
            let requested = if explicit_model {
                None
            } else {
                match fold_request.as_deref() {
                    Some("auto") | None => routing.and_then(|routing| {
                        if routing.routed_by == "laya" {
                            routing.laya_tier.as_deref()
                        } else {
                            routing.lead_tier.as_deref()
                        }
                    }),
                    Some(tier) => Some(tier),
                }
            };
            let ran_tier = routing
                .and_then(|routing| routing.ran_tier.as_deref())
                .or(worker.tier.as_deref());
            let routing_note = match (requested, ran_tier) {
                (Some(requested), Some(ran))
                    if requested != ran && TierLadder::default().get(requested).is_some() =>
                {
                    Some(format!("{requested} has no model; ran on {ran}"))
                }
                _ => None,
            };
            let output = serde_json::to_string(&json!({
                "action": action,
                "result": {
                    "worker": worker_status_view(&worker, false),
                    "reused": reused,
                    "routing_note": routing_note,
                },
            }))
            .unwrap_or_else(|_| action.to_string());
            let mut lines = Vec::new();
            if let Some(line) = dispatch_routing_line(mode, routing, laya_pending) {
                lines.push(line);
            }
            if let Some(note) = &routing_note {
                lines.push(note.clone());
            }
            let output = if lines.is_empty() {
                output
            } else {
                format!("{output}\n{}", lines.join("\n"))
            };
            let data = serde_json::to_value(&worker).unwrap_or(Value::Null);
            ToolResult::success(output).with_data(
                json!({"worker": data, "reused": reused, "routing_note": routing_note}),
            )
        }
        Err(err) => team_tool_error(err),
    }
}

pub(super) fn dispatch_routing_line(
    mode: RoutingMode,
    routing: Option<&WorkerRouting>,
    laya_pending: bool,
) -> Option<String> {
    let routing = routing?;
    if mode == RoutingMode::Off {
        return None;
    }
    // An explicit-choice dispatch returns before its classification lands
    // (plan c74b1987): say so instead of misreporting the still-null label
    // as an offline classifier.
    if laya_pending {
        return Some(
            "Laya: classifying in the background; the label will land on the routing record."
                .to_string(),
        );
    }
    // The dispatch-time ran tier (audit finding M1): a later manual reroute
    // may have moved the worker to another tier, but what the dispatch ran
    // is what this line reports.
    let ran_tier = routing.ran_tier.as_deref();
    let confidence = routing.confidence;

    if routing.routed_by == "lead_override" {
        let choice = match (ran_tier, routing.lead_tier.as_deref()) {
            (Some(ran), Some(requested)) if ran != requested => {
                format!("you ran {ran} (requested {requested})")
            }
            _ => format!(
                "you chose {}",
                routing
                    .lead_tier
                    .as_deref()
                    .or(ran_tier)
                    .unwrap_or("a custom model")
            ),
        };
        return Some(format!(
            "Laya: {} ({:.2}) — {choice}; logged as an override.",
            routing.laya_tier.as_deref().unwrap_or("unknown"),
            confidence.unwrap_or(0.0),
        ));
    }
    if routing.routed_by == "laya" {
        return Some(format!(
            "Laya: routed to {} ({:.2}).",
            routing.laya_tier.as_deref().unwrap_or("unknown"),
            confidence.unwrap_or(0.0)
        ));
    }
    if mode == RoutingMode::Shadow {
        return Some(match (routing.laya_tier.as_deref(), confidence, routing.cleared) {
            (Some(tier), Some(confidence), true) => {
                format!("Laya would pick {tier} ({confidence:.2}).")
            }
            (_, Some(confidence), _) => format!("Laya: unsure ({confidence:.2})."),
            _ => "Laya: offline.".to_string(),
        });
    }
    if !routing.cleared {
        return Some(match confidence {
            Some(confidence) => format!("Laya: unsure ({confidence:.2})."),
            None => "Laya: offline.".to_string(),
        });
    }
    Some(format!(
        "Laya: {} ({:.2}) — matches your tier.",
        routing.laya_tier.as_deref().unwrap_or("unknown"),
        confidence.unwrap_or(0.0)
    ))
}

/// Scan a plan document for its `## Acceptance scenarios` section.
///
/// Returns whether the section exists and how many observable bullets it
/// carries. The section runs from its level-2 heading to the next heading of
/// level 2 or above; a bullet is observable when its list-item text carries at
/// least eight characters of substance, so placeholders like `- x` do not
/// count.
fn acceptance_scenarios(doc: &str) -> (bool, usize) {
    let mut in_section = false;
    let mut found = false;
    let mut bullets = 0;
    for line in doc.lines() {
        let trimmed = line.trim();
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if level > 0 && trimmed[level..].starts_with(' ') {
            if level <= 2 {
                in_section = level == 2
                    && trimmed[level..]
                        .trim()
                        .eq_ignore_ascii_case("acceptance scenarios");
                found = found || in_section;
            }
            continue;
        }
        if !in_section {
            continue;
        }
        let text = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .map(str::trim)
            .unwrap_or("");
        if text.len() >= 8 {
            bullets += 1;
        }
    }
    (found, bullets)
}

/// Require observable acceptance scenarios before a high-risk dispatch.
/// Either the lead's `## Acceptance scenarios` bullets or a complete OpenSpec
/// `Scenario:` block under `## Spec changes` satisfies the requirement.
pub(super) fn require_acceptance_scenarios(plan_id: &str, doc: &str) -> Result<(), String> {
    let guidance = "add observable scenarios with plan_update add_acceptance_scenarios or spec_deltas, or dispatch with review_risk='standard'";
    let (found, bullets) = acceptance_scenarios(doc);
    if found && bullets > 0 {
        return Ok(());
    }

    let mut in_spec_changes = false;
    let mut in_scenario = false;
    let mut has_when = false;
    let mut has_then = false;
    for line in doc.lines() {
        let trimmed = line.trim();
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if level > 0 && trimmed[level..].starts_with(' ') {
            if in_scenario && has_when && has_then {
                return Ok(());
            }
            in_scenario = false;
            has_when = false;
            has_then = false;
            let heading = trimmed[level..].trim();
            if level <= 2 {
                in_spec_changes = level == 2 && heading.eq_ignore_ascii_case("spec changes");
            } else if in_spec_changes && heading.starts_with("Scenario:") {
                in_scenario = true;
            }
            continue;
        }
        if !in_spec_changes || !in_scenario {
            continue;
        }
        let Some(text) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        if let Some(value) = text.trim().strip_prefix("**WHEN**") {
            has_when = has_when || !value.trim().is_empty();
        } else if let Some(value) = text.trim().strip_prefix("**THEN**") {
            has_then = has_then || !value.trim().is_empty();
        }
    }
    if in_scenario && has_when && has_then {
        return Ok(());
    }

    if !found {
        return Err(format!(
            "review_risk='high' requires acceptance scenarios, but plan '{plan_id}' does not declare any; {guidance}"
        ));
    }
    Err(format!(
        "review_risk='high' requires acceptance scenarios, but plan '{plan_id}' has no observable bullet (a concrete, verifiable outcome of at least eight characters) or complete OpenSpec scenario; {guidance}"
    ))
}

impl TeamRuntime {
    /// The one production dispatch path: a task on a plan step (1-based),
    /// checked against the board before anything is provisioned. The lead's
    /// `team_dispatch` tool and the Control Room form both land here.
    // One parameter per dispatch knob — the tool and the Control Room form pass them explicitly.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch_on_plan(
        &self,
        brief: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
        review_risk: Option<String>,
        read_back: bool,
        plan_id: &str,
        step: usize,
    ) -> Result<(WorkerRecord, bool), String> {
        let plan_id = plan_id.trim();
        if plan_id.is_empty() || step < 1 {
            return Err(
                "team_dispatch requires 'plan_id' (a board plan id) and 'step' (a 1-based integer). Create the plan first with plan_create."
                    .to_string(),
            );
        }
        let plan = PlanRef {
            plan_id: plan_id.to_string(),
            step: step - 1,
        };
        self.dispatch_request(DispatchRequest {
            brief,
            tier: None,
            dispatch_routing: true,
            source: DispatchSource::Ipc,
            model,
            reasoning_effort,
            worker_id,
            plan: Some(plan),
            review_risk,
            read_back,
        })
        .await
        .map(|(worker, reused, _)| (worker, reused))
    }

    /// Plan-free dispatch, kept for tests that exercise worker lifecycle only.
    #[cfg(test)]
    pub async fn dispatch(
        &self,
        brief: String,
        model: Option<String>,
    ) -> Result<WorkerRecord, String> {
        self.dispatch_with_id(brief, model, None, None).await
    }

    #[cfg(test)]
    pub async fn dispatch_with_id(
        &self,
        brief: String,
        model: Option<String>,
        worker_id: Option<String>,
        review_risk: Option<String>,
    ) -> Result<WorkerRecord, String> {
        self.dispatch_request(DispatchRequest {
            brief,
            tier: None,
            dispatch_routing: false,
            source: DispatchSource::Ipc,
            model,
            reasoning_effort: None,
            worker_id,
            plan: None,
            review_risk,
            read_back: false,
        })
        .await
        .map(|(worker, _, _)| worker)
    }

    #[cfg(test)]
    pub async fn dispatch_task(
        &self,
        brief: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
    ) -> Result<WorkerRecord, String> {
        self.dispatch_request(DispatchRequest {
            brief,
            tier: None,
            dispatch_routing: false,
            source: DispatchSource::Ipc,
            model,
            reasoning_effort,
            worker_id,
            plan: None,
            review_risk: None,
            read_back: false,
        })
        .await
        .map(|(worker, _, _)| worker)
    }

    /// The one dispatch entry point. The returned triple is the worker, the
    /// idempotent-reuse flag, and whether a deferred classification is
    /// running in the background (plan c74b1987) — the dispatch answer
    /// reports that instead of a label that has not landed yet.
    pub(super) async fn dispatch_request(
        &self,
        request: DispatchRequest,
    ) -> Result<(WorkerRecord, bool, bool), String> {
        let routed = self.prepare_dispatch_routing(&request).await?;
        let result = self.dispatch_locked(request, routed).await;
        if matches!(result, Ok((_, false, _))) {
            self.signal_board_changed();
        }
        result
    }

    async fn prepare_dispatch_routing(
        &self,
        request: &DispatchRequest,
    ) -> Result<Option<RoutedDispatch>, String> {
        if !request.dispatch_routing {
            return Ok(None);
        }
        if let Some(worker_id) = request.worker_id.as_deref() {
            let records = self.records.lock().await;
            if records.iter().any(|worker| worker.id == worker_id) {
                return Ok(None);
            }
        }
        let plan = request
            .plan
            .as_ref()
            .ok_or_else(|| "dispatch routing requires a board plan step".to_string())?;
        let board = self
            .board()
            .ok_or_else(|| "the plan board is not available".to_string())?;
        let step_text = board
            .get(&plan.plan_id)?
            .steps
            .get(plan.step)
            .ok_or_else(|| format!("plan {} has no step {}", plan.plan_id, plan.step + 1))?
            .text
            .clone();
        self.route_implementation_dispatch(request, &step_text, &request.brief)
            .await
            .map(Some)
    }

    async fn dispatch_locked(
        &self,
        request: DispatchRequest,
        routed: Option<RoutedDispatch>,
    ) -> Result<(WorkerRecord, bool, bool), String> {
        let DispatchRequest {
            brief,
            tier,
            dispatch_routing,
            source: _,
            model,
            reasoning_effort,
            worker_id,
            plan,
            review_risk,
            read_back,
        } = request;
        let _guard = self.merge_lock.lock().await;
        if brief.trim().is_empty() {
            return Err("the worker brief is empty".to_string());
        }
        // High-risk work needs observable acceptance scenarios on the
        // dispatched board plan, which the lead verifies at closing
        // integration; standard dispatch needs none.
        let review_risk = match review_risk.as_deref() {
            None | Some("standard") => "standard".to_string(),
            Some("high") => {
                let plan = plan.as_ref().ok_or_else(|| {
                    "review_risk='high' requires a board plan step: dispatch with plan_id and step on a plan that declares acceptance scenarios".to_string()
                })?;
                let doc = self.plan_document(&plan.plan_id)?;
                require_acceptance_scenarios(&plan.plan_id, &doc)?;
                "high".to_string()
            }
            Some(other) => {
                return Err(format!(
                    "review_risk must be 'standard' or 'high', not '{other}'"
                ));
            }
        };
        if let Some(id) = worker_id.as_deref() {
            validate_worker_id(id)?;
            let existing = {
                let records = self.records.lock().await;
                records.iter().find(|worker| worker.id == id).cloned()
            };
            if let Some(existing) = existing {
                let retry_model = if dispatch_routing {
                    match tier.as_deref() {
                        Some("auto") | None if model.is_none() => existing.model.clone(),
                        _ => self.lead_model_choice(
                            tier.as_deref().filter(|tier| *tier != "auto"),
                            model.clone(),
                        )?,
                    }
                } else {
                    model.clone()
                };
                let retry_effort = if dispatch_routing && reasoning_effort.is_none() {
                    existing.reasoning_effort.clone()
                } else {
                    reasoning_effort.clone()
                };
                if existing.brief != brief
                    || existing.model != retry_model
                    || existing.reasoning_effort != retry_effort
                    || existing.read_back != read_back
                    || plan.as_ref().is_some_and(|plan| {
                        existing.plan_id.as_deref() != Some(plan.plan_id.as_str())
                            || existing.plan_step != Some(plan.step)
                    })
                {
                    return Err(format!(
                        "worker '{id}' already exists with a different brief, model, reasoning effort, read_back, or plan step; use team_status instead of guessing a new ID (to move it to another model, use team_resume with `tier` or `model`)"
                    ));
                }
                if existing.branch != agent_branch(id) {
                    return Err(format!("worker '{id}' has an invalid branch identity"));
                }
                self.validate_model(&existing.model, &existing.reasoning_effort)?;
                if !existing.brief_sent
                    && !self.live.lock().await.contains_key(id)
                    && matches!(
                        existing.status,
                        WorkerStatus::Dispatching
                            | WorkerStatus::Failed
                            | WorkerStatus::Interrupted
                    )
                {
                    if !existing.worktree.is_dir() {
                        return Err(format!("worker '{id}' has no recoverable worktree"));
                    }
                    return self
                        .start_worker(existing)
                        .await
                        .map(|worker| (worker, true, false));
                }
                return Ok((existing, true, false));
            }
        }
        // The plan board refuses before any record, branch, or worktree exists.
        let opening = match &plan {
            Some(plan) => {
                let board = self
                    .board()
                    .ok_or_else(|| "the plan board is not available".to_string())?;
                let header = board.brief_for(&plan.plan_id, plan.step + 1)?;
                // Steps of this plan with an open task, other than this one:
                // parallel work, which needs contracts for shared files.
                let in_flight = self.in_flight_steps(&plan.plan_id, plan.step).await;
                board.contract_gate(&plan.plan_id, plan.step, &in_flight)?;
                let blockers = board.dispatch_blockers(&plan.plan_id)?;
                if !blockers.is_empty() {
                    return Err(format!(
                        "plan {} cannot be dispatched until its OpenSpec artifacts are complete:\n\n{}",
                        plan.plan_id,
                        blockers.join("\n\n")
                    ));
                }
                Some(format!("{}\n---\n\n{brief}", header.trim_end()))
            }
            None => None,
        };
        let (model, reasoning_effort, tier, routing, routing_rows, routing_error, routing_pending) =
            if dispatch_routing {
                let routed = routed
                    .ok_or_else(|| "dispatch routing was not prepared".to_string())?;
                (
                    routed.model,
                    routed.reasoning_effort,
                    routed.tier,
                    routed.routing,
                    routed.rows,
                    routed.error,
                    routed.pending,
                )
            } else {
                (model, reasoning_effort, tier, None, Vec::new(), None, None)
            };
        if let Some(error) = routing_error {
            for row in &routing_rows {
                self.append_dispatch_routing_row(row);
            }
            return Err(error);
        }
        self.validate_model(&model, &reasoning_effort)?;
        if let Some(id) = worker_id.as_deref() {
            let branch = agent_branch(id);
            if self
                .records
                .lock()
                .await
                .iter()
                .any(|worker| worker.branch == branch)
            {
                return Err(format!(
                    "worker ID prefix collides with an existing {branch}"
                ));
            }
        }
        let active = self.live.lock().await.len();
        let slots = self.persistence.slots.load(Ordering::SeqCst);
        if active >= slots {
            return Err(format!("all {slots} worker slots are busy"));
        }
        let placed = plan.as_ref().map(|plan| (plan.plan_id.clone(), plan.step));
        let provisioned = self
            .provision_worker(
                brief,
                opening,
                model,
                reasoning_effort,
                worker_id,
                review_risk,
                tier,
                plan,
            )
            .await;
        // Provisioning leaves the worker stopped; the read-back flag lands on
        // the stored record before its session starts, so the opening kickoff
        // matches the record the session was started from.
        let mut pending_spawned = false;
        let result = match provisioned {
            Ok(record) => {
                for mut row in routing_rows {
                    row.with_worker_id(&record.id);
                    self.append_dispatch_routing_row(&row);
                }
                let routing_for_record = routing.clone();
                self.update_worker(&record.id, |worker| {
                    worker.read_back = read_back;
                    worker.routing = routing_for_record;
                })
                .await;
                // The classification an explicit-choice dispatch deferred:
                // spawned now that the worker id exists, never blocking the
                // dispatch that has already returned without it.
                if let Some(pending) = routing_pending {
                    self.spawn_pending_classification(pending, record.id.clone());
                    pending_spawned = true;
                }
                self.start_worker(WorkerRecord {
                    read_back,
                    routing,
                    ..record
                })
                .await
            }
            Err(err) => Err(err),
        };
        if result.is_ok() {
            if let Some((plan_id, step)) = &placed {
                // Parallel work: the seams between the steps need a final
                // integration check before the plan can close. Best-effort;
                // the dispatch already succeeded.
                let in_flight = self.in_flight_steps(plan_id, *step).await;
                if !in_flight.is_empty() {
                    if let Some(board) = self.board() {
                        let mut parallel = in_flight;
                        parallel.push(*step);
                        if let Err(e) = board.ensure_integration_step(plan_id, &parallel) {
                            eprintln!("team: integration step for {plan_id} not added: {e}");
                        }
                    }
                }
            }
        }
        result.map(|worker| (worker, false, pending_spawned))
    }

    /// The other steps (0-based) of `plan_id` with an open task: a worker not
    /// merged, cancelled, or stopped (the ledger's notion of closed).
    async fn in_flight_steps(&self, plan_id: &str, except_step: usize) -> Vec<usize> {
        let records = self.records.lock().await;
        let mut steps: Vec<usize> = records
            .iter()
            .filter(|w| w.plan_id.as_deref() == Some(plan_id) && !ledger::is_closed(w))
            .filter_map(|w| w.plan_step)
            .filter(|step| *step != except_step)
            .collect();
        steps.sort_unstable();
        steps.dedup();
        steps
    }
}
