// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Confidence-gated auto-typing of memory records — the Laya classifier's
//! first production consumer (backlog a147b63c, item 5 of the Laya chain).
//!
//! At `memory_write` time a record's typed prefix (SPEC/DECISION/BUG/PLAN/
//! HOW/REVIEW) is whatever the writing model chose — often omitted or wrong.
//! [`auto_type`] asks the optional [`Classifier`](super::Classifier) a single
//! choice question over the six kinds and, **only above
//! [`AUTO_TYPE_THRESHOLD`]**, retypes the title ([`retitle`]); anything less
//! keeps the writer's prefix. The shipped Laya checkpoints are near-chance
//! zero-shot on custom tasks and over-confident until fine-tuned, so the
//! whole path is opt-in (`[general.laya] auto_type_memories`, default off)
//! and meant to be pointed at a fine-tuned checkpoint — see
//! [`seed_dataset`] for the labeled set that fine-tuning trains on.
//!
//! Like the [`Classifier`](super::Classifier) contract itself, auto-typing is
//! infallible by design: no backend, no answer, an unknown label, or low
//! confidence all degrade to "keep the writer's title", so the tool's
//! behavior with auto-typing disabled (or Laya off) is byte-identical to
//! today's.

use std::collections::BTreeMap;
use std::path::Path;

use crate::memory::classifier::{Answer, Classifier, Question};
use crate::memory::knowledge::{dir_for_record_type, parse_file};
use crate::memory::MemoryRecordType;

/// The calibrated confidence an answer must reach before it may override
/// the writer's prefix. Below it the writer keeps the prefix (the
/// pre-classifier behavior) — the gate the [`Classifier`](super::Classifier)
/// docs require, since unvalidated probabilities must never gate a decision.
pub const AUTO_TYPE_THRESHOLD: f64 = 0.80;

/// Cap on the state text handed to the classifier (title + body). The
/// classifier only needs the gist of the record; the cap bounds latency and
/// request size for pathological bodies.
const STATE_TEXT_MAX_CHARS: usize = 4000;

/// Every typed-record prefix with its record type — the same table
/// [`MemoryRecordType::from_title`] parses with. This is the TITLE-prefix
/// view (all six kinds; PLAN/REVIEW prefixes ride DB rows), broader than
/// [`crate::memory::knowledge::title_prefix`], which is the file-naming
/// view (only the four knowledge-dir-backed kinds).
const PREFIXES: [(&str, MemoryRecordType); 6] = [
    ("SPEC:", MemoryRecordType::Spec),
    ("DECISION:", MemoryRecordType::Decision),
    ("BUG:", MemoryRecordType::Bug),
    ("PLAN:", MemoryRecordType::Plan),
    ("HOW:", MemoryRecordType::How),
    ("REVIEW:", MemoryRecordType::Review),
];

/// The typed-title prefix for a record type (`Bug` → `"BUG:"`), covering all
/// six kinds ([`PREFIXES`] — the title view, not the file-naming view).
pub fn prefix_for(record_type: MemoryRecordType) -> Option<&'static str> {
    PREFIXES
        .iter()
        .find(|(_, t)| *t == record_type)
        .map(|(prefix, _)| *prefix)
}

/// Why the writer's prefix was kept — surfaced so callers can note the
/// decision (calibration observability) without changing the write.
#[derive(Debug, Clone, PartialEq)]
pub enum TypingKeepReason {
    /// The backend returned no answer (disabled, unreachable, timed out, or
    /// an unparseable response) — the strict fallback.
    NoAnswer,
    /// The answer's calibrated confidence was below
    /// [`AUTO_TYPE_THRESHOLD`] (carries the confidence that was returned).
    LowConfidence(f64),
    /// The answer's label is not one of the six kinds (carries the label) —
    /// a protocol deviation, treated as no usable answer.
    UnknownLabel(String),
    /// The classifier agreed with the writer's prefix — nothing to change.
    Agrees,
}

/// The outcome of one auto-typing pass over a record.
#[derive(Debug, Clone, PartialEq)]
pub enum TypingDecision {
    /// Keep the writer's title unchanged (with the reason).
    KeepWriter {
        /// Why the writer's prefix stands.
        reason: TypingKeepReason,
    },
    /// Apply the classifier's prefix instead (carries both kinds and the
    /// calibrated confidence that cleared [`AUTO_TYPE_THRESHOLD`]).
    Retype {
        /// The kind the writer's title carried (`None`-kind when the writer
        /// omitted a prefix — represented by [`MemoryRecordType::None`]).
        from: MemoryRecordType,
        /// The kind the classifier picked.
        to: MemoryRecordType,
        /// The calibrated confidence of the pick.
        confidence: f64,
    },
}

