// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Mnemo2's dispatch-time Laya routing decision (plan 2ba2168a, design
//! `.coding/design/mnemo2-laya-routing.md` §4.2–§4.8).
//!
//! At `team_dispatch` — before a worker is provisioned — Mnemo2 asks Laya one
//! question about the worker's step spec and the lead's brief: `trivial`,
//! `routine` or `hard`, which map onto the `simple`, `average` and `difficult`
//! model tiers. The question, its descriptions, the folding of a label whose
//! tier has no available model, and the default gates all come from the
//! kernel's shared [`route_question`] module, so Mnemo1's first-prompt gate
//! asks the byte-identical question.
//!
//! This module owns the Mnemo2-specific pieces:
//!
//! - **Text** ([`routing_text`]): the step's text (a `- [ ] N. ` checkbox
//!   prefix stripped) plus the lead's brief, capped at [`TEXT_MAX_CHARS`]. A
//!   step under [`MIN_WORDS`] words carries no routing signal and is not
//!   classified.
//! - **Question and gate** ([`routing_question`], [`threshold_for`]): the
//!   kernel question over the offered labels, gated per label count from
//!   [`RoutingSettings`].
//! - **Decision** ([`classify_dispatch`]): one [`DispatchRoutingDecision`],
//!   with the answer carried verbatim even when it clears nothing.
//! - **Log rows** ([`DispatchRoutingRow`]): the append-only
//!   `dispatch_routing.jsonl` corpus — one row per event — plus the
//!   [`outcome_label`] a merged worker contributes to it.
//!
//! Nothing here switches a running worker: this layer decides once, at
//! dispatch, and the rest of the module is calibration and training data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use mnemo::agent::failure_triage::training_log_dir;
use mnemo::agent::route_question;
use mnemo::memory::classifier::{Answer, Classifier, Question};

/// Fewest words (step text plus brief) a dispatch must have to be classified.
/// A shorter spec is not a better routing signal than the lead's own tier.
pub const MIN_WORDS: usize = 20;

/// Maximum chars of the classified text handed to Laya and stored in the log.
pub const TEXT_MAX_CHARS: usize = 2000;

/// The model tiers the routing ladder climbs: `simple` < `average` <
/// `difficult`.
const TIER_LADDER: [&str; 3] = ["simple", "average", "difficult"];

/// Which of the three model tiers currently has at least one available model.
///
/// A tier without a model has no label to route to: its label is dropped and
/// its keywords fold into a neighbouring label (the kernel
/// [`route_question`] does the folding).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnabledTiers {
    /// The `simple` tier has at least one available model.
    pub simple: bool,
    /// The `average` tier has at least one available model.
    pub average: bool,
    /// The `difficult` tier has at least one available model.
    pub difficult: bool,
}

impl EnabledTiers {
    /// The offered labels: `simple` → `trivial`, `average` → `routine`,
    /// `difficult` → `hard`.
    pub fn offered(self) -> route_question::OfferedLabels {
        route_question::OfferedLabels {
            trivial: self.simple,
            routine: self.average,
            hard: self.difficult,
        }
    }
}

/// The model tier a Laya label routes to: `trivial` → `simple`, `routine` →
/// `average`, `hard` → `difficult`. `None` for a label the ladder does not
/// know, which never clears the gate.
pub fn label_tier(label: &str) -> Option<&'static str> {
    match label {
        route_question::LABEL_TRIVIAL => Some(TIER_LADDER[0]),
        route_question::LABEL_ROUTINE => Some(TIER_LADDER[1]),
        route_question::LABEL_HARD => Some(TIER_LADDER[2]),
        _ => None,
    }
}

/// The question a dispatch asks Laya: the kernel's shared choice question over
/// the offered labels, with un-offered labels folded.
///
/// `None` when fewer than two labels are offered — there is nothing to choose
/// between, so a dispatch skips classification (`skip: "one_tier"` in the
/// log).
pub fn routing_question(enabled: EnabledTiers) -> Option<Question> {
    route_question::route_question(enabled.offered())
}

/// The text a dispatch is classified on: the step's own text (a leading
/// `- [ ] N. ` or `- [x] N. ` checkbox prefix stripped), a blank line, then
/// the lead's brief — capped at [`TEXT_MAX_CHARS`] chars (char-safe).
///
/// The plan title, goal, context, interfaces and acceptance are deliberately
/// absent: they are shared by every step of the plan and carry no per-step
/// signal. `None` when the step and brief together hold fewer than
/// [`MIN_WORDS`] words — the lead's tier stands (`skip: "too_short"`).
pub fn routing_text(step_text: &str, brief: &str) -> Option<String> {
    let step = strip_step_prefix(step_text);
    if word_count(step) + word_count(brief) < MIN_WORDS {
        return None;
    }
    Some(cap_text(&format!("{step}\n\n{brief}")))
}

