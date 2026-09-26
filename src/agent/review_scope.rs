// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The harness-rendered reviewer preamble (backlog 85313a7e).
//!
//! A reviewer subagent's first prompt used to be *exactly* the dispatching
//! agent's `task` string. That made the review's contract — which verdict
//! lines `finish` accepts, the project constitution's checks, the
//! `.coding/**` bookkeeping rule — depend on the dispatcher remembering to
//! write it, and left a re-review (round 2+) with no notion of what the
//! previous round had already verified: every round re-read the whole
//! uncommitted diff. Plan febcd6f5 burned three rounds that way (~33 min),
//! each one re-reading ~28 files to verify a 1-3 file fix.
//!
//! This module makes both properties structural:
//!
//! - [`render_review_preamble`] is appended to EVERY reviewer spawn's task by
//!   the spawn tool, so the contract cannot be forgotten.
//! - For round >= 2 it also renders the delta scope for the round: the base
//!   revision the previous round verified (stamped on the plan frame,
//!   [`crate::workflow::Workflow::record_review_round`]), the
//!   delta-since-`<base>` instruction (naming the reviewer's own `git_read`
//!   ops), and the mechanically-derived changed file set — so the reviewer
//!   verifies the delta instead of the whole tree.
//!
//! [`ReviewScoper`] abstracts the two git reads (HEAD, changed paths) so the
//! spawn tool's tests stay hermetic; production uses [`GitReviewScoper`].

use std::path::Path;

use async_trait::async_trait;

/// The git facts a reviewer scope needs — abstraction over
/// [`crate::project::git_ops`] so tests can inject canned answers instead of
/// spawning git (the `GitRunner` pattern in the git module).
#[async_trait]
pub trait ReviewScoper: Send + Sync {
    /// The current HEAD sha in `root` — the base revision a round stamps.
    async fn head(&self, root: &Path) -> Result<String, String>;

    /// The files changed since `base`, one `<status> <path>` entry each.
    async fn changed_paths(&self, root: &Path, base: &str) -> Result<Vec<String>, String>;
}

/// The production [`ReviewScoper`] — delegates to the hardened git runner in
/// [`crate::project::git_ops`].
pub struct GitReviewScoper;

#[async_trait]
impl ReviewScoper for GitReviewScoper {
    async fn head(&self, root: &Path) -> Result<String, String> {
        crate::project::git_ops::head_commit_sha(root.to_path_buf()).await
    }

    async fn changed_paths(&self, root: &Path, base: &str) -> Result<Vec<String>, String> {
        crate::project::git_ops::changed_paths_since(root.to_path_buf(), base.to_string()).await
    }
}

/// How many changed paths the preamble lists before summarizing the rest.
/// The point of the delta scope is a SMALLER prompt; a pathological diff must
/// not grow it back (the reviewer can enumerate the rest via `git_read`).
const MAX_LISTED_PATHS: usize = 40;

/// Everything the preamble renders: the round number, the base revision the
/// PREVIOUS round verified (`None` for a first review), and the changed file
/// set (or why it could not be derived).
#[derive(Debug, Clone, Default)]
pub struct ReviewScope {
    /// The round being dispatched, 1-based (1 = first review of the plan).
    pub round: u32,
    /// The base revision the previous round verified — the delta's lower
    /// bound. `None` on a first review (nothing to delta against).
    pub base: Option<String>,
    /// The files that changed since `base`, `<status> <path>` per entry.
    pub changed: Vec<String>,
    /// Why the changed-file set is missing (a git failure) — rendered instead
    /// of the list, so a git problem never blocks the spawn.
    pub changed_note: Option<String>,
}

