// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Dispatch routing: the tier ladder over the model inventory, the
//! lead's model/tier choice, the classifier call at dispatch, and the
//! routing-log rows a dispatch or a reroute appends.

use std::sync::Arc;

use mnemo::config::endpoints::{Tier, TierLadder};
use mnemo::RouteKnn;

use crate::mnemo2::dispatch_routing::{
    classify_dispatch, load_routing_settings, outcome_label, routing_text,
    DispatchRoutingDecision, DispatchRoutingRow, EnabledTiers, RoutingMode, RoutingSettings,
};
use crate::mnemo2::runtime::LeadModel;

use super::nudge::append_reroute_metadata;
use super::record::{ChangeSource, WorkerRecord, WorkerRouting};
use super::dispatch::{
    dispatch_lead_override_row, dispatch_route_row, DispatchRequest, DispatchSource,
};
use super::{save_workers, status_name, unix_ms, TeamRuntime};

pub(super) struct RoutedDispatch {
    pub(super) model: Option<String>,
    pub(super) tier: Option<String>,
    pub(super) reasoning_effort: Option<String>,
    pub(super) routing: Option<WorkerRouting>,
    pub(super) rows: Vec<DispatchRoutingRow>,
    /// The classification an explicit-choice dispatch defers to the
    /// background (plan c74b1987): `Some` only when the lead's tier or
    /// model choice made the inline await pure overhead. The dispatch
    /// returns with a null Laya label; the spawned task writes the corpus
    /// rows and attaches the label to the worker's routing record.
    pub(super) pending: Option<PendingClassification>,
    pub(super) error: Option<String>,
}

/// Everything the background classification of an explicit-choice dispatch
/// needs: the same classifier, permit, timeout and corpus rows as the
/// inline path, plus the dispatch-time choices those rows record. The
/// worker id is attached when the task is spawned, after provisioning.
pub(super) struct PendingClassification {
    /// The classifier resolved at dispatch time, exactly like the inline
    /// path's.
    classifier: std::sync::Arc<dyn mnemo::memory::classifier::Classifier>,
    /// The kNN overlay over the routing log, cloned at dispatch time.
    knn: Option<Arc<RouteKnn>>,
    /// The serialized routing permit, shared with the inline path and the
    /// drift checks.
    permit: Arc<tokio::sync::Semaphore>,
    /// The classified step text plus brief.
    text: String,
    /// The tiers that resolve to a model.
    enabled: EnabledTiers,
    /// The dispatch-routing settings snapshot at dispatch time.
    settings: RoutingSettings,
    /// The plan the dispatch ran under, for the corpus rows.
    plan_id: String,
    /// The 1-based step for the corpus rows.
    step: usize,
    /// The lead's requested tier, when one was passed.
    explicit_tier: Option<String>,
    /// The tier the lead's choice resolved to.
    chosen_tier: Option<String>,
    /// The model spec the dispatch runs on.
    chosen_model: Option<String>,
    /// Whether a model choice was passed: the override comparison reads the
    /// resolved tier then, the requested tier otherwise.
    model_choice: bool,
}

/// Whether the Laya-derived routing rows (`nudge`, `nudge_ignored`) may be
/// written for this worker: it was dispatched with routing (its record
/// carries a decision) or routing is on now. With routing off and an
/// unrouted worker nothing is appended, so off mode keeps writing exactly
/// what it wrote before routing existed. Cleanup, which has no settings
/// handle, passes [`RoutingMode::Off`] and goes by the record alone. Label
/// rows — a hand's reroute and a rerouted worker's terminal outcome — go
/// through [`logs_label`] instead.
pub(super) fn logs_routing(record: &WorkerRecord, mode: RoutingMode) -> bool {
    record.routing.is_some() || mode != RoutingMode::Off
}

/// Whether a *label* row is logged for this worker: every user or lead
/// model change is a learning (decided 2026-11-04), so a reroute is appended
/// in every routing mode, and a worker that was rerouted at least once
/// keeps logging its terminal `outcome` row in every mode too — outcome
/// labels are what calibration learns from.
pub(super) fn logs_label(record: &WorkerRecord, mode: RoutingMode) -> bool {
    logs_routing(record, mode) || !record.reroutes.is_empty()
}

/// The tier a worker finished on: the destination of its last reroute, else
/// its recorded tier, `custom` when it never had one.
pub(super) fn final_tier(record: &WorkerRecord) -> String {
    record
        .reroutes
        .iter()
        .rev()
        .find_map(|row| match row {
            DispatchRoutingRow::LeadReroute { to_tier, .. }
            | DispatchRoutingRow::UserReroute { to_tier, .. } => Some(to_tier.clone()),
            _ => None,
        })
        .or_else(|| record.tier.clone())
        .unwrap_or_else(|| "custom".to_string())
}