/// Strip a plan board's `- [ ] N. ` / `- [x] N. ` checkbox prefix, when
/// present. A step stored without the prefix is returned unchanged (modulo
/// leading whitespace).
fn strip_step_prefix(step_text: &str) -> &str {
    let text = step_text.trim_start();
    let Some(rest) = text
        .strip_prefix("- [ ] ")
        .or_else(|| text.strip_prefix("- [x] "))
    else {
        return text;
    };
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits == 0 || !rest[digits..].starts_with(". ") {
        return rest;
    }
    &rest[digits + 2..]
}

/// `text`'s word count, by the same `split_whitespace` rule for every caller
/// (the step, the brief, and Mnemo1's first prompt all count words this way).
fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Cap the classified text at [`TEXT_MAX_CHARS`] chars without splitting a
/// multi-byte character (the same rule as `model_routing::cap_task_text`).
fn cap_text(text: &str) -> String {
    if text.chars().count() > TEXT_MAX_CHARS {
        text.chars().take(TEXT_MAX_CHARS).collect()
    } else {
        text.to_string()
    }
}

/// The confidence gate for a question offering `labels` labels: the
/// three-label gate for 3+ labels, the stricter two-label gate otherwise
/// (`route_question::route_question` never asks with fewer than two).
pub fn threshold_for(labels: usize, cfg: &RoutingSettings) -> f64 {
    if labels >= 3 {
        cfg.min_confidence_3
    } else {
        cfg.min_confidence_2
    }
}

/// One dispatch classification: what Laya answered and whether it clears the
/// gate.
///
/// The answer is carried verbatim even when it selects no tier — a
/// below-threshold or unknown-label pick is exactly the calibration signal the
/// dispatch log exists to collect.
#[derive(Clone, Debug, PartialEq)]
pub struct DispatchRoutingDecision {
    /// The answered label (verbatim; an unknown label is kept as-is).
    pub label: String,
    /// The tier the label routes to: `Some` only for a known label.
    pub tier: Option<&'static str>,
    /// The answer's calibrated confidence (0.0–1.0).
    pub confidence: f64,
    /// The per-option distribution, when the backend reported one.
    pub probabilities: BTreeMap<String, f64>,
    /// True when the answer is a known label at or above the gate: only then
    /// does a dispatch run the worker on Laya's tier.
    pub cleared: bool,
}

/// Classify one dispatch's text ([`routing_text`]'s output, already capped).
///
/// `None` = no usable answer: fewer than two offered labels (nothing to ask),
/// the classifier returned `None` (disabled backend, timeout, malformed
/// response), or a non-choice answer — the caller keeps the lead's tier and
/// logs `skip: "offline"` / `skip: "one_tier"`. `Some(decision)` with
/// `cleared: false` is an answer that did not clear the gate (below the
/// threshold or an unknown label): the caller keeps the lead's tier, but the
/// decision IS logged.
pub async fn classify_dispatch(
    classifier: &dyn Classifier,
    text: &str,
    enabled: EnabledTiers,
    cfg: &RoutingSettings,
) -> Option<DispatchRoutingDecision> {
    let question = routing_question(enabled)?;
    let Answer::Choice {
        label,
        confidence,
        probabilities,
    } = classifier.classify(text, &question).await?
    else {
        return None;
    };
    let tier = label_tier(&label);
    let cleared = tier.is_some() && confidence >= threshold_for(enabled.offered().count(), cfg);
    Some(DispatchRoutingDecision {
        label,
        tier,
        confidence,
        probabilities,
        cleared,
    })
}

/// Mnemo2's `[routing]` settings block (design §4.9): the mode, the two
/// per-label-count gates, the auto-mode fallback tier, the nudge switch, the
/// minimum word count and the optional per-tier default effort.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingSettings {
    /// What dispatch does with Laya's answer (default [`RoutingMode::Off`]).
    pub mode: RoutingMode,
    /// The gate for a three-label question. Provisional until the eval
    /// replaces it (design §4.4).
    pub min_confidence_3: f64,
    /// The gate for a two-label question. Provisional until the eval replaces
    /// it (design §4.4).
    pub min_confidence_2: f64,
    /// The tier `auto` mode runs on when Laya does not clear the gate.
    pub fallback_tier: String,
    /// Whether the strike / drift / review nudges are sent to the lead.
    pub nudges: bool,
    /// Fewest words a dispatch must have to be classified.
    pub min_words: usize,
    /// Optional per-tier default `reasoning_effort`, used only when the lead
    /// passes none.
    pub effort: BTreeMap<String, String>,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self {
            mode: RoutingMode::Off,
            min_confidence_3: route_question::DEFAULT_MIN_CONFIDENCE_3,
            min_confidence_2: route_question::DEFAULT_MIN_CONFIDENCE_2,
            fallback_tier: TIER_LADDER[1].to_string(),
            nudges: true,
            min_words: MIN_WORDS,
            effort: BTreeMap::new(),
        }
    }
}

/// Mnemo2's own settings file, kept separate from Mnemo1's `config.toml`.
pub fn routing_settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("mnemo2.toml")
}

