// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The routing eval's core (plan 2ba2168a, design §4.8; four labels since
//! design §5.1) — the corpora, the sequential classification pass, the
//! threshold table that suggests `min_confidence_4` / `min_confidence_3` /
//! `min_confidence_2`, and the `route_eval.json` record the Laya page reads
//! back.
//!
//! Two surfaces share it: the `mnemo2_route_eval` CLI
//! (`src-tauri/src/bin/mnemo2_route_eval.rs`, `--laya-url` required) and the
//! Laya page's Run-eval button (the `run_routing_eval` command in
//! [`super::laya_settings`], which asks the lead engine's attached
//! classifier). Everything except [`classify_corpus`] is pure — corpus
//! parsing, the table math and the suggestion — so no test touches the
//! network.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use mnemo::agent::failure_triage::training_log_dir;
use mnemo::agent::route_knn::RouteKnn;
use mnemo::agent::route_question::{self, OfferedLabels};
use mnemo::config::endpoints::TierLadder;
use mnemo::memory::classifier::{Answer, Classifier};

use super::dispatch_routing::DispatchRoutingRow;

/// The hand-set corpus shipped with the repo (`tests/fixtures/route_eval.jsonl`),
/// embedded so the app's Run-eval button needs no fixture path at runtime.
pub const HAND_SET_JSONL: &str = include_str!("../../tests/fixtures/route_eval.jsonl");

/// The four labels, easiest first — the eval's expected labels (`involved`
/// is `medium`, `hard` is `high`; design §5.1).
pub const TIERS: [&str; 4] = ["trivial", "routine", "involved", "hard"];

/// The lowest gate the sweep tries.
const GATE_START: f64 = 0.25;
/// The distance between two swept gates.
const GATE_STEP: f64 = 0.025;
/// How many gates the sweep tries (`0.25..=0.80` inclusive).
const GATE_COUNT: usize = 23;
/// The accuracy floor on routed prompts a suggested gate must keep.
const TARGET_ACCURACY: f64 = 0.90;

/// One eval case: the text Laya is asked about and the tier a correct answer
/// names.
#[derive(Clone, Debug, PartialEq)]
pub struct EvalCase {
    /// The step-spec / first-prompt text handed to Laya.
    pub text: String,
    /// The tier a correct answer names; `None` means the case must not clear
    /// the gate (a vague text a calibrated gate has to reject).
    pub expected: Option<String>,
}

/// The gates the sweep tries: `0.25..=0.80` in `0.025` steps, 23 rows.
pub fn sweep_gates() -> Vec<f64> {
    (0..GATE_COUNT)
        .map(|index| GATE_START + index as f64 * GATE_STEP)
        .collect()
}

/// One row of the hand-set fixture.
#[derive(Debug, Deserialize)]
struct HandRow {
    /// The step-spec text.
    text: String,
    /// The tier a correct answer names; absent or `null` = must not route.
    #[serde(default)]
    expected: Option<String>,
}

/// Parse the hand-set JSONL: one object per line with `text` and an optional
/// `expected` tier. Blank lines and `#` comments are skipped; any other
/// problem is an error naming the line.
pub fn parse_hand_set(jsonl: &str) -> Result<Vec<EvalCase>, String> {
    let mut cases = Vec::new();
    for (index, line) in jsonl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let row: HandRow =
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        let expected = match row.expected.as_deref() {
            None => None,
            Some(label) if TIERS.contains(&label) => Some(label.to_string()),
            Some(other) => {
                return Err(format!("line {}: unknown tier '{other}'", index + 1));
            }
        };
        let text = row.text.trim();
        if text.is_empty() {
            return Err(format!("line {}: empty text", index + 1));
        }
        cases.push(EvalCase { text: text.to_string(), expected });
    }
    if cases.is_empty() {
        return Err("the hand set is empty".to_string());
    }
    Ok(cases)
}

/// Strip the markdown emphasis a plan/review line carries (`**` and
/// backticks) and collapse whitespace — the text Laya reads should be the
/// prose, not the formatting. A single `*` survives because plan steps carry
/// globs (`src/headless/*`).
fn strip_markdown(text: &str) -> String {
    let cleaned = text.replace("**", "").replace('`', "");
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text after `N. ` on a `- [ ]` / `- [x]` plan step line, or `None` when
/// the line is not a numbered step.
fn step_headline(line: &str) -> Option<String> {
    let rest = line.strip_prefix("- [")?;
    let mut chars = rest.chars();
    match chars.next() {
        Some(' ') | Some('x') | Some('X') => {}
        _ => return None,
    }
    let rest = chars.as_str().strip_prefix("] ")?;
    let digits = rest.find('.').filter(|&index| {
        index > 0 && index <= 4 && rest[..index].chars().all(|c| c.is_ascii_digit())
    })?;
    let text = rest[digits + 1..].trim_start();
    (!text.is_empty()).then(|| text.to_string())
}

/// The step-spec texts in a plan document: every numbered `- [ ]` / `- [x]`
/// step line plus its indented continuation paragraphs, markdown emphasis
/// stripped. This is the same text `team_dispatch` classifies, so the eval
/// measures Laya on what it actually sees.
pub fn plan_step_texts(markdown: &str) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let indented = line.len() - trimmed.len() >= 2;
        if let Some(headline) = step_headline(trimmed) {
            if let Some(text) = current.take() {
                steps.push(text);
            }
            current = Some(headline);
            continue;
        }
        let mut ends_step = false;
        if let Some(text) = current.as_mut() {
            if trimmed.is_empty() {
                // A blank line may separate a step from its continuation.
            } else if indented && !trimmed.starts_with("- ") && !trimmed.starts_with('#') {
                text.push(' ');
                text.push_str(trimmed);
            } else {
                // A top-level bullet, heading or paragraph ends the step.
                ends_step = true;
            }
        }
        if ends_step {
            if let Some(text) = current.take() {
                steps.push(text);
            }
        }
    }
    if let Some(text) = current.take() {
        steps.push(text);
    }
    steps
        .into_iter()
        .map(|text| strip_markdown(&text))
        .filter(|text| !text.is_empty())
        .collect()
}