/// The choice question over the six record kinds. Labels are bare words
/// (never boolean-ish, per the classifier docs); descriptions carry the
/// kind taxonomy from the agent's MEMORY RECORDS guidance so the
/// fine-tuned checkpoint and the corpus share one vocabulary.
pub fn typing_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        "SPEC".to_string(),
        "How a feature or system works — architecture, invariants, mechanics.".to_string(),
    );
    criteria.insert(
        "DECISION".to_string(),
        "A choice and its rationale — what was chosen and why.".to_string(),
    );
    criteria.insert(
        "BUG".to_string(),
        "A defect record — symptom, root cause, fix, regression test.".to_string(),
    );
    criteria.insert(
        "PLAN".to_string(),
        "A plan digest — what a piece of work set out to do.".to_string(),
    );
    criteria.insert(
        "HOW".to_string(),
        "A recurring workflow — how to do a repeating task.".to_string(),
    );
    criteria.insert(
        "REVIEW".to_string(),
        "A review report digest — findings about a change.".to_string(),
    );
    Question::Choice {
        instructions: "Classify this memory record by its typed-record kind. \
Read the title and the body, then pick the ONE label that best describes \
what kind of record this is."
            .to_string(),
        criteria,
    }
}

/// Strip any of the six typed prefixes from a title (`"SPEC: x"` → `"x"`;
/// an unprefixed title passes through). The same six prefixes as
/// [`MemoryRecordType::from_title`], position-0 only — but the STRIP side
/// also matches case variants (`"spec: x"` → `"x"`) and repeats, because a
/// lowercase or doubled writer prefix must not survive INSIDE a retitled
/// title or the classifier state text even though it never PARSES as a
/// kind (`from_title` stays case-sensitive by design).
fn strip_typed_prefix(title: &str) -> String {
    let mut rest = title.trim_start();
    loop {
        let Some(next) = PREFIXES.iter().find_map(|(prefix, _)| {
            rest.strip_prefix(*prefix).or_else(|| strip_ci(rest, prefix))
        }) else {
            return rest.to_string();
        };
        rest = next.trim_start();
    }
}

/// Case-insensitive position-0 strip of an ASCII prefix (byte-safe: the
/// six typed prefixes are pure ASCII).
fn strip_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let len = prefix.len();
    if text.len() >= len
        && text.is_char_boundary(len)
        && text[..len].eq_ignore_ascii_case(prefix)
    {
        Some(&text[len..])
    } else {
        None
    }
}

/// The classifier state text for a record: the **bare** title (any typed
/// prefix stripped, so a wrong writer prefix cannot bias the pick) followed
/// by the body, capped at [`STATE_TEXT_MAX_CHARS`] chars.
pub fn state_text(title: &str, body: &str) -> String {
    let bare = strip_typed_prefix(title);
    let mut text = format!("{bare}\n{body}");
    if text.chars().count() > STATE_TEXT_MAX_CHARS {
        text = text.chars().take(STATE_TEXT_MAX_CHARS).collect();
    }
    text
}

/// Classify one record's kind. Infallible by design — every not-usable
/// outcome (no backend answer, a non-choice answer, an unknown label) is a
/// [`TypingDecision::KeepWriter`], and only a label that maps to one of the
/// six kinds with confidence ≥ [`AUTO_TYPE_THRESHOLD`] retypes. Agreement
/// with the writer is checked before the threshold so an agreeing answer is
/// reported as [`TypingKeepReason::Agrees`] (nothing to change) regardless
/// of its confidence.
pub async fn auto_type(classifier: &dyn Classifier, title: &str, body: &str) -> TypingDecision {
    let from = MemoryRecordType::from_title(title);
    let answer = classifier
        .classify(&state_text(title, body), &typing_question())
        .await;
    // No answer at all — or a non-choice answer to a choice question (a
    // protocol deviation) — is no usable answer.
    let (label, confidence) = match answer {
        Some(Answer::Choice { label, confidence, .. }) => (label, confidence),
        _ => {
            return TypingDecision::KeepWriter {
                reason: TypingKeepReason::NoAnswer,
            }
        }
    };
    let to = match label.as_str() {
        "SPEC" => MemoryRecordType::Spec,
        "DECISION" => MemoryRecordType::Decision,
        "BUG" => MemoryRecordType::Bug,
        "PLAN" => MemoryRecordType::Plan,
        "HOW" => MemoryRecordType::How,
        "REVIEW" => MemoryRecordType::Review,
        other => {
            // Bound + scrub the endpoint-controlled label before it rides
            // the decision: control characters stripped, echo capped at 40
            // chars — the label later reaches the agent-visible message and
            // structured data, so it must carry no injection surface and no
            // size blowup past the state-text cap the input side enforces.
            let clean: String = other
                .chars()
                .filter(|c| !c.is_control())
                .take(40)
                .collect();
            return TypingDecision::KeepWriter {
                reason: TypingKeepReason::UnknownLabel(clean),
            };
        }
    };
    if to == from {
        return TypingDecision::KeepWriter {
            reason: TypingKeepReason::Agrees,
        };
    }
    if confidence < AUTO_TYPE_THRESHOLD {
        return TypingDecision::KeepWriter {
            reason: TypingKeepReason::LowConfidence(confidence),
        };
    }
    TypingDecision::Retype {
        from,
        to,
        confidence,
    }
}