/// Render the preamble appended to a reviewer's task.
///
/// The contract block is ALWAYS present (round 1 included). The delta block is
/// added for round >= 2, when a base revision is known.
pub fn render_review_preamble(scope: &ReviewScope) -> String {
    let mut out = String::from(
        "---\n\
         ## Reviewer contract (rendered by the harness — the dispatching agent does not supply this)\n\
         \n\
         - **Verdict contract (fails closed).** Your report MUST open with exactly \
         `## Verdict: PASS` or `## Verdict: FINDINGS (n high, n low)`. Anything else is parsed \
         as FINDINGS and blocks the plan's finish.\n\
         - **Project constitution (agent.md), one line each:**\n\
         - *Documentation sync* — a behaviour change updates README.md / PLAN.md / module doc \
         comments; report stale or missing docs as a finding.\n\
         - *Multi-platform neutrality* — no Windows-only API, path or shell syntax in library \
         or app code (the WebView2 `game_*` tooling is the one sanctioned exception): the \
         change must hold on macOS and Windows.\n\
         - *File-tools-first* — flag shell-based file mutation (`Set-Content`, `Out-File`, \
         `Add-Content`, `>>`, `sed -i`, `tee`, ad-hoc python) where `file_edit`/`file_write` \
         would work.\n\
         - *Warning-free build* — `#![deny(warnings)]` sits at both crate roots, so a green \
         `cargo test` already proves zero warnings; `#[allow(...)]` to silence one is a finding.\n\
         - **Bookkeeping exclusion.** `.coding/**` (plans, reviews, knowledge, backlog.jsonl) \
         gets an ACCURACY check in one line — does it match what shipped? — never a line \
         review. Generated / lock file / dist noise (`target/`, `node_modules/`, `Cargo.lock`) \
         is out of scope entirely.\n\
         - **Empty-diff rule.** A clean working tree means the uncommitted diff IS empty — \
         that is not a review. Then verify what this branch added since it forked from main: \
         walk the branch's commits with `git_read` (`op=\"log\"`) and read each one with \
         `git_read` (`op=\"show\"`) (the task's scope states where the fork point is). Never \
         report PASS without saying what you actually read.\n\
         - **You are read-only.** Your only output channel is `write_review_report`.\n",
    );

    if scope.round < 2 {
        return out;
    }
    let Some(base) = scope.base.as_deref().filter(|b| !b.trim().is_empty()) else {
        // Round >= 2 without a recorded base: never invent a scope, and never
        // silently claim the tree is unchanged. Fall back to a full review.
        out.push_str(
            "\n## Re-review scope\n\
             No base revision is recorded for this plan, so the delta is unknown — review the \
             full change set as a first review.\n",
        );
        return out;
    };

    let previous = scope.round - 1;
    out.push_str(&format!(
        "\n## Re-review scope (round {round} of this plan)\n\
         \n\
         - **Base revision: `{base}`** — the tree state round {previous} verified, and the \
         lower bound of your scope. Everything up to it was already reviewed and is unchanged \
         unless it shows up in the delta below.\n\
         - **Your scope is the delta since `{base}`** — nothing earlier. Your surface is \
         read-only: enumerate the intervening commits with `git_read` (`op=\"log\"`), read \
         each one's diff with `git_read` (`op=\"show\"`), and take the uncommitted remainder \
         with `git_read` (`op=\"diff\"`). Verify ONLY those hunks: a file appearing in the \
         changed set below does not license a full re-read of it — read the hunks, not the \
         file.\n",
        round = scope.round,
        base = base,
        previous = previous,
    ));

    match (&scope.changed_note, scope.changed.is_empty()) {
        // A git failure deriving the set: name the READ-ONLY way to enumerate
        // it, not a raw `git diff <base>` — the reviewer has no shell and no
        // arbitrary base-diff, so that would be unrunnable advice.
        (Some(note), _) => {
            out.push_str(&format!(
                "- **Changed file set: unavailable** ({note}). Enumerate it yourself: `git_read` \
                 (`op=\"log\"`) for the commits since `{base}`, then `git_read` \
                 (`op=\"diff\"`) for the uncommitted remainder.\n",
            ));
        }
        // The no-change case must never read as a licence to PASS over a round
        // that ENDED in findings: an unchanged tree means nothing was fixed.
        (None, true) => {
            out.push_str(&format!(
                "- **Changed file set: empty** — nothing changed since round {previous}. If your \
                 task names NO findings awaiting a fix, report PASS without re-reading a file. \
                 If it DOES name findings, an unchanged tree means none of them were fixed — \
                 report that as FINDINGS, not PASS.\n",
            ));
        }
        (None, false) => {
            out.push_str("- **Changed file set** (mechanically derived at dispatch):\n");
            for entry in scope.changed.iter().take(MAX_LISTED_PATHS) {
                out.push_str(&format!("  - `{entry}`\n"));
            }
            let hidden = scope.changed.len().saturating_sub(MAX_LISTED_PATHS);
            if hidden > 0 {
                out.push_str(&format!(
                    "  - … and {hidden} more — enumerate the rest with `git_read` (`op=\"log\"`)
",
                ));
            }
        }
    }

    out.push_str(&format!(
        "- **Uncommitted carry-over.** If the delta re-shows hunks that a round ending in PASS \
         already verified (that material was never committed), note it in ONE line as a process \
         remark — do not re-line-review it. Hunks a round ending in FINDINGS reported are NOT in \
         that class: if your task names findings awaiting a fix, verify each fix is present in \
         the delta before you report PASS.\n",
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_round_renders_the_contract_without_a_delta_scope() {
        // Round 1 has nothing to delta against: the contract block is always
        // present, the delta block never is.
        let text = render_review_preamble(&ReviewScope {
            round: 1,
            ..Default::default()
        });
        assert!(text.contains("## Reviewer contract"), "{text}");
        assert!(text.contains("## Verdict: PASS"), "{text}");
        assert!(text.contains("## Verdict: FINDINGS (n high, n low)"), "{text}");
        assert!(!text.contains("Re-review scope"), "{text}");
        assert!(!text.contains("Base revision"), "{text}");
    }

    #[test]
    fn the_contract_carries_the_constitution_lines_and_the_bookkeeping_rule() {
        // Acceptance (b)+(d): the standard preamble — constitution checks as
        // one line each, and the `.coding/**` accuracy-not-line-review rule —
        // is present WITHOUT the dispatching agent supplying it.
        let text = render_review_preamble(&ReviewScope::default());
        assert!(text.contains("Documentation sync"), "{text}");
        assert!(text.contains("Multi-platform neutrality"), "{text}");
        assert!(text.contains("File-tools-first"), "{text}");
        assert!(text.contains("Warning-free build"), "{text}");
        assert!(text.contains(".coding/**"), "{text}");
        assert!(text.contains("never a line"), "{text}");
        assert!(text.contains("Generated / lock file / dist noise"), "{text}");
        // The step-boundary commit convention leaves an empty uncommitted diff
        // at review time — an empty diff must never read as a vacuous PASS.
        assert!(text.contains("Empty-diff rule"), "{text}");
        // The rule must point at the reviewer's own read-only surface — the
        // single `git_read` tool (ops diff/log/show/status), NOT the legacy
        // `git_log`/`git_show`/`git_diff` struct names it replaced (review
        // round 2, F1).
        assert!(
            text.contains("walk the branch's commits with `git_read` (`op=\"log\"`)"),
            "{text}"
        );
        assert!(text.contains("write_review_report"), "{text}");
        assert_no_retired_git_tool_names(&text);
    }

    #[test]
    fn second_round_names_the_base_and_the_delta_instruction_and_the_file_set() {
        // Acceptance (a): a 2nd+ reviewer spawn names its base revision, the
        // delta instruction and the changed file set.
        let text = render_review_preamble(&ReviewScope {
            round: 2,
            base: Some("abc1234".to_string()),
            changed: vec!["M src/a.rs".to_string(), "?? docs/new.md".to_string()],
            changed_note: None,
        });
        assert!(text.contains("Re-review scope (round 2 of this plan)"), "{text}");
        assert!(text.contains("Base revision: `abc1234`"), "{text}");
        assert!(text.contains("the tree state round 1 verified"), "{text}");
        // Review round 1 LOW 1: the delta instruction names commands the
        // reviewer's READ-ONLY surface actually exposes, not a bare
        // `git diff <base>` it has no way to run.
        assert!(text.contains("Your scope is the delta since `abc1234`"), "{text}");
        assert!(text.contains("`git_read` (`op=\"log\"`)"), "{text}");
        assert!(text.contains("`git_read` (`op=\"show\"`)"), "{text}");
        assert!(text.contains("`git_read` (`op=\"diff\"`)"), "{text}");
        // Review round 2 F1: the pre-fix wording named git_log/git_show/
        // git_diff, which are NOT registered tools — only `git_read` is (the
        // legacy structs survive as its internal delegates). Review round 3
        // LOW 3 moved this into a helper that EVERY tool-name-emitting render
        // calls, so the truncation line and the unavailable-set branch are
        // covered too — the single inline check here could not reach them.
        assert_no_retired_git_tool_names(&text);
        assert!(
            !text.contains("git diff abc1234"),
            "no unrunnable bare git diff in the scope instruction: {text}"
        );
        assert!(text.contains("M src/a.rs"), "{text}");
        assert!(text.contains("?? docs/new.md"), "{text}");
        // The contract rides along on a re-review too.
        assert!(text.contains("## Verdict: PASS"), "{text}");
    }

    #[test]
    fn a_third_round_deltas_against_the_second() {
        let text = render_review_preamble(&ReviewScope {
            round: 3,
            base: Some("def5678".to_string()),
            changed: vec!["M src/b.rs".to_string()],
            changed_note: None,
        });
        assert!(text.contains("(round 3 of this plan)"), "{text}");
        assert!(text.contains("round 2 verified"), "{text}");
        assert!(text.contains("`git diff def5678`") == false, "{text}");
        assert!(text.contains("the delta since `def5678`"), "{text}");
    }

    #[test]
    fn an_empty_changed_set_says_the_diff_should_be_empty_too() {
        let text = render_review_preamble(&ReviewScope {
            round: 2,
            base: Some("abc1234".to_string()),
            changed: Vec::new(),
            changed_note: None,
        });
        assert!(text.contains("Changed file set: empty"), "{text}");
        assert!(text.contains("report PASS without"), "{text}");
    }

    #[test]
    fn a_git_failure_renders_an_instruction_instead_of_a_file_list() {
        // Best-effort by design: a git failure must never block the spawn,
        // and must never be papered over as "nothing changed".
        let text = render_review_preamble(&ReviewScope {
            round: 2,
            base: Some("abc1234".to_string()),
            changed: Vec::new(),
            changed_note: Some("fatal: bad revision".to_string()),
        });
        assert!(text.contains("Changed file set: unavailable"), "{text}");
        assert!(text.contains("fatal: bad revision"), "{text}");
        assert!(text.contains("Enumerate it yourself: `git_read`"), "{text}");
        // Review round 3 LOW 3: pin the branch's TRAILING mention too — the
        // bare `op="diff"` check above was satisfied by the delta intro in the
        // same render, so a regression of this text used to pass.
        assert!(
            text.contains("then `git_read` (`op=\"diff\"`) for the uncommitted remainder"),
            "{text}"
        );
        assert!(
            !text.contains("nothing changed since"),
            "a git failure must never claim the tree is unchanged: {text}"
        );
        assert_no_retired_git_tool_names(&text);
    }

    #[test]
    fn a_missing_base_never_invents_a_delta() {
        // Defensive: round >= 2 with no recorded base falls back to a full
        // review rather than claiming the tree was verified.
        let text = render_review_preamble(&ReviewScope {
            round: 2,
            base: None,
            changed: vec!["M src/a.rs".to_string()],
            changed_note: None,
        });
        assert!(text.contains("No base revision is recorded"), "{text}");
        assert!(text.contains("review the full change set"), "{text}");
    }

    #[test]
    fn a_blank_base_is_treated_as_missing() {
        let text = render_review_preamble(&ReviewScope {
            round: 2,
            base: Some("   ".to_string()),
            changed: Vec::new(),
            changed_note: None,
        });
        assert!(text.contains("No base revision is recorded"), "{text}");
    }

    /// Assert a rendered preamble names no RETIRED git tool: `git_log`,
    /// `git_show` and `git_diff` are not registered tools — the reviewer's only
    /// read-only git tool is `git_read`, ops `diff`/`log`/`show`/`status`
    /// (review round 2 F1). Called on EVERY render whose branch can emit a tool
    /// name, so a wording change cannot reintroduce the trio (review round 3
    /// LOW 3: the truncation line and the unavailable-set branch sat outside
    /// the original single-render check).
    fn assert_no_retired_git_tool_names(text: &str) {
        for name in ["git_log", "git_show", "git_diff"] {
            assert!(
                !text.contains(name),
                "retired git tool name `{name}` in a rendered preamble: {text}"
            );
        }
    }

    #[test]
    fn the_rendered_preamble_stays_within_a_prompt_budget() {
        // The point of this change is a SMALLER prompt (backlog 85313a7e: the
        // dispatch brief had grown to a plan recap + file inventory + six
        // verify questions). Pin the budget so a future addition cannot
        // quietly grow the boilerplate back.
        let contract = render_review_preamble(&ReviewScope::default());
        // Measured 1705 chars when the block landed (2027-01-11), 1791 after
        // review round 2 F1 named `git_read`/its op instead of the
        // unregistered trio (bounds loosened 1800 → 2000 with it, restoring
        // tweak-class headroom without admitting a paragraph).
        assert!(
            contract.len() < 2000,
            "contract block is {} chars",
            contract.len()
        );
        let delta = render_review_preamble(&ReviewScope {
            round: 2,
            base: Some("abc1234".to_string()),
            changed: vec!["M src/a.rs".to_string()],
            changed_note: None,
        });
        // Measured 2460 chars at landing, 2853 after round 1's LOW 1/LOW 2
        // rewording, 2909 after round 2's F1 (`git_read` + op). Bounds loosened
        // 3000 → 3100 with F1: ~190 chars of tweak headroom, while a
        // paragraph-class addition (a LOW-2-style rewording measures ~390)
        // still breaches it.
        assert!(
            delta.len() < 3100,
            "delta-scoped block is {} chars",
            delta.len()
        );
    }

    #[test]
    fn an_unchanged_tree_never_passes_over_unresolved_findings() {
        // Review round 1 LOW 2: the no-change paths must not become a licence
        // to PASS over a round that ENDED in findings — an unchanged tree means
        // nothing was fixed, and the carry-over exemption is scoped to a round
        // that ended PASS.
        let text = render_review_preamble(&ReviewScope {
            round: 2,
            base: Some("abc1234".to_string()),
            changed: Vec::new(),
            changed_note: None,
        });
        assert!(text.contains("report that as FINDINGS, not PASS"), "{text}");
        assert!(text.contains("a round ending in PASS"), "{text}");
        assert!(
            text.contains("verify each fix is present in the delta"),
            "{text}"
        );
    }

    #[test]
    fn a_huge_changed_set_is_capped() {
        // The whole point is a smaller prompt: a pathological diff must not
        // grow it back past the cap.
        let changed: Vec<String> = (0..MAX_LISTED_PATHS + 5)
            .map(|i| format!("M src/f{i}.rs"))
            .collect();
        let text = render_review_preamble(&ReviewScope {
            round: 2,
            base: Some("abc1234".to_string()),
            changed,
            changed_note: None,
        });
        assert!(text.contains("`M src/f0.rs`"), "{text}");
        assert!(text.contains("… and 5 more"), "{text}");
        assert!(!text.contains("M src/f44.rs"), "{text}");
        // Review round 3 LOW 3: the truncation LINE emits a tool name and sat
        // outside the negative check — pin it, and run the check on THIS render.
        assert!(
            text.contains("enumerate the rest with `git_read` (`op=\"log\"`)"),
            "{text}"
        );
        assert_no_retired_git_tool_names(&text);
    }
}
