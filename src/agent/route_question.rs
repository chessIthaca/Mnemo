// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The shared Laya routing question (plan 2ba2168a, design §4.3).
//!
//! Both routing surfaces — Mnemo2's per-dispatch tier choice and Mnemo1's
//! once-per-session route — ask Laya the SAME calibrated choice question over
//! four neutral labels:
//!
//! - [`LABEL_TRIVIAL`] — a mechanical edit ([`DESCRIBE_TRIVIAL`]);
//! - [`LABEL_ROUTINE`] — specified implementation work ([`DESCRIBE_ROUTINE`]);
//! - [`LABEL_INVOLVED`] — a normal multi-file change with a clear spec
//!   ([`DESCRIBE_INVOLVED`]);
//! - [`LABEL_HARD`] — work with a subtle failure mode, a shared contract or a
//!   design decision the spec leaves open ([`DESCRIBE_HARD`]).
//!
//! The labels are deliberately neutral English — never model-quality
//! adjectives, and never the harder tier ids they map onto (`medium` /
//! `high`; see [`tier_for_label`]) — because laya-router found that model- or
//! tier-shaped names skew the checkpoint; they are also never yes/no-ish
//! words (see `Question::Choice`'s docs in `crate::memory::classifier`). The
//! text is **byte-stable**: the eval that calibrates the gates (design §4.8)
//! and both consumers must ask byte-identical questions, so changing one of
//! these constants means re-running that eval.
//!
//! # Folding, and the per-count gate
//!
//! A consumer offers a label only when it has somewhere to route it
//! ([`OfferedLabels`]). An un-offered label's description is folded into the
//! nearest offered label — **up** first, so a dropped easy or middle label's
//! work is judged by the next harder label still offered, and **down** only
//! for [`LABEL_HARD`], whose work lands on the hardest label still offered
//! below it. An offered label's own description always comes first, followed
//! by `", "` and the folded text. Fewer than two offered labels means no
//! question at all ([`route_question`] returns `None`): there is nothing to
//! choose between.
//!
//! The gate depends on how many labels the question offered:
//! [`DEFAULT_MIN_CONFIDENCE_4`] for four, [`DEFAULT_MIN_CONFIDENCE_3`] for
//! three, [`DEFAULT_MIN_CONFIDENCE_2`] for two ([`default_min_confidence`]).
//! All three defaults are provisional until the eval sets this checkpoint's
//! measured values.
//!
//! # Nudge support
//!
//! A routed session is never switched after it starts; it may only be nudged
//! once when the work visibly goes sideways. [`call_signature`] is the
//! canonical fingerprint of one tool call (name + arguments, with JSON key
//! order and the provider's call ids ignored) and [`Strikes`] counts
//! consecutive failed or repeated calls, firing exactly once per streak when
//! the count reaches its limit. Both live here so Mnemo2's worker pump and
//! Mnemo1's turn loop share one definition.

use std::collections::BTreeMap;

use crate::memory::classifier::{Answer, Question};

/// The label for a small, local, mechanical change.
pub const LABEL_TRIVIAL: &str = "trivial";

/// The label for specified implementation work that follows an existing
/// pattern.
pub const LABEL_ROUTINE: &str = "routine";

/// The label for a normal multi-file change with a clear spec — the
/// `medium` tier's work (design §3.1).
pub const LABEL_INVOLVED: &str = "involved";

/// The label for work with a subtle failure mode, a shared contract or an
/// open design decision.
pub const LABEL_HARD: &str = "hard";

/// The question shown to Laya. One shared string, so Mnemo1 and Mnemo2 ask
/// byte-identical questions.
pub const ROUTE_INSTRUCTIONS: &str =
    "How demanding is this task for a coding agent that has the full spec in front of it?";

/// What [`LABEL_TRIVIAL`] covers.
pub const DESCRIBE_TRIVIAL: &str = "rename, typo, version bump, config value, copy change, add a log line, delete dead code, update a doc comment, single-file mechanical edit";