/// The tier a reroute row moves the worker to — the label the record's
/// `tier` field tracks after a switch, read from the same row [`final_tier`]
/// prefers so they (and team_status) agree. `None` when the destination is a
/// custom model.
pub(super) fn reroute_destination(row: &DispatchRoutingRow) -> Option<String> {
    let to_tier = match row {
        DispatchRoutingRow::LeadReroute { to_tier, .. }
        | DispatchRoutingRow::UserReroute { to_tier, .. } => to_tier,
        _ => return None,
    };
    (to_tier != "custom").then(|| to_tier.clone())
}

/// The `outcome` row for a worker that reached a terminal status (design
/// §4.7), with the label derived from its own routing history
/// ([`outcome_label`], design §4.8). An audit worker is a reviewer, never a
/// routed outcome: `None`.
pub(super) fn outcome_row(record: &WorkerRecord, status: &str) -> Option<DispatchRoutingRow> {
    if record.kind == "audit" {
        return None;
    }
    let final_tier = final_tier(record);
    let high_findings = record
        .findings
        .iter()
        .filter(|finding| finding.severity == "high")
        .count();
    let label = outcome_label(
        &record.reroutes,
        status,
        &final_tier,
        record.fix_rounds,
        high_findings,
    );
    Some(DispatchRoutingRow::Outcome {
        ts: unix_ms(),
        worker_id: record.id.clone(),
        plan_id: record.plan_id.clone().unwrap_or_default(),
        step: record.plan_step.map_or(0, |step| step + 1),
        status: status.to_string(),
        final_tier,
        models_used: record.models_used.clone(),
        review_rounds: record.review_rounds,
        fix_rounds: record.fix_rounds,
        findings: record.findings.len(),
        findings_high: high_findings,
        audits: record.audits as usize,
        cost_micros: record.cost_micros.unwrap_or(0),
        label: label.map(|(label, _)| label.to_string()),
    })
}

/// Convert the settings wire shape into the only tier-reading interface.
/// An entirely empty ladder is allowed here (no models configured yet); the
/// Settings save path performs the stricter non-empty-`trivial` validation.
pub(super) fn tier_ladder_from_dto(tiers: &mnemo::headless::TiersDto) -> TierLadder {
    TierLadder {
        tiers: tiers
            .tiers
            .iter()
            .map(|tier| Tier {
                id: tier.id.clone(),
                name: tier.name.clone(),
                definition: tier.definition.clone(),
                models: tier.models.clone(),
                effort: tier.effort.clone(),
                role: tier.role.clone(),
            })
            .collect(),
    }
}

/// Ladder ids in their fixed order, for schemas and diagnostics.
pub(super) fn tier_ids(ladder: &TierLadder) -> Vec<&str> {
    ladder.ordered().iter().map(|tier| tier.id.as_str()).collect()
}

/// Resolve a requested tier to its nearest available model at or below it.
pub(super) fn resolve_tier_over(
    models: &[LeadModel],
    ladder: &TierLadder,
    tier: &str,
) -> Result<(String, LeadModel), String> {
    if ladder.get(tier).is_none() {
        return Err(format!(
            "unknown tier '{tier}'; expected one of: {}",
            tier_ids(ladder).join(", ")
        ));
    }
    let (mut ran_tier, _) = ladder.resolve(tier).ok_or_else(|| {
        format!("tier '{tier}' has no available models; pass a `model` or another tier")
    })?;
    loop {
        if let Some(model) = ladder
            .get(&ran_tier)
            .into_iter()
            .flat_map(|rung| rung.models.iter())
            .find_map(|spec| models.iter().find(|model| model.spec == *spec))
        {
            return Ok((ran_tier.to_string(), model.clone()));
        }
        let rank = ladder
            .ordered()
            .iter()
            .position(|candidate| candidate.id == ran_tier)
            .expect("a resolved tier is on the ladder");
        if rank == 0 {
            break;
        }
        ran_tier = ladder.ordered()[rank - 1].id.clone();
    }
    Err(format!(
        "tier '{tier}' has no available models; pass a `model` or another tier"
    ))
}

/// The tier ladder the nudges climb, lowest first. `escalate` is the
/// deliberate final destination after the highest ordinary tier fails.
pub(super) const NUDGE_LADDER: [&str; 5] = ["trivial", "routine", "medium", "high", "escalate"];

/// The enabled-tier ladder over a model inventory and its tier assignment:
/// a tier is enabled when at least one of the models the tier names is
/// available. [`TeamRuntime::enabled_routing_tiers`] over shared handles,
/// so a worker pump computes the same ladder without the runtime.
fn enabled_tiers_over(
    models: &[LeadModel],
    tiers: &mnemo::headless::TiersDto,
) -> EnabledTiers {
    let ladder = tier_ladder_from_dto(tiers);
    let resolves = |tier| resolve_tier_over(models, &ladder, tier).is_ok();
    EnabledTiers {
        trivial: resolves("trivial"),
        routine: resolves("routine"),
        medium: resolves("medium"),
        high: resolves("high"),
        escalate: resolves("escalate"),
    }
}