/// The task texts a team review records under its `## Tasks` heading — one
/// per bullet, the leading backticked worker id and the trailing
/// `- model ...` metadata stripped. Bullets under any other heading are not
/// tasks.
pub fn review_task_texts(markdown: &str) -> Vec<String> {
    let mut in_tasks = false;
    let mut tasks = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            in_tasks = heading.trim().eq_ignore_ascii_case("tasks");
            continue;
        }
        if !in_tasks {
            continue;
        }
        let Some(bullet) = trimmed.strip_prefix("- ") else {
            continue;
        };
        let head = match bullet.split_once(" - ") {
            Some((head, _metadata)) => head,
            None => bullet,
        };
        // A task bullet opens with the worker's backticked id; the
        // descriptive name after it is the task text.
        let head = match head.strip_prefix('`') {
            Some(rest) => rest.split_once('`').map_or(head, |(_id, tail)| tail.trim()),
            None => head,
        };
        let text = strip_markdown(head);
        if !text.is_empty() {
            tasks.push(text);
        }
    }
    tasks
}

/// The `--history` corpus: every mined plan step and review task, unlabelled
/// — they feed the routed share, while the labelled corpora feed accuracy.
pub fn history_corpus(plans: &[String], reviews: &[String]) -> Vec<EvalCase> {
    let mut cases = Vec::new();
    for plan in plans {
        cases.extend(
            plan_step_texts(plan)
                .into_iter()
                .map(|text| EvalCase { text, expected: None }),
        );
    }
    for review in reviews {
        cases.extend(
            review_task_texts(review)
                .into_iter()
                .map(|text| EvalCase { text, expected: None }),
        );
    }
    cases
}

/// The cases the dispatch log already labels: a worker's routed text joined
/// to its derived outcome label (design §4.8). The log does not record which
/// labels were weak, so every labelled outcome is mined. Outcome rows carry
/// tier ids — legacy ones (`simple` / `average` / `difficult`) are converted
/// on read — mapped onto the Laya labels ([`logged_tier_label`]); anything
/// that is not a pickable tier (`escalate`, `custom`) is not a case.
pub fn outcome_cases(rows: &[DispatchRoutingRow]) -> Vec<EvalCase> {
    let mut texts: BTreeMap<&str, &str> = BTreeMap::new();
    for row in rows {
        if let DispatchRoutingRow::Route { worker_id, text, skip: None, .. } = row {
            texts.insert(worker_id.as_str(), text.as_str());
        }
    }
    let mut cases = Vec::new();
    for row in rows {
        if let DispatchRoutingRow::Outcome { worker_id, label: Some(label), .. } = row {
            let Some(label) = logged_tier_label(label) else {
                continue;
            };
            if let Some(text) = texts.get(worker_id.as_str()) {
                cases.push(EvalCase {
                    text: (*text).to_string(),
                    expected: Some(label.to_string()),
                });
            }
        }
    }
    cases
}

/// The Laya label for a tier id stored in a log row, converting legacy ids on
/// the way (`simple` → `trivial`, `average` → `routine`, `difficult` →
/// `high`); `None` for anything that is not a pickable tier — `escalate` (an
/// escalation is never a training label), `custom`, and unknown strings. The
/// two sanctioned mappings composed ([`TierLadder::convert_old_id`] then
/// [`route_question::label_for_tier`]): nothing maps around them.
pub fn logged_tier_label(tier: &str) -> Option<&'static str> {
    let converted = TierLadder::convert_old_id(tier).unwrap_or(tier);
    route_question::label_for_tier(converted)
}

/// The `--mnemo1` corpus: plain-language first-prompt twins of `cases` — the
/// same work phrased as a chat request, which is the text Mnemo1's
/// first-prompt route reads (design §4.12).
pub fn mnemo1_twins(cases: &[EvalCase]) -> Vec<EvalCase> {
    cases
        .iter()
        .map(|case| {
            let plain = strip_markdown(&case.text);
            let plain = plain.trim_end_matches(['.', ':', ';']).trim();
            EvalCase {
                text: format!("Can you {plain}?"),
                expected: case.expected.clone(),
            }
        })
        .collect()
}

