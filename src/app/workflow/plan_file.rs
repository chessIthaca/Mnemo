// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `.coding/plans/<id>.md` parse/serialize (markdown checkboxes).
//!
//! The plan file is the source of truth for the workflow. In-memory state is
//! derived from the file, never the other way around — so a crash or restart
//! loses nothing.

use std::path::{Path, PathBuf};

use crate::error::Result;

pub use crate::memory::plan_format::{PlanFile, PlanKind, ReviewRoundStamp, Step};
use crate::memory::plan_format::{extract_bold_header, parse_checkbox};

impl PlanFile {
    /// Create a new plan with the given title, goal, context, and unchecked steps.
    pub fn new(
        title: impl Into<String>,
        goal: impl Into<String>,
        context: impl Into<String>,
        steps: Vec<String>,
    ) -> Self {
        let steps = steps
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let header = extract_bold_header(&text);
                Step {
                    index: i,
                    text,
                    header,
                    done: false,
                }
            })
            .collect();
        Self {
            title: title.into(),
            goal: goal.into(),
            context: context.into(),
            kind: PlanKind::default(),
            bug_symptom: None,
            regression_test: None,
            landed_design: false,
            detailed_steps: None,
            reviews: Vec::new(),
            steps,
        }
    }

    /// Serialize the plan back to markdown.
    pub fn serialize(&self) -> String {
        let mut out = format!(
            "# Plan: {}\n\n## Goal\n{}\n\n## Kind\n{}\n\n## Context\n{}\n\n## Steps\n",
            self.title,
            self.goal,
            self.kind.as_str(),
            self.context
        );
        for step in &self.steps {
            let mark = if step.done { 'x' } else { ' ' };
            out.push_str(&format!("- [{}] {}. {}\n", mark, step.index + 1, step.text));
        }
        if let Some(detail) = &self.detailed_steps {
            out.push_str(&format!("\n## Detailed steps\n{detail}\n"));
        }
        if let Some(symptom) = &self.bug_symptom {
            out.push_str(&format!("\n## Bug\n{symptom}\n"));
        }
        if let Some(test) = &self.regression_test {
            out.push_str(&format!("\n## Regression test\n{test}\n"));
        }
        if self.landed_design {
            out.push_str("\n## Landed design\nyes\n");
        }
        if !self.reviews.is_empty() {
            out.push_str("\n## Reviews\n");
            for stamp in &self.reviews {
                out.push_str(&format!("{} {}\n", stamp.round, stamp.base));
            }
        }
        out
    }

    /// The first unchecked step (the resume point), or None if all done.
    pub fn current_step(&self) -> Option<&Step> {
        self.steps.iter().find(|s| !s.done)
    }

    /// Whether all steps are complete.
    pub fn is_complete(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|s| s.done)
    }

    /// The number of completed steps.
    pub fn completed_count(&self) -> usize {
        self.steps.iter().filter(|s| s.done).count()
    }

    /// Mark a step (by 0-indexed index) as done. Returns an error if out of range.
    pub fn complete_step(&mut self, step_index: usize) -> Result<()> {
        let len = self.steps.len();
        let step = self.steps.get_mut(step_index).ok_or_else(|| {
            crate::error::Error::InvalidInput(format!(
                "step index {step_index} out of range (have {len} steps) — \
                 complete_step step numbers are 1-indexed, valid 1..={len}"
            ))
        })?;
        step.done = true;
        Ok(())
    }

    /// Mark a detailed sub-step (by 1-indexed number) as done — the
    /// checkable sub-items under `## Detailed steps` (backlog 9441d776: a
    /// mid-fix restart must see exactly which sub-steps shipped, so the
    /// plan file is the complete crash-resumption document at sub-step
    /// granularity too). The section body is the single on-disk truth:
    /// the tick rewrites the nth `- [ ]`/`- [x]` bullet in place — ONLY
    /// the mark changes; the raw text after `] ` is preserved verbatim
    /// (a leading "N. " or extra spaces survives, review L2) — and the
    /// marks round-trip through serialize→parse verbatim (the raw
    /// section always has).
    ///
    /// Legacy bodies (plain `- ` bullets from plans created before the
    /// checkable render) are normalized to unchecked checkboxes on the
    /// first tick, so in-flight older plans become tickable too. Ticking
    /// an already-done sub-step is an idempotent no-op. The count is
    /// line-oriented — the render escapes `- `-leading continuation lines
    /// of multi-line step texts (review L3), so only a step text quoting
    /// a literal `- [ ] ` checkbox line at column 0 can still count as its
    /// own sub-step (the residual line-oriented ambiguity). Returns a
    /// progress summary ("2/5 detailed steps done — {opening words}") for
    /// the tool result.
    pub fn complete_detailed_step(&mut self, detailed_step_index: usize) -> Result<String> {
        let body = self.detailed_steps.as_mut().ok_or_else(|| {
            crate::error::Error::InvalidInput(
                "this plan has no '## Detailed steps' section — detailed_step_index \
                 only applies to bug_fixing plans created with detailed steps"
                    .into(),
            )
        })?;
        let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
        // Normalize legacy plain `- ` bullets to unchecked checkboxes so
        // pre-checkable plans become tickable (checkbox lines and non-bullet
        // continuation lines pass through unchanged).
        for line in lines.iter_mut() {
            let normalized = {
                let trimmed = line.trim_start();
                match trimmed.strip_prefix("- ") {
                    Some(text) if parse_checkbox(trimmed).is_none() => {
                        let indent_len = line.len() - trimmed.len();
                        let (indent, _) = line.split_at(indent_len);
                        Some(format!("{indent}- [ ] {}", text.trim_start()))
                    }
                    _ => None,
                }
            };
            if let Some(new_line) = normalized {
                *line = new_line;
            }
        }
        // Flip the nth checkbox (1-indexed), preserving any indent.
        let mut seen = 0usize;
        let mut ticked: Option<String> = None;
        for line in lines.iter_mut() {
            if let Some((_, text)) = parse_checkbox(line.as_str()) {
                seen += 1;
                if seen == detailed_step_index {
                    // Rebuild from the RAW text after `] ` —
                    // parse_checkbox's processed text strips a leading
                    // "N. " (skeleton-step numbering) and leading spaces,
                    // which would mutate the persisted text beyond the
                    // mark flip (review L2: only the mark may change).
                    let new_line = {
                        let trimmed = line.trim_start();
                        let indent_len = line.len() - trimmed.len();
                        let (indent, _) = line.split_at(indent_len);
                        let raw_text = trimmed
                            .strip_prefix("- [")
                            .and_then(|rest| rest.split_once(']'))
                            .map(|(_, after)| after.strip_prefix(' ').unwrap_or(after))
                            .unwrap_or_default();
                        format!("{indent}- [x] {raw_text}")
                    };
                    *line = new_line;
                    ticked = Some(text);
                    break;
                }
            }
        }
        let text = ticked.ok_or_else(|| {
            crate::error::Error::InvalidInput(format!(
                "detailed step {detailed_step_index} out of range (have {seen} \
                 checkable detailed steps) — detailed_step_index numbers are \
                 1-indexed, valid 1..={seen}"
            ))
        })?;
        *body = lines.join("\n");
        // Progress summary: done/total + the ticked step's opening words.
        let done = lines
            .iter()
            .filter(|l| parse_checkbox(l.as_str()).is_some_and(|(d, _)| d))
            .count();
        let total = lines
            .iter()
            .filter(|l| parse_checkbox(l.as_str()).is_some())
            .count();
        let preview: String = if text.chars().count() > 60 {
            let mut s: String = text.chars().take(57).collect();
            s.push('…');
            s
        } else {
            text
        };
        Ok(format!("{done}/{total} detailed steps done — {preview}"))
    }

    /// Whether marking `step_index` done would complete the plan: the step
    /// is in range, not already done, and every other step is done. Used by
    /// the dispatch layer's review-exit gate to detect the FINAL
    /// complete_step call without mutating anything.
    pub fn step_completes_plan(&self, step_index: usize) -> bool {
        match self.steps.get(step_index) {
            Some(step) if !step.done => self
                .steps
                .iter()
                .enumerate()
                .all(|(i, s)| i == step_index || s.done),
            _ => false,
        }
    }

    /// Write the plan to a file in the given plans directory.
    /// Returns the path it was written to.
    pub fn write_to_dir(&self, plans_dir: &Path, id: &str) -> Result<PathBuf> {
        let path = plans_dir.join(format!("{id}.md"));
        std::fs::write(&path, self.serialize())?;
        Ok(path)
    }

    /// Read a plan from a file.
    pub fn read_from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }
}

