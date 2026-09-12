# Review — Prompt re-tiering (universal rules → compiled stable head)

**Date:** 2026-11-23
**Scope:** All uncommitted changes (`git diff HEAD`).
**Changed files:** `src/agent/prompt.rs`, `src/agent/context.rs`,
`src/tool/agent/file_append.rs`, `agent.md`, `src-tauri/src/main.rs`
(+ bookkeeping: `.coding/plans/*`).

## Summary

The diff re-tiers the system prompt so universal app-mechanics rules move from
the project `agent.md` into a new `APP_RULES` const baked into the compiled
stable head (`build_stable_head`), genericizes "cargo test" → "the project's
tests" in the compiled prompt while preserving "cargo test" in `agent.md`,
dedupes `CODING_SYSTEM_PREAMBLE`, trims the `workflow_section` Executing arm,
DRYs the summarization prompt via a shared `SUMMARY_FORMAT` const, and moves
the file_append ~5000-char guidance into the tool schema.

All six review criteria pass. **One minor doc-quality finding** (below).

---

## Criterion-by-criterion verification

### 1. CORRECTNESS — No universal rule lost in the agent.md trim ✅

Compared every removed line in the old `agent.md` against the new `APP_RULES`
const (`src/agent/prompt.rs:95-133`). All 11 universal rules are preserved:

| Old agent.md rule | APP_RULES location | Status |
|---|---|---|
| Never commit to main (except merge_to_main skill) | L98-99 | ✅ |
| Core operations always approval-gated | L100-101 | ✅ |
| Bookkeeping tools never prompt | L102-104 | ✅ |
| Never resend failed tool call | L105-106 | ✅ |
| Preserve line-ending style | L107-108 | ✅ |
| Final summaries ≤3 lines | L109-110 | ✅ |
| Closing seq step 1 — Test | L114 | ✅ ("the project's tests") |
| Closing seq step 2 — Review (read-only reviewer, review ALL changes, self-contained task, never sleep/poll) | L115-124 | ✅ |
| Closing seq step 3 — Act on findings (fix EVERY finding, never defer, re-run tests) | L125-128 | ✅ |
| Closing seq step 4 — Commit to feature branch, include report | L129-130 | ✅ |
| Research plans skip review | L112 | ✅ |