/// Whether `label` is one of the four labels and is offered.
fn is_offered_label(label: &str, offered: OfferedLabels) -> bool {
    match label {
        route_question::LABEL_TRIVIAL => offered.trivial,
        route_question::LABEL_ROUTINE => offered.routine,
        route_question::LABEL_INVOLVED => offered.involved,
        route_question::LABEL_HARD => offered.hard,
        _ => false,
    }
}

/// The offered label an answer's label maps onto: an offered label passes
/// through, an un-offered one folds up onto the nearest offered label (the
/// kernel's §4.3 folding), and a label outside the ladder is no answer.
fn fold_label(label: &str, offered: OfferedLabels) -> Option<String> {
    if is_offered_label(label, offered) {
        return Some(label.to_string());
    }
    route_question::label_above(label, offered).map(str::to_string)
}

/// One case's classification result: what Laya named, how sure, and the tier
/// the case should have got.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseScore {
    /// The text handed to Laya.
    pub text: String,
    /// The tier the case should get; `None` = it must not clear the gate.
    pub expected: Option<String>,
    /// The offered label Laya named, `None` for no answer.
    pub label: Option<String>,
    /// The gated confidence — the top option's probability when the backend
    /// reported a distribution, else the pick's confidence (`None` for no
    /// answer).
    pub confidence: Option<f64>,
}

impl CaseScore {
    /// The tier a correct answer must name under `offered`: an expected label
    /// that is not offered folds onto its offered neighbour, exactly as
    /// Laya's answer does, so a two-label run is not penalised for the
    /// missing rung.
    pub fn expectation(&self, offered: OfferedLabels) -> Option<String> {
        self.expected.as_deref().and_then(|label| fold_label(label, offered))
    }

    /// Whether the answer named a tier and cleared `gate`.
    pub fn routed_at(&self, gate: f64) -> bool {
        self.label.is_some() && self.confidence.is_some_and(|confidence| confidence >= gate)
    }
}

/// One row of the threshold table.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ThresholdRow {
    /// The gate this row measured.
    pub threshold: f64,
    /// How many cases cleared it.
    pub routed: usize,
    /// How many of those named the expected tier.
    pub correct: usize,
    /// `routed / cases` (`0.0` for an empty corpus).
    pub routed_share: f64,
    /// `correct / routed` (`0.0` when nothing routed).
    pub accuracy: f64,
}

/// The swept threshold table: for every gate, the share of cases routed and
/// the accuracy on the routed ones.
pub fn threshold_table(scores: &[CaseScore], offered: OfferedLabels) -> Vec<ThresholdRow> {
    let mut table = Vec::with_capacity(GATE_COUNT);
    for threshold in sweep_gates() {
        let mut routed = 0;
        let mut correct = 0;
        for score in scores {
            if !score.routed_at(threshold) {
                continue;
            }
            routed += 1;
            if score.label == score.expectation(offered) {
                correct += 1;
            }
        }
        table.push(ThresholdRow {
            threshold,
            routed,
            correct,
            routed_share: if scores.is_empty() {
                0.0
            } else {
                routed as f64 / scores.len() as f64
            },
            accuracy: if routed == 0 { 0.0 } else { correct as f64 / routed as f64 },
        });
    }
    table
}

/// The lowest swept gate that keeps at least [`TARGET_ACCURACY`] on routed
/// cases — the suggested `min_confidence_<n>` (design §4.8). `None` when no
/// gate reaches the floor, in which case the provisional default stands.
pub fn suggested_gate(table: &[ThresholdRow]) -> Option<f64> {
    table
        .iter()
        .find(|row| row.routed > 0 && row.accuracy >= TARGET_ACCURACY)
        .map(|row| row.threshold)
}

/// One case the default gate gets wrong: a wrong tier on a routed case, or a
/// case with an expected tier that never cleared the gate.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Miss {
    /// The case's text.
    pub text: String,
    /// The tier the case should get.
    pub expected: Option<String>,
    /// The label Laya named, `None` for no answer.
    pub got: Option<String>,
    /// Laya's confidence, `None` for no answer.
    pub confidence: Option<f64>,
}

/// The kNN overlay's side of an eval: its coverage and accuracy on the same
/// cases, then the combined kNN+Laya policy that dispatch actually runs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct KnnEvalReport {
    /// How many labelled workers the overlay had indexed.
    pub indexed_rows: usize,
    /// Cases where the overlay's vote cleared the share gate.
    pub answered: usize,
    /// Of those, how many named the expected (folded) tier.
    pub correct: usize,
    /// `correct / answered` (`0.0` when the overlay never answered).
    pub accuracy: f64,
    /// Laya-only: cases that cleared the default gate.
    pub laya_routed: usize,
    /// Laya-only: of those, how many named the expected tier.
    pub laya_correct: usize,
    /// `laya_correct / laya_routed` (`0.0` when nothing routed).
    pub laya_accuracy: f64,
    /// kNN+Laya: cases where either the overlay answered or Laya cleared its
    /// gate.
    pub combined_answered: usize,
    /// kNN+Laya: of those, how many named the expected tier.
    pub combined_correct: usize,
    /// `combined_correct / combined_answered`.
    pub combined_accuracy: f64,
}