/// Load `[routing]` from Mnemo2's settings, defaulting leniently when the file
/// or table is absent or invalid.
pub fn load_routing_settings(config_dir: &Path) -> RoutingSettings {
    let path = routing_settings_path(config_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            log::warn!(
                "using default Mnemo2 routing settings; cannot read {}: {error}",
                path.display()
            );
            return RoutingSettings::default();
        }
    };
    let document = match toml::from_str::<toml::Value>(&text) {
        Ok(document) => document,
        Err(error) => {
            log::warn!(
                "using default Mnemo2 routing settings; invalid {}: {error}",
                path.display()
            );
            return RoutingSettings::default();
        }
    };
    let Some(value) = document.get("routing").cloned() else {
        return RoutingSettings::default();
    };
    match value.try_into() {
        Ok(settings) => settings,
        Err(error) => {
            log::warn!(
                "using default Mnemo2 routing settings; invalid [routing] in {}: {error}",
                path.display()
            );
            RoutingSettings::default()
        }
    }
}

/// Save `[routing]` without disturbing any other table in `mnemo2.toml`.
pub fn save_routing_settings(
    config_dir: &Path,
    settings: &RoutingSettings,
) -> Result<(), String> {
    let path = routing_settings_path(config_dir);
    let mut document = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => toml::from_str::<toml::Value>(&text)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?,
        Ok(_) => toml::Value::Table(toml::map::Map::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(toml::map::Map::new())
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let table = document
        .as_table_mut()
        .ok_or_else(|| format!("{} must contain a TOML table", path.display()))?;
    let encoded = toml::to_string_pretty(settings)
        .map_err(|error| format!("cannot serialize Mnemo2 routing settings: {error}"))?;
    let routing = toml::from_str::<toml::Value>(&encoded)
        .map_err(|error| format!("cannot encode Mnemo2 routing settings: {error}"))?;
    table.insert("routing".into(), routing);
    let encoded = toml::to_string_pretty(&document)
        .map_err(|error| format!("cannot serialize {}: {error}", path.display()))?;
    std::fs::create_dir_all(config_dir)
        .map_err(|error| format!("cannot create {}: {error}", config_dir.display()))?;
    std::fs::write(&path, encoded)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// What dispatch does with Laya's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMode {
    /// Laya is never asked; the lead's tier is required and runs, as before.
    #[default]
    Off,
    /// Laya is asked and its pick only logged: the lead's tier always runs.
    Shadow,
    /// `tier: "auto"` runs Laya's cleared tier, else the dispatch is refused;
    /// an explicit tier always wins.
    Suggest,
    /// An omitted or `auto` tier runs Laya's cleared tier, else
    /// `fallback_tier`; an explicit tier always wins.
    Auto,
}

/// One row of the dispatch routing log
/// (`~/.mnemo/laya/training/dispatch_routing.jsonl`, design §4.7).
///
/// Every row carries the same envelope — `ts` (unix ms), `event` tag,
/// `worker_id`, `plan_id` and `step` (1-based) — plus the variant's own
/// fields. The enum serializes as a tagged JSON object (`{"event":"route",
/// …}`), so a row is one line in the append-only log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DispatchRoutingRow {
    /// One dispatch's routing decision: what Laya answered, what the lead
    /// passed, what ran, and why the dispatch may have been skipped.
    Route {
        /// Unix milliseconds when the dispatch was classified.
        ts: u64,
        /// The worker the dispatch created.
        worker_id: String,
        /// The plan the step belongs to.
        plan_id: String,
        /// The 1-based step number in the plan.
        step: usize,
        /// The routing mode the dispatch ran under.
        mode: RoutingMode,
        /// The answered label (empty on a skip row).
        label: String,
        /// The tier the label routes to, when the label is known.
        tier: Option<String>,
        /// The answer's calibrated confidence (0.0 when nothing was asked).
        confidence: f64,
        /// The per-option distribution, when the backend reported one.
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
        /// The gate the answer was compared against.
        threshold: f64,
        /// Whether the answer cleared the gate.
        cleared: bool,
        /// The tier the lead passed; `None` when it passed `auto` or omitted
        /// it.
        lead_tier: Option<String>,
        /// The tier the worker actually started on (`None` when no worker
        /// started, e.g. a refused `suggest` dispatch).
        ran_tier: Option<String>,
        /// The model the run tier resolved to.
        ran_model: Option<String>,
        /// Hex SHA-256 of the classified text — joins the row to a
        /// fine-tuning corpus without storing the text twice.
        text_sha256: String,
        /// The classified text, capped at [`TEXT_MAX_CHARS`].
        text: String,
        /// Why classification was skipped: `None` (classified), or one of
        /// `too_short` / `one_tier` / `offline` / `audit`.
        skip: Option<String>,
    },
    /// A lead passed an explicit tier or model that disagrees with a decision
    /// Laya cleared on a different tier.
    LeadOverride {
        /// Unix milliseconds when the override was logged.
        ts: u64,
        /// The worker the dispatch created.
        worker_id: String,
        /// The plan the step belongs to.
        plan_id: String,
        /// The 1-based step number in the plan.
        step: usize,
        /// The tier Laya cleared on (`None` when it cleared nothing).
        laya_tier: Option<String>,
        /// The tier the lead's model actually ran on.
        lead_tier: String,
        /// The requested tier, when it differed from the ran tier.
        requested_tier: Option<String>,
        /// The confidence of the overridden answer.
        confidence: f64,
    },
    /// `team_resume` (or a lead `change_model`) moved a worker to another
    /// tier.
    LeadReroute {
        /// Unix milliseconds when the reroute was logged.
        ts: u64,
        /// The rerouted worker.
        worker_id: String,
        /// The plan the step belongs to.
        plan_id: String,
        /// The 1-based step number in the plan.
        step: usize,
        /// The tier the worker was on.
        from_tier: String,
        /// The tier the worker moved to.
        to_tier: String,
        /// The model the worker was on, when known.
        from_model: Option<String>,
        /// The model the worker moved to, when known.
        to_model: Option<String>,
        /// The worker's status when the reroute happened.
        status: String,
        /// Review rounds the worker had at the time.
        review_rounds: u32,
        /// Fix rounds the worker had at the time.
        fix_rounds: u32,
        /// Whether a Laya nudge preceded the reroute.
        nudged: bool,
    },
    /// A mission-control `change_model` moved a worker to another tier.
    UserReroute {
        /// Unix milliseconds when the reroute was logged.
        ts: u64,
        /// The rerouted worker.
        worker_id: String,
        /// The plan the step belongs to.
        plan_id: String,
        /// The 1-based step number in the plan.
        step: usize,
        /// The tier the worker was on.
        from_tier: String,
        /// The tier the worker moved to.
        to_tier: String,
        /// The model the worker was on, when known.
        from_model: Option<String>,
        /// The model the worker moved to, when known.
        to_model: Option<String>,
        /// The worker's status when the reroute happened.
        status: String,
        /// Review rounds the worker had at the time.
        review_rounds: u32,
        /// Fix rounds the worker had at the time.
        fix_rounds: u32,
        /// Whether a Laya nudge preceded the reroute.
        nudged: bool,
    },
    /// Laya suggested a tier change to the lead (never applied automatically).
    Nudge {
        /// Unix milliseconds when the nudge was sent.
        ts: u64,
        /// The nudged worker.
        worker_id: String,
        /// The plan the step belongs to.
        plan_id: String,
        /// The 1-based step number in the plan.
        step: usize,
        /// Why the nudge fired: `strikes`, `circles`, `drift` or `review`.
        reason: String,
        /// The tier the worker was on.
        from_tier: String,
        /// The tier the nudge suggested.
        to_tier: String,
        /// The human-facing detail line.
        detail: String,
    },
    /// A nudged worker reached a terminal status on the tier it was nudged
    /// from — the lead did not act on the nudge.
    NudgeIgnored {
        /// Unix milliseconds when the outcome was observed.
        ts: u64,
        /// The worker that was nudged.
        worker_id: String,
        /// The plan the step belongs to.
        plan_id: String,
        /// The 1-based step number in the plan.
        step: usize,
        /// The tier the worker finished on.
        tier: String,
    },
    /// One implementation worker reached a terminal status: the outcome half
    /// of the corpus.
    Outcome {
        /// Unix milliseconds when the worker reached the status.
        ts: u64,
        /// The worker that finished.
        worker_id: String,
        /// The plan the step belongs to.
        plan_id: String,
        /// The 1-based step number in the plan.
        step: usize,
        /// The terminal status: `merged`, `failed`, `stopped` or `abandoned`.
        status: String,
        /// The tier the worker finished on.
        final_tier: String,
        /// Every model the worker ran on, oldest first.
        models_used: Vec<String>,
        /// Review rounds the worker went through.
        review_rounds: u32,
        /// Fix rounds the worker went through.
        fix_rounds: u32,
        /// Findings the reviews recorded.
        findings: usize,
        /// Findings the reviews recorded as high.
        findings_high: usize,
        /// Audits the worker went through.
        audits: usize,
        /// The worker's recorded cost, in micro-units of the billing currency.
        cost_micros: u64,
        /// The derived training label ([`outcome_label`]), absent for a
        /// worker that did not merge.
        label: Option<String>,
    },
}