The warning-free / `#![deny(warnings)]` / no-`#[allow]` detail correctly
**stays in `agent.md`** (L40-44) — it is project-specific (depends on this
project's crate roots using `#![deny(warnings)]`), not a universal rule. This
matches the plan's intent (that rule is not in the criterion-1 list).

Minor condensations (not losses): the merge_to_main skill mechanics
(stash/merge/resolve/delete/skill_end) and the `Tool::never_auto_for`
implementation reference are dropped — both are skill/implementation details,
not universal rules. The backlog-IPC note is dropped (those are UI→Tauri calls,
not agent tools). All acceptable.

### 2. CORRECTNESS — "cargo test" genericized in compiled prompt, preserved in agent.md ✅

- `src/agent/prompt.rs`: **zero** "cargo test" matches (search confirmed).
  `APP_RULES` step 1 says "run the project's tests"; step 3 says "Re-run the
  project's tests"; `workflow_section` Reviewing arm says "re-run the project's
  tests" (was "re-run cargo test"); `WORKFLOW_LIFECYCLE` REVIEWING arm says
  "re-run tests".
- `agent.md`: "cargo test" preserved at L27 (powershell example), L39 ("Run
  `cargo test` before marking a workflow step complete"), L42 ("a green `cargo
  test` already proves there are zero warnings").

### 3. CACHE STABILITY — APP_RULES byte-stable ✅

- `APP_RULES` is `const APP_RULES: &str = "...";` — a fixed literal, no
  interpolation, no `format!`, no timestamps. ✅
- Wired into `build_stable_head` via `prompt.push_str(APP_RULES)` (L171).
  `build_stable_head` takes only `&Constitution` — it does **not** take
  `&Workflow`, so the head is byte-identical across Planning/Executing/Reviewing/
  Complete. ✅
- `CONTEXT_FOOTER` (L148) is unchanged. ✅
- `stable_head_byte_stable_across_workflow_changes` (L658-688): builds the head
  in three workflow states and asserts equality — sound, since
  `build_stable_head` ignores workflow. ✅
- `stable_head_excludes_volatile_sections` (L628-656): asserts the head lacks
  "WORKFLOW STATE" / "RECALLED MEMORIES" / "CURRENT STEP" — none of these
  substrings appear in `CODING_SYSTEM_PREAMBLE`, `WORKFLOW_LIFECYCLE`, or
  `APP_RULES`. ✅
- New test `stable_head_contains_universal_closing_rules` (L518-558) asserts the
  key markers ("CLOSING SEQUENCE", `role:"reviewer"`, ".coding/reviews/",
  "fix EVERY finding", "Never commit to the main branch", "feature branch",
  "never prompt", "Never resend a failed tool call") — all present in `APP_RULES`.
  ✅

### 4. BUILD — warning-free, no #[allow], doc comments ✅ (one minor doc issue)

- No `#[allow(...)]` added anywhere in the diff. ✅
- New private consts (`APP_RULES`, `SUMMARY_FORMAT`) have doc comments. ✅
- No new public items lack docs. ✅
- The renamed test `preamble_carries_core_working_principles` (was
  `preamble_emphasizes_workflow_tool_gating_research_and_subplans`) uses
  substrings genuinely unique to the preamble ("Plan before you code", "Read
  before you write", "Be precise") — disjoint from the lifecycle test. ✅
- The old preamble assertions ("gates your tools", "Plan even for
  research/investigation work", "create sub-plans AT ANY POINT during
  EXECUTING") are now covered by `lifecycle_encourages_research_planning_and_subplans`
  (asserts "research", "sub-plan", "AT ANY POINT"). Coverage preserved. ✅

### 5. DRY — SUMMARY_FORMAT shared by both branches ✅

`SUMMARY_FORMAT` (`src/agent/context.rs:334-349`) is referenced by both branches
of `build_summary_prompt`:
- Running-update branch (L377): `...Keep the same structured format:{SUMMARY_FORMAT}`
- Fresh branch (L385): `...## Conversation turns\n\n{conv_text}{SUMMARY_FORMAT}`

The two branches now differ only in intro + payload placement (previous-summary
block vs conversation-turns block). ✅

Cosmetic note (not a finding): the fresh branch now carries the phrase "focus on
the older context that will be dropped" twice (once in its intro, once in the
shared `SUMMARY_FORMAT` which was derived from the running-update branch). This
is harmless reinforcement, not a bug.

### 6. No behavior change to tool execution / workflow state machine / turn loop ✅

- `prompt.rs`: text-only changes to consts + `workflow_section` strings + tests.
  No logic change. ✅
- `context.rs`: `SUMMARY_FORMAT` extraction — the produced prompt text is
  near-identical (minor "## Instructions" header repositioning); no change to
  the summarization flow, message-slot logic, or interrupt handling. ✅
- `file_append.rs`: only the `content` param `description` string changed (added
  the ~5000-char guidance). No logic change. ✅
- `agent.md`: content only. ✅

---

## Findings

### LOW — Doc-comment misattachment in `src/agent/context.rs` (L319-351)

The doc-comment block at `src/agent/context.rs:319-333` was originally written
to document `build_summary_prompt`, but the new `SUMMARY_FORMAT` const was
inserted between the doc comment and the function (L334). In Rust, a doc
comment attaches to the *immediately following* item, so:

- **`SUMMARY_FORMAT` (L334)** now carries a doc comment whose first half
  (L319-328: "Build the structured summarization prompt for the LLM. Detects
  whether `messages[1]` is an existing summary…") describes the *function*, not
  the const. Only L329-333 actually describes the const.
- **`build_summary_prompt` (L351)** lost its doc comment entirely.

This does **not** fail the build: `build_summary_prompt` is private (`fn`, not
`pub fn`), so the constitution rule "All public functions must have doc
comments" does not apply, and there is no `#![deny(missing_docs)]`. It is a
doc-quality regression the plan introduced.

**Fix:** Move the function-describing portion (L319-328, through "See the
module-level docs for the full design rationale.") to immediately precede
`fn build_summary_prompt` (L351), and keep only the const-describing portion
(L329-333) above `const SUMMARY_FORMAT`. Result:

```rust
/// The shared summary-format + compression instructions, used by both the
/// fresh-summary and running-update branches of [`build_summary_prompt`].
/// Extracted so the two branches differ only in their intro + payload
/// placement (previous-summary block vs conversation-turns block), not in
/// the ~700-char format spec they both carry.
const SUMMARY_FORMAT: &str = "\n\n\
             ...
             needs.";

/// Build the structured summarization prompt for the LLM.
///
/// Detects whether `messages[1]` is an existing summary (starts with
/// `## Conversation summary`) and, if so, includes it so the model UPDATES it
/// rather than re-summarizing from scratch (less lossy, more efficient). The
/// prompt uses structured sections (Current Task, Key Decisions, Files &
/// Identifiers, Errors & Blockers, Open Items) and instructs aggressive
/// compression of tool outputs + verbatim preservation of identifiers.
///
/// See the module-level docs for the full design rationale.
fn build_summary_prompt(messages: &[Message], to_summarize: &[Message]) -> String {
```

---

## Observations (not findings)

### Out-of-scope but correct: `src-tauri/src/main.rs:226-231`

The diff changes `tokio::spawn` → `tauri::async_runtime::spawn` in the `setup`
closure (embedder auto-start probe). This is unrelated to the prompt re-tiering
plan (it belongs to the embedder work) but is a **correct bug fix**: the `setup`
closure runs outside any tokio runtime context, so bare `tokio::spawn` panics
("there is no reactor running"). `tauri::async_runtime::spawn` is the
established pattern in this codebase (`ipc/browser.rs:30`, `ipc/events.rs:169`,
`ipc/spawn.rs:155`). The added comment is accurate and helpful. No action
needed.

### Bookkeeping changes

`.coding/plans/stack.json` (stack pointer update) and
`.coding/plans/8a482f2f-*.md` (step 12 marked complete) are plan-state
bookkeeping, not source changes. The untracked `.coding/analysis/
system-prompt-review.md` and two untracked plan `.md` files are bookkeeping
artifacts. None affect the review.

---

## Verdict

The diff is clean against all six criteria. The single finding (doc-comment
misattachment in `context.rs`) is low-severity, does not affect the build or
runtime behavior, and has a one-block fix. Recommend applying the fix before
commit.