/// One offered-label count's report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EvalReport {
    /// How many labels were offered (4, 3 or 2).
    pub labels: usize,
    /// The default gate for that count
    /// ([`route_question::default_min_confidence`]).
    pub default_gate: f64,
    /// The swept threshold table.
    pub table: Vec<ThresholdRow>,
    /// The suggested gate, when one reached the accuracy floor.
    pub suggested: Option<f64>,
    /// The mismatches at [`default_gate`](Self::default_gate).
    pub misses: Vec<Miss>,
    /// The overlay comparison, when the eval had a live [`RouteKnn`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knn: Option<KnnEvalReport>,
}

impl EvalReport {
    /// The table row for `gate`, when the sweep contains it.
    pub fn row_at(&self, gate: f64) -> Option<&ThresholdRow> {
        self.table
            .iter()
            .find(|row| (row.threshold - gate).abs() < 1e-9)
    }
}

/// Build one offered-label count's report: the table, the suggestion and the
/// misses at the default gate.
pub fn report(scores: &[CaseScore], offered: OfferedLabels) -> EvalReport {
    let labels = offered.count();
    let default_gate = route_question::default_min_confidence(labels);
    let table = threshold_table(scores, offered);
    let suggested = suggested_gate(&table);
    let misses = scores
        .iter()
        .filter_map(|score| {
            let expected = score.expectation(offered);
            let routed = score.routed_at(default_gate);
            let wrong = match (&expected, &score.label) {
                (Some(want), Some(got)) => want != got,
                // An expected tier with no answer never cleared the gate.
                (Some(_), None) => true,
                // A vague case that routed is a false route.
                (None, Some(_)) => true,
                (None, None) => false,
            };
            (routed && wrong || (!routed && expected.is_some())).then(|| Miss {
                text: score.text.clone(),
                expected,
                got: score.label.clone(),
                confidence: score.confidence,
            })
        })
        .collect();
    EvalReport {
        labels,
        default_gate,
        table,
        suggested,
        misses,
        knn: None,
    }
}

/// Attach the overlay comparison to a report built by [`report`].
pub fn report_with_knn(
    scores: &[CaseScore],
    offered: OfferedLabels,
    knn: Option<KnnEvalReport>,
) -> EvalReport {
    let mut report = report(scores, offered);
    report.knn = knn;
    report
}

/// The label a kNN tier vote names, for comparison with a case's expected
/// label. Votes carry ladder tier ids ([`RouteKnnVote::tier`]), already
/// converted from legacy ids when the row was indexed, so this is a straight
/// delegation to the kernel's only label↔tier mapping.
fn label_for_tier(tier: &str) -> Option<&'static str> {
    route_question::label_for_tier(tier)
}

/// Score the kNN overlay and the combined kNN+Laya policy on the same cases.
///
/// The overlay answers when its vote clears `min_share` after at least
/// `min_rows` labelled workers (exactly dispatch's rule); otherwise the case
/// falls back to Laya at its default gate. The hand set is a fixed prompt set,
/// so this is coverage/accuracy where the overlay answered; log-derived cases
/// are the caller's choice and are not treated as leave-one-out here (the
/// eval's report is a sample, not a training fit).
pub async fn evaluate_knn(
    cases: &[EvalCase],
    scores: &[CaseScore],
    offered: OfferedLabels,
    knn: &RouteKnn,
    min_rows: usize,
    min_share: f64,
) -> KnnEvalReport {
    let default_gate = route_question::default_min_confidence(offered.count());
    let indexed_rows = knn.indexed_rows().await;
    let mut answered = 0;
    let mut correct = 0;
    let mut laya_routed = 0;
    let mut laya_correct = 0;
    let mut combined_answered = 0;
    let mut combined_correct = 0;
    for (index, score) in scores.iter().enumerate() {
        let expected = score.expectation(offered);
        let vote = match cases.get(index) {
            Some(case) => knn
                .classify(&case.text, offered, min_rows)
                .await
                .filter(|vote| vote.share >= min_share),
            None => None,
        };
        let knn_label = vote.as_ref().and_then(|vote| label_for_tier(&vote.tier));
        if let Some(label) = knn_label {
            answered += 1;
            if expected.as_deref() == Some(label) {
                correct += 1;
            }
        }
        let laya_cleared = score.routed_at(default_gate);
        if laya_cleared {
            laya_routed += 1;
            if score.label == expected {
                laya_correct += 1;
            }
        }
        // The combined policy: the overlay's answer wins when it cleared;
        // otherwise Laya's gated answer decides.
        let chosen: Option<String> = match knn_label {
            Some(label) => Some(label.to_string()),
            None if laya_cleared => score.label.clone(),
            None => None,
        };
        if chosen.is_some() {
            combined_answered += 1;
            if chosen.as_deref() == expected.as_deref() {
                combined_correct += 1;
            }
        }
    }
    let share = |part: usize, whole: usize| {
        if whole == 0 {
            0.0
        } else {
            part as f64 / whole as f64
        }
    };
    KnnEvalReport {
        indexed_rows,
        answered,
        correct,
        accuracy: share(correct, answered),
        laya_routed,
        laya_correct,
        laya_accuracy: share(laya_correct, laya_routed),
        combined_answered,
        combined_correct,
        combined_accuracy: share(combined_correct, combined_answered),
    }
}

