// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The shared Laya routing question (plan 2ba2168a, design §4.3).
//!
//! Both routing surfaces — Mnemo2's per-dispatch tier choice and Mnemo1's
//! once-per-session route — ask Laya the SAME calibrated choice question over
//! three neutral labels:
//!
//! - [`LABEL_TRIVIAL`] — a mechanical edit ([`DESCRIBE_TRIVIAL`]);
//! - [`LABEL_ROUTINE`] — specified implementation work ([`DESCRIBE_ROUTINE`]);
//! - [`LABEL_HARD`] — work with a subtle failure mode, a shared contract or a
//!   design decision the spec leaves open ([`DESCRIBE_HARD`]).
//!
//! The labels are deliberately neutral English, never the tier names the
//! router maps them onto (`simple` / `average` / `difficult`) and never
//! model-quality adjectives, because laya-router found that model- or
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
//! [`DEFAULT_MIN_CONFIDENCE_3`] for three, [`DEFAULT_MIN_CONFIDENCE_2`] for
//! two ([`default_min_confidence`]). Both defaults are provisional until the
//! eval sets this checkpoint's measured values.
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

use crate::memory::classifier::Question;

/// The label for a small, local, mechanical change.
pub const LABEL_TRIVIAL: &str = "trivial";

/// The label for specified implementation work that follows an existing
/// pattern.
pub const LABEL_ROUTINE: &str = "routine";

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

/// What [`LABEL_HARD`] covers.
pub const DESCRIBE_HARD: &str = "subtle bug, race or deadlock, flaky test, data loss, memory or performance regression, security, persistence format, cross-module or shared-contract change, concurrency, migration, design decision the spec leaves open";

/// The default confidence gate for a three-label question. Provisional until
/// the eval runs (design §4.4).
pub const DEFAULT_MIN_CONFIDENCE_3: f64 = 0.475;

/// The default confidence gate for a two-label question. Provisional until
/// the eval runs (design §4.4).
pub const DEFAULT_MIN_CONFIDENCE_2: f64 = 0.60;

/// The three labels in ascending difficulty order, each with the description
/// Laya reads for it.
const LABEL_LADDER: [(&str, &str); 3] = [
    (LABEL_TRIVIAL, DESCRIBE_TRIVIAL),
    (LABEL_ROUTINE, DESCRIBE_ROUTINE),
    (LABEL_HARD, DESCRIBE_HARD),
];

/// Which of the three labels have somewhere to route to.
///
/// A label is offered only when its target resolves to a live endpoint; an
/// un-offered label's description is folded into a neighbour by
/// [`route_question`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferedLabels {
    /// [`LABEL_TRIVIAL`] has a target.
    pub trivial: bool,
    /// [`LABEL_ROUTINE`] has a target.
    pub routine: bool,
    /// [`LABEL_HARD`] has a target.
    pub hard: bool,
}

impl OfferedLabels {
    /// How many labels have somewhere to route to.
    pub fn count(self) -> usize {
        usize::from(self.trivial) + usize::from(self.routine) + usize::from(self.hard)
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

/// Whether `label` is one of the three labels and is offered.
fn is_offered(label: &str, offered: OfferedLabels) -> bool {
    if label == LABEL_TRIVIAL {
        offered.trivial
    } else if label == LABEL_ROUTINE {
        offered.routine
    } else if label == LABEL_HARD {
        offered.hard
    } else {
        false
    }
}

/// Where `label` sits on the ladder (0 `trivial`, 1 `routine`, 2 `hard`),
/// or `None` for a label the ladder does not know.
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

/// The default gate for an offered-label count: three labels use
/// [`DEFAULT_MIN_CONFIDENCE_3`], every other count the stricter two-label
/// gate [`DEFAULT_MIN_CONFIDENCE_2`] (callers only pass 2 or 3 —
/// [`route_question`] returns `None` below two).
pub fn default_min_confidence(labels: usize) -> f64 {
    if labels >= 3 {
        DEFAULT_MIN_CONFIDENCE_3
    } else {
        DEFAULT_MIN_CONFIDENCE_2
    }
}

/// The label one level harder, if offered (`trivial` → `routine` → `hard`),
/// skipping un-offered labels. `None` for [`LABEL_HARD`], an unknown label,
/// or when nothing harder is offered.
pub fn label_above(label: &str, offered: OfferedLabels) -> Option<&'static str> {
    let rank = ladder_rank(label)?;
    LABEL_LADDER
        .into_iter()
        .skip(rank + 1)
        .find(|(candidate, _)| is_offered(candidate, offered))
        .map(|(candidate, _)| candidate)
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
        OfferedLabels { trivial: true, routine: true, hard: true }
    }

    fn criteria_of(offered: OfferedLabels) -> BTreeMap<String, String> {
        match route_question(offered).expect("a question for two or more labels") {
            Question::Choice { criteria, .. } => criteria,
            other => panic!("the routing question must be a choice, got {other:?}"),
        }
    }