/// What [`LABEL_ROUTINE`] covers.
pub const DESCRIBE_ROUTINE: &str = "implement a specified function, add tests for existing code, wire an existing API into a new caller, UI component from a spec, schema or DTO field, follow an existing pattern in a new place, refactor inside one module";

/// What [`LABEL_INVOLVED`] covers — written from the `medium` tier's
/// definition (design §3.1) and byte-stable like every other description:
/// the question text never follows user-edited tier names or definitions.
pub const DESCRIBE_INVOLVED: &str = "multi-file change with a clear spec, feature across a few modules, endpoint with its tests and UI, migration of call sites, non-trivial refactor";

/// What [`LABEL_HARD`] covers.
pub const DESCRIBE_HARD: &str = "subtle bug, race or deadlock, flaky test, data loss, memory or performance regression, security, persistence format, cross-module or shared-contract change, concurrency, migration, design decision the spec leaves open";

/// The default confidence gate for a four-label question. Provisional until
/// the eval runs (design §5.1); the 4-label top probability has a floor of
/// 0.25, so it needs its own gate next to the 3- and 2-label ones.
pub const DEFAULT_MIN_CONFIDENCE_4: f64 = 0.40;

/// The default confidence gate for a three-label question. Provisional until
/// the eval runs (design §4.4).
pub const DEFAULT_MIN_CONFIDENCE_3: f64 = 0.475;

/// The default confidence gate for a two-label question. Provisional until
/// the eval runs (design §4.4).
pub const DEFAULT_MIN_CONFIDENCE_2: f64 = 0.60;

/// The four labels in ascending difficulty order, each with the description
/// Laya reads for it.
const LABEL_LADDER: [(&str, &str); 4] = [
    (LABEL_TRIVIAL, DESCRIBE_TRIVIAL),
    (LABEL_ROUTINE, DESCRIBE_ROUTINE),
    (LABEL_INVOLVED, DESCRIBE_INVOLVED),
    (LABEL_HARD, DESCRIBE_HARD),
];

/// Which of the four labels have somewhere to route to.
///
/// A label is offered only when its target resolves to a live endpoint; an
/// un-offered label's description is folded into a neighbour by
/// [`route_question`]. All four are offered whenever routing runs on the
/// tier ladder: an empty tier folds down to the tier below it at run time
/// (design §3.2), so its label still has somewhere to route to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferedLabels {
    /// [`LABEL_TRIVIAL`] has a target.
    pub trivial: bool,
    /// [`LABEL_ROUTINE`] has a target.
    pub routine: bool,
    /// [`LABEL_INVOLVED`] has a target.
    pub involved: bool,
    /// [`LABEL_HARD`] has a target.
    pub hard: bool,
}

impl OfferedLabels {
    /// How many labels have somewhere to route to.
    pub fn count(self) -> usize {
        usize::from(self.trivial)
            + usize::from(self.routine)
            + usize::from(self.involved)
            + usize::from(self.hard)
    }
}

/// The choice question over the offered labels, with folding. `None` when
/// fewer than two labels are offered — there is nothing to choose between.
///
/// An offered label's criteria entry starts with its own description; an
/// un-offered label's description is appended to the nearest offered label
/// (`", "` + the folded text), so no description is silently dropped.
pub fn route_question(offered: OfferedLabels) -> Option<Question> {
    if offered.count() < 2 {
        return None;
    }
    let mut criteria = BTreeMap::new();
    for (label, description) in LABEL_LADDER {
        if !is_offered(label, offered) {
            continue;
        }
        let mut text = description.to_string();
        for (dropped, dropped_description) in LABEL_LADDER {
            if is_offered(dropped, offered) || fold_target(dropped, offered) != Some(label) {
                continue;
            }
            text.push_str(", ");
            text.push_str(dropped_description);
        }
        criteria.insert(label.to_string(), text);
    }
    Some(Question::Choice { instructions: ROUTE_INSTRUCTIONS.to_string(), criteria })
}

/// Whether `label` is one of the four labels and is offered.
fn is_offered(label: &str, offered: OfferedLabels) -> bool {
    if label == LABEL_TRIVIAL {
        offered.trivial
    } else if label == LABEL_ROUTINE {
        offered.routine
    } else if label == LABEL_INVOLVED {
        offered.involved
    } else if label == LABEL_HARD {
        offered.hard
    } else {
        false
    }
}

