// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Correction detection — flag user prompts that correct the agent.
//!
//! The agent's memory pipeline only auto-captures **tool calls**, so a user
//! correction ("that's wrong", "you broke the rule", "did that land?") — the
//! highest-value learning signal — evaporates at session end unless the model
//! happens to `memory_write` it. This module provides a cheap, deterministic
//! detector so the runtime can record corrections **without** relying on the
//! model's in-the-moment judgment.
//!
//! Detection is a pure function over the prompt text (case-insensitive
//! substring match against a list of correction triggers). It's deliberately
//! conservative: a few false positives (recording a prompt that wasn't really
//! a correction) are harmless — the memory is just extra context — whereas a
//! false negative loses a lesson the user explicitly tried to teach.

/// Case-insensitive phrases that signal the user is correcting the agent.
///
/// Ordered roughly from explicit rule violations to softer "that's not right"
/// signals. Matching is a plain substring check (not word-boundary), so
/// "that's wrong" also matches "that's wrong," / "that's wrong." etc.
const CORRECTION_TRIGGERS: &[&str] = &[
    // Explicit rule / instruction violations.
    "you broke",
    "you're breaking",
    "you violated",
    "against the rules",
    "broke the rule",
    "didn't follow",
    "did not follow",
    "not what i asked",
    "not what i wanted",
    "that's not what i",
    "you ignored",
    "you didn't do what",
    "you didn't listen",
    "stop doing that",
    "stop breaking",
    // Correctness corrections.
    "that's wrong",
    "that is wrong",
    "that's incorrect",
    "that is incorrect",
    "that's not right",
    "that is not right",
    "you're wrong",
    "you are wrong",
    "this is wrong",
    // Verification prompts — the user is checking whether the agent actually
    // complied (a strong signal the agent's last action is in doubt).
    "did that land",
    "did it land",
    "did you actually",
    "did you really",
    "are you sure that",
    // Behavioral nudges tied to the agent's own rules.
    "too verbose",
    "too long",
    // Feedback about the agent's *summary output* specifically — NOT the bare
    // "summar" stem, which would also match benign requests like "summarize
    // this file" or "add a summary field".
    "your summary",
    "the summary",
    "summary is too",
    "summaries should",
    "summary should",
    "shorter",
    "concise",
    "3 lines",
    "three lines",
];

/// Detect whether `text` looks like a user correction.
///
/// Returns `Some(trigger)` with the first matched trigger phrase when the
/// prompt signals a correction, `None` otherwise. The returned trigger is
/// useful context to store alongside the captured memory (so a later reader —
/// or consolidation — can see *why* the prompt was flagged).
pub fn detect_correction(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    CORRECTION_TRIGGERS
        .iter()
        .find(|trigger| lower.contains(**trigger))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_explicit_rule_violations() {
        assert_eq!(detect_correction("you broke the rule"), Some("you broke"));
        assert!(detect_correction("That's not what I asked for").is_some());
        assert!(detect_correction("You didn't follow the instructions").is_some());
        assert!(detect_correction("stop doing that").is_some());
    }

    #[test]
    fn matches_correctness_corrections() {
        assert!(detect_correction("that's wrong, try again").is_some());
        assert!(detect_correction("This is wrong.").is_some());
        assert!(detect_correction("You're wrong about the exit code").is_some());
    }

    #[test]
    fn matches_verification_prompts() {
        assert!(detect_correction("did that land with agent.md?").is_some());
        assert!(detect_correction("did you actually run the tests").is_some());
    }

    #[test]
    fn matches_brevity_nudges() {
        // The actual case that motivated this: the ≤3-line summary rule.
        assert!(detect_correction("your summary is too long").is_some());
        assert!(detect_correction("keep it to 3 lines").is_some());
        assert!(detect_correction("be more concise").is_some());
        // Feedback specifically about the summary output still matches.
        assert!(detect_correction("the summary is too long").is_some());
        assert!(detect_correction("summaries should be shorter").is_some());
        assert!(detect_correction("your summary should be 3 lines").is_some());
    }

    #[test]
    fn benign_summarize_requests_do_not_match() {
        // The bare "summar" stem was too broad — a request to *produce* a
        // summary is not a correction of the agent's output.
        assert_eq!(detect_correction("summarize this file"), None);
        assert_eq!(detect_correction("add a summary field"), None);
        assert_eq!(detect_correction("summarize the last three commits"), None);
        assert_eq!(detect_correction("write a summary for the README"), None);
    }

    #[test]
    fn is_case_insensitive() {
        assert!(detect_correction("THAT'S WRONG").is_some());
        assert!(detect_correction("You Broke The Rule").is_some());
    }

    #[test]
    fn ignores_normal_prompts() {
        assert_eq!(detect_correction("add a spawn_agent tool"), None);
        assert_eq!(detect_correction("run the tests please"), None);
        assert_eq!(detect_correction("refactor the factory"), None);
        assert_eq!(detect_correction("what does this function do?"), None);
    }

    #[test]
    fn returns_first_matched_trigger() {
        // "that's wrong" appears before later triggers; the returned trigger
        // should be a real pattern string for storage context.
        let t = detect_correction("that's wrong and you broke it").unwrap();
        assert!(CORRECTION_TRIGGERS.contains(&t));
    }
}