/// Directly modelled tiers for nudges. Unlike dispatch routing, a nudge must
/// not target an empty tier that would fold back to the worker's current one,
/// nor a tier whose first available model is the model the worker already
/// runs: a suggestion that changes nothing is noise.
pub(super) fn direct_tiers_over(
    models: &[LeadModel],
    tiers: &mnemo::headless::TiersDto,
    current_model: Option<&str>,
) -> EnabledTiers {
    let ladder = tier_ladder_from_dto(tiers);
    let direct = |tier| {
        let Some(rung) = ladder.get(tier) else {
            return false;
        };
        // The model a dispatch on this tier would actually run: the first
        // spec in the rung that the inventory serves.
        let first = rung
            .models
            .iter()
            .find(|spec| models.iter().any(|model| model.spec == **spec));
        match first {
            Some(spec) => current_model != Some(spec.as_str()),
            None => false,
        }
    };
    EnabledTiers {
        trivial: direct("trivial"),
        routine: direct("routine"),
        medium: direct("medium"),
        high: direct("high"),
        escalate: direct("escalate"),
    }
}

/// The lowest enabled tier strictly above `current`: the tier a nudge
/// suggests. `None` when `current` is not on the ladder or already at its
/// enabled top — a worker on the top tier is never nudged.
pub(super) fn next_tier_above(enabled: EnabledTiers, current: &str) -> Option<&'static str> {
    let rank = NUDGE_LADDER.iter().position(|tier| *tier == current)?;
    NUDGE_LADDER
        .iter()
        .zip([
            enabled.trivial,
            enabled.routine,
            enabled.medium,
            enabled.high,
            enabled.escalate,
        ])
        .skip(rank + 1)
        .find(|(_, offered)| *offered)
        .map(|(tier, _)| *tier)
}

/// Whether `candidate` sits strictly above `tier` on the nudge ladder.
pub(super) fn tier_is_above(tier: &str, candidate: &str) -> bool {
    match (
        NUDGE_LADDER.iter().position(|name| *name == tier),
        NUDGE_LADDER.iter().position(|name| *name == candidate),
    ) {
        (Some(rank), Some(candidate)) => candidate > rank,
        _ => false,
    }
}

impl TeamRuntime {
    /// The live dispatch-routing kNN overlay, when the engine exposes its
    /// shared embedder. The eval reads it to report Laya-only and kNN+Laya
    /// accuracy side by side.
    pub fn route_knn(&self) -> Option<Arc<RouteKnn>> {
        self.route_knn.clone()
    }

    /// Replace the model tiers new dispatches resolve against.
    pub fn set_tiers(&self, tiers: mnemo::headless::TiersDto) {
        *self.tiers.write().unwrap_or_else(|e| e.into_inner()) = tiers;
    }

    /// Refresh the model inventory, tiers and prices after a settings save.
    pub fn set_catalog(
        &self,
        models: Vec<LeadModel>,
        tiers: mnemo::headless::TiersDto,
        pricing: Vec<mnemo::config::PricingEntry>,
    ) {
        *self.models.write().unwrap_or_else(|e| e.into_inner()) = models;
        self.set_tiers(tiers);
        self.reload_routing_settings();
        *self.pricing.write().expect("pricing lock poisoned") = pricing;
    }

    /// Re-read the `[routing]` table of `mnemo2.toml` into the live team, so a
    /// routing-settings save (the Laya page's Save, the eval's Apply) takes
    /// effect on the next dispatch without a restart. [`Self::set_catalog`] is
    /// the other reload path — a model-settings save.
    pub fn reload_routing_settings(&self) {
        *self.routing.write().unwrap_or_else(|e| e.into_inner()) =
            load_routing_settings(&self.config_dir);
    }