/// Public wrapper so other modules (e.g. `Workflow::update_plan`) can extract
/// a bold header from step text. See [`extract_bold_header`].
pub(crate) fn extract_bold_header_pub(text: &str) -> Option<String> {
    extract_bold_header(text)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::plan_format::parse_step_header;
    use tempfile::tempdir;

    #[test]
    fn serialize_and_parse_roundtrip() {
        let plan = PlanFile::new(
            "Build the thing",
            "Make it work",
            "Found existing code",
            vec!["Step one".into(), "Step two".into(), "Step three".into()],
        );
        let text = plan.serialize();
        let parsed = PlanFile::parse(&text).unwrap();
        assert_eq!(parsed.title, "Build the thing");
        assert_eq!(parsed.goal, "Make it work");
        assert_eq!(parsed.context, "Found existing code");
        assert_eq!(parsed.steps.len(), 3);
        assert_eq!(parsed.steps[0].text, "Step one");
        assert!(!parsed.steps[0].done);
    }

    #[test]
    fn detailed_steps_roundtrip() {
        // Backlog 77ff8f45: the caller's detailed steps persist as their
        // own section and survive a serialize→parse round-trip — the plan
        // file is the crash-resumption document, so a restarted session
        // reads the full step-level recipe back from disk.
        let mut plan = PlanFile::new("T", "G", "C", vec!["s1".into()]);
        plan.kind = PlanKind::BugFixing;
        plan.bug_symptom = Some("the app crashes on open".into());
        let detail = "- Reproduce — run `cargo test crashy`, observe the panic in src/app.rs:42\n- Fix — guard the unwrap at src/app.rs:42";
        plan.detailed_steps = Some(detail.into());
        let text = plan.serialize();
        assert!(text.contains("## Detailed steps"), "{text}");
        let parsed = PlanFile::parse(&text).unwrap();
        assert_eq!(parsed.detailed_steps.as_deref(), Some(detail));
        // Absent → None: plans written before the section (or bug plans
        // created without steps) parse fine.
        let plain = PlanFile::new("T2", "G", "C", vec!["s".into()]);
        let parsed2 = PlanFile::parse(&plain.serialize()).unwrap();
        assert!(parsed2.detailed_steps.is_none());
    }

    #[test]
    fn research_kind_roundtrips_through_serialize() {
        // A research plan serializes a `## Kind` section and parses back to
        // PlanKind::Research.
        let mut plan = PlanFile::new(
            "Investigate the bug",
            "Find the root cause",
            "Read the relevant files",
            vec!["Read src/foo.rs".into()],
        );
        plan.kind = PlanKind::Research;
        let text = plan.serialize();
        // The serialized form must contain the Kind section.
        assert!(
            text.contains("## Kind\nresearch"),
            "missing kind section: {text}"
        );
        let parsed = PlanFile::parse(&text).unwrap();
        assert_eq!(parsed.kind, PlanKind::Research);
    }

    #[test]
    fn bug_fixing_kind_roundtrips_with_symptom_and_regression_test() {
        // A bug_fixing plan serializes `## Kind\nbug_fixing` plus the
        // `## Bug` + `## Regression test` sections and parses all three back.
        let mut plan = PlanFile::new(
            "Fix the crash",
            "Make the crash go away",
            "C",
            vec!["a".into()],
        );
        plan.kind = PlanKind::BugFixing;
        plan.bug_symptom = Some("the app crashes on open".into());
        plan.regression_test = Some("crash_on_open_regression".into());
        let text = plan.serialize();
        assert!(text.contains("## Kind\nbug_fixing"), "missing kind: {text}");
        assert!(
            text.contains("## Bug\nthe app crashes on open"),
            "missing bug: {text}"
        );
        assert!(
            text.contains("## Regression test\ncrash_on_open_regression"),
            "missing regression test: {text}"
        );
        let parsed = PlanFile::parse(&text).unwrap();
        assert_eq!(parsed.kind, PlanKind::BugFixing);
        assert_eq!(
            parsed.bug_symptom.as_deref(),
            Some("the app crashes on open")
        );
        assert_eq!(
            parsed.regression_test.as_deref(),
            Some("crash_on_open_regression")
        );
    }

    #[test]
    fn landed_design_flag_roundtrips() {
        // A feature-scale bug fix records landed_design=true (backlog
        // e5a84ce9) — the flag serializes as the `## Landed design`
        // section and parses back. Default plans (no section) parse false;
        // a hand-edited "true" body also parses.
        let mut plan = PlanFile::new(
            "Fix the slowness",
            "Make indexing fast",
            "C",
            vec!["a".into()],
        );
        plan.kind = PlanKind::BugFixing;
        plan.landed_design = true;
        let text = plan.serialize();
        assert!(
            text.contains("## Landed design\nyes"),
            "missing landed design: {text}"
        );
        let parsed = PlanFile::parse(&text).unwrap();
        assert!(parsed.landed_design);

        // Default: no section → false.
        let plain = PlanFile::new("T", "G", "C", vec!["s".into()]);
        let parsed = PlanFile::parse(&plain.serialize()).unwrap();
        assert!(!parsed.landed_design);

        // Hand-edited "true" body also parses (single-line literal — the
        // emission guard rejects multi-line markdown fixtures).
        let hand_edited =
            "# Plan: T\n\n## Goal\nG\n\n## Kind\nbug_fixing\n\n## Context\nC\n\n## Steps\n\n## Landed design\ntrue\n";
        let parsed = PlanFile::parse(hand_edited).unwrap();
        assert!(parsed.landed_design);
    }

    #[test]
    fn review_round_stamps_roundtrip_and_skip_malformed_lines() {
        // Backlog 85313a7e: each reviewer round stamps its base revision in
        // the `## Reviews` section, so a re-review can delta-scope against
        // it. Default plans (no section) parse to an empty vec; a malformed
        // line is skipped while its siblings survive.
        let mut plan = PlanFile::new("T", "G", "C", vec!["s".into()]);
        plan.reviews.push(ReviewRoundStamp {
            round: 1,
            base: "abc1234".into(),
        });
        plan.reviews.push(ReviewRoundStamp {
            round: 2,
            base: "def5678".into(),
        });
        let text = plan.serialize();
        assert!(
            text.contains("## Reviews\n1 abc1234\n2 def5678\n"),
            "missing review stamps: {text}"
        );
        let parsed = PlanFile::parse(&text).unwrap();
        assert_eq!(parsed.reviews.len(), 2);
        assert_eq!(parsed.reviews[0].round, 1);
        assert_eq!(parsed.reviews[0].base, "abc1234");
        assert_eq!(parsed.reviews[1].round, 2);
        assert_eq!(parsed.reviews[1].base, "def5678");

        // Default: no section → empty.
        let plain = PlanFile::new("T", "G", "C", vec!["s".into()]);
        let parsed = PlanFile::parse(&plain.serialize()).unwrap();
        assert!(parsed.reviews.is_empty());

        // Hand-edited: the malformed first line is skipped, its sibling
        // survives (single-line literal — the emission guard rejects
        // multi-line markdown fixtures).
        let hand = "# Plan: T\n\n## Goal\nG\n\n## Kind\nimplementation\n\n## Context\nC\n\n## Steps\n\n## Reviews\nnonsense\n2 def5678\n";
        let parsed = PlanFile::parse(hand).unwrap();
        assert_eq!(parsed.reviews.len(), 1);
        assert_eq!(parsed.reviews[0].round, 2);
        assert_eq!(parsed.reviews[0].base, "def5678");
    }

    #[test]
    fn legacy_plan_without_bug_sections_parses_to_none() {
        // Old plans (and non-bug plans) have no `## Bug` / `## Regression
        // test` sections — the fields default to None.
        let text = "\
# Plan: Legacy

## Goal
Do things

## Kind
implementation

## Context
C

## Steps
- [ ] 1. First
";
        let parsed = PlanFile::parse(text).unwrap();
        assert_eq!(parsed.bug_symptom, None);
        assert_eq!(parsed.regression_test, None);
    }

    #[test]
    fn implementation_kind_is_default_and_roundtrips() {
        // A default (implementation) plan round-trips as Implementation.
        let plan = PlanFile::new("T", "G", "C", vec!["a".into()]);
        assert_eq!(plan.kind, PlanKind::Implementation);
        let text = plan.serialize();
        assert!(
            text.contains("## Kind\nimplementation"),
            "missing kind: {text}"
        );
        let parsed = PlanFile::parse(&text).unwrap();
        assert_eq!(parsed.kind, PlanKind::Implementation);
    }

    #[test]
    fn plan_without_kind_section_defaults_to_implementation() {
        // Backward compat: an old plan .md with no `## Kind` section parses to
        // Implementation (the historical behavior — every plan was reviewed).
        let text = "\
# Plan: Legacy

## Goal
Do things

## Context
Some context

## Steps
- [ ] 1. First
";
        let plan = PlanFile::parse(text).unwrap();
        assert_eq!(plan.kind, PlanKind::Implementation);
        assert_eq!(plan.steps.len(), 1);
    }

    #[test]
    fn unknown_kind_value_defaults_to_implementation() {
        // An unrecognized kind token falls back to Implementation (safe default
        // — an unknown kind is reviewed, not silently skipped).
        let text = "\
# Plan: T

## Goal
G

## Kind
bogus

## Steps
- [ ] 1. a
";
        let plan = PlanFile::parse(text).unwrap();
        assert_eq!(plan.kind, PlanKind::Implementation);
    }

    #[test]
    fn kind_is_case_insensitive() {
        // "Research", "RESEARCH", "research" all parse to Research.
        for token in ["research", "Research", "RESEARCH", "  research  "] {
            let text =
                format!("# Plan: T\n\n## Goal\nG\n\n## Kind\n{token}\n\n## Steps\n- [ ] 1. a\n");
            let plan = PlanFile::parse(&text).unwrap();
            assert_eq!(
                plan.kind,
                PlanKind::Research,
                "token {token:?} should be Research"
            );
        }
    }

    #[test]
    fn parse_checked_and_unchecked() {
        let text = "\
# Plan: Test

## Goal
Do things

## Context
Some context

## Steps
- [ ] 1. First
- [x] 2. Second
- [ ] 3. Third
";
        let plan = PlanFile::parse(text).unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert!(!plan.steps[0].done);
        assert!(plan.steps[1].done);
        assert!(!plan.steps[2].done);
        assert_eq!(plan.steps[0].text, "First");
        assert_eq!(plan.steps[1].text, "Second");
    }

    #[test]
    fn current_step_is_first_unchecked() {
        let mut plan = PlanFile::new("T", "G", "C", vec!["a".into(), "b".into(), "c".into()]);
        plan.steps[0].done = true;
        assert_eq!(plan.current_step().unwrap().text, "b");
        plan.steps[1].done = true;
        assert_eq!(plan.current_step().unwrap().text, "c");
        plan.steps[2].done = true;
        assert!(plan.current_step().is_none());
    }

    #[test]
    fn is_complete() {
        let mut plan = PlanFile::new("T", "G", "C", vec!["a".into(), "b".into()]);
        assert!(!plan.is_complete());
        plan.complete_step(0).unwrap();
        assert!(!plan.is_complete());
        plan.complete_step(1).unwrap();
        assert!(plan.is_complete());
    }

    #[test]
    fn complete_step_out_of_range_errors() {
        let mut plan = PlanFile::new("T", "G", "C", vec!["a".into()]);
        let result = plan.complete_step(5);
        assert!(result.is_err());
    }

    #[test]
    fn complete_detailed_step_ticks_the_nth_checkbox() {
        // Backlog 9441d776: the detailed sub-steps are checkable — the
        // tick flips the nth `- [ ]` bullet in the raw section body, and
        // the marks round-trip through serialize→parse verbatim (the
        // crash-resumption document shows which sub-steps shipped).
        let mut plan = PlanFile::new("T", "G", "C", vec!["a".into()]);
        plan.detailed_steps =
            Some("- [ ] Reproduce — run the test\n- [ ] Fix the unwrap\n- [ ] Verify the suite".into());
        let summary = plan.complete_detailed_step(2).unwrap();
        assert!(summary.starts_with("1/3 detailed steps done"), "{summary}");
        assert!(summary.contains("Fix the unwrap"), "{summary}");
        let body = plan.detailed_steps.as_deref().unwrap();
        assert!(body.contains("- [ ] Reproduce — run the test"), "{body}");
        assert!(body.contains("- [x] Fix the unwrap"), "{body}");
        assert!(body.contains("- [ ] Verify the suite"), "{body}");
        // The marks survive a serialize→parse round-trip.
        let parsed = PlanFile::parse(&plan.serialize()).unwrap();
        assert!(
            parsed
                .detailed_steps
                .as_deref()
                .unwrap()
                .contains("- [x] Fix the unwrap")
        );
    }

    #[test]
    fn complete_detailed_step_normalizes_legacy_bullets() {
        // Plans created before the checkable render carry plain `- `
        // bullets — the first tick normalizes them to checkboxes so
        // in-flight older plans become tickable too.
        let mut plan = PlanFile::new("T", "G", "C", vec!["a".into()]);
        plan.detailed_steps = Some("- Reproduce the crash\n- Fix the unwrap".into());
        let summary = plan.complete_detailed_step(1).unwrap();
        assert!(summary.starts_with("1/2 detailed steps done"), "{summary}");
        let body = plan.detailed_steps.as_deref().unwrap();
        assert!(body.contains("- [x] Reproduce the crash"), "{body}");
        assert!(body.contains("- [ ] Fix the unwrap"), "{body}");
    }

    #[test]
    fn complete_detailed_step_out_of_range_and_absent_errors() {
        // No section at all → a clear error (not a panic).
        let mut plan = PlanFile::new("T", "G", "C", vec!["a".into()]);
        let err = plan.complete_detailed_step(1).unwrap_err();
        assert!(format!("{err}").contains("no '## Detailed steps' section"));
        // Out of range → the 1-indexed hint (mirrors complete_step).
        plan.detailed_steps = Some("- [ ] only".into());
        let err = plan.complete_detailed_step(2).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("1-indexed"), "{msg}");
        assert!(msg.contains("valid 1..=1"), "{msg}");
        // Re-ticking an already-done sub-step is an idempotent no-op.
        let summary = plan.complete_detailed_step(1).unwrap();
        assert!(summary.starts_with("1/1 detailed steps done"), "{summary}");
        let summary = plan.complete_detailed_step(1).unwrap();
        assert!(summary.starts_with("1/1 detailed steps done"), "{summary}");
    }

    #[test]
    fn complete_detailed_step_preserves_the_raw_text_beyond_the_mark() {
        // Review L2: the tick must flip ONLY the mark — a step authored
        // with a leading "N. " (or extra spaces after the mark) keeps it;
        // the rewrite uses the raw text after `] `, not parse_checkbox's
        // processed text (which strips skeleton-style numbering).
        let mut plan = PlanFile::new("T", "G", "C", vec!["a".into()]);
        plan.detailed_steps = Some("- [ ] 2. Guard the unwrap\n- [ ]   spaced text".into());
        let summary = plan.complete_detailed_step(1).unwrap();
        assert!(summary.starts_with("1/2 detailed steps done"), "{summary}");
        let body = plan.detailed_steps.as_deref().unwrap();
        assert!(body.contains("- [x] 2. Guard the unwrap"), "{body}");
        assert!(body.contains("- [ ]   spaced text"), "{body}");
        let summary = plan.complete_detailed_step(2).unwrap();
        assert!(summary.starts_with("2/2 detailed steps done"), "{summary}");
        let body = plan.detailed_steps.as_deref().unwrap();
        assert!(body.contains("- [x]   spaced text"), "{body}");
    }

    #[test]
    fn completed_count() {
        let mut plan = PlanFile::new("T", "G", "C", vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(plan.completed_count(), 0);
        plan.complete_step(1).unwrap();
        assert_eq!(plan.completed_count(), 1);
    }

    #[test]
    fn write_and_read_file() {
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        let plan = PlanFile::new("T", "G", "C", vec!["a".into(), "b".into()]);
        let path = plan.write_to_dir(&plans_dir, "abc123").unwrap();
        assert!(path.ends_with("abc123.md"));
        let read = PlanFile::read_from_file(&path).unwrap();
        assert_eq!(read.title, "T");
        assert_eq!(read.steps.len(), 2);
    }

    #[test]
    fn parse_empty_plan() {
        let plan = PlanFile::parse("").unwrap();
        assert!(plan.title.is_empty());
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn checkbox_parsing_edge_cases() {
        assert_eq!(
            parse_checkbox("- [ ] do something"),
            Some((false, "do something".into()))
        );
        assert_eq!(parse_checkbox("- [x] done"), Some((true, "done".into())));
        assert_eq!(parse_checkbox("- [X] done"), Some((true, "done".into())));
        assert_eq!(
            parse_checkbox("- [ ] 1. numbered step"),
            Some((false, "numbered step".into()))
        );
        assert_eq!(parse_checkbox("not a checkbox"), None);
        assert_eq!(parse_checkbox("- [y] bad mark"), None);
    }

    #[test]
    fn parse_step_header_variants() {
        // Em dash (the common case in our plan docs).
        assert_eq!(
            parse_step_header("Step 1 — Store: font family + font size state"),
            Some("Store: font family + font size state".into())
        );
        // En dash.
        assert_eq!(
            parse_step_header("Step 2 – CSS variables"),
            Some("CSS variables".into())
        );
        // Hyphen.
        assert_eq!(
            parse_step_header("Step 3 - ConfigDialog component"),
            Some("ConfigDialog component".into())
        );
        // Colon.
        assert_eq!(
            parse_step_header("Step 4: Apply font vars"),
            Some("Apply font vars".into())
        );
        // No separator — just "Step N".
        assert_eq!(parse_step_header("Step 5"), Some(String::new()));
        // Not a step header.
        assert_eq!(parse_step_header("Goal"), None);
        assert_eq!(parse_step_header("Why"), None);
        assert_eq!(parse_step_header("Scope (frontend only)"), None);
    }

    #[test]
    fn parse_freeform_plan_with_step_headers() {
        // A plan authored as a free-form doc (not via create_plan), using
        // "## Step N — Title" headers instead of a "## Steps" checklist.
        let text = "\
# Plan: Config dialog — font family + font size

## Goal
Add a Settings button to the left Sidebar toolbar.

## Why
The user wants to control the font used across the main surfaces.

## Step 1 — Store: font family + font size state
Add fontFamily/fontSize to the Zustand store.

## Step 2 — CSS variables + defaults
Add --app-font-family and --app-font-size to :root.

## Step 3 — ConfigDialog component (new)
Modal with a font-family select and font-size input.
";
        let plan = PlanFile::parse(text).unwrap();
        assert_eq!(plan.title, "Config dialog — font family + font size");
        assert_eq!(
            plan.goal,
            "Add a Settings button to the left Sidebar toolbar."
        );
        // The three "## Step N — Title" headers are parsed as steps.
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].text, "Store: font family + font size state");
        assert_eq!(plan.steps[1].text, "CSS variables + defaults");
        assert_eq!(plan.steps[2].text, "ConfigDialog component (new)");
        // Free-form step headers are always unchecked.
        assert!(!plan.steps[0].done);
        assert!(!plan.steps[1].done);
        assert!(!plan.steps[2].done);
    }

    #[test]
    fn parse_mixed_checklist_and_step_headers() {
        // A plan that uses both a "## Steps" checklist AND "## Step N" headers
        // elsewhere — the checklist steps and header steps are all collected.
        let text = "\
# Plan: Mixed

## Goal
Do things.

## Steps
- [ ] 1. First
- [x] 2. Second

## Step 3 — Cleanup
Remove old code.
";
        let plan = PlanFile::parse(text).unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].text, "First");
        assert!(!plan.steps[0].done);
        assert_eq!(plan.steps[1].text, "Second");
        assert!(plan.steps[1].done);
        assert_eq!(plan.steps[2].text, "Cleanup");
        assert!(!plan.steps[2].done);
    }

    #[test]
    fn bold_header_extracted_from_step() {
        // A step starting with **Header** parses with header = Some(...) and
        // text = the full line (header + body).
        let plan = PlanFile::new(
            "T",
            "G",
            "C",
            vec![
                "**Add ask_user tool** — create src/tool/workflow/ask_user.rs".into(),
                "Plain step with no bold".into(),
            ],
        );
        assert_eq!(
            plan.steps[0].header.as_deref(),
            Some("Add ask_user tool"),
            "bold header should be extracted"
        );
        assert!(
            plan.steps[0].text.contains("**Add ask_user tool**"),
            "full text should be preserved"
        );
        assert!(
            plan.steps[0].text.contains("create src/tool"),
            "body should be preserved in text"
        );
        // Plain step has no header.
        assert!(
            plan.steps[1].header.is_none(),
            "plain step should have no header"
        );
        assert_eq!(plan.steps[1].text, "Plain step with no bold");
    }

    #[test]
    fn bold_header_extracted_from_parsed_checklist() {
        // A checklist step with a bold header parses the header too.
        let text = "\
# Plan: T

## Goal
G

## Steps
- [ ] 1. **Header one** — body text
- [ ] 2. No bold here
";
        let plan = PlanFile::parse(text).unwrap();
        assert_eq!(plan.steps[0].header.as_deref(), Some("Header one"));
        assert!(plan.steps[0].text.contains("body text"));
        assert!(plan.steps[1].header.is_none());
    }

    #[test]
    fn bold_header_round_trips_through_serialize() {
        // The header is preserved on a round-trip (serialize → parse) because
        // it's stored in the text (the bold markdown survives).
        let plan = PlanFile::new("T", "G", "C", vec!["**Header** — body".into()]);
        let text = plan.serialize();
        let back = PlanFile::parse(&text).unwrap();
        assert_eq!(back.steps[0].header.as_deref(), Some("Header"));
        assert!(back.steps[0].text.contains("**Header**"));
    }

    #[test]
    fn empty_bold_header_is_none() {
        // A `****` (empty bold) should not produce an empty header.
        let plan = PlanFile::new("T", "G", "C", vec!["**** — body".into()]);
        assert!(plan.steps[0].header.is_none());
    }

    #[test]
    fn bold_header_tolerates_double_wrapped_runs() {
        // Regression (backlog 2026-08-20, "****Verify builds/tests**** should
        // be rendered bold"): legacy plans written by the double-wrapping
        // StepInput::to_text stored `****Header****` (and the asymmetric
        // `****Header**`). The old extractor stripped exactly 2 leading `*`
        // then found the closer immediately → EMPTY header → None → the plan
        // view fell back to the raw text with literal asterisks. Runs of 2+
        // markers must parse as ONE header.
        assert_eq!(
            extract_bold_header("****Verify builds/tests** — run them"),
            Some("Verify builds/tests".to_string())
        );
        assert_eq!(
            extract_bold_header("****Header**** — body"),
            Some("Header".to_string())
        );
        // Header-only double-wrapped form.
        assert_eq!(
            extract_bold_header("****Header**"),
            Some("Header".to_string())
        );
        // A single-asterisk header wrapped by to_text becomes `***X***` —
        // the 3-run still parses with the inner text.
        assert_eq!(
            extract_bold_header("***Add X***"),
            Some("Add X".to_string())
        );
    }

    #[test]
    fn parse_double_wrapped_checklist_line_extracts_header() {
        // Parse-level regression using the exact legacy plan-file line shape
        // (from .coding/plans/5866e160-…md): the step number is stripped by
        // parse_checkbox, then the header must extract cleanly so
        // PlanProgress/StatusBar show a bold header instead of raw
        // asterisks.
        let text = "\
# Plan: T

## Goal
G

## Kind
implementation

## Context
C

## Steps
- [x] 4. ****Verify builds/tests** — run tests
";
        let plan = PlanFile::parse(text).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert!(plan.steps[0].done);
        // The full text is preserved verbatim (the .md stays the source of
        // truth)…
        assert_eq!(plan.steps[0].text, "****Verify builds/tests** — run tests");
        // …but the header extracts for display.
        assert_eq!(plan.steps[0].header.as_deref(), Some("Verify builds/tests"));
    }

    #[test]
    fn mid_sentence_bold_is_not_a_header() {
        // B3: a step that merely *contains* bold mid-sentence (e.g.
        // "Use **bold** for emphasis") must NOT extract a header — only a
        // leading `**Header**` punchline is a header.
        let plan = PlanFile::new("T", "G", "C", vec!["Use **bold** for emphasis".into()]);
        assert!(
            plan.steps[0].header.is_none(),
            "mid-sentence bold should not be a header"
        );
        // A leading bold header followed by a separator IS a header.
        let plan2 = PlanFile::new("T", "G", "C", vec!["**Header** — body".into()]);
        assert_eq!(plan2.steps[0].header.as_deref(), Some("Header"));
        // A leading bold header with no separator (just "**Header**") IS a header.
        let plan3 = PlanFile::new("T", "G", "C", vec!["**Header**".into()]);
        assert_eq!(plan3.steps[0].header.as_deref(), Some("Header"));
        // A header containing `*` (e.g. a glob) is still extracted.
        let plan4 = PlanFile::new("T", "G", "C", vec!["**src/*.rs** — clean up".into()]);
        assert_eq!(plan4.steps[0].header.as_deref(), Some("src/*.rs"));
    }
}
