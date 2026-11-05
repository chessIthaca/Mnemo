// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Mnemo2's dispatch-time Laya routing decision (plan 2ba2168a, design
//! `.coding/design/mnemo2-laya-routing.md` §4.2–§4.8).
//!
//! At `team_dispatch` — before a worker is provisioned — Mnemo2 asks Laya one
//! question about the worker's step spec and the lead's brief: `trivial`,
//! `routine`, `involved` or `hard`, which map onto the ladder's `trivial`,
//! `routine`, `medium` and `high` tiers. The question, its descriptions and
//! the default gates all come from the kernel's shared [`route_question`]
//! module, so Mnemo1's first-prompt gate asks the byte-identical question.
//! Empty tiers remain offered and fold down to a model when the dispatch runs.
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
use std::time::Duration;

use serde::{Deserialize, Serialize};

use mnemo::agent::failure_triage::training_log_dir;
use mnemo::agent::route_knn::RouteKnn;
use mnemo::agent::route_question;
use mnemo::config::endpoints::TierLadder;
use mnemo::memory::classifier::{Answer, Classifier, Question};

/// Fewest words (step text plus brief) a dispatch must have to be classified.
/// A shorter spec is not a better routing signal than the lead's own tier.
pub const MIN_WORDS: usize = 20;

/// The most time one dispatch gives the kNN overlay before falling back to
/// Laya. The overlay's index build runs in the background and is never awaited;
/// this sub-budget bounds only the query embedding/vote, leaving Laya at least
/// 2.5 s of the outer 3 s classification timeout.
const KNN_OVERLAY_BUDGET: Duration = Duration::from_millis(500);

/// Maximum chars of the classified text handed to Laya and stored in the log.
pub const TEXT_MAX_CHARS: usize = 2000;

/// The fixed ladder ids in cheapest-first order. `escalate` is included for
/// reroute comparisons, but is never offered to Laya or the kNN overlay.
const TIER_LADDER: [&str; 5] = ["trivial", "routine", "medium", "high", "escalate"];

/// Which non-escalate tiers resolve to an available model, directly or by
/// folding down. All four labels remain independent: an empty `medium` tier is
/// still offered as `involved` and resolves to `routine` at run time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnabledTiers {
    /// The `trivial` tier resolves to an available model.
    pub trivial: bool,
    /// The `routine` tier resolves to an available model.
    pub routine: bool,
    /// The `medium` tier resolves to an available model.
    pub medium: bool,
    /// The `high` tier resolves to an available model.
    pub high: bool,
    /// The escalation-only rung has a model. Never offered to Laya or kNN;
    /// this is used only by the deterministic nudge ladder.
    pub escalate: bool,
}

impl EnabledTiers {
    /// The four neutral labels offered to the shared routing question.
    pub fn offered(self) -> route_question::OfferedLabels {
        route_question::OfferedLabels {
            trivial: self.trivial,
            routine: self.routine,
            involved: self.medium,
            hard: self.high,
        }
    }
}

/// The ladder tier a Laya label routes to. Unknown labels never clear the
/// confidence gate.
pub fn label_tier(label: &str) -> Option<&'static str> {
    route_question::tier_for_label(label)
}