impl DispatchRoutingRow {
    /// Replace the envelope's worker id with the provisioned worker's real id.
    pub(crate) fn with_worker_id(&mut self, id: &str) {
        match self {
            Self::Route { worker_id, .. }
            | Self::LeadOverride { worker_id, .. }
            | Self::LeadReroute { worker_id, .. }
            | Self::UserReroute { worker_id, .. }
            | Self::Nudge { worker_id, .. }
            | Self::NudgeIgnored { worker_id, .. }
            | Self::Outcome { worker_id, .. } => *worker_id = id.to_string(),
        }
    }
}

/// The dispatch routing log's file:
/// `~/.mnemo/laya/training/dispatch_routing.jsonl`, beside the Mnemo1
/// `routing.jsonl`.
pub fn dispatch_routing_log_path() -> PathBuf {
    training_log_dir().join("dispatch_routing.jsonl")
}

/// The tier's rank on the ladder (`simple` 0, `average` 1, `difficult` 2), or
/// `None` for a string the ladder does not know.
fn tier_rank(tier: &str) -> Option<usize> {
    TIER_LADDER.iter().position(|candidate| *candidate == tier)
}

/// The destination rank of the last tier-changing reroute in `history`
/// (`lead_reroute` or `user_reroute`), if any.
///
/// A reroute that left the worker on the same tier (a model-only change) is
/// not a tier reroute and is skipped. Both directions of a tier-changing
/// reroute contribute their DESTINATION: an up-reroute proves the harder tier
/// was needed, a down-reroute proves the cheaper one was enough (design §4.8
/// rules 2 and 3).
fn last_reroute_rank(history: &[DispatchRoutingRow]) -> Option<usize> {
    for row in history.iter().rev() {
        let (from_tier, to_tier) = match row {
            DispatchRoutingRow::LeadReroute {
                from_tier, to_tier, ..
            }
            | DispatchRoutingRow::UserReroute {
                from_tier, to_tier, ..
            } => (from_tier, to_tier),
            _ => continue,
        };
        let (Some(from_rank), Some(to_rank)) = (tier_rank(from_tier), tier_rank(to_tier)) else {
            continue;
        };
        if from_rank != to_rank {
            return Some(to_rank);
        }
    }
    None
}

