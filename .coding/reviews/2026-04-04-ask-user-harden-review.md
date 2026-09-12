# Review: Harden `ask_user` — require ≥2 options + choice-framing guidance

**Date:** 2026-04-04
**Reviewer:** read-only reviewer subagent
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD` + untracked files).
**Plan:** `.coding/plans/58a076dc-be04-4c24-a74f-aa6b46063453.md` — "Harden ask_user: require ≥2 options + choice-framing guidance"

## Verdict

**No findings.** The diff is clean, correct, and constitution-compliant. The
`ask_user` hardening is implemented exactly as the plan specified, the schema and
runtime validation are consistent, the tests exercise the new behavior, and the
unrelated bookkeeping changes are benign.

## What changed

- `src/tool/workflow/ask_user.rs` — the substantive change:
  - `parse_ask_user_args` now rejects `< 2` options with a clear error mentioning "at least 2".
  - JSON schema marks `options` as `required` and adds `"minItems": 2`.
  - Tool description rewritten to frame the tool as a choice between ≥2 concrete options and to tell the agent to skip the tool (post as text) for open-ended questions.
  - Tests: old `parses_args_with_no_options` replaced with `rejects_zero_options`, `rejects_single_option`, `accepts_exactly_two_options`.
- Bookkeeping (non-source): 7 stale `.claude/plans/*.md` docs deleted; `.coding/backlog.json` (item 30 → done, item 29 removed); `.coding/plans/stack.json` (stack pointer → new plan id); new plan file `.coding/plans/58a076dc-…md` added.

## Detailed checks

### Correctness — validation logic (`ask_user.rs:156-180`)

- The `a.options.len() < 2` check (line 164) is placed **after** the empty-question
  check (line 161-163). Ordering is correct — a missing/empty question is caught
  first with its own message, then the options count is enforced.
- The error message is clear and actionable: it states the requirement ("at least
  2 options"), reports the actual count (`got {n}`), and tells the agent what to
  do instead ("skip this tool and post the question as text"). ✓
- The dispatch path surfaces this error correctly: `dispatch.rs:305-310` returns
  `parse_ask_user_args` errors as `ToolResult::error(e)`, so the agent receives
  the "at least 2" error and can react (post the question as text). The plan's
  premise — that enforcement is "by construction" via the dispatch error path —
  holds. ✓

### Schema consistency (`ask_user.rs:97-125`)

- `"required": ["question", "options"]` (line 124) and `"minItems": 2` (line 107)
  are both present and consistent with the runtime `len() < 2` check. ✓
- The options-array description ("at least 2; 2–6 recommended", line 106) matches
  the struct field doc comment (line 39) and the tool description ("At least 2
  options (2–6 ideal)", line 95). No contradiction. ✓
- The tool description (lines 87-96) accurately frames the tool as a choice
  between concrete options, gives the "A or B?" / "Which approach?" framing
  guidance, and explicitly says to skip the tool for open-ended questions. It
  does not contradict the schema or struct docs. ✓

### `#[serde(default)]` on `options` (line 41) — verified OK, not a finding

The `options` field retains `#[serde(default)]` (pre-existing — **not** introduced
by this diff) even though the schema now marks `options` as `required`. This is
harmless and arguably better than removing it: a missing `options` key
deserializes to an empty `Vec`, which the `len() < 2` check then rejects with the
clear "at least 2 options" message — rather than a terse serde "missing field
`options`" error. The runtime validation is the real enforcement and it is
correct. Defense-in-depth, not a bug.

### Tests (`ask_user.rs:186-273`)

- `rejects_zero_options` (line 205): passes `{"question": "What now?"}` (no
  `options` key). `#[serde(default)]` → empty vec → `len() < 2` → error
  containing "at least 2". Correctly exercises the missing-key path. ✓
- `rejects_single_option` (line 217): 1 option → error mentioning "at least 2". ✓
- `accepts_exactly_two_options` (line 231): exactly 2 options → success, `len() == 2`. ✓
- `parses_args_with_options` (line 187): 2 options with descriptions → still
  succeeds (unchanged, still valid). ✓
- `rejects_empty_question` (line 242) / `rejects_missing_question` (line 248):
  still assert `is_err()`. `rejects_missing_question` passes 1 option + no
  question; deserialization fails on the missing `question` field before the
  options check — still `is_err()`. Correct. ✓
- `execute_fallback_returns_clear_error` (line 253): passes `options: []` but
  calls `execute()` directly (NOT `parse_ask_user_args`). `execute` is the
  never-reached-in-production fallback (dispatch intercepts `ask_user` first);
  it deserializes fine with the empty vec and returns the "use the question
  path" error. The test asserts `!success` + `contains("question path")` — still
  passes. The fact that `execute` doesn't enforce ≥2 options is correct: the
  real enforcement lives in `parse_ask_user_args` on the dispatch path. ✓
- `schema_has_question_and_options` (line 264): still valid. ✓

The tests genuinely exercise the new behavior (zero, one, and two options) and
assert on the error message text, not just `is_err()`.

### Constitution compliance

- **`#![deny(warnings)]` / no `#[allow(...)]`:** No `#[allow(...)]` suppressions
  were added anywhere in the diff. ✓
- **No dead code / unused imports:** The diff only touches the description
  strings, the schema JSON, the validation block, and tests. No new imports, no
  unused items. The `#[serde(default)]` is pre-existing. ✓
- **Public functions have doc comments:** `AskUserTool::new()` (line 62) and
  `parse_ask_user_args` (lines 151-155) both have doc comments. Trait-impl
  methods (`name`/`category`/`schema`/`safety`/`execute`) are trait method
  implementations — fine. ✓
- **Build warning-free:** Per the task, `cargo test` is green with 0 warnings
  (8 ask_user tests pass). Consistent with the diff — nothing here would
  introduce a warning. ✓

### Security

- `ask_user` remains `SafetyLevel::AutoRun` (line 131) — asking a question is not
  a mutation and is never approval-gated. Unchanged by this diff. ✓
- The new validation introduces no panic surface (`len()` on a `Vec` is
  infallible; `format!` is infallible) and no injection vector (the count is
  interpolated into an error string sent back to the agent, not executed). ✓
- No new file/shell/git surface introduced. ✓

### Bookkeeping changes (non-source) — all benign

- **Deleted `.claude/plans/*.md` (7 files):** Old planning docs under a
  `.claude/` tree (a different/older planning namespace than the project's
  `.coding/plans/`). Pure documentation cleanup — no source impact. ✓
- **`.coding/backlog.json`:** Item 30 (the request this plan implements) marked
  `done`; item 29 removed. Valid JSON, consistent with the plan's completion.
  The git "LF will be replaced by CRLF" warning is the backlog store's
  write-through persistence behavior (pre-existing), not something this plan's
  code introduced. ✓
- **`.coding/plans/stack.json`:** Stack pointer updated from the old plan id to
  `58a076dc-…` (this plan's id). Correct plan-stack bookkeeping. ✓
- **New `.coding/plans/58a076dc-…md`:** The plan document itself — well-formed,
  `kind: implementation`, 3 steps all marked done. ✓

## Conclusion

No findings. The `ask_user` hardening is correct, the schema and validation are
consistent, the tests are meaningful, and the build is warning-free. The
bookkeeping changes are benign. Ready to commit.