/// Ask Laya about every case, one prompt at a time, and collect the answers.
/// An un-offered label in the answer folds onto the nearest offered one; any
/// other label counts as no answer.
pub async fn classify_corpus(
    classifier: &dyn Classifier,
    offered: OfferedLabels,
    cases: &[EvalCase],
) -> Vec<CaseScore> {
    let Some(question) = route_question::route_question(offered) else {
        return Vec::new();
    };
    let mut scores = Vec::with_capacity(cases.len());
    for case in cases {
        let answer = classifier.classify(&case.text, &question).await;
        // The score carries the top option's probability, the number the
        // gates are calibrated on (`route_question::answer_confidence`).
        let (label, confidence) = match &answer {
            Some(answer) => match answer {
                Answer::Choice { label, .. } => (
                    fold_label(label, offered),
                    Some(route_question::answer_confidence(answer)),
                ),
                _ => (None, None),
            },
            None => (None, None),
        };
        scores.push(CaseScore {
            text: case.text.clone(),
            expected: case.expected.clone(),
            label,
            confidence,
        });
    }
    scores
}

/// `<training_dir>/route_eval.json`: the last eval's suggestions, which the
/// Laya page shows beside the gates (design §4.8).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalRecord {
    /// The day the eval ran (`YYYY-MM-DD`, UTC).
    pub date: String,
    /// The suggested four-label gate, when the sweep reached the floor (the
    /// run dispatch actually makes: every pickable tier is offered).
    #[serde(default)]
    pub min_confidence_4: Option<f64>,
    /// The suggested three-label gate, when the sweep reached the floor.
    #[serde(default)]
    pub min_confidence_3: Option<f64>,
    /// The suggested two-label gate.
    #[serde(default)]
    pub min_confidence_2: Option<f64>,
    /// The share of the four-label corpus that routed at the suggested gate.
    #[serde(default)]
    pub routed_share: Option<f64>,
    /// The accuracy on the routed four-label prompts at the suggested gate.
    #[serde(default)]
    pub accuracy: Option<f64>,
}

/// The eval record's file: `~/.mnemo/laya/training/route_eval.json`.
pub fn route_eval_path() -> PathBuf {
    training_log_dir().join("route_eval.json")
}