/// The tier a merged step SHOULD have run on, derived from its routing history
/// and outcome (design §4.8), plus whether the label is weak:
///
/// 1. Merged on tier T with at most one fix round, no high finding and no
///    reroute gives **T** — weak unless T is `simple`, because the lead chose
///    it and nothing proves a cheaper tier would have failed.
/// 2. Merged after a reroute UP to U gives **U**, strong.
/// 3. Merged after a reroute DOWN to D gives **D**, strong (the cheaper tier
///    sufficed).
/// 4. Merged on T with two or more fix rounds or a high finding, and no
///    reroute, gives the tier above T (capped at `difficult`), weak.
/// 5. Failed, stopped or abandoned gives no label.
///
/// `None` also when `final_tier` is not a ladder tier — there is no label to
/// derive without one.
pub fn outcome_label(
    history: &[DispatchRoutingRow],
    final_status: &str,
    final_tier: &str,
    fix_rounds: u32,
    high_findings: usize,
) -> Option<(&'static str, bool)> {
    if final_status != "merged" {
        return None;
    }
    let rank = tier_rank(final_tier)?;
    if let Some(to_rank) = last_reroute_rank(history) {
        return Some((TIER_LADDER[to_rank], false));
    }
    if fix_rounds <= 1 && high_findings == 0 {
        return Some((TIER_LADDER[rank], rank != 0));
    }
    Some((TIER_LADDER[(rank + 1).min(TIER_LADDER.len() - 1)], true))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;

    /// A classifier with a canned answer that records what it was asked --
    /// `None` models every no-answer path (disabled backend, timeout,
    /// malformed response).
    struct StubClassifier {
        answer: Option<Answer>,
        calls: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl StubClassifier {
        fn new(answer: Option<Answer>) -> Self {
            Self {
                answer,
                calls: Arc::new(AtomicUsize::new(0)),
                seen: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().expect("seen lock").clone()
        }
    }

    #[async_trait]
    impl Classifier for StubClassifier {
        async fn classify(&self, state: &str, _question: &Question) -> Option<Answer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.seen.lock().expect("seen lock").push(state.to_string());
            self.answer.clone()
        }
    }

    fn choice(label: &str, confidence: f64) -> Option<Answer> {
        Some(Answer::Choice {
            label: label.to_string(),
            confidence,
            probabilities: BTreeMap::new(),
        })
    }

    fn enabled(simple: bool, average: bool, difficult: bool) -> EnabledTiers {
        EnabledTiers {
            simple,
            average,
            difficult,
        }
    }

    fn all_enabled() -> EnabledTiers {
        enabled(true, true, true)
    }

    #[test]
    fn missing_mnemo2_toml_gives_defaults() {
        let config = tempfile::tempdir().expect("config dir");
        assert_eq!(
            load_routing_settings(config.path()),
            RoutingSettings::default()
        );
        assert!(!routing_settings_path(config.path()).exists());
    }

    #[test]
    fn routing_settings_round_trip_and_keep_other_tables() {
        let config = tempfile::tempdir().expect("config dir");
        let path = routing_settings_path(config.path());
        assert_eq!(path, config.path().join("mnemo2.toml"));
        std::fs::write(&path, "[ui]\ncompact = true\n").expect("existing settings");
        let settings = RoutingSettings {
            mode: RoutingMode::Auto,
            min_confidence_3: 0.51,
            fallback_tier: "simple".into(),
            effort: BTreeMap::from([("simple".into(), "low".into())]),
            ..RoutingSettings::default()
        };
        save_routing_settings(config.path(), &settings).expect("save routing settings");
        assert_eq!(load_routing_settings(config.path()), settings);
        let saved: toml::Value = toml::from_str(
            &std::fs::read_to_string(path).expect("read saved settings"),
        )
        .expect("valid TOML");
        assert_eq!(saved["ui"]["compact"], toml::Value::Boolean(true));
    }

    #[test]
    fn partial_routing_table_fills_defaults() {
        let config = tempfile::tempdir().expect("config dir");
        std::fs::write(
            routing_settings_path(config.path()),
            "[routing]\nmode = \"shadow\"\n",
        )
        .expect("partial settings");
        assert_eq!(
            load_routing_settings(config.path()),
            RoutingSettings {
                mode: RoutingMode::Shadow,
                ..RoutingSettings::default()
            }
        );
    }

    #[test]
    fn enabled_tiers_map_to_offered_labels() {
        for bits in 0..8 {
            let tiers = enabled(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            assert_eq!(
                tiers.offered(),
                route_question::OfferedLabels {
                    trivial: tiers.simple,
                    routine: tiers.average,
                    hard: tiers.difficult,
                },
                "bits: {bits}"
            );
            assert_eq!(
                tiers.offered().count(),
                usize::from(tiers.simple)
                    + usize::from(tiers.average)
                    + usize::from(tiers.difficult),
                "bits: {bits}"
            );
        }
    }

    #[test]
    fn routing_question_uses_the_kernel_question() {
        for bits in 0..8 {
            let tiers = enabled(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            let ours = routing_question(tiers);
            let kernel = route_question::route_question(tiers.offered());
            assert_eq!(ours, kernel, "bits: {bits}");
            // Byte for byte, not just semantically equal: Mnemo1 and Mnemo2
            // must send the backend identical bytes.
            assert_eq!(
                serde_json::to_string(&ours).expect("ours"),
                serde_json::to_string(&kernel).expect("kernel"),
                "bits: {bits}"
            );
        }
    }

    #[tokio::test]
    async fn one_enabled_tier_asks_nothing() {
        for tiers in [
            enabled(true, false, false),
            enabled(false, true, false),
            enabled(false, false, true),
        ] {
            assert!(routing_question(tiers).is_none(), "{tiers:?}");
            let stub = StubClassifier::new(choice(route_question::LABEL_ROUTINE, 0.99));
            let decision =
                classify_dispatch(&stub, "some text", tiers, &RoutingSettings::default()).await;
            assert!(decision.is_none(), "{tiers:?}");
            assert_eq!(stub.calls(), 0, "{tiers:?}");
        }
    }

    #[test]
    fn routing_text_strips_the_checkbox_and_caps_at_2000_chars() {
        let step = "- [ ] 7. Add the dispatch routing module with its rows and tests and docs";
        let brief = "The step spec, the gate, the log rows and the outcome label all land here";
        let text = routing_text(step, brief).expect("long enough");
        let first_line = "Add the dispatch routing module with its rows and tests and docs";
        assert!(text.starts_with(first_line));
        assert!(text.ends_with(brief));
        // A blank line (two newline characters) separates the step from the
        // brief.
        assert_eq!(text.len(), first_line.len() + 2 + brief.len());

        let done = "- [x] 12. Close the plan after the verification pass and the review report";
        let text = routing_text(done, brief).expect("long enough");
        assert!(text.starts_with("Close the plan after the verification pass"));

        let step = format!("- [ ] 3. {}", "é ".repeat(1500));
        let text = routing_text(&step, "").expect("long enough");
        assert_eq!(text.chars().count(), TEXT_MAX_CHARS);
        // Char-safe: the é bytes survive the cap whole (2 bytes each).
        assert!(text.len() > TEXT_MAX_CHARS);
        assert!(text.starts_with('é'));
    }

    #[test]
    fn short_step_is_not_classified() {
        let nineteen: Vec<String> = (1..=19).map(|word| format!("w{word}")).collect();
        let step = format!("- [ ] 1. {}", nineteen.join(" "));
        assert!(routing_text(&step, "").is_none());

        let twenty: Vec<String> = (1..=20).map(|word| format!("w{word}")).collect();
        let step = format!("- [ ] 1. {}", twenty.join(" "));
        assert!(routing_text(&step, "").is_some());

        // The brief's words count too: 15 + 5 reaches the 20-word floor.
        let fifteen = twenty[..15].join(" ");
        let brief = twenty[15..].join(" ");
        assert!(routing_text(&fifteen, &brief).is_some());
    }

    #[tokio::test]
    async fn confidence_at_the_threshold_clears() {
        let cfg = RoutingSettings::default();
        let tiers = all_enabled();
        assert_eq!(
            threshold_for(3, &cfg),
            route_question::DEFAULT_MIN_CONFIDENCE_3
        );
        assert_eq!(
            threshold_for(2, &cfg),
            route_question::DEFAULT_MIN_CONFIDENCE_2
        );

        let at = StubClassifier::new(choice(
            route_question::LABEL_ROUTINE,
            route_question::DEFAULT_MIN_CONFIDENCE_3,
        ));
        let decision = classify_dispatch(&at, "text", tiers, &cfg)
            .await
            .expect("decision");
        assert_eq!(decision.tier, Some("average"));
        assert!(decision.cleared);

        let below = StubClassifier::new(choice(
            route_question::LABEL_ROUTINE,
            route_question::DEFAULT_MIN_CONFIDENCE_3 - 0.001,
        ));
        let decision = classify_dispatch(&below, "text", tiers, &cfg)
            .await
            .expect("decision");
        assert_eq!(decision.tier, Some("average"));
        assert!(!decision.cleared);

        // The two-label gate is stricter: the same 0.60 answer clears there
        // only at the threshold itself.
        let two_labels = enabled(true, true, false);
        let at = StubClassifier::new(choice(
            route_question::LABEL_ROUTINE,
            route_question::DEFAULT_MIN_CONFIDENCE_2,
        ));
        let decision = classify_dispatch(&at, "text", two_labels, &cfg)
            .await
            .expect("decision");
        assert!(decision.cleared);

        let below = StubClassifier::new(choice(
            route_question::LABEL_ROUTINE,
            route_question::DEFAULT_MIN_CONFIDENCE_2 - 0.001,
        ));
        let decision = classify_dispatch(&below, "text", two_labels, &cfg)
            .await
            .expect("decision");
        assert!(!decision.cleared);
    }

    #[tokio::test]
    async fn unknown_label_never_clears() {
        assert_eq!(label_tier("medium"), None);
        let odd = StubClassifier::new(choice("medium", 0.99));
        let decision = classify_dispatch(&odd, "text", all_enabled(), &RoutingSettings::default())
            .await
            .expect("decision");
        assert_eq!(decision.label, "medium");
        assert_eq!(decision.tier, None);
        assert!(!decision.cleared);
    }

    #[tokio::test]
    async fn no_answer_is_none() {
        let none = StubClassifier::new(None);
        assert!(
            classify_dispatch(&none, "text", all_enabled(), &RoutingSettings::default())
                .await
                .is_none()
        );

        let scored = StubClassifier::new(Some(Answer::Score {
            value: 1.0,
            confidence: 0.99,
        }));
        assert!(
            classify_dispatch(&scored, "text", all_enabled(), &RoutingSettings::default())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn classify_dispatch_hands_the_capped_text_to_the_classifier() {
        let stub = StubClassifier::new(choice(route_question::LABEL_HARD, 0.9));
        let step = format!("- [ ] 2. {}", "word ".repeat(1000));
        let text = routing_text(&step, "a brief").expect("long enough");
        assert_eq!(text.chars().count(), TEXT_MAX_CHARS);
        classify_dispatch(&stub, &text, all_enabled(), &RoutingSettings::default())
            .await
            .expect("decision");
        assert_eq!(stub.seen(), vec![text]);
    }

    fn route_row() -> DispatchRoutingRow {
        let mut probabilities = BTreeMap::new();
        probabilities.insert(route_question::LABEL_ROUTINE.to_string(), 0.62);
        probabilities.insert(route_question::LABEL_HARD.to_string(), 0.38);
        DispatchRoutingRow::Route {
            ts: 1_700_000_000_000,
            worker_id: "worker-1".into(),
            plan_id: "2ba2168a".into(),
            step: 3,
            mode: RoutingMode::Suggest,
            label: route_question::LABEL_ROUTINE.into(),
            tier: Some("average".into()),
            confidence: 0.62,
            probabilities,
            threshold: route_question::DEFAULT_MIN_CONFIDENCE_3,
            cleared: true,
            lead_tier: Some("auto".into()),
            ran_tier: Some("average".into()),
            ran_model: Some("endpoint/model-a".into()),
            text_sha256: "0".repeat(64),
            text: "a dispatched step".into(),
            skip: None,
        }
    }

    fn reroute_row() -> DispatchRoutingRow {
        DispatchRoutingRow::LeadReroute {
            ts: 1_700_000_000_001,
            worker_id: "worker-1".into(),
            plan_id: "2ba2168a".into(),
            step: 3,
            from_tier: "average".into(),
            to_tier: "difficult".into(),
            from_model: Some("endpoint/model-a".into()),
            to_model: Some("endpoint/model-b".into()),
            status: "running".into(),
            review_rounds: 1,
            fix_rounds: 1,
            nudged: true,
        }
    }

    fn rows() -> Vec<DispatchRoutingRow> {
        vec![
            route_row(),
            DispatchRoutingRow::LeadOverride {
                ts: 1_700_000_000_002,
                worker_id: "worker-2".into(),
                plan_id: "2ba2168a".into(),
                step: 4,
                laya_tier: Some("average".into()),
                lead_tier: "difficult".into(),
                requested_tier: None,
                confidence: 0.62,
            },
            reroute_row(),
            DispatchRoutingRow::UserReroute {
                ts: 1_700_000_000_003,
                worker_id: "worker-2".into(),
                plan_id: "2ba2168a".into(),
                step: 4,
                from_tier: "difficult".into(),
                to_tier: "simple".into(),
                from_model: Some("endpoint/model-b".into()),
                to_model: Some("endpoint/model-c".into()),
                status: "running".into(),
                review_rounds: 0,
                fix_rounds: 2,
                nudged: false,
            },
            DispatchRoutingRow::Nudge {
                ts: 1_700_000_000_004,
                worker_id: "worker-3".into(),
                plan_id: "2ba2168a".into(),
                step: 5,
                reason: "strikes".into(),
                from_tier: "average".into(),
                to_tier: "difficult".into(),
                detail: "3 failing tool calls in a row".into(),
            },
            DispatchRoutingRow::NudgeIgnored {
                ts: 1_700_000_000_005,
                worker_id: "worker-3".into(),
                plan_id: "2ba2168a".into(),
                step: 5,
                tier: "average".into(),
            },
            DispatchRoutingRow::Outcome {
                ts: 1_700_000_000_006,
                worker_id: "worker-1".into(),
                plan_id: "2ba2168a".into(),
                step: 3,
                status: "merged".into(),
                final_tier: "difficult".into(),
                models_used: vec!["endpoint/model-a".into(), "endpoint/model-b".into()],
                review_rounds: 1,
                fix_rounds: 1,
                findings: 2,
                findings_high: 0,
                audits: 0,
                cost_micros: 12_345,
                label: Some("difficult".into()),
            },
        ]
    }

    #[test]
    fn rows_round_trip_as_tagged_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        let rows = rows();
        for row in &rows {
            mnemo::agent::model_routing::append_row(&path, row);
        }

        let text = std::fs::read_to_string(&path).expect("log");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), rows.len());
        let tags = [
            "route",
            "lead_override",
            "lead_reroute",
            "user_reroute",
            "nudge",
            "nudge_ignored",
            "outcome",
        ];
        for ((line, expected), tag) in lines.iter().zip(&rows).zip(tags) {
            let value: serde_json::Value = serde_json::from_str(line).expect("value");
            assert_eq!(value["event"], tag);
            let parsed: DispatchRoutingRow = serde_json::from_str(line).expect("row");
            assert_eq!(&parsed, expected, "event: {tag}");
        }
    }

    /// A history holding one reroute from `from` to `to`.
    fn reroute_history(from: &str, to: &str) -> Vec<DispatchRoutingRow> {
        let mut history = vec![route_row()];
        history.push(DispatchRoutingRow::LeadReroute {
            ts: 1_700_000_000_001,
            worker_id: "worker-1".into(),
            plan_id: "2ba2168a".into(),
            step: 3,
            from_tier: from.into(),
            to_tier: to.into(),
            from_model: None,
            to_model: None,
            status: "running".into(),
            review_rounds: 0,
            fix_rounds: 0,
            nudged: false,
        });
        history
    }

    #[test]
    fn outcome_label_rules() {
        let no_reroute = vec![route_row()];

        // Rule 1: merged on T, at most one fix round, no high finding, no
        // reroute gives T — weak unless T is simple.
        assert_eq!(
            outcome_label(&no_reroute, "merged", "simple", 1, 0),
            Some(("simple", false))
        );
        assert_eq!(
            outcome_label(&no_reroute, "merged", "average", 1, 0),
            Some(("average", true))
        );
        assert_eq!(
            outcome_label(&no_reroute, "merged", "difficult", 0, 0),
            Some(("difficult", true))
        );

        // Rule 2: merged after a reroute UP to U gives U, strong.
        assert_eq!(
            outcome_label(
                &reroute_history("average", "difficult"),
                "merged",
                "difficult",
                0,
                0
            ),
            Some(("difficult", false))
        );

        // Rule 3: merged after a reroute DOWN to D gives D, strong.
        assert_eq!(
            outcome_label(
                &reroute_history("difficult", "simple"),
                "merged",
                "simple",
                3,
                2
            ),
            Some(("simple", false))
        );

        // A model-only reroute (same tier) is not a tier reroute: rules 1 and
        // 4 still decide.
        assert_eq!(
            outcome_label(
                &reroute_history("average", "average"),
                "merged",
                "average",
                0,
                0
            ),
            Some(("average", true))
        );

        // Rule 4: merged on T with two or more fix rounds or a high finding,
        // and no reroute, gives the tier above T — weak, capped at difficult.
        assert_eq!(
            outcome_label(&no_reroute, "merged", "simple", 2, 0),
            Some(("average", true))
        );
        assert_eq!(
            outcome_label(&no_reroute, "merged", "average", 0, 1),
            Some(("difficult", true))
        );
        assert_eq!(
            outcome_label(&no_reroute, "merged", "difficult", 2, 1),
            Some(("difficult", true))
        );

        // Rule 5: failed, stopped or abandoned gives no label.
        for status in ["failed", "stopped", "abandoned"] {
            assert_eq!(
                outcome_label(&no_reroute, status, "average", 0, 0),
                None,
                "status: {status}"
            );
        }

        // A tier outside the ladder has no label to derive.
        assert_eq!(outcome_label(&no_reroute, "merged", "unknown", 0, 0), None);
    }
}