/// Where `label` sits on the ladder (0 `trivial`, 1 `routine`, 2 `involved`,
/// 3 `hard`), or `None` for a label the ladder does not know.
fn ladder_rank(label: &str) -> Option<usize> {
    LABEL_LADDER.iter().position(|(candidate, _)| *candidate == label)
}

/// The offered label an un-offered label's description folds into: the
/// nearest offered label above it, else the hardest offered label below it.
fn fold_target(label: &str, offered: OfferedLabels) -> Option<&'static str> {
    let rank = ladder_rank(label)?;
    // Up first (design §4.3: a dropped label's work is judged by the next
    // harder label still offered), so a dropped `routine` folds into `hard`.
    if let Some((candidate, _)) = LABEL_LADDER
        .into_iter()
        .skip(rank + 1)
        .find(|(candidate, _)| is_offered(candidate, offered))
    {
        return Some(candidate);
    }
    // Nothing above is offered: fold down onto the hardest label still
    // offered below (the dropped `hard` case).
    LABEL_LADDER
        .into_iter()
        .take(rank)
        .rev()
        .find(|(candidate, _)| is_offered(candidate, offered))
        .map(|(candidate, _)| candidate)
}

/// The default gate for an offered-label count, one gate per count: four
/// labels use [`DEFAULT_MIN_CONFIDENCE_4`], three [`DEFAULT_MIN_CONFIDENCE_3`],
/// every other count the stricter two-label gate
/// [`DEFAULT_MIN_CONFIDENCE_2`] (callers only pass 2, 3 or 4 —
/// [`route_question`] returns `None` below two).
pub fn default_min_confidence(labels: usize) -> f64 {
    match labels {
        4 => DEFAULT_MIN_CONFIDENCE_4,
        3 => DEFAULT_MIN_CONFIDENCE_3,
        _ => DEFAULT_MIN_CONFIDENCE_2,
    }
}

/// The confidence a routing gate reads from one Laya answer: for a choice,
/// the largest per-option probability when the backend reported a
/// distribution, else the pick's own calibrated [`Answer::confidence`];
/// every other primitive keeps its [`Answer::confidence`] as-is.
///
/// A live `/v1/systemone` choice answer carries both numbers at once: the
/// top value of the `probabilities` map — Laya's `answer_confidence` —
/// which the routing gates ([`DEFAULT_MIN_CONFIDENCE_4`] /
/// [`DEFAULT_MIN_CONFIDENCE_3`] / [`DEFAULT_MIN_CONFIDENCE_2`]) are
/// calibrated for, and the lower,
/// margin-like `confidence` on the pick. Routing gates on the top
/// probability; other consumers (failure triage, memory auto-typing, tool
/// choice) are calibrated on [`Answer::confidence`] and keep reading that.
pub fn answer_confidence(answer: &Answer) -> f64 {
    match answer {
        Answer::Choice { confidence, probabilities, .. } => probabilities
            .values()
            .copied()
            .max_by(|a, b| a.total_cmp(b))
            .unwrap_or(*confidence),
        other => other.confidence(),
    }
}

/// The label one level harder, if offered (`trivial` → `routine` →
/// `involved` → `hard`), skipping un-offered labels. `None` for
/// [`LABEL_HARD`], an unknown label, or when nothing harder is offered.
pub fn label_above(label: &str, offered: OfferedLabels) -> Option<&'static str> {
    let rank = ladder_rank(label)?;
    LABEL_LADDER
        .into_iter()
        .skip(rank + 1)
        .find(|(candidate, _)| is_offered(candidate, offered))
        .map(|(candidate, _)| candidate)
}