/// Apply a record type's prefix to a title: any typed prefix the writer used
/// is stripped first, then the chosen prefix is applied
/// (`"SPEC: tabs lost"` + `Bug` → `"BUG: tabs lost"`), so the result always
/// round-trips through [`MemoryRecordType::from_title`] and never
/// double-prefixes. A type with no title prefix (never true for the six
/// kinds) would pass the title through unchanged.
pub fn retitle(title: &str, to: MemoryRecordType) -> String {
    let Some(prefix) = prefix_for(to) else {
        return title.to_string();
    };
    let bare = strip_typed_prefix(title);
    let titled = if bare.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix} {bare}")
    };
    titled.trim_end().to_string()
}

/// One labeled training example for the fine-tuning set.
#[derive(Debug, Clone, PartialEq)]
pub struct LabeledExample {
    /// The classifier-facing text (bare title + body — the same
    /// [`state_text`] shape inference sees).
    pub text: String,
    /// The record's true kind (the directory it lives in).
    pub label: MemoryRecordType,
}

/// Build the seed labeled set for fine-tuning the typing classifier: every
/// parseable knowledge file under the four typed dirs
/// (`spec`/`decision`/`bug`/`how`) becomes one
/// [`LabeledExample`] whose label is the directory's kind. Best-effort and
/// deterministic — missing dirs, unreadable files, and non-`.md` files are
/// skipped, and files are visited in sorted order per directory.
///
/// Scope note (be honest when enabling): PLAN and REVIEW records have no
/// knowledge home (`dir_for_record_type` → `None`), so this seed set
/// trains only the four dir-backed kinds — a confident PLAN/REVIEW retype
/// rides an UNTRAINED class until those corpora can be seeded too (the
/// documented follow-up).
pub fn seed_dataset(knowledge_dir: &Path) -> Vec<LabeledExample> {
    let mut examples = Vec::new();
    for record_type in [
        MemoryRecordType::Spec,
        MemoryRecordType::Decision,
        MemoryRecordType::Bug,
        MemoryRecordType::How,
    ] {
        let Some(dir) = dir_for_record_type(record_type) else {
            continue;
        };
        let dir_path = knowledge_dir.join(dir);
        let Ok(entries) = std::fs::read_dir(&dir_path) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".md"))
            .collect();
        names.sort();
        for name in names {
            let rel_path = format!("{dir}/{name}");
            let Ok(contents) = std::fs::read_to_string(dir_path.join(&name)) else {
                continue;
            };
            if let Some(record) = parse_file(&rel_path, &contents) {
                examples.push(LabeledExample {
                    text: state_text(&record.title, &record.body),
                    label: record.record_type,
                });
            }
        }
    }
    examples
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;

    /// A classifier with a canned answer — `None` models every no-answer
    /// path (disabled backend, timeout, malformed response).
    struct StubClassifier {
        answer: Option<Answer>,
    }

    #[async_trait]
    impl Classifier for StubClassifier {
        async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
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

    #[test]
    fn typing_question_covers_the_six_kinds_with_descriptions() {
        let Question::Choice { instructions, criteria } = typing_question() else {
            panic!("typing_question must be a choice question");
        };
        assert!(!instructions.is_empty());
        let mut labels: Vec<_> = criteria.keys().cloned().collect();
        labels.sort();
        assert_eq!(
            labels,
            vec!["BUG", "DECISION", "HOW", "PLAN", "REVIEW", "SPEC"]
        );
        for description in criteria.values() {
            assert!(!description.is_empty());
        }
    }

    #[tokio::test]
    async fn high_confidence_retypes_a_mistyped_title() {
        let classifier = StubClassifier {
            answer: choice("BUG", 0.93),
        };
        let decision = auto_type(&classifier, "SPEC: editor tabs lost on save", "…").await;
        assert_eq!(
            decision,
            TypingDecision::Retype {
                from: MemoryRecordType::Spec,
                to: MemoryRecordType::Bug,
                confidence: 0.93,
            }
        );
    }

    #[tokio::test]
    async fn high_confidence_types_an_unprefixed_title() {
        let classifier = StubClassifier {
            answer: choice("HOW", 0.9),
        };
        let decision = auto_type(&classifier, "full test coverage checklist", "…").await;
        assert_eq!(
            decision,
            TypingDecision::Retype {
                from: MemoryRecordType::None,
                to: MemoryRecordType::How,
                confidence: 0.9,
            }
        );
    }

    #[tokio::test]
    async fn low_confidence_keeps_the_writer() {
        let classifier = StubClassifier {
            answer: choice("BUG", 0.79),
        };
        let decision = auto_type(&classifier, "SPEC: editor tabs lost on save", "…").await;
        assert_eq!(
            decision,
            TypingDecision::KeepWriter {
                reason: TypingKeepReason::LowConfidence(0.79),
            }
        );
    }

    #[tokio::test]
    async fn the_threshold_is_inclusive() {
        let classifier = StubClassifier {
            answer: choice("BUG", AUTO_TYPE_THRESHOLD),
        };
        let decision = auto_type(&classifier, "SPEC: editor tabs lost on save", "…").await;
        assert!(matches!(decision, TypingDecision::Retype { .. }));
    }

    #[tokio::test]
    async fn no_answer_keeps_the_writer() {
        let classifier = StubClassifier { answer: None };
        let decision = auto_type(&classifier, "SPEC: anything", "…").await;
        assert_eq!(
            decision,
            TypingDecision::KeepWriter {
                reason: TypingKeepReason::NoAnswer,
            }
        );
    }

    #[tokio::test]
    async fn a_non_choice_answer_is_no_usable_answer() {
        let classifier = StubClassifier {
            answer: Some(Answer::Score {
                value: 3.0,
                confidence: 0.99,
            }),
        };
        let decision = auto_type(&classifier, "SPEC: anything", "…").await;
        assert_eq!(
            decision,
            TypingDecision::KeepWriter {
                reason: TypingKeepReason::NoAnswer,
            }
        );
    }

    #[tokio::test]
    async fn an_unknown_label_keeps_the_writer() {
        let classifier = StubClassifier {
            answer: choice("NOTE", 0.99),
        };
        let decision = auto_type(&classifier, "SPEC: anything", "…").await;
        assert_eq!(
            decision,
            TypingDecision::KeepWriter {
                reason: TypingKeepReason::UnknownLabel("NOTE".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn agreement_keeps_the_writer_regardless_of_confidence() {
        // Above the threshold AND agreeing — still a keep (nothing to change).
        let confident = StubClassifier {
            answer: choice("SPEC", 0.95),
        };
        let decision = auto_type(&confident, "SPEC: layout invariants", "…").await;
        assert_eq!(
            decision,
            TypingDecision::KeepWriter {
                reason: TypingKeepReason::Agrees,
            }
        );
        // Below the threshold and agreeing — same keep, the agreeing reason.
        let unconfident = StubClassifier {
            answer: choice("SPEC", 0.30),
        };
        let decision = auto_type(&unconfident, "SPEC: layout invariants", "…").await;
        assert_eq!(
            decision,
            TypingDecision::KeepWriter {
                reason: TypingKeepReason::Agrees,
            }
        );
    }

    #[tokio::test]
    async fn an_unknown_label_is_bounded_and_scrubbed() {
        // The label is endpoint-controlled text: control characters are
        // stripped and the echo capped at construction, so neither the
        // agent-visible note nor the structured data can carry injection
        // phrasing or a size blowup (review LOW 1).
        let hostile = format!("BAD\n{}", "x".repeat(10_000));
        let classifier = StubClassifier {
            answer: choice(&hostile, 0.99),
        };
        let decision = auto_type(&classifier, "SPEC: anything", "…").await;
        let TypingDecision::KeepWriter {
            reason: TypingKeepReason::UnknownLabel(label),
        } = decision
        else {
            panic!("expected an unknown-label keep, got {decision:?}");
        };
        assert!(!label.contains('\n'));
        assert!(label.chars().count() <= 40);
        assert!(label.starts_with("BAD"));
    }

    #[test]
    fn state_text_strips_the_writers_prefix_and_caps_the_body() {
        assert_eq!(state_text("SPEC: tabs", "body text"), "tabs\nbody text");
        let long_body = "x".repeat(STATE_TEXT_MAX_CHARS);
        let text = state_text("BUG: crash", &long_body);
        assert!(text.chars().count() <= STATE_TEXT_MAX_CHARS);
        assert!(text.starts_with("crash\n"));
    }

    #[test]
    fn retitle_strips_any_prefix_and_applies_the_new_one() {
        assert_eq!(retitle("SPEC: tabs lost", MemoryRecordType::Bug), "BUG: tabs lost");
        assert_eq!(retitle("tabs lost", MemoryRecordType::Bug), "BUG: tabs lost");
        // Idempotent: retyping to the same kind never double-prefixes.
        assert_eq!(retitle("BUG: tabs lost", MemoryRecordType::Bug), "BUG: tabs lost");
        // Plan/Review prefixes are title-level kinds too.
        assert_eq!(retitle("HOW: x", MemoryRecordType::Plan), "PLAN: x");
    }

    #[test]
    fn strip_typed_prefix_handles_case_variants_and_repeats() {
        // from_title stays case-sensitive (parse parity), but a lowercase
        // or doubled writer prefix must not survive INSIDE a retitled title
        // or the classifier state text (review LOW 3).
        assert_eq!(retitle("spec: tabs lost", MemoryRecordType::Spec), "SPEC: tabs lost");
        assert_eq!(retitle("BUG: bug: mixed", MemoryRecordType::How), "HOW: mixed");
        assert!(state_text("bug: crash", "body").starts_with("crash\n"));
        // A merely similar leading word is NOT a prefix — untouched.
        assert_eq!(
            retitle("speculative: keep", MemoryRecordType::Spec),
            "SPEC: speculative: keep"
        );
    }

    #[test]
    fn prefix_for_covers_all_six_kinds() {
        assert_eq!(prefix_for(MemoryRecordType::Spec), Some("SPEC:"));
        assert_eq!(prefix_for(MemoryRecordType::Decision), Some("DECISION:"));
        assert_eq!(prefix_for(MemoryRecordType::Bug), Some("BUG:"));
        assert_eq!(prefix_for(MemoryRecordType::Plan), Some("PLAN:"));
        assert_eq!(prefix_for(MemoryRecordType::How), Some("HOW:"));
        assert_eq!(prefix_for(MemoryRecordType::Review), Some("REVIEW:"));
        assert_eq!(prefix_for(MemoryRecordType::None), None);
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mnemo-auto-typing-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec")).expect("create spec dir");
        std::fs::create_dir_all(dir.join("bug")).expect("create bug dir");
        dir
    }

    #[test]
    fn seed_dataset_labels_files_by_their_directory() {
        let root = temp_root("seed");
        std::fs::write(
            root.join("spec/2026-01-01-layout-invariants.md"),
            "+++\ntitle = \"layout invariants\"\n+++\n\nThe editor keeps tabs…",
        )
        .expect("write spec file");
        std::fs::write(
            root.join("bug/2026-01-02-tabs-lost.md"),
            "+++\ntitle = \"tabs lost on save\"\n+++\n\nSymptom: tabs vanish…",
        )
        .expect("write bug file");
        // Not a knowledge shape: wrong extension is skipped.
        std::fs::write(root.join("spec/notes.txt"), "not markdown").expect("write txt");

        let examples = seed_dataset(&root);
        assert_eq!(examples.len(), 2, "one example per parseable .md file");
        // Sorted per directory, spec before bug (fixed type order).
        assert_eq!(examples[0].label, MemoryRecordType::Spec);
        assert!(examples[0].text.starts_with("layout invariants\n"));
        assert_eq!(examples[1].label, MemoryRecordType::Bug);
        assert!(examples[1].text.starts_with("tabs lost on save\n"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn seed_dataset_skips_missing_and_unparseable() {
        let root = temp_root("empty");
        // An empty file parses (degrades to defaults) — a NON-knowledge
        // shape would be skipped by parse_file; empty dirs yield nothing.
        let examples = seed_dataset(&root);
        assert!(examples.is_empty());

        std::fs::write(root.join("spec/2026-01-01-broken.md"), "").expect("write broken");
        let examples = seed_dataset(&root);
        assert_eq!(examples.len(), 1, "empty file still parses with defaults");

        std::fs::remove_dir_all(&root).ok();
    }
}
