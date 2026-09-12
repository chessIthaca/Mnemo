# Review: Ignore .claude + workflow prompt + report-path passback + visual-line ArrowUp

**Date:** 2026-04-04
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD` + untracked new files).
**Plan:** Four independent improvements — (1) ignore `.claude/` in search + gitignore; (2) enhance the workflow-lifecycle system prompt; (3) pass the reviewer's report path back to the parent; (4) multiline-safe ArrowUp/Down based on visual lines.

## Summary

Three of the four changes are **correct and clean**: the `.claude` ignore (search + gitignore consistent, test genuinely exercises the skip), the workflow-prompt rewrite (byte-stable const, assertions hold), and the report-path passback (field name matches `write_review_report`'s `.with_data`, no deadlock, no injection). The report-path wiring was verified end-to-end: `write_review_report.rs:188` emits `json!({ "path": target.display().to_string() })`, `turn.rs:805` reads `d.get("path")`, and `events.rs:448` reads it off the child's `AgentLoop`.

**One high-severity bug** found in the visual-line ArrowUp logic: `isOnFirstVisualLine` always returns `false` in a real browser, breaking history navigation. It is masked by the test suite (vitest runs in `environment: "node"`, so only the `\n` fallback path is exercised — the mirror-div measurement has zero test coverage).

---

## Correctness

### C1 — `.claude` ignore is consistent and correctly tested ✓

- `src/tool/agent/search.rs:32` adds `".claude"` to `IGNORED_DIRS`; `should_search` (which both `search` and `search_read` reuse) skips it.
- `.gitignore:48` changes `.claude/settings.local.json` → `.claude/` (whole dir). Consistent with the search skip.
- The `skips_ignored_dirs` test (`search.rs:322-323`) creates `.claude/plans/old.md` containing the search term and asserts `.claude` does NOT appear in results — genuinely exercises the new skip. ✓

### C2 — Workflow-lifecycle prompt rewrite is byte-stable + assertions hold ✓

- `src/agent/prompt.rs` `WORKFLOW_LIFECYCLE` is a `const` in the stable head (built from the constitution, not workflow state), so it is byte-stable across workflow states — the existing `stable_head_byte_stable_across_workflow_changes` test still holds.
- The new test `lifecycle_encourages_research_planning_and_subplans` asserts "research", "sub-plan", "AT ANY POINT" — all present in the rewritten text (verified in the diff). ✓
- No contradiction with the schema/struct docs (research plans skip review; sub-plans auto-resume the parent).

### C3 — Report-path passback wiring is correct end-to-end ✓

- **Field name match:** `write_review_report.rs:188` → `.with_data(json!({ "path": target.display().to_string() }))`; `turn.rs:805` → `result.data.as_ref().and_then(|d| d.get("path")).and_then(|v| v.as_str())`. Match. ✓
- **Capture site:** `turn.rs:801-810` gates on `tc.name == "write_review_report" && result.success` — correct (only capture on a successful write). ✓
- **Storage:** `AgentLoop::last_review_report` (`loop_impl.rs:134`) is a `std::sync::Mutex<Option<String>>`, init `None` in **both** constructors (`:214`, `:254`). Accessors `set_last_review_report`/`last_review_report` (`:358`, `:368`) mirror the existing `forced_model`/`session_id` pattern. ✓
- **Read site:** `events.rs:446-450` looks up the child's loop by `agent_id` and reads `last_review_report()`. The `agent_id` passed to `notify_parent_on_completion` is the child's (the one that finished), and `set_last_review_report` is called on the child's `self` during its turn — so the path is set on the child's loop and read from the child's loop. Correct wiring. ✓
- **Happens-before:** `set_last_review_report` runs synchronously during the child's turn (after `execute_tool_call`); the finish event (which triggers `notify_parent_on_completion`) is emitted later, after the turn ends. The set happens-before the read. No race. ✓
- **Both call sites updated:** `events.rs:210` and `:219` pass `&agent_loops`. No other callers of `notify_parent_on_completion` (search confirmed only these two). ✓

---

## Bugs

### B1 (HIGH) — `isOnFirstVisualLine` always returns `false` in a real browser; ArrowUp history navigation is broken

**File:** `frontend/src/lib/promptHistory.ts:124-141, 163-166`

`cursorVisualLine` is documented to return "the visual line the cursor is on (0-indexed)", but its implementation returns a **1-indexed line count** that is always `>= 1`:

```ts
function cursorVisualLine(ta, selStart) {
  return visualLineCount(ta, ta.value.slice(0, selStart));  // measures prefix + "\n"
}
// visualLineCount appends a "\n" sentinel: mirror.textContent = text + "\n";
// then returns Math.round(height / lineHeight)  — a COUNT, >= 1 for any rendered content.
```

`isOnFirstVisualLine` then calls `isFirstVisualLineFromCounts(cursorLine, total)`, which checks `cursorLine <= 0`. Because the sentinel inflates the count to `>= 1` for any non-empty prefix (and `>= 1` even for an empty prefix, since a lone `"\n"` renders a line box of height `>= lineHeight`), **`cursorLine <= 0` is never true when layout is available**. So `isOnFirstVisualLine` returns `false` for every cursor position in a real browser.

**Concrete regression (the most common case):** user types `"hello world"` (single line, no `\n`), cursor at the end (position 11), presses ArrowUp to recall history.
- Old behavior: `isOnFirstLine("hello world", 11)` → `!"hello world".includes("\n")` → `true` → history loads. ✓
- New behavior (browser): prefix `"hello world"` → `"hello world\n"` → 2 lines → `cursorLine = 2` → `isFirstVisualLineFromCounts(2, 2)` → `2 <= 0` → `false` → ArrowUp does NOT load history (default cursor-up, a no-op for a single line). ✗

This regresses the primary feature. `isOnLastVisualLine` is **not** affected — the sentinel inflates both `cursorLine` and `total` equally, and the `cursorLine >= total - 1` comparison is invariant under that inflation (verified by tracing wrapped 3-line and single-line cases).

**Why the tests pass:** `frontend/vitest.config.ts:5` sets `environment: "node"` (no DOM). `getMirror` returns `null` when `typeof document === "undefined"`, so `visualLineCount` returns `null`, and both visual-line helpers fall back to the `\n`-based `isOnFirstLine`/`isOnLastLine` — which work. The test at `promptHistory.test.ts:95-106` explicitly passes a duck-typed `{ value: ... }` object and only exercises the fallback. **The actual mirror-div measurement (`getMirror`/`visualLineCount`/`cursorVisualLine`) has zero test coverage.**

**Root cause:** `cursorVisualLine` conflates a 1-indexed line *count* (returned by `visualLineCount`, which appends the `"\n"` sentinel) with a 0-indexed line *number* (expected by `isFirstVisualLineFromCounts`'s `<= 0` check). The sentinel is appropriate for the *total* measurement (to count a trailing empty line) but not for the *cursor* measurement, where it inflates the result by one.

**Suggested fix:** make `cursorVisualLine` return a 0-indexed line number. Either:
- Measure the raw prefix without the sentinel and subtract 1: `cursorLine = visualLineCountRaw(ta, prefix) - 1` (clamped to `>= 0`), where `visualLineCountRaw` does not append `"\n"`; or
- Keep the sentinel but subtract it: `cursorLine = visualLineCount(ta, prefix) - 1` (the sentinel adds exactly one line box under standard browser rendering, so this yields the 0-indexed line). Then `cursorLine <= 0` correctly identifies the first visual line.

Either way, add a test that exercises the mirror path (e.g., switch this test file to `environment: "jsdom"` or add a jsdom-scoped suite) so the measurement logic is actually covered — otherwise this regression will recur silently.

---

## Code quality / low-severity

### Q1 (low) — Dead `total` variable in `isFirstVisualLineFromCounts`

**File:** `frontend/src/lib/promptHistory.ts:163-166`

```ts
export function isFirstVisualLineFromCounts(cursorLine: number, totalLines: number): boolean {
  const total = Math.max(1, totalLines);   // computed, never used
  return cursorLine <= 0;
}
```

`total` is computed but never read; `totalLines` is effectively unused (the first-line check legitimately doesn't depend on the total). This is misleading (suggests `totalLines` affects the result) and is a symptom of the confused logic in B1. Drop the dead line, or — if keeping the symmetric signature — leave a comment that `totalLines` is intentionally unused for the first-line check. (Not a build failure: the project's tsc config does not flag unused locals, since `npm run build` is clean.)

### Q2 (low) — Doc comment in `notify_parent_on_completion` doesn't match the code

**File:** `src-tauri/src/ipc/events.rs:416-418, 446-450`

The doc comment says "the lock is held only briefly (clone the `Arc`, drop the lock, then read)", but the code reads `last_review_report()` **while still holding** the `agent_loops` tokio lock (the `.map(|l| l.last_review_report())` closure runs before the block ends and the guard drops):

```rust
let report_path: Option<String> = {
    let loops = agent_loops.lock().await;
    loops.get(&agent_id).map(|l| l.last_review_report())  // read WHILE holding the lock
}.flatten();
```

This is **not a bug**: the `last_review_report` std mutex is held for nanoseconds (clone a `String`), is never held across an `.await`, and `set_last_review_report` (the only other acquirer of that std mutex, from `turn.rs`) does **not** take the `agent_loops` tokio lock — so there is no reverse-order acquisition and no deadlock. But the comment should describe what the code actually does (read under the lock) rather than the clone-then-drop pattern it claims.

---

## Security — no findings

### S1 — Report path is a validated filesystem path, not executed ✓

The path interpolated into the Suggestion text (`events.rs:483`) originates from `write_review_report`'s `.with_data`, where `target` is canonicalized and validated to `starts_with` the canonicalized `.coding/reviews/` dir (`write_review_report.rs:84-110`), with absolute paths, `..`, and separators rejected (`:56-76`). It is interpolated into a display string shown to the parent LLM — never executed, parsed as a command, or used as a path argument by the parent without its own validation. No injection vector. ✓

### S2 — No deadlock from the `agent_loops` lock ✓

As noted in Q2, the `agent_loops` tokio mutex and the `last_review_report` std mutex are never acquired in conflicting orders across code paths (`set_last_review_report` takes only the std mutex). The tokio lock is held briefly and never across an `.await`. ✓

---

## Constitution compliance — no findings

- **`#![deny(warnings)]`:** No new `#[allow(...)]` added to silence warnings. The `#[allow(clippy::too_many_arguments)]` on the two `AgentLoop` constructors (`loop_impl.rs:183, 221`) is pre-existing, not introduced by this diff. No dead code or unused imports in the Rust changes (`last_review_report` field + accessors are all used; `completion_suggestion_text` is used + tested). ✓
- **Doc comments on public functions:** `set_last_review_report`, `last_review_report` (`loop_impl.rs:352, 365`), and `completion_suggestion_text` (`events.rs:469`) all have doc comments. ✓
- **Windows paths / PowerShell:** No shell commands or path handling introduced in the Rust changes; the report path flows as a `String`. ✓
- **Line-ending preservation:** Edits use targeted `file_edit`-style changes; no mixed endings introduced. ✓
- **No commit to main:** Changes are uncommitted on the working tree. ✓
- **`cargo test` before step complete:** Not verifiable by this read-only reviewer, but the new code includes unit tests for the prompt (`prompt.rs:338`), the completion-suggestion helper (`events.rs:677, 691`), and the search skip (`search.rs:322`). The vitest suite was extended with pure-helper + fallback tests. (Note: per B1, the vitest tests do not cover the mirror-div path.)

---

## Verdict

**Approve with one high-severity bug to fix (B1).** The `.claude` ignore, workflow prompt, and report-path passback are correct and well-wired. The visual-line ArrowUp change has a real logic error — `isOnFirstVisualLine` always returns `false` in a real browser because `cursorVisualLine` returns a 1-indexed count while the first-line check expects a 0-indexed line — which breaks ArrowUp history navigation (the primary feature) in production. It is masked by the node-environment test suite, which only exercises the `\n` fallback. Fix `cursorVisualLine` to return a 0-indexed line number and add a jsdom-scoped test that actually exercises the mirror measurement.