/// The tier id a Laya label routes to (design §3.1): `trivial` → `trivial`,
/// `routine` → `routine`, `involved` → `medium`, `hard` → `high`. `None` for
/// anything else — `escalate` has no label (it is never a first pick, so
/// neither Laya nor the kNN is ever offered it) and unknown strings do not
/// route. This is the ONLY label↔tier mapping in the codebase (interface
/// contract, 2026-11-04): consumers map through it, never around it.
pub fn tier_for_label(label: &str) -> Option<&'static str> {
    if label == LABEL_TRIVIAL {
        Some("trivial")
    } else if label == LABEL_ROUTINE {
        Some("routine")
    } else if label == LABEL_INVOLVED {
        Some("medium")
    } else if label == LABEL_HARD {
        Some("high")
    } else {
        None
    }
}

/// The Laya label offered for a tier id — the inverse of [`tier_for_label`]:
/// `trivial` → `trivial`, `routine` → `routine`, `medium` → `involved`,
/// `high` → `hard`. `None` for `escalate` (never offered to Laya or the
/// kNN) and for unknown ids.
pub fn label_for_tier(tier_id: &str) -> Option<&'static str> {
    if tier_id == "trivial" {
        Some(LABEL_TRIVIAL)
    } else if tier_id == "routine" {
        Some(LABEL_ROUTINE)
    } else if tier_id == "medium" {
        Some(LABEL_INVOLVED)
    } else if tier_id == "high" {
        Some(LABEL_HARD)
    } else {
        None
    }
}

/// Canonical tool-call signature for strike counting: `{name, args}` with
/// every object's keys sorted (recursively) and any `id` / `tool_use_id` /
/// `call_id` key dropped, so a provider's call ids and JSON key order cannot
/// make two identical calls look different.
pub fn call_signature(name: &str, args: &serde_json::Value) -> String {
    format!("{name} {}", canonical_value(args))
}

/// The provider's per-call id keys, dropped from every object so a call id
/// never makes two otherwise-identical calls look different.
const CALL_ID_KEYS: [&str; 3] = ["id", "tool_use_id", "call_id"];

/// Rebuild `value` with every object's keys sorted and [`CALL_ID_KEYS`]
/// dropped, recursively (arrays keep their order).
fn canonical_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> = map
                .iter()
                .filter(|(key, _)| !CALL_ID_KEYS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), canonical_value(value)))
                .collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_value).collect())
        }
        other => other.clone(),
    }
}

/// Consecutive failed-or-repeated tool calls: the strike counter behind the
/// one-shot "this session looks stuck" nudge.
///
/// A strike is a tool call that failed, or that repeats the previous call's
/// [`call_signature`]; any successful non-repeated call ends the streak.
/// [`observe`](Self::observe) returns `true` exactly once per streak, on the
/// call that brings the count to `limit` — never again on the ones after it
/// until the streak ends (or [`reset`](Self::reset) is called).
#[derive(Debug, Clone)]
pub struct Strikes {
    count: u32,
    last: Option<String>,
    limit: u32,
}

impl Strikes {
    /// A counter that fires on the `limit`-th consecutive strike.
    pub fn new(limit: u32) -> Self {
        Self { count: 0, last: None, limit }
    }

    /// Observe one tool call: its [`call_signature`] and whether it failed.
    /// Returns `true` exactly once per streak, on the call that brings the
    /// count to the limit.
    pub fn observe(&mut self, signature: String, failed: bool) -> bool {
        let repeated = self.last.as_deref() == Some(signature.as_str());
        if failed || repeated {
            self.count += 1;
        } else {
            self.count = 0;
        }
        self.last = Some(signature);
        self.count == self.limit
    }