/// The Laya label for a ladder tier, used when a kNN tier vote is recorded in
/// the Laya-shaped decision fields.
fn label_for_tier(tier: &str) -> Option<&'static str> {
    route_question::label_for_tier(tier)
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
/// four-label gate for the full ladder, the three-label gate for three,
/// the stricter two-label gate otherwise (`route_question::route_question`
/// never asks with fewer than two).
pub fn threshold_for(labels: usize, cfg: &RoutingSettings) -> f64 {
    if labels >= 4 {
        cfg.min_confidence_4
    } else if labels >= 3 {
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
    /// The confidence the gate reads (0.0–1.0): the top option's probability
    /// when the backend reported a distribution, else the pick's confidence
    /// (`route_question::answer_confidence`).
    pub confidence: f64,
    /// The per-option distribution, when the backend reported one.
    pub probabilities: BTreeMap<String, f64>,
    /// True when the answer is a known label at or above the gate: only then
    /// does a dispatch run the worker on Laya's tier.
    pub cleared: bool,
    /// Where the decision came from: `"knn"` when the log overlay answered,
    /// `"laya"` when the backend did.
    pub source: &'static str,
    /// The kNN vote's folded label, when the overlay answered.
    pub knn_label: Option<String>,
    /// The kNN vote's share, when the overlay answered.
    pub knn_share: Option<f64>,
    /// How many neighbours voted in the kNN answer.
    pub knn_k: Option<usize>,
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
    knn: Option<&RouteKnn>,
    text: &str,
    enabled: EnabledTiers,
    cfg: &RoutingSettings,
) -> Option<DispatchRoutingDecision> {
    let question = routing_question(enabled)?;
    // The labelled dispatch log answers first once it has learned enough. An
    // overlay vote that clears `knn_min_share` is the decision; a cold index,
    // an under-gate vote or an embedder failure leaves Laya to answer.
    if let Some(knn) = knn {
        // The background build is kicked off inside `classify`; this budget
        // bounds only the query embed and vote, so a slow overlay can never
        // eat Laya's answer.
        let vote = tokio::time::timeout(
            KNN_OVERLAY_BUDGET,
            knn.classify(text, enabled.offered(), cfg.knn_min_rows),
        )
        .await
        .ok()
        .flatten();
        if let Some(vote) = vote.filter(|vote| vote.share >= cfg.knn_min_share) {
            if let Some(label) = label_for_tier(&vote.tier) {
                let probabilities = vote
                    .probabilities
                    .iter()
                    .filter_map(|(tier, share)| {
                        label_for_tier(tier).map(|label| (label.to_string(), *share))
                    })
                    .collect();
                return Some(DispatchRoutingDecision {
                    label: label.to_string(),
                    tier: label_tier(label),
                    confidence: vote.share,
                    probabilities,
                    cleared: true,
                    source: "knn",
                    knn_label: Some(label.to_string()),
                    knn_share: Some(vote.share),
                    knn_k: Some(vote.k),
                });
            }
        }
    }
    let answer = classifier.classify(text, &question).await?;
    let Answer::Choice { label, probabilities, .. } = &answer else {
        return None;
    };
    // The gate is calibrated on the top option's probability
    // (`route_question::answer_confidence`), not the pick's lower,
    // margin-like `confidence`.
    let confidence = route_question::answer_confidence(&answer);
    let tier = label_tier(label);
    let cleared = tier.is_some() && confidence >= threshold_for(enabled.offered().count(), cfg);
    Some(DispatchRoutingDecision {
        label: label.clone(),
        tier,
        confidence,
        probabilities: probabilities.clone(),
        cleared,
        source: "laya",
        knn_label: None,
        knn_share: None,
        knn_k: None,
    })
}

/// Mnemo2's `[routing]` settings block (design §4.9): the mode, the three
/// per-label-count gates, the auto-mode fallback tier, the nudge switch and
/// the minimum word count.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingSettings {
    /// What dispatch does with Laya's answer (default [`RoutingMode::Off`]).
    pub mode: RoutingMode,
    /// The gate for the four-label question the full ladder asks. Provisional
    /// until the eval replaces it (design §4.4).
    pub min_confidence_4: f64,
    /// The gate for a three-label question. Provisional until the eval
    /// replaces it (design §4.4).
    pub min_confidence_3: f64,
    /// The gate for a two-label question. Provisional until the eval replaces
    /// it (design §4.4).
    pub min_confidence_2: f64,
    /// The kNN overlay answers only when its winning tier holds at least this
    /// share of the K nearest votes (default 0.80).
    pub knn_min_share: f64,
    /// The kNN overlay answers only after the labelled log has at least this
    /// many indexed workers (default 30).
    pub knn_min_rows: usize,
    /// The tier `auto` mode runs on when Laya does not clear the gate.
    pub fallback_tier: String,
    /// Whether the strike / drift / review nudges are sent to the lead.
    pub nudges: bool,
    /// Fewest words a dispatch must have to be classified.
    pub min_words: usize,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self {
            mode: RoutingMode::Off,
            min_confidence_4: route_question::DEFAULT_MIN_CONFIDENCE_4,
            min_confidence_3: route_question::DEFAULT_MIN_CONFIDENCE_3,
            min_confidence_2: route_question::DEFAULT_MIN_CONFIDENCE_2,
            knn_min_share: 0.80,
            knn_min_rows: 30,
            fallback_tier: "routine".to_string(),
            nudges: true,
            min_words: MIN_WORDS,
        }
    }
}

/// Mnemo2's own settings file, kept separate from Mnemo1's `config.toml`.
pub fn routing_settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("mnemo2.toml")
}

