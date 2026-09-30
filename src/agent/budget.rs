// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The budget layer's PURE logic (backlog a25a5323, the cost-saving chain's
//! fourth item): the per-plan spend counters, the cap gate, and the per-plan
//! cost-report renderer — no channels, no config plumbing, no loop, so every
//! rule below is unit-testable alone (the [`super::step_lanes`] precedent).
//!
//! The caps are deterministic and the model can never override them: the gate
//! runs in the harness at the plan-step boundary and a reached cap PAUSES the
//! plan through a pending question (continue for this plan / double the
//! reached cap / end the turn) — never a hard kill, because plans are
//! crash-resumable and the plan file is the resumption document.
//!
//! The counters are in-memory and live-session scoped: a crash-resume
//! restarts the count while the durable `spend_events` ledger still backs the
//! per-plan cost report (documented boundary).

use std::collections::HashMap;

use crate::config::BudgetConfig;
use crate::memory::SpendEvent;

/// Why a request ran the model it ran — the `spend_events.reason` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetReason {
    /// A lane/turn routing decision chose the model.
    Route,
    /// A failed-cycle retry re-ran the step.
    Retry,
    /// An escalation arm moved the step up a rung.
    Escalate,
    /// No lane decision applied — the plan token cap still counts it.
    Default,
    /// A harness-verify evidence row (the deterministic checks).
    Verify,
}

impl BudgetReason {
    /// The wire label written to `spend_events.reason`.
    pub fn wire_label(&self) -> &'static str {
        match self {
            BudgetReason::Route => "route",
            BudgetReason::Retry => "retry",
            BudgetReason::Escalate => "escalate",
            BudgetReason::Default => "default",
            BudgetReason::Verify => "verify",
        }
    }

    /// Parse a wire label; `None` for an unknown string (fail-open — the
    /// caller keeps its own default).
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "route" => Some(BudgetReason::Route),
            "retry" => Some(BudgetReason::Retry),
            "escalate" => Some(BudgetReason::Escalate),
            "default" => Some(BudgetReason::Default),
            "verify" => Some(BudgetReason::Verify),
            _ => None,
        }
    }
}

/// The live per-plan spend counters (in-memory, one set per agent loop).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanSpend {
    /// The plan the counters belong to; a DIFFERENT plan id resets them all.
    pub plan_id: Option<String>,
    /// Prompt tokens spent under the current plan.
    pub tokens_in: u64,
    /// Completion tokens spent under the current plan.
    pub tokens_out: u64,
    /// Escalations armed under the current plan.
    pub escalations: u32,
    /// Failed-cycle retries taken, per step (`step` → count).
    pub lane_retries: HashMap<usize, u32>,
    /// The human answered "continue" — cap checks stay off for this plan.
    pub bypassed: bool,
    /// The human doubled the token cap: this replaces the configured cap for
    /// the current plan.
    pub token_cap_override: Option<u64>,
    /// The human doubled the escalation cap, when that was the cap reached.
    pub escalation_cap_override: Option<u32>,
    /// The human doubled the per-step retry cap, when that was the cap
    /// reached.
    pub retry_cap_override: Option<u32>,
}

impl PlanSpend {
    /// Point the counters at `plan_id`; a different plan resets every counter
    /// (the bypass and any doubled caps are per plan and die with it).
    pub fn note_plan(&mut self, plan_id: &str) {
        if self.plan_id.as_deref() != Some(plan_id) {
            let fresh = PlanSpend {
                plan_id: Some(plan_id.to_string()),
                ..PlanSpend::default()
            };
            *self = fresh;
        }
    }

    /// Add one request's token cost.
    pub fn note_tokens(&mut self, tokens_in: u64, tokens_out: u64) {
        self.tokens_in = self.tokens_in.saturating_add(tokens_in);
        self.tokens_out = self.tokens_out.saturating_add(tokens_out);
    }

    /// Count one armed escalation.
    pub fn note_escalation(&mut self) {
        self.escalations = self.escalations.saturating_add(1);
    }

    /// Count one failed-cycle retry on `step` and return the new count.
    pub fn note_lane_retry(&mut self, step: usize) -> u32 {
        let count = self.lane_retries.entry(step).or_insert(0);
        *count = count.saturating_add(1);
        *count
    }