/// Read the last eval record; a missing or unreadable file is no record.
pub fn load_eval_record() -> Option<EvalRecord> {
    let text = std::fs::read_to_string(route_eval_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the eval record, creating the training directory.
pub fn save_eval_record(record: &EvalRecord) -> Result<(), String> {
    let path = route_eval_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let encoded = serde_json::to_string_pretty(record)
        .map_err(|error| format!("cannot encode the eval record: {error}"))?;
    std::fs::write(&path, encoded).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Build the record from the three runs (four-, three- and two-label),
/// dated today. The stored share and accuracy are the four-label run's —
/// the question dispatch actually asks — at the gate the sweep suggested
/// (the default when it suggested none).
pub fn eval_record(four: &EvalReport, three: &EvalReport, two: &EvalReport) -> EvalRecord {
    let gate = four.suggested.unwrap_or(four.default_gate);
    let row = four.row_at(gate);
    EvalRecord {
        date: today_utc(),
        min_confidence_4: four.suggested,
        min_confidence_3: three.suggested,
        min_confidence_2: two.suggested,
        routed_share: row.map(|row| row.routed_share),
        accuracy: row.map(|row| row.accuracy),
    }
}

/// Format epoch seconds as a UTC calendar date (`YYYY-MM-DD`) — Howard
/// Hinnant's `civil_from_days`, exact for the full day range (the crate has
/// no chrono dependency; mirrors `src/memory/knowledge.rs`).
fn format_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

/// Today's UTC calendar date (`YYYY-MM-DD`) — the eval record's stamp.
pub fn today_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    format_date(secs)
}
#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use mnemo::memory::classifier::Question;

    use super::*;
    use crate::mnemo2::dispatch_routing::RoutingMode;

    /// The four-label run: the question dispatch actually asks (design §5.1
    /// — all four are offered whenever routing runs).
    fn four() -> OfferedLabels {
        OfferedLabels { trivial: true, routine: true, involved: true, hard: true }
    }

    /// The three-label run: the interim `EnabledTiers` shape, without the new
    /// middle rung.
    fn three() -> OfferedLabels {
        OfferedLabels { trivial: true, routine: true, involved: false, hard: true }
    }

    fn two() -> OfferedLabels {
        OfferedLabels { trivial: true, routine: false, involved: false, hard: true }
    }

    fn score(text: &str, expected: Option<&str>, label: Option<&str>, confidence: f64) -> CaseScore {
        CaseScore {
            text: text.to_string(),
            expected: expected.map(str::to_string),
            label: label.map(str::to_string),
            confidence: Some(confidence),
        }
    }

    fn route_row(worker: &str, text: &str, skip: Option<&str>) -> DispatchRoutingRow {
        DispatchRoutingRow::Route {
            ts: 1,
            worker_id: worker.to_string(),
            plan_id: "plan".to_string(),
            step: 1,
            mode: RoutingMode::Shadow,
            label: "routine".to_string(),
            tier: Some("routine".to_string()),
            confidence: 0.6,
            probabilities: BTreeMap::new(),
            threshold: 0.475,
            cleared: true,
            lead_tier: Some("routine".to_string()),
            ran_tier: Some("routine".to_string()),
            ran_model: None,
            text_sha256: String::new(),
            text: text.to_string(),
            skip: skip.map(str::to_string),
            source: Some("laya".to_string()),
            knn_label: None,
            knn_share: None,
            knn_k: None,
        }
    }

    fn outcome_row(worker: &str, label: Option<&str>) -> DispatchRoutingRow {
        DispatchRoutingRow::Outcome {
            ts: 2,
            worker_id: worker.to_string(),
            plan_id: "plan".to_string(),
            step: 1,
            status: "merged".to_string(),
            final_tier: "routine".to_string(),
            models_used: vec![],
            review_rounds: 1,
            fix_rounds: 0,
            findings: 0,
            findings_high: 0,
            audits: 0,
            cost_micros: 0,
            label: label.map(str::to_string),
        }
    }

    /// A classifier with one canned answer — enough to drive
    /// [`classify_corpus`], the only networked function under test here.
    struct StubClassifier {
        answer: Option<Answer>,
    }

    #[async_trait]
    impl Classifier for StubClassifier {
        async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
            self.answer.clone()
        }
    }

    #[test]
    fn sweep_bounds_and_dates() {
        let gates = sweep_gates();
        assert_eq!(gates.len(), 23);
        assert!((gates[0] - 0.25).abs() < 1e-9);
        assert!((gates[22] - 0.80).abs() < 1e-9);
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_700_000_000), "2023-11-14");
        let today = today_utc();
        assert_eq!(today.len(), 10, "got: {today}");
        assert_eq!(today.as_bytes()[4], b'-');
    }

    #[test]
    fn eval_table_picks_the_lowest_threshold_at_90_percent() {
        // Below 0.375 the eight wrong high-confidence answers swamp accuracy;
        // from 0.375 up only correct answers remain, so that is the
        // suggestion — the lowest gate that keeps the 90% floor. The four
        // wrong low-confidence cases never route at any swept gate.
        let mut scores = Vec::new();
        for index in 0..8 {
            scores.push(score(&format!("wrong {index}"), Some("routine"), Some("hard"), 0.35));
        }
        for index in 0..4 {
            scores.push(score(&format!("low {index}"), Some("routine"), Some("hard"), 0.20));
        }
        scores.push(score("right a", Some("routine"), Some("routine"), 0.50));
        scores.push(score("right b", Some("hard"), Some("hard"), 0.60));
        scores.push(score("right c", Some("trivial"), Some("trivial"), 0.70));
        scores.push(score("no answer", Some("trivial"), None, 0.99));

        let table = threshold_table(&scores, three());
        let suggested = suggested_gate(&table).expect("a gate reaches the floor");
        assert!((suggested - 0.375).abs() < 1e-9, "got: {suggested}");

        // Routed counts fall monotonically as the gate rises.
        let mut previous = usize::MAX;
        for row in &table {
            assert!(row.routed <= previous, "{:?}", row);
            previous = row.routed;
        }

        // A corpus no gate can rescue suggests nothing.
        let hopeless: Vec<CaseScore> = (0..4)
            .map(|index| score(&format!("bad {index}"), Some("routine"), Some("hard"), 0.70))
            .collect();
        assert_eq!(suggested_gate(&threshold_table(&hopeless, three())), None);
    }

    #[test]
    fn history_corpus_parses_plan_steps_and_review_tasks() {
        let plan = "# Plan\n\n- [x] 1. Kernel: sessions can skip per-turn routing — Files: src/headless/*. Details: add the flag.\n\n  The spawn struct gains `skip_turn_routing: true` on every Mnemo2 session.\n\n- [ ] 2. **Kernel: the shared routing question** (new module) — one question.\n\n## Acceptance scenarios\n\n- With mode off nothing is classified.\n";
        let review = "## Verdict: PASS\n\n## Tasks\n\n- `alpha` alpha - model glm-5.3, status merged, plan step 1, merge commit `abc123` (3 files, +165 -0)\n\n- `beta` beta - model glm-5.3, status merged, plan step 2, merge commit `def456`\n\n## Findings\n\n- `alpha` / row-style [low] Something (file.rs:51) - fixed\n";

        let steps = plan_step_texts(plan);
        assert_eq!(steps.len(), 2, "got: {steps:?}");
        assert_eq!(
            steps[0],
            "Kernel: sessions can skip per-turn routing — Files: src/headless/*. Details: add the flag. The spawn struct gains skip_turn_routing: true on every Mnemo2 session."
        );
        assert_eq!(steps[1], "Kernel: the shared routing question (new module) — one question.");

        let tasks = review_task_texts(review);
        assert_eq!(tasks, vec!["alpha".to_string(), "beta".to_string()]);

        let corpus = history_corpus(&[plan.to_string()], &[review.to_string()]);
        assert_eq!(corpus.len(), 4);
        assert!(corpus.iter().all(|case| case.expected.is_none()));
    }

    #[test]
    fn hand_set_has_at_least_ten_per_label_and_three_vague() {
        let cases = parse_hand_set(HAND_SET_JSONL).expect("the shipped hand set parses");
        for label in TIERS {
            let count = cases.iter().filter(|case| case.expected.as_deref() == Some(label)).count();
            assert!(count >= 10, "label {label}: {count} cases, need at least 10");
        }
        assert_eq!(cases.iter().filter(|case| case.expected.is_none()).count(), 3);
    }

    #[test]
    fn parse_hand_set_rejects_an_unknown_tier() {
        // Tier ids are not labels: `medium` (the tier `involved` routes to)
        // must be rejected just like an unknown string.
        let error = parse_hand_set("{\"text\": \"x\", \"expected\": \"medium\"}").unwrap_err();
        assert!(error.contains("unknown tier"), "got: {error}");
        let error = parse_hand_set("{\"text\": \"x\"}\nnot json").unwrap_err();
        assert!(error.contains("line 2"), "got: {error}");
    }

    #[test]
    fn outcome_cases_join_route_text_with_labels() {
        // Outcome rows carry tier ids: a legacy `average` converts to `routine`
        // on read, a new-id row maps straight through, and anything that is
        // not a pickable tier (`escalate`, `custom`) is not a case.
        let rows = vec![
            route_row("w1", "Add the log row", None),
            route_row("w2", "Fix the typo", Some("too_short")),
            outcome_row("w1", Some("average")),
            outcome_row("w2", Some("trivial")),
            outcome_row("w3", Some("high")),
            outcome_row("w1", None),
            outcome_row("w4", Some("escalate")),
            outcome_row("w4", Some("custom")),
        ];
        let cases = outcome_cases(&rows);
        assert_eq!(
            cases,
            vec![EvalCase { text: "Add the log row".to_string(), expected: Some("routine".to_string()) }],
            "only a classified route row with a mappable outcome label is a case"
        );
    }

    #[test]
    fn two_label_run_folds_the_missing_rungs() {
        // `routine` and `involved` have no target in the two-label run, so a
        // `hard` answer is the folded expectation — not a miss — while the
        // three-label run counts it wrong.
        let scores = vec![score("add the picker", Some("routine"), Some("hard"), 0.9)];
        let folded = report(&scores, two());
        assert_eq!(folded.labels, 2);
        assert!((folded.default_gate - route_question::default_min_confidence(2)).abs() < 1e-9);
        assert!(folded.table.iter().all(|row| row.accuracy == 1.0));
        let three_labels = report(&scores, three());
        assert!((three_labels.default_gate - route_question::default_min_confidence(3)).abs() < 1e-9);
        assert!((three_labels.row_at(0.75).unwrap().accuracy - 0.0).abs() < 1e-9);
        // The four-label run keeps `routine` offered, so the answer is wrong
        // there too — and it carries its own, lower default gate.
        let four_labels = report(&scores, four());
        assert_eq!(four_labels.labels, 4);
        assert!((four_labels.default_gate - route_question::DEFAULT_MIN_CONFIDENCE_4).abs() < 1e-9);
        assert!((four_labels.row_at(0.75).unwrap().accuracy - 0.0).abs() < 1e-9);
        // An `involved` expectation is itself offered there: a matching answer
        // is right at every gate.
        let scores = vec![score("wire the feature", Some("involved"), Some("involved"), 0.9)];
        let four_labels = report(&scores, four());
        assert!(four_labels.table.iter().all(|row| row.accuracy == 1.0));
    }

    #[test]
    fn report_lists_misses_at_the_default_gate() {
        let scores = vec![
            score("right", Some("routine"), Some("routine"), 0.90),
            score("never routed", Some("trivial"), None, 0.99),
            score("wrong tier", Some("hard"), Some("trivial"), 0.90),
            score("vague routed", None, Some("hard"), 0.90),
            score("vague ignored", None, Some("routine"), 0.10),
        ];
        let report = report(&scores, three());
        let texts: Vec<&str> = report.misses.iter().map(|miss| miss.text.as_str()).collect();
        assert_eq!(texts, vec!["never routed", "wrong tier", "vague routed"]);
        assert_eq!(report.misses[0].got, None);
        assert_eq!(report.misses[1].got.as_deref(), Some("trivial"));
    }

    #[test]
    fn eval_record_uses_the_suggested_gate() {
        let mut scores = Vec::new();
        for index in 0..4 {
            scores.push(score(&format!("right {index}"), Some("routine"), Some("routine"), 0.60));
        }
        for index in 0..4 {
            scores.push(score(&format!("wrong {index}"), Some("routine"), Some("hard"), 0.30));
        }
        let four_labels = report(&scores, four());
        let three_labels = report(&scores, three());
        let two_labels = report(&scores, two());
        let record = eval_record(&four_labels, &three_labels, &two_labels);
        assert_eq!(record.date, today_utc());
        assert_eq!(record.min_confidence_4, four_labels.suggested);
        assert_eq!(record.min_confidence_3, three_labels.suggested);
        assert_eq!(record.min_confidence_2, two_labels.suggested);
        // The share and accuracy are the four-label run's — the question
        // dispatch actually asks.
        let gate = four_labels.suggested.unwrap_or(four_labels.default_gate);
        let row = four_labels.row_at(gate).expect("suggested gate is swept");
        assert_eq!(record.routed_share, Some(row.routed_share));
        assert_eq!(record.accuracy, Some(row.accuracy));
    }

    #[test]
    fn mnemo1_twins_are_plain_language() {
        let cases = vec![EvalCase {
            text: "Add a `routine` model picker to the classifier settings section.".to_string(),
            expected: Some("routine".to_string()),
        }];
        let twins = mnemo1_twins(&cases);
        assert_eq!(
            twins,
            vec![EvalCase {
                text: "Can you Add a routine model picker to the classifier settings section?".to_string(),
                expected: Some("routine".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn classify_corpus_scores_the_top_probability() {
        // A live-shaped answer: the pick's margin-like `confidence` (0.30)
        // sits below the default gate, the top option's probability (0.9113)
        // above it — the eval scores the latter.
        let answer = Answer::Choice {
            label: "hard".to_string(),
            confidence: 0.30,
            probabilities: BTreeMap::from([
                ("trivial".to_string(), 0.0328),
                ("routine".to_string(), 0.0559),
                ("hard".to_string(), 0.9113),
            ]),
        };
        let stub = StubClassifier { answer: Some(answer) };
        let cases = [EvalCase {
            text: "a subtle race in the dispatch pump".to_string(),
            expected: Some("hard".to_string()),
        }];
        let scores = classify_corpus(&stub, three(), &cases).await;
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].label.as_deref(), Some("hard"));
        assert!((scores[0].confidence.expect("confidence") - 0.9113).abs() < 1e-9);
        assert!(scores[0].routed_at(route_question::DEFAULT_MIN_CONFIDENCE_3));

        // A non-choice answer is still no answer at all.
        let scored = StubClassifier {
            answer: Some(Answer::Score { value: 1.0, confidence: 0.99 }),
        };
        let scores = classify_corpus(&scored, three(), &cases).await;
        assert_eq!(scores[0].label, None);
        assert_eq!(scores[0].confidence, None);
    }

    #[test]
    fn report_with_knn_keeps_the_laya_table_and_attaches_the_overlay() {
        let scores = vec![score("right", Some("routine"), Some("routine"), 0.90)];
        let overlay = KnnEvalReport {
            indexed_rows: 30,
            answered: 1,
            correct: 1,
            accuracy: 1.0,
            laya_routed: 1,
            laya_correct: 1,
            laya_accuracy: 1.0,
            combined_answered: 1,
            combined_correct: 1,
            combined_accuracy: 1.0,
        };
        let with = report_with_knn(&scores, three(), Some(overlay.clone()));
        let without = report(&scores, three());
        assert_eq!(with.table, without.table);
        assert_eq!(with.knn, Some(overlay));
    }

    struct ColdEmbedder;

    #[async_trait]
    impl mnemo::memory::embedder::Embedder for ColdEmbedder {
        async fn embed(&self, _text: &str) -> Vec<f32> {
            vec![0.0; 4]
        }

        fn model_id(&self) -> &str {
            "cold"
        }
    }

    #[tokio::test]
    async fn evaluate_knn_reports_laya_only_when_the_overlay_is_cold() {
        use std::sync::Arc;

        use mnemo::agent::failure_triage_knn::EmbedderGetter;
        use mnemo::agent::route_knn::RouteKnn;
        use mnemo::memory::embedder::Embedder;

        let getter: EmbedderGetter = Arc::new(|| {
            Arc::new(ColdEmbedder) as Arc<dyn Embedder>
        });
        let dir = tempfile::tempdir().expect("tempdir");
        let knn = RouteKnn::new(getter, dir.path().join("missing.jsonl"));
        let cases = vec![EvalCase {
            text: "a routine step".to_string(),
            expected: Some("routine".to_string()),
        }];
        let scores = vec![score("a routine step", Some("routine"), Some("routine"), 0.90)];
        let report = evaluate_knn(&cases, &scores, three(), &knn, 30, 0.80).await;
        assert_eq!(report.indexed_rows, 0);
        assert_eq!(report.answered, 0);
        assert_eq!(report.laya_routed, 1);
        assert_eq!(report.laya_correct, 1);
        assert_eq!(report.combined_answered, 1);
        assert_eq!(report.combined_correct, 1);
        assert!((report.combined_accuracy - 1.0).abs() < 1e-9);
    }
}
