## Verdict: PASS

Round-2's single finding (F2: the cap-arm pin in `bad_json_aborts_at_higher_cap` was non-discriminating) is fixed exactly as prescribed in commit 5c798cc, and nothing else changed. The strengthened assertion counts ToolResults per arm by distinct payload and is now discriminating in both directions — deleting either bad-JSON arm's send loop fails the test.

## Scope & method

Commit 5c798cc on `wt/agenticcoder` (HEAD; worktree clean — `git diff HEAD` and `git status --short` both empty), reviewed via full diff plus the live sources: the assertion block (`src/agent/tests.rs:1402-1434`), both emit sites (`src/agent/turn.rs:1349-1458`), `ToolResult::error` (`src/tool/mod.rs:76-82`), and `MAX_BAD_JSON_RETRIES` (`src/agent/mod.rs:60`). Reported test evidence (cargo 1696/0/1 warning-free) accepted as-is; not re-run, per the round-3 brief.

## (a) Payload discriminators match the emit sites ✓

`ToolResult::error` stores the message verbatim in `output` (tool/mod.rs:76-82), so `result.output.contains(...)` matches the literal strings:

- **Retry arm** (turn.rs:1451-1453): `"arguments malformed or truncated — not run; retrying"` → matches the retry filter `contains("not run; retrying")` (tests.rs:1414). Cannot match the cap filter's negative guard.
- **Cap arm** (turn.rs:1361-1363): `"arguments malformed or truncated — not run"` → matches the cap filter `contains("not run") && !contains("; retrying")` (tests.rs:1423-1424). Cannot match the retry filter's full substring.

The two filters are **mutually exclusive**: the retry payload contains "; retrying" so it is excluded by the cap filter's negative guard; the cap payload lacks the substring the retry filter requires. Each arm's emission is counted by exactly one filter, exactly once per firing.

## (b) The assertion is genuinely discriminating ✓

`MAX_BAD_JSON_RETRIES = 8` (mod.rs:60). The test drives 8 bad-JSON iterations: iterations 1-7 leave `bad_json_count < 8` (retry arm, one ToolResult per iteration), iteration 8 trips the cap (one ToolResult). The bad-JSON path `continue`s or returns before any tool executes, so the two synthetic send loops are the only ToolResult events for `call_1` in the collected stream. Simulated deletions:

- **Delete the cap arm's send loop (turn.rs:1355-1367):** `cap_arm_results = 0 ≠ 1` → `assert_eq!` fails. Note `saw_final_error` still passes in this scenario (the `Error { retrying: false }` send at 1368-1376 is outside the deleted loop), so this count assertion is the sole pin for exactly the regression round 1's F2 named — as intended.
- **Delete the retry arm's send loop (turn.rs:1445-1457):** `retry_arm_results = 0 ≠ 7` → `assert_eq!` fails. (The cap count and `saw_final_error` are unaffected, so the retry count is the sole pin here.)

Exact-count semantics also catch over-emission regressions (e.g. a duplicated send loop or an off-by-one in the cap condition would shift 7/1).

## (c) No other assertion weakened or removed ✓

The diff replaces only the round-1 `any()`-by-tool_call_id assertion. `saw_final_error` (tests.rs:1395-1401), the 7-tool-messages count (tests.rs:1436-1442), and the trailing `let _ = outcome;` are untouched context lines. The 7-tool-messages count remains consistent with the 7-retry/1-cap arithmetic (only the retry arm pushes tool messages; the cap arm returns before pushing).

## (d) Commit scope is exactly as stated ✓

`git show --stat 5c798cc`: 2 files, +84/−8 — `src/agent/tests.rs` (+31/−8, the assertion rewrite) and the new round-2 report `.coding/reviews/2026-09-18-tool-cards-stuck-running-63cbc20f-review-round2.md` (53 lines). No production code touched — correct for a test-strength-only fix; no frontend, no bookkeeping, no OS-specific surface, no docs impact.

## Summary

The round-2 F2 fix is complete and correct: the pin now counts both bad-JSON arms' synthetic ToolResults by mutually exclusive payload discriminators with exact counts (7 retry / 1 cap), and deleting either arm's send loop fails the test. No collateral changes in the commit, worktree clean, HEAD == 5c798cc. The plan's verification chain (F1, F2, F3 across three rounds) is closed.