    /// Total tokens (in + out) spent under the current plan.
    pub fn total_tokens(&self) -> u64 {
        self.tokens_in.saturating_add(self.tokens_out)
    }

    /// The effective token cap for this plan: the human's doubled value when
    /// they chose it, else the configured cap.
    pub fn effective_token_cap(&self, cfg: &BudgetConfig) -> u64 {
        self.token_cap_override.unwrap_or(cfg.max_tokens_per_plan)
    }

    /// The effective escalation cap for this plan (doubled value, else the
    /// configured one).
    pub fn effective_escalation_cap(&self, cfg: &BudgetConfig) -> u32 {
        self.escalation_cap_override
            .unwrap_or(cfg.max_escalations_per_plan)
    }

    /// The effective per-step retry cap for this plan (doubled value, else
    /// the configured one).
    pub fn effective_retry_cap(&self, cfg: &BudgetConfig) -> u32 {
        self.retry_cap_override.unwrap_or(cfg.max_retries_per_lane)
    }

    /// The first reached cap, or `None` — the gate. Disabled config or a
    /// bypassed plan never caps. The token cap triggers at `>=` (spending the
    /// cap reaches it); the count caps at `>` (the configured number is
    /// allowed, the next one asks). `0` disables the token cap.
    pub fn cap_reached(&self, cfg: &BudgetConfig) -> Option<CapKind> {
        if !cfg.enabled || self.bypassed {
            return None;
        }
        let cap = self.effective_token_cap(cfg);
        if cap > 0 && self.total_tokens() >= cap {
            return Some(CapKind::Tokens {
                spent: self.total_tokens(),
                cap,
            });
        }
        let escalation_cap = self.effective_escalation_cap(cfg);
        if self.escalations > escalation_cap {
            return Some(CapKind::Escalations {
                count: self.escalations,
                cap: escalation_cap,
            });
        }
        let retry_cap = self.effective_retry_cap(cfg);
        let mut over: Vec<(usize, u32)> = self
            .lane_retries
            .iter()
            .filter(|(_, count)| **count > retry_cap)
            .map(|(step, count)| (*step, *count))
            .collect();
        over.sort_unstable();
        if let Some(&(step, count)) = over.first() {
            return Some(CapKind::LaneRetries {
                step,
                count,
                cap: retry_cap,
            });
        }
        None
    }

    /// The human chose "double the cap": raise the REACHED cap's effective
    /// value to twice what it was, for this plan (a second doubling doubles
    /// the doubled value again).
    pub fn double_cap(&mut self, kind: CapKind, cfg: &BudgetConfig) {
        match kind {
            CapKind::Tokens { .. } => {
                self.token_cap_override = Some(self.effective_token_cap(cfg).saturating_mul(2));
            }
            CapKind::Escalations { .. } => {
                self.escalation_cap_override =
                    Some(self.effective_escalation_cap(cfg).saturating_mul(2));
            }
            CapKind::LaneRetries { .. } => {
                self.retry_cap_override = Some(self.effective_retry_cap(cfg).saturating_mul(2));
            }
        }
    }
}

/// Which cap the gate hit, with the REAL numbers the pause question cites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapKind {
    /// The plan's token budget is spent.
    Tokens {
        /// Tokens spent so far.
        spent: u64,
        /// The effective cap.
        cap: u64,
    },
    /// The plan armed more escalations than allowed.
    Escalations {
        /// Escalations armed so far.
        count: u32,
        /// The effective cap.
        cap: u32,
    },
    /// One step took more failed-cycle retries than allowed.
    LaneRetries {
        /// The step (0-indexed, the plan file's indexing).
        step: usize,
        /// Retries taken on that step.
        count: u32,
        /// The effective cap.
        cap: u32,
    },
}

impl CapKind {
    /// The one-line question body the pause shows the human — real numbers,
    /// no judgment call.
    pub fn question(&self) -> String {
        match self {
            CapKind::Tokens { spent, cap } => {
                format!("Budget: this plan has spent {spent} tokens (cap {cap})")
            }
            CapKind::Escalations { count, cap } => {
                format!("Budget: this plan has armed {count} escalations (cap {cap})")
            }
            CapKind::LaneRetries { step, count, cap } => format!(
                "Budget: step {} has taken {count} retries (cap {cap} per step)",
                step + 1
            ),
        }
    }
}

