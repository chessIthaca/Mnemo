# Code Review — Slim the volatile tail: header-first CURRENT STEP + memory cap

**Reviewer:** read-only reviewer subagent
**Date:** 2026-04-16
**Scope:** ALL uncommitted working-tree changes (`git diff HEAD` + untracked)
for plan "Slim the volatile tail: header-first CURRENT STEP + memory cap".

## Files reviewed

- `src/agent/prompt.rs` (the only source change)
- `.coding/plans/34c66325-…md`, `.coding/plans/stack.json` (bookkeeping — scanned, nothing alarming)
- `.coding/plans/2ca99f43-…md` (untracked new plan doc — scanned, nothing alarming)
- Cross-checked against `src/workflow/plan_file.rs` (Step.header extraction),
  `src/workflow/mod.rs` (update_plan re-extracts headers at :461),
  `src/memory/types.rs` (MemoryTier Display), and all callers/tests
  (`CURRENT STEP`, `format_recall_context`, `take(300)` search — repo-wide).

---

## Correctness

**No findings.**

1. **Header fallback is correct** (prompt.rs:290-294).
   `step.header.as_deref().map(short_step_text).unwrap_or_else(|| short_step_text(&step.text))`
   — a step with a header renders ONLY the header; a headerless step falls back
   to `short_step_text(&step.text)`. Verified against `extract_bold_header_impl`
   (plan_file.rs:401-428): `header` is `Some` only for a leading `**Header**`
   run; `None` for plain steps and for empty bold (`****` → None). So the
   fallback never produces an empty CURRENT STEP from a headerless step: if
   `step.text` is non-empty, `short_step_text` returns it (or its first 120
   chars + '…'). An empty step text would yield an empty string, but plan steps
   are authored non-empty and `create_plan` rejects an empty steps list; this
   matches pre-change behavior (the old code rendered `step.text` verbatim).

2. **Header is freshly extracted on all mutation paths.** `PlanFile::new`,
   `PlanFile::parse`, and `Workflow::update_plan` (mod.rs:461 via
   `extract_bold_header_pub`) all populate `header`, so a step added by
   update_plan also renders header-first. No stale-header path exists.

3. **Char counting is multibyte-safe.** `short_step_text` (prompt.rs:209-217)
   uses `text.chars().count()` for the ≤120 test and `text.chars().take(120)`
   for the truncation — both operate on `char`, never bytes, so no mid-UTF-8
   split is possible. The `'…'` ellipsis is appended after the char-safe head.
   (Note: `chars()` counts Unicode scalar values, not grapheme clusters, so a
   120-"char" cap could split a multi-codepoint grapheme such as an emoji with
   a skin-tone modifier. That produces valid UTF-8 — never a panic or mojibake
   — just a visually split glyph in a prompt string. Cosmetic at worst; not a
   finding.)

4. **Memory cap 300 → 160** (prompt.rs:232) is the only change to
   `format_recall_context`; the tier/title/score header line (prompt.rs:228-231)
   is untouched. The single production caller (turn.rs:277) passes the result
   through unchanged.

5. **Stable head and back-compat wrapper untouched.** `build_stable_head`
   (:123-145) is byte-identical to before; `build_system_prompt` (:192-201)
   body is unchanged (only doc comments). GOAL (:276), title (:267-274), and
   PROGRESS (:277-281) lines are untouched — only CURRENT STEP and the memory
   cap shrank. Confirmed by reading the full file, not just the diff.

## Bugs

**No findings.**

- Test negative assertions are genuinely past the cap:
  - `workflow_section_current_step_prefers_header_and_truncates` (:606-647):
    `plain` is built via a Rust line-continuation string (the `\` strips the
    newline + leading whitespace, so it is one long single-spaced line);
    `assert!(plain.chars().count() > 120)` (:635) guards the premise; the
    truncated marker "character cutoff point" (:644) sits in the tail segment
    past char 120, so `!tail2.contains(...)` is a real negative. The positive
    assertion reconstructs `head_120` independently via `chars().take(120)`
    and requires the exact `CURRENT STEP: "{head_120}…"` form.
  - `recall_context_caps_memory_length` (:650-680): content is
    `"alpha ".repeat(50)` (300 chars) + `"UNIQUE-TAIL-MARKER"`; the marker
    starts at char 300, well past 160, so `!out.contains("UNIQUE-TAIL-MARKER")`
    truly tests the cap. The `[semantic] some title (score: 0.75)` assertion
    matches `MemoryTier`'s Display (types.rs:47-51 → `as_str()` → "semantic")
    and `{:.2}` formatting of 0.75. The `head_160` assertion is independent.
- No other tests or callers depend on the old behavior: repo-wide search for
  `CURRENT STEP` / `take(300)` / `format_recall_context` shows the existing
  tests (`executing_prompt_shows_plan` :466 uses the short step "step 1",
  ≤120 chars so unchanged; `volatile_tail_includes_workflow_and_memories`
  :597 asserts only the `CURRENT STEP` literal, not a body). No regression.

## Security

**No findings.** The change only shortens two prompt strings; no new input
surface, no secrets, no injection vector (the step text and memory content
were already embedded verbatim before — this embeds strictly less).

## Constitution compliance

**No findings.**

- No `#[allow(...)]` suppressions added anywhere in the diff.
- All new/changed items have doc comments: `short_step_text` (:203-208),
  `format_recall_context` (:219-223), `build_volatile_tail` (:147-159) — and
  `short_step_text` is private, so no public-API breakage. The public
  functions (`build_stable_head`, `build_volatile_tail`, `build_system_prompt`,
  `format_recall_context`, `CONTEXT_FOOTER`) all retain their doc comments.
- Reported `cargo test` green (754 lib + integration) implies warning-free
  under `#![deny(warnings)]`; nothing in the diff introduces an unused import,
  dead code, or an unneeded `mut` (verified by reading the changed hunks).
- Line endings: the only Git warning is the pre-existing LF→CRLF notice on the
  *old* plan bookkeeping file (`.coding/plans/34c66325-…md`), not on
  `src/agent/prompt.rs`; the source file's style is preserved.
- `.coding/` bookkeeping diff (stack.json id swap + the previous plan's
  step-4 checkbox) is routine plan state, nothing alarming.

---

## Verdict

**No findings.** The diff is clean across correctness, bugs, security, and
constitution compliance. The header-first CURRENT STEP and 160-char memory cap
behave exactly as the plan describes, the char-safe truncation cannot split a
UTF-8 sequence, the stable head and back-compat wrapper are untouched, and the
two new tests are meaningful (their negative markers are verifiably past the
respective caps).