    /// Start over: the streak and the previous call are both forgotten, so
    /// the next call is never a repeat of a pre-reset one.
    pub fn reset(&mut self) {
        self.count = 0;
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn all_offered() -> OfferedLabels {
        OfferedLabels { trivial: true, routine: true, involved: true, hard: true }
    }

    fn criteria_of(offered: OfferedLabels) -> BTreeMap<String, String> {
        match route_question(offered).expect("a question for two or more labels") {
            Question::Choice { criteria, .. } => criteria,
            other => panic!("the routing question must be a choice, got {other:?}"),
        }
    }

    #[test]
    fn question_has_four_neutral_labels_when_all_are_offered() {
        let offered = all_offered();
        assert_eq!(offered.count(), 4);
        let Question::Choice { instructions, criteria } =
            route_question(offered).expect("four labels offered")
        else {
            panic!("the routing question must be a choice");
        };
        assert_eq!(instructions, ROUTE_INSTRUCTIONS);
        assert_eq!(criteria.len(), 4);
        // Every label carries exactly its own byte-stable description — no
        // folding salt when all four are offered.
        assert_eq!(criteria.get(LABEL_TRIVIAL).map(String::as_str), Some(DESCRIBE_TRIVIAL));
        assert_eq!(criteria.get(LABEL_ROUTINE).map(String::as_str), Some(DESCRIBE_ROUTINE));
        assert_eq!(criteria.get(LABEL_INVOLVED).map(String::as_str), Some(DESCRIBE_INVOLVED));
        assert_eq!(criteria.get(LABEL_HARD).map(String::as_str), Some(DESCRIBE_HARD));
        // Neutral labels only: never the tier ids the harder rungs map onto,
        // never model-quality adjectives (design §4.3).
        for forbidden in ["simple", "average", "difficult", "medium", "high", "escalate", "cheap", "capable"] {
            assert!(!criteria.contains_key(forbidden), "{forbidden} must not be a label");
        }
    }

    #[test]
    fn unoffered_hard_folds_down_into_involved() {
        let criteria = criteria_of(OfferedLabels {
            trivial: true,
            routine: true,
            involved: true,
            hard: false,
        });
        assert_eq!(criteria.len(), 3);
        assert_eq!(criteria[LABEL_TRIVIAL].as_str(), DESCRIBE_TRIVIAL);
        assert_eq!(criteria[LABEL_ROUTINE].as_str(), DESCRIBE_ROUTINE);
        // A dropped `hard` folds DOWN onto the hardest label still offered
        // below it (design §4.3): `involved` judges it.
        assert_eq!(
            criteria[LABEL_INVOLVED].as_str(),
            format!("{DESCRIBE_INVOLVED}, {DESCRIBE_HARD}")
        );
    }

    #[test]
    fn unoffered_trivial_folds_up_into_routine() {
        let criteria = criteria_of(OfferedLabels {
            trivial: false,
            routine: true,
            involved: true,
            hard: true,
        });
        assert_eq!(criteria.len(), 3);
        assert_eq!(
            criteria[LABEL_ROUTINE].as_str(),
            format!("{DESCRIBE_ROUTINE}, {DESCRIBE_TRIVIAL}")
        );
        assert_eq!(criteria[LABEL_INVOLVED].as_str(), DESCRIBE_INVOLVED);
        assert_eq!(criteria[LABEL_HARD].as_str(), DESCRIBE_HARD);
    }

    #[test]
    fn unoffered_routine_folds_up_into_involved() {
        // A dropped middle label goes to the next label UP (design §4.3), so
        // `routine` work is judged by `involved` when `routine` has no target.
        let criteria = criteria_of(OfferedLabels {
            trivial: true,
            routine: false,
            involved: true,
            hard: true,
        });
        assert_eq!(criteria.len(), 3);
        assert_eq!(criteria[LABEL_TRIVIAL].as_str(), DESCRIBE_TRIVIAL);
        assert_eq!(
            criteria[LABEL_INVOLVED].as_str(),
            format!("{DESCRIBE_INVOLVED}, {DESCRIBE_ROUTINE}")
        );
        assert_eq!(criteria[LABEL_HARD].as_str(), DESCRIBE_HARD);
    }

    #[test]
    fn two_extreme_labels_fold_both_middles_into_hard() {
        // The eval's two-label run: only `trivial` and `hard` are offered, so
        // both middle labels fold UP into `hard`, in ladder order.
        let criteria = criteria_of(OfferedLabels {
            trivial: true,
            routine: false,
            involved: false,
            hard: true,
        });
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[LABEL_TRIVIAL].as_str(), DESCRIBE_TRIVIAL);
        assert_eq!(
            criteria[LABEL_HARD].as_str(),
            format!("{DESCRIBE_HARD}, {DESCRIBE_ROUTINE}, {DESCRIBE_INVOLVED}")
        );
    }