/// The attribution for one request's spend row: which plan/step/lane the
/// request belongs to and WHY the model ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpendAttribution {
    /// The plan the spend belongs to.
    pub plan_id: String,
    /// The turn id the request belongs to (always present — the caller mints
    /// one per turn when the route decision has none).
    pub turn_id: String,
    /// The plan step in progress, when one was.
    pub step_index: Option<usize>,
    /// The lane the request ran on, when a lane decision applied.
    pub lane: Option<String>,
    /// Why the model ran.
    pub reason: BudgetReason,
}

/// Render the per-plan cost report — the section appended to the plan file at
/// `finish`. Real numbers only, straight from the plan's `spend_events` rows:
/// lane per step, tokens per lane, retries, escalations (with their reasons),
/// the deterministic checks with their results, and the cache hits. There is
/// deliberately NO counterfactual "vs single-model baseline" column (rejected
/// in the cost-saving DECISION memory): the ledger records what WAS spent,
/// not what might have been. The `verify` evidence rows are excluded from the
/// request/token totals — they carry no usage and are listed separately under
/// "Deterministic checks".
pub fn cost_report(rows: &[SpendEvent]) -> String {
    let mut out = String::from("## Cost report\n\n");
    if rows.is_empty() {
        out.push_str("No spend was recorded for this plan.\n");
        return out;
    }
    // Model requests only: the `verify` rows are deterministic-check EVIDENCE
    // (zero tokens, no model) listed separately under "Deterministic checks" —
    // counting them as requests would overstate the bill (round-1 finding L2).
    let billed: Vec<&SpendEvent> = rows.iter().filter(|r| r.reason != "verify").collect();
    let tokens_in: i64 = billed.iter().map(|r| r.tokens_in).sum();
    let tokens_out: i64 = billed.iter().map(|r| r.tokens_out).sum();
    let cached: i64 = billed.iter().map(|r| r.cached_tokens.unwrap_or(0)).sum();
    out.push_str(&format!(
        "Totals: {} request(s), {} tokens in + {} tokens out = {} tokens; {} cached prompt tokens (a subset of tokens in).\n",
        billed.len(),
        tokens_in,
        tokens_out,
        tokens_in + tokens_out,
        cached
    ));

    // Per-lane breakdown, biggest spender first (ties break on the lane name
    // so the report is deterministic across runs).
    let mut lanes: HashMap<&str, (usize, i64)> = HashMap::new();
    for r in &billed {
        let lane = r.lane.as_deref().unwrap_or("(no lane)");
        let entry = lanes.entry(lane).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += r.tokens_in + r.tokens_out;
    }
    let mut lane_rows: Vec<(&str, (usize, i64))> = lanes.into_iter().collect();
    lane_rows.sort_by(|a, b| (b.1).1.cmp(&(a.1).1).then_with(|| a.0.cmp(b.0)));
    // The header is emitted only with content: a verify-only ledger has no
    // lane rows and must not render a dangling "Per lane:" line (round-2 L1).
    if !lane_rows.is_empty() {
        out.push_str("\nPer lane:\n");
        for (lane, (count, tokens)) in lane_rows {
            out.push_str(&format!("- {lane}: {count} request(s), {tokens} tokens\n"));
        }
    }

    // The lane each step ran on, from the route rows (the step's LAST route
    // row wins).
    let mut per_step: Vec<(usize, &str)> = Vec::new();
    for r in rows {
        if r.reason == "route" {
            if let (Some(step), Some(lane)) = (r.step_index, r.lane.as_deref()) {
                match per_step.iter_mut().find(|(s, _)| *s == step) {
                    Some(slot) => slot.1 = lane,
                    None => per_step.push((step, lane)),
                }
            }
        }
    }
    if !per_step.is_empty() {
        per_step.sort_unstable();
        out.push_str("\nLane per step:\n");
        for (step, lane) in per_step {
            out.push_str(&format!("- step {}: {lane}\n", step + 1));
        }
    }

    // Failed-cycle retries, with their class.
    let retries: Vec<&SpendEvent> = rows.iter().filter(|r| r.reason == "retry").collect();
    if !retries.is_empty() {
        out.push_str("\nRetries:\n");
        for r in retries {
            out.push_str(&format!("- {}: {} — {} tokens\n", step_label(r), detail_label(r), r.tokens_in + r.tokens_out));
        }
    }

    // Escalations, with the classifier's reason.
    let escalations: Vec<&SpendEvent> = rows.iter().filter(|r| r.reason == "escalate").collect();
    if !escalations.is_empty() {
        out.push_str("\nEscalations:\n");
        for r in escalations {
            out.push_str(&format!(
                "- {}: {} ({} tokens)\n",
                step_label(r),
                detail_label(r),
                r.tokens_in + r.tokens_out
            ));
        }
    }

    // The deterministic checks (harness-verify rows), with their results.
    let checks: Vec<&SpendEvent> = rows.iter().filter(|r| r.reason == "verify").collect();
    if !checks.is_empty() {
        out.push_str("\nDeterministic checks:\n");
        for r in checks {
            out.push_str(&format!("- {}: {}\n", step_label(r), detail_label(r)));
        }
    }

    out
}