    pub(super) fn current_models(&self) -> Vec<LeadModel> {
        self.models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The live ladder new dispatches and tool descriptions resolve against.
    pub(super) fn tier_ladder(&self) -> TierLadder {
        tier_ladder_from_dto(&self.tiers.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// The tier's configured models that are still available, in order.
    pub(crate) fn tier_models(&self, tier: &str) -> Vec<LeadModel> {
        let ladder = self.tier_ladder();
        let models = self.current_models();
        ladder
            .get(tier)
            .into_iter()
            .flat_map(|rung| rung.models.iter())
            .filter_map(|spec| models.iter().find(|model| model.spec == *spec).cloned())
            .collect()
    }

    /// Resolve one lead tier through the ladder's fold-down rule.
    pub(super) fn resolve_tier_choice(&self, tier: &str) -> Result<(String, LeadModel), String> {
        resolve_tier_over(&self.current_models(), &self.tier_ladder(), tier)
    }

    /// Every available model some tier names, in tier order, once each.
    pub(super) fn tiered_models(&self) -> Vec<LeadModel> {
        let mut models: Vec<LeadModel> = Vec::new();
        let ladder = self.tier_ladder();
        for tier in tier_ids(&ladder) {
            for model in self.tier_models(tier) {
                if !models.iter().any(|known| known.spec == model.spec) {
                    models.push(model);
                }
            }
        }
        models
    }

    /// The model a lead's `tier`/`model` choice runs on. With tiers
    /// configured the lead stays inside them: an explicit model must be one
    /// a tier names. `None` when neither was given.
    pub(super) fn lead_model_choice(
        &self,
        tier: Option<&str>,
        model: Option<String>,
    ) -> Result<Option<String>, String> {
        if let Some(tier) = tier {
            if self.tier_ladder().get(tier).is_none() {
                return Err(format!(
                    "unknown tier '{tier}'; expected one of: {}",
                    tier_ids(&self.tier_ladder()).join(", ")
                ));
            }
        }
        let model = match (model, tier) {
            (None, Some(tier)) => Some(self.resolve_tier_choice(tier)?.1.spec),
            (model, _) => model,
        };
        let tiered = self.tiered_models();
        match &model {
            Some(spec) if !tiered.is_empty() && !tiered.iter().any(|m| &m.spec == spec) => {
                Err(format!(
                    "'{spec}' is not in any model tier (Settings > Tiers); pass a `tier`, or a model a tier names: {}",
                    tiered.iter().map(|m| m.spec.as_str()).collect::<Vec<_>>().join(", ")
                ))
            }
            _ => Ok(model),
        }
    }

    /// Resolve a lead's model/tier choice and retain the tier that actually
    /// runs (which may be below the requested tier).
    pub(super) fn lead_choice_with_tier(
        &self,
        tier: Option<&str>,
        model: Option<String>,
    ) -> Result<(Option<String>, Option<String>), String> {
        if model.is_none() {
            if let Some(tier) = tier {
                let (ran_tier, choice) = self.resolve_tier_choice(tier)?;
                return Ok((Some(choice.spec), Some(ran_tier)));
            }
        }
        let model = self.lead_model_choice(tier, model)?;
        let ran_tier = self.tier_for_model(model.as_deref());
        Ok((model, ran_tier))
    }

    pub(super) async fn route_implementation_dispatch(
        &self,
        request: &DispatchRequest,
        step_text: &str,
        brief: &str,
    ) -> Result<RoutedDispatch, String> {
        let settings = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let requested_tier = request.tier.as_deref();
        if requested_tier == Some("auto") && matches!(settings.mode, RoutingMode::Off | RoutingMode::Shadow) {
            return Err("tier auto needs routing mode suggest or auto (Settings > Tiers)".into());
        }
        // The escalation rung is never a first pick: fresh work dispatches on
        // high, and team_resume or team_route_fix moves an existing worker up.
        if request.source == DispatchSource::LeadTool {
            let ladder = self.tier_ladder();
            if let (Some(tier), Some(rung)) = (requested_tier, ladder.escalation()) {
                if tier == rung.id {
                    return Err(
                        "tier escalate is escalation only, never a first pick; dispatch on high, or move an existing worker up with team_resume or team_route_fix and tier escalate"
                            .into(),
                    );
                }
            }
        }

        if requested_tier.is_none()
            && request.model.is_none()
            && settings.mode != RoutingMode::Auto
            && request.source == DispatchSource::LeadTool
        {
            if let Some(required) = self.tier_required() {
                return Err(required);
            }
        }
        if settings.mode == RoutingMode::Off {
            let (model, tier) =
                self.lead_choice_with_tier(requested_tier, request.model.clone())?;
            let reasoning_effort = request.reasoning_effort.clone().or_else(|| {
                tier.as_deref()
                    .and_then(|tier| self.tier_ladder().get(tier).and_then(|rung| rung.effort.clone()))
            });
            return Ok(RoutedDispatch {
                model,
                tier,
                reasoning_effort,
                routing: None,
                rows: Vec::new(),
                pending: None,
                error: None,
            });
        }

        let explicit_tier = requested_tier.filter(|tier| *tier != "auto");
        let has_explicit_choice = explicit_tier.is_some() || request.model.is_some();
        let enabled = self.enabled_routing_tiers();
        let classified_text = routing_text(step_text, brief).filter(|text| {
            text.split_whitespace().count() >= settings.min_words
        });
        let classifier = self.classifier.as_ref().and_then(|slot| {
            slot.read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
        // An explicit lead/user choice never waits on the classification
        // (plan c74b1987): the decision cannot change what runs — refusals
        // only occur on non-explicit paths — so it runs in the background
        // under the same permit and timeout, writes the same corpus rows,
        // and attaches its label to the worker's routing record when it
        // lands. Skip conditions (too short, one tier, no classifier) stay
        // inline: their outcome is decided without a call, exactly as
        // before.
        if has_explicit_choice
            && classified_text.is_some()
            && enabled.offered().count() >= 2
        {
            if let Some(classifier) = classifier {
                let (chosen_model, chosen_tier) =
                    self.lead_choice_with_tier(explicit_tier, request.model.clone())?;
                let reasoning_effort = request.reasoning_effort.clone().or_else(|| {
                    chosen_tier.as_deref().and_then(|tier| {
                        self.tier_ladder()
                            .get(tier)
                            .and_then(|rung| rung.effort.clone())
                    })
                });
                let routing = WorkerRouting {
                    laya_tier: None,
                    confidence: None,
                    cleared: false,
                    lead_tier: explicit_tier
                        .map(str::to_string)
                        .or_else(|| chosen_tier.clone()),
                    routed_by: "lead".to_string(),
                    ran_tier: chosen_tier.clone(),
                    nudged: None,
                };
                return Ok(RoutedDispatch {
                    model: chosen_model.clone(),
                    tier: chosen_tier.clone(),
                    reasoning_effort,
                    routing: Some(routing),
                    rows: Vec::new(),
                    pending: Some(PendingClassification {
                        classifier,
                        knn: self.route_knn.clone(),
                        permit: self.routing_permit.clone(),
                        text: classified_text.unwrap_or_default(),
                        enabled,
                        settings,
                        plan_id: request
                            .plan
                            .as_ref()
                            .map(|plan| plan.plan_id.clone())
                            .unwrap_or_default(),
                        step: request.plan.as_ref().map_or(0, |plan| plan.step + 1),
                        explicit_tier: explicit_tier.map(str::to_string),
                        chosen_tier: chosen_tier.clone(),
                        chosen_model,
                        model_choice: request.model.is_some(),
                    }),
                    error: None,
                });
            }
        }
        let (decision, skip_reason): (Option<DispatchRoutingDecision>, Option<&'static str>) =
            if classified_text.is_none() {
                (None, Some("too_short"))
            } else if enabled.offered().count() < 2 {
                (None, Some("one_tier"))
            } else if let Some(classifier) = classifier {
                let attempt = async {
                    let _permit = self
                        .routing_permit
                        .acquire()
                        .await
                        .map_err(|_| "Laya routing is closed".to_string())?;
                    Ok::<Option<DispatchRoutingDecision>, String>(
                        classify_dispatch(
                            classifier.as_ref(),
                            self.route_knn.as_deref(),
                            classified_text.as_deref().unwrap_or(""),
                            enabled,
                            &settings,
                        )
                        .await,
                    )
                };
                match tokio::time::timeout(std::time::Duration::from_secs(3), attempt).await {
                    Ok(Ok(Some(decision))) => (Some(decision), None),
                    Ok(Ok(None)) | Err(_) => (None, Some("offline")),
                    Ok(Err(error)) => return Err(error),
                }
            } else {
                (None, Some("offline"))
            };
        let cleared_tier = decision
            .as_ref()
            .filter(|decision| decision.cleared)
            .and_then(|decision| decision.tier);

        let mut refusal = None;
        let (chosen_model, chosen_tier) = if has_explicit_choice {
            self.lead_choice_with_tier(explicit_tier, request.model.clone())?
        } else if let Some(tier) = cleared_tier.filter(|_| {
            settings.mode == RoutingMode::Auto
                || (settings.mode == RoutingMode::Suggest && requested_tier == Some("auto"))
        }) {
            let (ran_tier, choice) = self.resolve_tier_choice(tier)?;
            (Some(choice.spec), Some(ran_tier))
        } else if settings.mode == RoutingMode::Suggest && requested_tier == Some("auto") {
            refusal = Some(if skip_reason == Some("offline") {
                "Laya is not running; pass a tier".to_string()
            } else {
                format!(
                    "Laya is unsure ({:.2}); pass a tier",
                    decision.as_ref().map_or(0.0, |decision| decision.confidence)
                )
            });
            (None, None)
        } else if settings.mode == RoutingMode::Auto {
            let tier = settings.fallback_tier.clone();
            if self.tiered_models().is_empty() {
                (self.lead_model_choice(None, None)?, None)
            } else {
                let (ran_tier, choice) = self.resolve_tier_choice(&tier)?;
                (Some(choice.spec), Some(ran_tier))
            }
        } else {
            (self.lead_model_choice(None, None)?, None)
        };
        let auto_fallback_tier = (!has_explicit_choice
            && settings.mode == RoutingMode::Auto
            && cleared_tier.is_none()
            && chosen_tier.is_some())
        .then(|| settings.fallback_tier.clone());
        let routed_by = if has_explicit_choice {
            let lead_tier = if request.model.is_some() {
                chosen_tier.as_deref()
            } else {
                explicit_tier
            };
            if cleared_tier.is_some() && cleared_tier != lead_tier {
                "lead_override"
            } else {
                "lead"
            }
        } else if cleared_tier.is_some()
            && refusal.is_none()
            && (settings.mode == RoutingMode::Auto
                || (settings.mode == RoutingMode::Suggest && requested_tier == Some("auto")))
        {
            "laya"
        } else {
            "lead"
        };
        let reasoning_effort = request.reasoning_effort.clone().or_else(|| {
            chosen_tier.as_deref().and_then(|tier| {
                self.tier_ladder()
                    .get(tier)
                    .and_then(|rung| rung.effort.clone())
            })
        });
        let ran_tier = refusal.is_none().then(|| chosen_tier.clone()).flatten();
        let ran_model = refusal.is_none().then(|| chosen_model.clone()).flatten();
        let routing = WorkerRouting {
            laya_tier: decision
                .as_ref()
                .and_then(|decision| decision.tier)
                .map(str::to_string),
            confidence: decision.as_ref().map(|decision| decision.confidence),
            cleared: decision.as_ref().is_some_and(|decision| decision.cleared),
            lead_tier: (routed_by != "laya")
                .then(|| {
                    explicit_tier
                        .map(str::to_string)
                        .or_else(|| auto_fallback_tier.clone())
                        .or_else(|| chosen_tier.clone())
                })
                .flatten(),
            routed_by: routed_by.to_string(),
            ran_tier: ran_tier.clone(),
            nudged: None,
        };
        let plan_id = request.plan.as_ref().map(|plan| plan.plan_id.clone());
        let step = request.plan.as_ref().map(|plan| plan.step + 1);
        let ts = unix_ms();
        let row = dispatch_route_row(
            ts,
            request.worker_id.as_deref().unwrap_or_default(),
            plan_id.as_deref().unwrap_or_default(),
            step.unwrap_or_default(),
            settings.mode,
            decision.as_ref(),
            skip_reason,
            routing.laya_tier.as_deref(),
            routing.cleared,
            routing.lead_tier.as_deref(),
            ran_tier.as_deref(),
            ran_model.as_deref(),
            classified_text.as_deref().unwrap_or_default(),
            enabled,
            &settings,
        );
        let mut rows = vec![row];
        if routed_by == "lead_override" {
            if let Some(laya_tier) = routing.laya_tier.clone() {
                rows.push(dispatch_lead_override_row(
                    ts,
                    request.worker_id.as_deref().unwrap_or_default(),
                    plan_id.as_deref().unwrap_or_default(),
                    step.unwrap_or_default(),
                    &laya_tier,
                    chosen_tier.as_deref(),
                    explicit_tier
                        .map(str::to_string)
                        .filter(|requested| Some(requested.as_str()) != chosen_tier.as_deref())
                        .as_deref(),
                    decision.as_ref().map_or(0.0, |decision| decision.confidence),
                ));
            }
        }
        Ok(RoutedDispatch {
            model: chosen_model,
            tier: chosen_tier,
            reasoning_effort,
            routing: Some(routing),
            rows,
            pending: None,
            error: refusal,
        })
    }

    /// Run the classification an explicit-choice dispatch deferred
    /// (plan c74b1987): the same permit and 3-second timeout as the
    /// inline path, the same corpus rows, and — when the worker record
    /// still exists — the label attached to its routing block, following
    /// the nudge path's live-record mutation. The dispatch has already
    /// returned; nothing here can block or fail it, so a record gone to
    /// cleanup drops the attach silently while the corpus rows still land.
    pub(super) fn spawn_pending_classification(&self, pending: PendingClassification, worker_id: String) {
        let records = self.records.clone();
        let routing_log_path = self.routing_log_path.clone();
        let usage = self.usage.clone();
        let save_lock = self.save_lock.clone();
        let persistence = self.persistence.clone();
        tokio::spawn(async move {
            let attempt = async {
                let _permit = pending
                    .permit
                    .acquire()
                    .await
                    .map_err(|_| "Laya routing is closed".to_string())?;
                Ok::<Option<DispatchRoutingDecision>, String>(
                    classify_dispatch(
                        pending.classifier.as_ref(),
                        pending.knn.as_deref(),
                        &pending.text,
                        pending.enabled,
                        &pending.settings,
                    )
                    .await,
                )
            };
            // The same 3-second budget as the inline path, permit wait
            // included; a closed permit or a timeout is an offline skip.
            let decision = match tokio::time::timeout(
                std::time::Duration::from_secs(3),
                attempt,
            )
            .await
            {
                Ok(Ok(decision)) => decision,
                Ok(Err(_)) | Err(_) => None,
            };
            let skip = decision.is_none().then_some("offline");
            let laya_tier = decision
                .as_ref()
                .and_then(|decision| decision.tier)
                .map(str::to_string);
            let cleared = decision.as_ref().is_some_and(|decision| decision.cleared);
            let lead_tier = pending
                .explicit_tier
                .clone()
                .or_else(|| pending.chosen_tier.clone());
            let ts = unix_ms();
            let row = dispatch_route_row(
                ts,
                &worker_id,
                &pending.plan_id,
                pending.step,
                pending.settings.mode,
                decision.as_ref(),
                skip,
                laya_tier.as_deref(),
                cleared,
                lead_tier.as_deref(),
                pending.chosen_tier.as_deref(),
                pending.chosen_model.as_deref(),
                &pending.text,
                pending.enabled,
                &pending.settings,
            );
            mnemo::model_routing::append_row(&routing_log_path, &row);
            // The override row an explicit choice logs when Laya cleared a
            // different tier — the same condition the inline path's
            // `routed_by` computation applies to an explicit choice.
            let cleared_tier = decision
                .as_ref()
                .filter(|decision| decision.cleared)
                .and_then(|decision| decision.tier);
            let lead_choice_tier = if pending.model_choice {
                pending.chosen_tier.as_deref()
            } else {
                pending.explicit_tier.as_deref()
            };
            if cleared_tier.is_some() && cleared_tier != lead_choice_tier {
                if let Some(laya_tier) = laya_tier.clone() {
                    let row = dispatch_lead_override_row(
                        ts,
                        &worker_id,
                        &pending.plan_id,
                        pending.step,
                        &laya_tier,
                        pending.chosen_tier.as_deref(),
                        pending
                            .explicit_tier
                            .as_deref()
                            .map(str::to_string)
                            .filter(|requested| {
                                Some(requested.as_str()) != pending.chosen_tier.as_deref()
                            })
                            .as_deref(),
                        decision.as_ref().map_or(0.0, |decision| decision.confidence),
                    );
                    mnemo::model_routing::append_row(&routing_log_path, &row);
                }
            }
            // Attach the label to the live record; a record gone to cleanup
            // drops the attach silently — the corpus rows above still land.
            let attached = {
                let mut records = records.lock().await;
                records
                    .iter_mut()
                    .find(|worker| worker.id == worker_id)
                    .and_then(|worker| worker.routing.as_mut())
                    .map(|routing| {
                        routing.laya_tier = laya_tier.clone();
                        routing.confidence =
                            decision.as_ref().map(|decision| decision.confidence);
                        routing.cleared = cleared;
                    })
                    .is_some()
            };
            if attached {
                let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
            }
        });
    }

    /// The tier a model spec belongs to: the first ladder rung whose list
    /// names an available model.
    fn tier_of(&self, spec: &str) -> Option<String> {
        let ladder = self.tier_ladder();
        let models = self.current_models();
        tier_ids(&ladder).into_iter().find(|tier| {
            ladder
                .get(tier)
                .into_iter()
                .flat_map(|rung| rung.models.iter())
                .any(|candidate| {
                    candidate == spec && models.iter().any(|model| model.spec == *candidate)
                })
        }).map(str::to_string)
    }

    fn tier_for_model(&self, model: Option<&str>) -> Option<String> {
        self.tier_of(model?)
    }

    pub(super) fn enabled_routing_tiers(&self) -> EnabledTiers {
        enabled_tiers_over(
            &self.current_models(),
            &self.tiers
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        )
    }

    /// Nudge destinations have their own models, so a nudge cannot fold back
    /// onto the tier the worker is already running, nor onto a tier whose
    /// first available model is the model the worker already runs.
    pub(super) fn direct_nudge_tiers(&self, current_model: Option<&str>) -> EnabledTiers {
        direct_tiers_over(
            &self.current_models(),
            &self.tiers
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            current_model,
        )
    }

    pub(super) fn append_dispatch_routing_row(&self, row: &DispatchRoutingRow) {
        mnemo::model_routing::append_row(&self.routing_log_path, row);
    }

    /// Append a reroute with the step-6 metadata kept outside the legacy typed
    /// row, so old readers and parallel settings code retain their contract.
    pub(super) fn append_reroute_row(
        &self,
        row: &DispatchRoutingRow,
        source: ChangeSource,
        finding_closed: Option<bool>,
    ) {
        append_reroute_metadata(&self.routing_log_path, row, source, finding_closed);
    }

    /// The routing mode in force right now.
    pub(super) fn routing_mode(&self) -> RoutingMode {
        self.routing.read().unwrap_or_else(|e| e.into_inner()).mode
    }

    /// The refusal for a dispatch with no tier while tiers are configured.
    fn tier_required(&self) -> Option<String> {
        self.tier_guidance().map(|guidance| {
            format!("choose a `tier` for this worker; its model comes from that tier:\n{guidance}")
        })
    }

    /// `$in/$out` per 1M tokens for a model, when priced.
    fn price_label(&self, model: &LeadModel) -> Option<String> {
        let pricing = self.pricing.read().expect("pricing lock poisoned");
        pricing
            .iter()
            .find(|p| p.model == model.model)
            .map(|p| format!("${}/${}", p.input_per_1m, p.output_per_1m))
    }

    /// Generate the lead's tier guidance from the ladder itself: editable
    /// names and definitions, available models with prices, effort and the
    /// fold-down destination of an empty tier.
    pub(super) fn tier_guidance(&self) -> Option<String> {
        if self.tiered_models().is_empty() {
            return None;
        }
        let ladder = self.tier_ladder();
        let mut lines = Vec::new();
        for rung in ladder.ordered() {
            let tier = rung.id.as_str();
            let models = self.tier_models(tier);
            let list = if models.is_empty() {
                match self.resolve_tier_choice(tier) {
                    Ok((ran_tier, _)) if ran_tier != tier => {
                        format!("no model; folds down to {ran_tier}")
                    }
                    _ => "no model".to_string(),
                }
            } else {
                models
                    .iter()
                    .map(|model| match self.price_label(model) {
                        Some(price) => format!("{} ({price})", model.spec),
                        None => model.spec.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let effort = rung
                .effort
                .as_deref()
                .map(|effort| format!("; effort {effort}"))
                .unwrap_or_default();
            let escalation = if rung.role.as_deref() == Some("escalate") {
                "; escalation only, never a first pick"
            } else {
                ""
            };
            lines.push(format!(
                "{tier} ({}) - {}: {list}{effort}{escalation}",
                rung.name, rung.definition
            ));
        }
        if matches!(
            self.routing.read().unwrap_or_else(|e| e.into_inner()).mode,
            RoutingMode::Suggest | RoutingMode::Auto
        ) {
            lines.push("auto - let Laya choose from the step spec (never escalate)".into());
        }
        Some(lines.join("\n"))
    }

    /// The reroute row for a model switch (design §4.7): the worker's route
    /// before and after, the status it was in, the nudge it carried, and who
    /// moved it. `None` when the switch does not move the model (an
    /// effort-only change): nothing was rerouted, nothing is logged.
    pub(super) fn reroute_row(
        &self,
        record: &WorkerRecord,
        from_spec: &str,
        to_model: Option<&str>,
        source: ChangeSource,
        nudged: Option<&str>,
    ) -> Option<DispatchRoutingRow> {
        let to_spec = to_model.unwrap_or("default");
        if to_spec == from_spec {
            return None;
        }
        let ts = unix_ms();
        let worker_id = record.id.clone();
        let plan_id = record.plan_id.clone().unwrap_or_default();
        let step = record.plan_step.map_or(0, |step| step + 1);
        let from_tier = self
            .tier_of(from_spec)
            .or_else(|| record.tier.clone())
            .unwrap_or_else(|| "custom".to_string());
        let to_tier = self
            .tier_of(to_spec)
            .unwrap_or_else(|| "custom".to_string());
        let from_model = record.model.clone().filter(|spec| !spec.trim().is_empty());
        let to_model = to_model.map(str::to_string);
        let status = status_name(record.status);
        let review_rounds = record.review_rounds;
        let fix_rounds = record.fix_rounds;
        // A downshift stays marked until its delta review records the outcome;
        // this is the durable pending marker in team.json.
        let nudged = nudged.is_some() || source == ChangeSource::FixDownshift;
        Some(match source {
            ChangeSource::Lead
            | ChangeSource::Escalation
            | ChangeSource::FixDownshift
            | ChangeSource::FixDownshiftRestore => DispatchRoutingRow::LeadReroute {
                ts,
                worker_id,
                plan_id,
                step,
                from_tier,
                to_tier,
                from_model,
                to_model,
                status,
                review_rounds,
                fix_rounds,
                nudged,
            },
            ChangeSource::User => DispatchRoutingRow::UserReroute {
                ts,
                worker_id,
                plan_id,
                step,
                from_tier,
                to_tier,
                from_model,
                to_model,
                status,
                review_rounds,
                fix_rounds,
                nudged,
            },
        })
    }
}