    #[test]
    fn one_offered_label_asks_nothing() {
        let singles = [
            OfferedLabels { trivial: true, routine: false, involved: false, hard: false },
            OfferedLabels { trivial: false, routine: true, involved: false, hard: false },
            OfferedLabels { trivial: false, routine: false, involved: true, hard: false },
            OfferedLabels { trivial: false, routine: false, involved: false, hard: true },
        ];
        for offered in singles {
            assert_eq!(offered.count(), 1);
            assert!(route_question(offered).is_none(), "one label leaves nothing to choose");
        }
        let none = OfferedLabels { trivial: false, routine: false, involved: false, hard: false };
        assert_eq!(none.count(), 0);
        assert!(route_question(none).is_none());
    }

    #[test]
    fn default_min_confidence_by_label_count() {
        assert_eq!(default_min_confidence(4), DEFAULT_MIN_CONFIDENCE_4);
        assert_eq!(default_min_confidence(3), DEFAULT_MIN_CONFIDENCE_3);
        assert_eq!(default_min_confidence(2), DEFAULT_MIN_CONFIDENCE_2);
        // All three are provisional (design §5.1): pin the shipped values so
        // the eval that replaces them is a deliberate edit.
        assert_eq!(DEFAULT_MIN_CONFIDENCE_4, 0.40);
        assert_eq!(DEFAULT_MIN_CONFIDENCE_3, 0.475);
        assert_eq!(DEFAULT_MIN_CONFIDENCE_2, 0.60);
    }

    #[test]
    fn label_above_skips_unoffered_labels() {
        let all = all_offered();
        assert_eq!(label_above(LABEL_TRIVIAL, all), Some(LABEL_ROUTINE));
        assert_eq!(label_above(LABEL_ROUTINE, all), Some(LABEL_INVOLVED));
        assert_eq!(label_above(LABEL_INVOLVED, all), Some(LABEL_HARD));
        assert_eq!(label_above(LABEL_HARD, all), None);

        let no_involved = OfferedLabels {
            trivial: true,
            routine: true,
            involved: false,
            hard: true,
        };
        assert_eq!(label_above(LABEL_ROUTINE, no_involved), Some(LABEL_HARD));
        assert_eq!(label_above(LABEL_INVOLVED, no_involved), Some(LABEL_HARD));

        let only_trivial =
            OfferedLabels { trivial: true, routine: false, involved: false, hard: false };
        assert_eq!(label_above(LABEL_TRIVIAL, only_trivial), None);
        assert_eq!(label_above("unknown", all), None);
    }

    #[test]
    fn labels_and_tier_ids_map_both_ways() {
        // The four labels map to their four pickable tiers (interface
        // contract, 2026-11-04) and back, round-trip total.
        for (label, tier) in [
            (LABEL_TRIVIAL, "trivial"),
            (LABEL_ROUTINE, "routine"),
            (LABEL_INVOLVED, "medium"),
            (LABEL_HARD, "high"),
        ] {
            assert_eq!(tier_for_label(label), Some(tier));
            assert_eq!(label_for_tier(tier), Some(label));
        }
        // `escalate` has no label in either direction: it is never a first
        // pick, so Laya and the kNN are never offered it.
        assert_eq!(label_for_tier("escalate"), None);
        assert_eq!(tier_for_label("escalate"), None);
        // Legacy ids and unknown strings do not map.
        for stray in ["simple", "average", "difficult", "custom", ""] {
            assert_eq!(tier_for_label(stray), None);
            assert_eq!(label_for_tier(stray), None);
        }
    }