/// `step N` (1-indexed, the display form) for a report row.
fn step_label(row: &SpendEvent) -> String {
    row.step_index
        .map(|s| format!("step {}", s + 1))
        .unwrap_or_else(|| "step ?".to_string())
}

/// The row's detail, or a placeholder.
fn detail_label(row: &SpendEvent) -> &str {
    row.detail.as_deref().unwrap_or("(no detail)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, tokens: u64, escalations: u32, retries: u32) -> BudgetConfig {
        BudgetConfig {
            enabled,
            max_tokens_per_plan: tokens,
            max_escalations_per_plan: escalations,
            max_retries_per_lane: retries,
        }
    }

    #[test]
    fn disabled_or_bypassed_never_caps() {
        let mut spend = PlanSpend::default();
        spend.note_plan("p1");
        spend.note_tokens(1_000_000, 1_000_000);
        spend.note_escalation();
        spend.note_escalation();
        for _ in 0..5 {
            spend.note_lane_retry(0);
        }
        // Budget off: no cap, ever — byte-identical behavior.
        assert_eq!(spend.cap_reached(&cfg(false, 10, 0, 0)), None);
        // The human answered "continue": cap checks stay off for this plan.
        let mut bypassed = spend.clone();
        bypassed.bypassed = true;
        assert_eq!(bypassed.cap_reached(&cfg(true, 10, 0, 0)), None);
    }

    #[test]
    fn token_cap_triggers_at_the_boundary_inclusive() {
        let c = cfg(true, 1_000, 100, 100);
        let mut spend = PlanSpend::default();
        spend.note_plan("p1");
        spend.note_tokens(999, 0);
        assert_eq!(spend.cap_reached(&c), None);
        spend.note_tokens(1, 0);
        assert_eq!(
            spend.cap_reached(&c),
            Some(CapKind::Tokens {
                spent: 1_000,
                cap: 1_000
            })
        );
        // 0 = the token cap is off.
        let off = cfg(true, 0, 100, 100);
        assert_eq!(spend.cap_reached(&off), None);
    }

    #[test]
    fn count_caps_allow_the_configured_number_then_ask() {
        let c = cfg(true, 0, 1, 2);
        let mut spend = PlanSpend::default();
        spend.note_plan("p1");
        spend.note_escalation();
        assert_eq!(spend.cap_reached(&c), None, "one escalation is allowed");
        spend.note_escalation();
        assert_eq!(
            spend.cap_reached(&c),
            Some(CapKind::Escalations { count: 2, cap: 1 })
        );

        let mut spend2 = PlanSpend::default();
        spend2.note_plan("p2");
        spend2.note_lane_retry(3);
        spend2.note_lane_retry(3);
        assert_eq!(spend2.cap_reached(&c), None, "two retries are allowed");
        assert_eq!(spend2.note_lane_retry(3), 3);
        assert_eq!(
            spend2.cap_reached(&c),
            Some(CapKind::LaneRetries {
                step: 3,
                count: 3,
                cap: 2
            })
        );
    }

    #[test]
    fn the_lowest_over_cap_step_is_reported_deterministically() {
        let c = cfg(true, 0, 100, 1);
        let mut spend = PlanSpend::default();
        spend.note_plan("p1");
        for _ in 0..2 {
            spend.note_lane_retry(5);
        }
        for _ in 0..2 {
            spend.note_lane_retry(2);
        }
        // HashMap iteration order must never leak: the lowest step wins.
        assert_eq!(
            spend.cap_reached(&c),
            Some(CapKind::LaneRetries {
                step: 2,
                count: 2,
                cap: 1
            })
        );
    }

    #[test]
    fn doubling_raises_the_reached_cap_for_this_plan() {
        let c = cfg(true, 1_000, 1, 1);
        let mut spend = PlanSpend::default();
        spend.note_plan("p1");
        spend.note_tokens(1_000, 0);
        let hit = spend.cap_reached(&c).unwrap();
        spend.double_cap(hit, &c);
        assert_eq!(spend.effective_token_cap(&c), 2_000);
        assert_eq!(spend.cap_reached(&c), None);
        spend.note_tokens(1_000, 0);
        assert_eq!(
            spend.cap_reached(&c),
            Some(CapKind::Tokens {
                spent: 2_000,
                cap: 2_000
            })
        );
        // A second doubling doubles the doubled value again.
        let hit = spend.cap_reached(&c).unwrap();
        spend.double_cap(hit, &c);
        assert_eq!(spend.effective_token_cap(&c), 4_000);

        // The count caps double the same way.
        let mut s2 = PlanSpend::default();
        s2.note_plan("p2");
        s2.note_lane_retry(0);
        s2.note_lane_retry(0);
        let hit = s2.cap_reached(&c).unwrap();
        assert_eq!(
            hit,
            CapKind::LaneRetries {
                step: 0,
                count: 2,
                cap: 1
            }
        );
        s2.double_cap(hit, &c);
        assert_eq!(s2.effective_retry_cap(&c), 2);
        assert_eq!(s2.cap_reached(&c), None);

        let mut s3 = PlanSpend::default();
        s3.note_plan("p3");
        s3.note_escalation();
        s3.note_escalation();
        let hit = s3.cap_reached(&c).unwrap();
        assert_eq!(hit, CapKind::Escalations { count: 2, cap: 1 });
        s3.double_cap(hit, &c);
        assert_eq!(s3.effective_escalation_cap(&c), 2);
        assert_eq!(s3.cap_reached(&c), None);
    }

    #[test]
    fn a_new_plan_resets_every_counter() {
        let mut spend = PlanSpend::default();
        spend.note_plan("p1");
        spend.note_tokens(500, 500);
        spend.note_escalation();
        spend.note_lane_retry(1);
        spend.bypassed = true;
        spend.token_cap_override = Some(9_999);
        spend.note_plan("p2");
        assert_eq!(spend.plan_id.as_deref(), Some("p2"));
        assert_eq!(spend.total_tokens(), 0);
        assert_eq!(spend.escalations, 0);
        assert!(spend.lane_retries.is_empty());
        assert!(!spend.bypassed);
        assert_eq!(spend.token_cap_override, None);
        // Re-noting the SAME plan keeps the counters.
        spend.note_tokens(1, 2);
        spend.note_plan("p2");
        assert_eq!(spend.total_tokens(), 3);
    }

    #[test]
    fn reason_wire_labels_round_trip() {
        for reason in [
            BudgetReason::Route,
            BudgetReason::Retry,
            BudgetReason::Escalate,
            BudgetReason::Default,
            BudgetReason::Verify,
        ] {
            assert_eq!(BudgetReason::from_label(reason.wire_label()), Some(reason));
        }
        assert_eq!(BudgetReason::from_label("nonsense"), None);
    }

    /// One report row with minimal attribution.
    fn row(
        reason: &str,
        lane: Option<&str>,
        step: Option<usize>,
        tokens_in: i64,
        tokens_out: i64,
        cached: Option<i64>,
        detail: Option<&str>,
    ) -> SpendEvent {
        SpendEvent {
            id: "r".into(),
            session_id: None,
            turn_id: None,
            agent_id: None,
            plan_id: "p1".into(),
            step_index: step,
            lane: lane.map(|l| l.to_string()),
            model: "m".into(),
            reason: reason.into(),
            tokens_in,
            tokens_out,
            cached_tokens: cached,
            detail: detail.map(|d| d.to_string()),
            created_at: 0,
        }
    }

    #[test]
    fn report_renders_every_section_from_real_numbers() {
        let rows = vec![
            row("route", Some("medium"), Some(0), 1_000, 200, Some(800), None),
            row(
                "retry",
                Some("high"),
                Some(0),
                900,
                100,
                Some(700),
                Some("flaky_test"),
            ),
            row(
                "escalate",
                Some("escalate"),
                Some(0),
                2_000,
                300,
                Some(1_500),
                Some("complexity high"),
            ),
            row(
                "verify",
                None,
                Some(0),
                0,
                0,
                None,
                Some("[verify] `cargo test` — FAILED (exit 101)"),
            ),
            row("route", Some("high"), Some(1), 500, 100, None, None),
            row("default", None, Some(0), 300, 50, None, None),
        ];
        let report = cost_report(&rows);
        assert!(report.starts_with("## Cost report\n"));
        // 6 ledger rows, 5 MODEL requests: the 0-token `verify` evidence row
        // is not a request (round-1 finding L2).
        assert!(
            report.contains("5 request(s), 4700 tokens in + 750 tokens out = 5450 tokens"),
            "{report}"
        );
        assert!(!report.contains("6 request(s)"), "{report}");
        assert!(report.contains("3000 cached prompt tokens"), "{report}");
        assert!(report.contains("- escalate: 1 request(s), 2300 tokens"));
        assert!(report.contains("- high: 2 request(s), 1600 tokens"));
        assert!(report.contains("- medium: 1 request(s), 1200 tokens"));
        assert!(report.contains("- (no lane): 1 request(s), 350 tokens"));
        assert!(report.contains("Lane per step:"));
        assert!(report.contains("- step 1: medium"));
        assert!(report.contains("- step 2: high"));
        assert!(report.contains("Retries:"));
        assert!(report.contains("- step 1: flaky_test — 1000 tokens"));
        assert!(report.contains("Escalations:"));
        assert!(report.contains("- step 1: complexity high (2300 tokens)"));
        assert!(report.contains("Deterministic checks:"));
        assert!(report.contains("- step 1: [verify] `cargo test` — FAILED (exit 101)"));
        // REAL numbers only — the rejected counterfactual column stays out.
        assert!(!report.contains("baseline"), "{report}");
    }

    #[test]
    fn empty_report_says_so() {
        let report = cost_report(&[]);
        assert_eq!(
            report,
            "## Cost report\n\nNo spend was recorded for this plan.\n"
        );
        // A verify row without a step still renders (the placeholder keeps
        // the section honest).
        let report = cost_report(&[row(
            "verify",
            None,
            None,
            0,
            0,
            None,
            Some("[verify] `cargo test` — passed (exit 0)"),
        )]);
        // A verify-only ledger still reports zero MODEL requests, and no
        // dangling "Per lane:" header with nothing under it (round-2 L1).
        assert!(report.contains("0 request(s)"), "{report}");
        assert!(!report.contains("Per lane"), "{report}");
        assert!(report.contains("- step ?: [verify] `cargo test` — passed (exit 0)"));
    }

    #[test]
    fn cap_questions_cite_real_numbers() {
        assert_eq!(
            CapKind::Tokens {
                spent: 12_000,
                cap: 10_000
            }
            .question(),
            "Budget: this plan has spent 12000 tokens (cap 10000)"
        );
        assert_eq!(
            CapKind::Escalations { count: 2, cap: 1 }.question(),
            "Budget: this plan has armed 2 escalations (cap 1)"
        );
        // The display form is 1-indexed like the plan document.
        assert_eq!(
            CapKind::LaneRetries {
                step: 3,
                count: 4,
                cap: 3
            }
            .question(),
            "Budget: step 4 has taken 4 retries (cap 3 per step)"
        );
    }
}