    #[test]
    fn question_has_three_neutral_labels_when_all_are_offered() {
        let offered = all_offered();
        assert_eq!(offered.count(), 3);
        let Question::Choice { instructions, criteria } =
            route_question(offered).expect("three labels offered")
        else {
            panic!("the routing question must be a choice");
        };
        assert_eq!(instructions, ROUTE_INSTRUCTIONS);
        assert_eq!(criteria.len(), 3);
        // Every label carries exactly its own byte-stable description — no
        // folding salt when all three are offered.
        assert_eq!(criteria.get(LABEL_TRIVIAL).map(String::as_str), Some(DESCRIBE_TRIVIAL));
        assert_eq!(criteria.get(LABEL_ROUTINE).map(String::as_str), Some(DESCRIBE_ROUTINE));
        assert_eq!(criteria.get(LABEL_HARD).map(String::as_str), Some(DESCRIBE_HARD));
        // Neutral labels only: never the tier names the router maps onto,
        // never model-quality adjectives (design §4.3).
        for forbidden in ["simple", "average", "difficult", "cheap", "capable"] {
            assert!(!criteria.contains_key(forbidden), "{forbidden} must not be a label");
        }
    }

    #[test]
    fn unoffered_hard_folds_down_into_routine() {
        let criteria = criteria_of(OfferedLabels { trivial: true, routine: true, hard: false });
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[LABEL_TRIVIAL].as_str(), DESCRIBE_TRIVIAL);
        assert_eq!(
            criteria[LABEL_ROUTINE].as_str(),
            format!("{DESCRIBE_ROUTINE}, {DESCRIBE_HARD}")
        );
    }

    #[test]
    fn unoffered_trivial_folds_up_into_routine() {
        let criteria = criteria_of(OfferedLabels { trivial: false, routine: true, hard: true });
        assert_eq!(criteria.len(), 2);
        assert_eq!(
            criteria[LABEL_ROUTINE].as_str(),
            format!("{DESCRIBE_ROUTINE}, {DESCRIBE_TRIVIAL}")
        );
        assert_eq!(criteria[LABEL_HARD].as_str(), DESCRIBE_HARD);
    }

    #[test]
    fn unoffered_routine_folds_up_into_hard() {
        // A dropped middle label goes to the next label UP (design §4.3), so
        // `routine` work is judged by `hard` when `routine` has no target.
        let criteria = criteria_of(OfferedLabels { trivial: true, routine: false, hard: true });
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[LABEL_TRIVIAL].as_str(), DESCRIBE_TRIVIAL);
        assert_eq!(
            criteria[LABEL_HARD].as_str(),
            format!("{DESCRIBE_HARD}, {DESCRIBE_ROUTINE}")
        );
    }

    #[test]
    fn one_offered_label_asks_nothing() {
        let singles = [
            OfferedLabels { trivial: true, routine: false, hard: false },
            OfferedLabels { trivial: false, routine: true, hard: false },
            OfferedLabels { trivial: false, routine: false, hard: true },
        ];
        for offered in singles {
            assert_eq!(offered.count(), 1);
            assert!(route_question(offered).is_none(), "one label leaves nothing to choose");
        }
        let none = OfferedLabels { trivial: false, routine: false, hard: false };
        assert_eq!(none.count(), 0);
        assert!(route_question(none).is_none());
    }

    #[test]
    fn default_min_confidence_by_label_count() {
        assert_eq!(default_min_confidence(3), DEFAULT_MIN_CONFIDENCE_3);
        assert_eq!(default_min_confidence(2), DEFAULT_MIN_CONFIDENCE_2);
        // Both are provisional (design §4.4): pin the shipped values so the
        // eval that replaces them is a deliberate edit.
        assert_eq!(DEFAULT_MIN_CONFIDENCE_3, 0.475);
        assert_eq!(DEFAULT_MIN_CONFIDENCE_2, 0.60);
    }

    #[test]
    fn label_above_skips_unoffered_labels() {
        let all = all_offered();
        assert_eq!(label_above(LABEL_TRIVIAL, all), Some(LABEL_ROUTINE));
        assert_eq!(label_above(LABEL_ROUTINE, all), Some(LABEL_HARD));
        assert_eq!(label_above(LABEL_HARD, all), None);

        let no_routine = OfferedLabels { trivial: true, routine: false, hard: true };
        assert_eq!(label_above(LABEL_TRIVIAL, no_routine), Some(LABEL_HARD));
        assert_eq!(label_above(LABEL_ROUTINE, no_routine), Some(LABEL_HARD));

        let only_trivial = OfferedLabels { trivial: true, routine: false, hard: false };
        assert_eq!(label_above(LABEL_TRIVIAL, only_trivial), None);
        assert_eq!(label_above("unknown", all), None);
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