    #[test]
    fn answer_confidence_reads_the_top_probability() {
        // The live `/v1/systemone` shape: the pick's `confidence` is a lower,
        // margin-like score while the top option's probability is the number
        // the gates are calibrated on.
        let answer = Answer::Choice {
            label: LABEL_HARD.to_string(),
            confidence: 0.6742,
            probabilities: BTreeMap::from([
                (LABEL_TRIVIAL.to_string(), 0.0328),
                (LABEL_ROUTINE.to_string(), 0.0559),
                (LABEL_HARD.to_string(), 0.9113),
            ]),
        };
        assert!((answer_confidence(&answer) - 0.9113).abs() < 1e-9);

        // The maximum may sit on any option, not the pick alone.
        let answered = Answer::Choice {
            label: LABEL_ROUTINE.to_string(),
            confidence: 0.42,
            probabilities: BTreeMap::from([
                (LABEL_TRIVIAL.to_string(), 0.91),
                (LABEL_ROUTINE.to_string(), 0.05),
                (LABEL_HARD.to_string(), 0.04),
            ]),
        };
        assert!((answer_confidence(&answered) - 0.91).abs() < 1e-9);

        // An empty distribution falls back to the pick's own confidence.
        let bare = Answer::Choice {
            label: LABEL_HARD.to_string(),
            confidence: 0.6742,
            probabilities: BTreeMap::new(),
        };
        assert!((answer_confidence(&bare) - 0.6742).abs() < 1e-9);

        // Other primitives keep their calibrated confidence.
        let scored = Answer::Score { value: 2.0, confidence: 0.42 };
        assert!((answer_confidence(&scored) - 0.42).abs() < 1e-9);
        let noul = Answer::NoUl { probability: 0.7 };
        assert!((answer_confidence(&noul) - 0.7).abs() < 1e-9);
    }

    #[test]
    fn call_signature_ignores_ids_and_key_order() {
        let one = json!({
            "command": "cargo test",
            "id": "call-1",
            "options": {"b": 2, "a": 1, "tool_use_id": "call-1"},
        });
        let other = json!({
            "options": {"a": 1, "b": 2, "call_id": "call-9"},
            "command": "cargo test",
        });
        assert_eq!(call_signature("run_shell", &one), call_signature("run_shell", &other));
        // The name and the real arguments still tell calls apart.
        assert_ne!(call_signature("run_shell", &one), call_signature("read_files", &one));
        assert_ne!(
            call_signature("run_shell", &one),
            call_signature("run_shell", &json!({"command": "cargo build"}))
        );
    }

    #[test]
    fn strikes_fire_once_at_three_failures() {
        let mut strikes = Strikes::new(3);
        assert!(!strikes.observe("call-a".into(), true));
        assert!(!strikes.observe("call-a".into(), true));
        assert!(strikes.observe("call-a".into(), true), "the third consecutive failure fires");
        assert!(!strikes.observe("call-a".into(), true), "fires once per streak, not every call");
        assert!(!strikes.observe("call-a".into(), true));
    }

    #[test]
    fn identical_calls_count_as_strikes() {
        let mut strikes = Strikes::new(3);
        // The first call has no predecessor to repeat; each identical call
        // after it is one strike.
        assert!(!strikes.observe("same".into(), false));
        assert!(!strikes.observe("same".into(), false));
        assert!(!strikes.observe("same".into(), false));
        assert!(strikes.observe("same".into(), false), "the third repeat fires");
    }

    #[test]
    fn a_new_successful_call_resets() {
        let mut strikes = Strikes::new(3);
        assert!(!strikes.observe("call-a".into(), true));
        assert!(!strikes.observe("call-a".into(), true));
        // A different call that succeeded ends the streak.
        assert!(!strikes.observe("call-b".into(), false));
        assert!(!strikes.observe("call-a".into(), true));
        assert!(!strikes.observe("call-a".into(), true));
        assert!(strikes.observe("call-a".into(), true), "a fresh streak fires at three again");
    }

    #[test]
    fn reset_forgets_the_streak_and_the_previous_call() {
        let mut strikes = Strikes::new(3);
        assert!(!strikes.observe("call-a".into(), true));
        assert!(!strikes.observe("call-a".into(), true));
        strikes.reset();
        // After a reset the next call is not a repeat of the pre-reset one,
        // and the streak starts from zero.
        assert!(!strikes.observe("call-a".into(), false));
        assert!(!strikes.observe("call-a".into(), false));
        assert!(!strikes.observe("call-a".into(), false));
        assert!(strikes.observe("call-a".into(), false));
    }
}