/// Load `[routing]` from Mnemo2's settings, defaulting leniently when the
/// file or table is absent or invalid. Retired tier ids convert to their
/// ladder ids, and a table that converted and loaded cleanly is written back
/// to the file once, so the stored ids and the app agree (a failed
/// write-back only logs; the converted settings still load).
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
    let Some(mut routing) = document.get("routing").cloned() else {
        return RoutingSettings::default();
    };
    let converted = convert_retired_tier_ids(&mut routing);
    match routing.clone().try_into() {
        Ok(settings) => {
            if converted {
                if let Err(error) = save_converted_routing_table(config_dir, &routing) {
                    log::warn!(
                        "using converted Mnemo2 routing settings; cannot rewrite {}: {error}",
                        path.display()
                    );
                }
            }
            settings
        }
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

/// Rewrite retired tier ids in the raw `[routing]` table so the file and the
/// app agree (plan 5310e9bc): the `fallback_tier` value and the keys of the
/// vestigial `[routing.effort]` table — plan 87932c46 moved per-tier effort
/// onto the ladder rungs, but older installs still carry the retired keys.
/// An effort entry whose ladder id is already keyed keeps its current value;
/// the retired alias is stale and drops. Returns whether anything changed,
/// so [`load_routing_settings`] rewrites the file only when it did.
fn convert_retired_tier_ids(routing: &mut toml::Value) -> bool {
    let Some(table) = routing.as_table_mut() else {
        return false;
    };
    let mut converted = false;
    if let Some(toml::Value::String(tier)) = table.get_mut("fallback_tier") {
        if let Some(ladder_id) = TierLadder::convert_old_id(tier.as_str()) {
            *tier = ladder_id.to_string();
            converted = true;
        }
    }
    if let Some(toml::Value::Table(effort)) = table.get_mut("effort") {
        let retired: Vec<(String, &'static str)> = effort
            .keys()
            .filter_map(|key| TierLadder::convert_old_id(key).map(|id| (key.clone(), id)))
            .collect();
        for (key, ladder_id) in retired {
            let stale = effort.remove(&key);
            // Removing the alias alone is a change: the retired id leaves
            // the file even when the ladder key already carries a value.
            converted = true;
            if effort.contains_key(ladder_id) {
                continue;
            }
            if let Some(value) = stale {
                effort.insert(ladder_id.to_string(), value);
            }
        }
    }
    converted
}

/// Write a converted raw `[routing]` table back to `mnemo2.toml` without
/// disturbing any other table — the read-modify-write discipline
/// [`save_routing_settings`] established — so retired tier ids a load
/// converted do not linger on disk. Unlike [`save_routing_settings`] this
/// takes the raw table, not [`RoutingSettings`]: keys the struct no longer
/// knows, like the vestigial `[routing.effort]`, survive the rewrite with
/// their ids converted.
fn save_converted_routing_table(config_dir: &Path, routing: &toml::Value) -> Result<(), String> {
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
    table.insert("routing".into(), routing.clone());
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
        /// The gated confidence — the top option's probability when the
        /// backend reported a distribution, else the pick's confidence
        /// (0.0 when nothing was asked).
        confidence: f64,
        /// The per-option distribution, when the backend reported one.
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
        /// The gate the answer was compared against: Laya's confidence gate,
        /// or `knn_min_share` when the overlay answered.
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
        /// Where the decision came from: `"knn"` when the labelled log
        /// overlay answered, `"laya"` when the backend did. Absent on skipped
        /// rows and on rows written before the overlay existed.
        #[serde(default)]
        source: Option<String>,
        /// The kNN vote's folded label, when the overlay answered.
        #[serde(default)]
        knn_label: Option<String>,
        /// The kNN vote's share, when the overlay answered.
        #[serde(default)]
        knn_share: Option<f64>,
        /// How many neighbours voted in the kNN answer.
        #[serde(default)]
        knn_k: Option<usize>,
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

/// The tier's rank on the fixed five-tier ladder, or `None` for an unknown id.
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
            return (to_rank < 4).then_some(to_rank);
        }
    }
    None
}

/// The tier a merged step SHOULD have run on, derived from its routing history
/// and outcome (design §4.8), plus whether the label is weak:
///
/// 1. Merged on tier T with at most one fix round, no high finding and no
///    reroute gives **T** — weak unless T is `trivial`, because the lead chose
///    it and nothing proves a cheaper tier would have failed.
/// 2. Merged after a reroute UP to U gives **U**, strong.
/// 3. Merged after a reroute DOWN to D gives **D**, strong (the cheaper tier
///    sufficed).
/// 4. Merged on T with two or more fix rounds or a high finding, and no
///    reroute, gives the tier above T (capped at `high`; `escalate` is never
///    a training label), weak.
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
    let rank = tier_rank(final_tier).filter(|rank| *rank < 4)?;
    if let Some(to_rank) = last_reroute_rank(history) {
        return Some((TIER_LADDER[to_rank], false));
    }
    if fix_rounds <= 1 && high_findings == 0 {
        return Some((TIER_LADDER[rank], rank != 0));
    }
    Some((TIER_LADDER[(rank + 1).min(3)], true))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

    fn enabled(trivial: bool, routine: bool, medium: bool, high: bool) -> EnabledTiers {
        EnabledTiers {
            trivial,
            routine,
            medium,
            high,
            escalate: false,
        }
    }

    fn all_enabled() -> EnabledTiers {
        enabled(true, true, true, true)
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
            min_confidence_4: 0.42,
            min_confidence_3: 0.51,
            fallback_tier: "trivial".into(),
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
    fn gates_follow_the_label_count() {
        let cfg = RoutingSettings {
            min_confidence_4: 0.40,
            min_confidence_3: 0.45,
            min_confidence_2: 0.60,
            ..RoutingSettings::default()
        };
        // The full ladder asks the four-label question; three and two labels
        // use their own gates.
        assert_eq!(threshold_for(4, &cfg), cfg.min_confidence_4);
        assert_eq!(threshold_for(3, &cfg), cfg.min_confidence_3);
        assert_eq!(threshold_for(2, &cfg), cfg.min_confidence_2);
    }

    #[test]
    fn legacy_fallback_tiers_convert_on_load() {
        let config = tempfile::tempdir().expect("config dir");
        for (old_id, ladder_id) in [
            ("simple", "trivial"),
            ("average", "routine"),
            ("difficult", "high"),
        ] {
            std::fs::write(
                routing_settings_path(config.path()),
                format!("[routing]\nfallback_tier = \"{old_id}\"\n"),
            )
            .expect("legacy routing settings");
            assert_eq!(load_routing_settings(config.path()).fallback_tier, ladder_id);
        }
    }

    #[test]
    fn retired_tier_ids_are_rewritten_on_disk() {
        let config = tempfile::tempdir().expect("config dir");
        let path = routing_settings_path(config.path());
        std::fs::write(
            &path,
            r#"[ui]
compact = true

[routing]
fallback_tier = "average"

[routing.effort]
difficult = "high"
"#,
        )
        .expect("legacy routing settings");
        assert_eq!(load_routing_settings(config.path()).fallback_tier, "routine");
        let text = std::fs::read_to_string(&path).expect("rewritten routing settings");
        let saved: toml::Value = toml::from_str(&text).expect("valid TOML");
        assert_eq!(
            saved["routing"]["fallback_tier"],
            toml::Value::String("routine".to_string())
        );
        assert_eq!(
            saved["routing"]["effort"]["high"],
            toml::Value::String("high".to_string())
        );
        assert_eq!(saved["ui"]["compact"], toml::Value::Boolean(true));
        assert!(!text.contains("average"), "no retired ids: {text}");
        assert!(!text.contains("difficult"), "no retired ids: {text}");
    }

    #[test]
    fn ladder_tier_ids_are_not_rewritten_on_load() {
        let config = tempfile::tempdir().expect("config dir");
        let path = routing_settings_path(config.path());
        let text = r#"[ui]
compact = true

[routing]
fallback_tier = "routine"

[routing.effort]
high = "high"
"#;
        std::fs::write(&path, text).expect("current routing settings");
        assert_eq!(load_routing_settings(config.path()).fallback_tier, "routine");
        assert_eq!(
            std::fs::read(&path).expect("unchanged routing settings"),
            text.as_bytes()
        );
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
        for bits in 0..16 {
            let tiers = enabled(
                bits & 1 != 0,
                bits & 2 != 0,
                bits & 4 != 0,
                bits & 8 != 0,
            );
            assert_eq!(
                tiers.offered(),
                route_question::OfferedLabels {
                    trivial: tiers.trivial,
                    routine: tiers.routine,
                    involved: tiers.medium,
                    hard: tiers.high,
                },
                "bits: {bits}"
            );
            assert_eq!(
                tiers.offered().count(),
                usize::from(tiers.trivial)
                    + usize::from(tiers.routine)
                    + usize::from(tiers.medium)
                    + usize::from(tiers.high),
                "bits: {bits}"
            );
        }
    }

    #[test]
    fn routing_question_uses_the_kernel_question() {
        for bits in 0..16 {
            let tiers = enabled(
                bits & 1 != 0,
                bits & 2 != 0,
                bits & 4 != 0,
                bits & 8 != 0,
            );
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
            enabled(true, false, false, false),
            enabled(false, true, false, false),
            enabled(false, false, true, false),
            enabled(false, false, false, true),
        ] {
            assert!(routing_question(tiers).is_none(), "{tiers:?}");
            let stub = StubClassifier::new(choice(route_question::LABEL_ROUTINE, 0.99));
            let decision =
                classify_dispatch(&stub, None, "some text", tiers, &RoutingSettings::default()).await;
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
            threshold_for(4, &cfg),
            route_question::DEFAULT_MIN_CONFIDENCE_4
        );
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
            route_question::DEFAULT_MIN_CONFIDENCE_4,
        ));
        let decision = classify_dispatch(&at, None, "text", tiers, &cfg)
            .await
            .expect("decision");
        assert_eq!(decision.tier, Some("routine"));
        assert!(decision.cleared);

        let below = StubClassifier::new(choice(
            route_question::LABEL_ROUTINE,
            route_question::DEFAULT_MIN_CONFIDENCE_4 - 0.001,
        ));
        let decision = classify_dispatch(&below, None, "text", tiers, &cfg)
            .await
            .expect("decision");
        assert_eq!(decision.tier, Some("routine"));
        assert!(!decision.cleared);

        // The two-label gate is stricter: the same 0.60 answer clears there
        // only at the threshold itself.
        let two_labels = enabled(true, true, false, false);
        let at = StubClassifier::new(choice(
            route_question::LABEL_ROUTINE,
            route_question::DEFAULT_MIN_CONFIDENCE_2,
        ));
        let decision = classify_dispatch(&at, None, "text", two_labels, &cfg)
            .await
            .expect("decision");
        assert!(decision.cleared);

        let below = StubClassifier::new(choice(
            route_question::LABEL_ROUTINE,
            route_question::DEFAULT_MIN_CONFIDENCE_2 - 0.001,
        ));
        let decision = classify_dispatch(&below, None, "text", two_labels, &cfg)
            .await
            .expect("decision");
        assert!(!decision.cleared);
    }

    #[tokio::test]
    async fn the_gate_reads_the_top_probability_not_the_margin() {
        // A live-shaped answer: the pick's margin-like `confidence` (0.30)
        // sits below the four-label gate (0.40), the top option's probability
        // (0.9113) above it — the gate and the logged confidence read the
        // latter (`route_question::answer_confidence`).
        let answer = Answer::Choice {
            label: route_question::LABEL_HARD.to_string(),
            confidence: 0.30,
            probabilities: BTreeMap::from([
                (route_question::LABEL_TRIVIAL.to_string(), 0.0328),
                (route_question::LABEL_ROUTINE.to_string(), 0.0559),
                (route_question::LABEL_HARD.to_string(), 0.9113),
            ]),
        };
        let stub = StubClassifier::new(Some(answer));
        let decision =
            classify_dispatch(&stub, None, "text", all_enabled(), &RoutingSettings::default())
                .await
                .expect("decision");
        assert_eq!(decision.label, route_question::LABEL_HARD);
        assert_eq!(decision.tier, Some("high"));
        assert!((decision.confidence - 0.9113).abs() < 1e-9);
        assert!(decision.cleared);
    }

    #[tokio::test]
    async fn unknown_label_never_clears() {
        assert_eq!(label_tier("medium"), None);
        let odd = StubClassifier::new(choice("medium", 0.99));
        let decision = classify_dispatch(&odd, None, "text", all_enabled(), &RoutingSettings::default())
            .await
            .expect("decision");
        assert_eq!(decision.label, "medium");
        assert_eq!(decision.tier, None);
        assert!(!decision.cleared);
    }

    #[tokio::test]
    async fn involved_maps_to_medium_and_escalate_is_never_a_pick() {
        assert_eq!(label_tier(route_question::LABEL_INVOLVED), Some("medium"));
        assert_eq!(label_tier("escalate"), None);
        let stub = StubClassifier::new(choice(route_question::LABEL_INVOLVED, 0.99));
        let decision =
            classify_dispatch(&stub, None, "text", all_enabled(), &RoutingSettings::default())
                .await
                .expect("involved decision");
        assert_eq!(decision.tier, Some("medium"));
        assert!(decision.cleared);

    }

    #[tokio::test]
    async fn no_answer_is_none() {
        let none = StubClassifier::new(None);
        assert!(
            classify_dispatch(&none, None, "text", all_enabled(), &RoutingSettings::default())
                .await
                .is_none()
        );

        let scored = StubClassifier::new(Some(Answer::Score {
            value: 1.0,
            confidence: 0.99,
        }));
        assert!(
            classify_dispatch(&scored, None, "text", all_enabled(), &RoutingSettings::default())
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
        classify_dispatch(&stub, None, &text, all_enabled(), &RoutingSettings::default())
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
            tier: Some("routine".into()),
            confidence: 0.62,
            probabilities,
            threshold: route_question::DEFAULT_MIN_CONFIDENCE_3,
            cleared: true,
            lead_tier: Some("auto".into()),
            ran_tier: Some("routine".into()),
            ran_model: Some("endpoint/model-a".into()),
            text_sha256: "0".repeat(64),
            text: "a dispatched step".into(),
            skip: None,
            source: Some("laya".into()),
            knn_label: None,
            knn_share: None,
            knn_k: None,
        }
    }

    fn reroute_row() -> DispatchRoutingRow {
        DispatchRoutingRow::LeadReroute {
            ts: 1_700_000_000_001,
            worker_id: "worker-1".into(),
            plan_id: "2ba2168a".into(),
            step: 3,
            from_tier: "routine".into(),
            to_tier: "high".into(),
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
                laya_tier: Some("routine".into()),
                lead_tier: "high".into(),
                requested_tier: None,
                confidence: 0.62,
            },
            reroute_row(),
            DispatchRoutingRow::UserReroute {
                ts: 1_700_000_000_003,
                worker_id: "worker-2".into(),
                plan_id: "2ba2168a".into(),
                step: 4,
                from_tier: "high".into(),
                to_tier: "trivial".into(),
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
                from_tier: "routine".into(),
                to_tier: "high".into(),
                detail: "3 failing tool calls in a row".into(),
            },
            DispatchRoutingRow::NudgeIgnored {
                ts: 1_700_000_000_005,
                worker_id: "worker-3".into(),
                plan_id: "2ba2168a".into(),
                step: 5,
                tier: "routine".into(),
            },
            DispatchRoutingRow::Outcome {
                ts: 1_700_000_000_006,
                worker_id: "worker-1".into(),
                plan_id: "2ba2168a".into(),
                step: 3,
                status: "merged".into(),
                final_tier: "high".into(),
                models_used: vec!["endpoint/model-a".into(), "endpoint/model-b".into()],
                review_rounds: 1,
                fix_rounds: 1,
                findings: 2,
                findings_high: 0,
                audits: 0,
                cost_micros: 12_345,
                label: Some("high".into()),
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
            outcome_label(&no_reroute, "merged", "trivial", 1, 0),
            Some(("trivial", false))
        );
        assert_eq!(
            outcome_label(&no_reroute, "merged", "routine", 1, 0),
            Some(("routine", true))
        );
        assert_eq!(
            outcome_label(&no_reroute, "merged", "high", 0, 0),
            Some(("high", true))
        );

        // Rule 2: merged after a reroute UP to U gives U, strong.
        assert_eq!(
            outcome_label(
                &reroute_history("routine", "high"),
                "merged",
                "high",
                0,
                0
            ),
            Some(("high", false))
        );

        // Rule 3: merged after a reroute DOWN to D gives D, strong.
        assert_eq!(
            outcome_label(
                &reroute_history("high", "trivial"),
                "merged",
                "trivial",
                3,
                2
            ),
            Some(("trivial", false))
        );

        // A model-only reroute (same tier) is not a tier reroute: rules 1 and
        // 4 still decide.
        assert_eq!(
            outcome_label(
                &reroute_history("routine", "routine"),
                "merged",
                "routine",
                0,
                0
            ),
            Some(("routine", true))
        );

        // Rule 4: merged on T with two or more fix rounds or a high finding,
        // and no reroute, gives the tier above T — weak, capped at high.
        assert_eq!(
            outcome_label(&no_reroute, "merged", "trivial", 2, 0),
            Some(("routine", true))
        );
        assert_eq!(
            outcome_label(&no_reroute, "merged", "routine", 0, 1),
            Some(("medium", true))
        );
        assert_eq!(
            outcome_label(&no_reroute, "merged", "high", 2, 1),
            Some(("high", true))
        );

        // Rule 5: failed, stopped or abandoned gives no label.
        for status in ["failed", "stopped", "abandoned"] {
            assert_eq!(
                outcome_label(&no_reroute, status, "routine", 0, 0),
                None,
                "status: {status}"
            );
        }

        // Escalation and tiers outside the ladder never become training labels.
        assert_eq!(outcome_label(&no_reroute, "merged", "escalate", 2, 1), None);
        assert_eq!(outcome_label(&no_reroute, "merged", "unknown", 0, 0), None);
    }

    // ── The kNN overlay ────────────────────────────────────────────────────
    //
    // These use a tiny deterministic bag-of-words embedder, not the real
    // shared embedding backend: only the overlay's dispatch contract is under
    // test (source, gate fallback, provenance fields).

    use std::path::Path;

    use mnemo::agent::failure_triage_knn::EmbedderGetter;
    use mnemo::agent::route_knn::RouteKnn;
    use mnemo::memory::embedder::Embedder;

    struct TokenEmbedder {
        calls: AtomicUsize,
        failing: AtomicBool,
        delay_ms: AtomicU64,
    }

    impl TokenEmbedder {
        fn new() -> Self {
            Self::with_delay(0)
        }

        fn with_delay(delay_ms: u64) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                failing: AtomicBool::new(false),
                delay_ms: AtomicU64::new(delay_ms),
            }
        }
    }

    #[async_trait]
    impl Embedder for TokenEmbedder {
        async fn embed(&self, text: &str) -> Vec<f32> {
            let delay = self.delay_ms.load(Ordering::Relaxed);
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.failing.load(Ordering::Relaxed) {
                return vec![0.0; 8];
            }
            let mut vector = vec![0.0f32; 8];
            let dims = vector.len();
            for word in text.split_whitespace() {
                let hash = word
                    .bytes()
                    .fold(0usize, |hash, byte| hash.wrapping_mul(31) + byte as usize);
                vector[hash % dims] += 1.0;
            }
            vector
        }

        fn model_id(&self) -> &str {
            "token-test"
        }
    }

    fn overlay(dir: &Path, embedder: Arc<TokenEmbedder>) -> RouteKnn {
        let getter: EmbedderGetter = Arc::new(move || embedder.clone() as Arc<dyn Embedder>);
        RouteKnn::new(getter, dir.join("dispatch_routing.jsonl"))
    }

    fn append_labelled(path: &Path, worker: &str, text: &str, label: &str) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open routing log");
        writeln!(
            file,
            "{}",
            serde_json::json!({"event": "route", "worker_id": worker, "text": text})
        )
        .expect("route row");
        writeln!(
            file,
            "{}",
            serde_json::json!({"event": "outcome", "worker_id": worker, "label": label})
        )
        .expect("outcome row");
    }

    #[tokio::test]
    async fn knn_cold_start_defers_to_laya() {
        let dir = tempfile::tempdir().expect("tempdir");
        let knn = overlay(dir.path(), Arc::new(TokenEmbedder::new()));
        let stub = StubClassifier::new(choice(route_question::LABEL_HARD, 0.9));
        let decision = classify_dispatch(
            &stub,
            Some(&knn),
            "alpha beta gamma",
            all_enabled(),
            &RoutingSettings::default(),
        )
        .await
        .expect("decision");
        assert_eq!(decision.source, "laya");
        assert_eq!(decision.knn_label, None);
        assert_eq!(stub.calls(), 1);
    }

    #[tokio::test]
    async fn knn_consistent_rows_answer_before_laya() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        for index in 0..30 {
            let worker = format!("worker-{index}");
            append_labelled(&path, &worker, "alpha beta gamma", "routine");
        }
        let knn = overlay(dir.path(), Arc::new(TokenEmbedder::new()));
        // Warm the overlay: dispatch itself never waits for a build.
        knn.indexed_rows().await;
        let stub = StubClassifier::new(choice(route_question::LABEL_HARD, 0.9));
        let decision = classify_dispatch(
            &stub,
            Some(&knn),
            "alpha beta gamma",
            all_enabled(),
            &RoutingSettings::default(),
        )
        .await
        .expect("decision");
        assert_eq!(decision.source, "knn");
        assert_eq!(decision.label, route_question::LABEL_ROUTINE);
        assert_eq!(decision.tier, Some("routine"));
        assert!(decision.cleared);
        assert_eq!(decision.knn_label.as_deref(), Some(route_question::LABEL_ROUTINE));
        assert_eq!(decision.knn_k, Some(5));
        assert!((decision.knn_share.expect("share") - 1.0).abs() < 1e-9);
        // Laya was never asked.
        assert_eq!(stub.calls(), 0);
    }

    #[tokio::test]
    async fn knn_tie_under_the_share_gate_falls_back_to_laya() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        for (index, tier) in [("a", "trivial"), ("b", "trivial"), ("c", "routine"), ("d", "routine")]
            .into_iter()
        {
            let worker = format!("worker-{index}");
            append_labelled(&path, &worker, "alpha beta gamma", tier);
        }
        let knn = overlay(dir.path(), Arc::new(TokenEmbedder::new()));
        knn.indexed_rows().await;
        let stub = StubClassifier::new(choice(route_question::LABEL_HARD, 0.9));
        let cfg = RoutingSettings {
            knn_min_rows: 4,
            ..RoutingSettings::default()
        };
        let decision = classify_dispatch(&stub, Some(&knn), "alpha beta gamma", all_enabled(), &cfg)
            .await
            .expect("decision");
        assert_eq!(decision.source, "laya");
        assert_eq!(decision.label, route_question::LABEL_HARD);
        assert_eq!(decision.knn_label, None);
        assert_eq!(stub.calls(), 1);
    }

    #[tokio::test]
    async fn knn_embedder_failure_falls_back_to_laya() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        for index in 0..30 {
            let worker = format!("worker-{index}");
            append_labelled(&path, &worker, "alpha beta gamma", "routine");
        }
        let embedder = Arc::new(TokenEmbedder::new());
        embedder.failing.store(true, Ordering::Relaxed);
        let knn = overlay(dir.path(), embedder);
        assert_eq!(knn.indexed_rows().await, 0);
        let stub = StubClassifier::new(choice(route_question::LABEL_HARD, 0.9));
        let decision = classify_dispatch(
            &stub,
            Some(&knn),
            "alpha beta gamma",
            all_enabled(),
            &RoutingSettings::default(),
        )
        .await
        .expect("decision");
        assert_eq!(decision.source, "laya");
        assert_eq!(stub.calls(), 1);
    }

    #[tokio::test]
    async fn a_slow_overlay_build_never_starves_laya_and_finishes_in_the_background() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dispatch_routing.jsonl");
        for index in 0..30 {
            let worker = format!("worker-{index}");
            append_labelled(&path, &worker, "alpha beta gamma", "routine");
        }
        // Each row takes 110 ms to embed; a cold build is ~3.3 s, past the 3 s
        // dispatch timeout that used to drop the refresh future. The background
        // build must not hold up Laya, and must still publish for a later call.
        let knn = overlay(dir.path(), Arc::new(TokenEmbedder::with_delay(110)));
        let stub = StubClassifier::new(choice(route_question::LABEL_HARD, 0.9));
        let first = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            classify_dispatch(
                &stub,
                Some(&knn),
                "alpha beta gamma",
                all_enabled(),
                &RoutingSettings::default(),
            ),
        )
        .await
        .expect("classification returned inside the dispatch budget")
        .expect("decision");
        // The cold overlay did not hold up Laya; it kept building off-path.
        assert_eq!(first.source, "laya");
        assert_eq!(stub.calls(), 1);
        // Let the spawned build take the refresh lock so the wait below
        // observes the background build rather than a synchronous catch-up.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(knn.indexed_rows().await, 30);
        let second = classify_dispatch(
            &stub,
            Some(&knn),
            "alpha beta gamma",
            all_enabled(),
            &RoutingSettings::default(),
        )
        .await
        .expect("decision");
        assert_eq!(second.source, "knn");
        assert_eq!(second.label, route_question::LABEL_ROUTINE);
        assert_eq!(second.knn_k, Some(5));
        assert!(second.knn_share.unwrap_or_default() >= 0.80);
    }

    #[test]
    fn old_route_rows_without_the_knn_fields_still_parse() {
        let line = r#"{"event":"route","ts":1700000000000,"worker_id":"worker-1","plan_id":"2ba2168a","step":3,"mode":"suggest","label":"routine","tier":"routine","confidence":0.62,"probabilities":{"routine":0.62,"hard":0.38},"threshold":0.475,"cleared":true,"lead_tier":"auto","ran_tier":"routine","ran_model":"endpoint/model-a","text_sha256":"0000000000000000000000000000000000000000000000000000000000000000","text":"a dispatched step","skip":null}"#;
        let row: DispatchRoutingRow = serde_json::from_str(line).expect("old row");
        match row {
            DispatchRoutingRow::Route {
                source,
                knn_label,
                knn_share,
                knn_k,
                ..
            } => {
                assert_eq!(source, None);
                assert_eq!(knn_label, None);
                assert_eq!(knn_share, None);
                assert_eq!(knn_k, None);
            }
            other => panic!("not a route row: {other:?}"),
        }
    }

    #[test]
    fn settings_without_knn_keys_load_the_defaults() {
        let config = tempfile::tempdir().expect("config dir");
        std::fs::write(
            routing_settings_path(config.path()),
            "[routing]\nmode = \"shadow\"\n",
        )
        .expect("partial settings");
        let settings = load_routing_settings(config.path());
        assert!((settings.knn_min_share - 0.80).abs() < 1e-9);
        assert_eq!(settings.knn_min_rows, 30);
    }
}
