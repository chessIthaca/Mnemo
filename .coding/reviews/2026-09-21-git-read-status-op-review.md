## Verdict: FINDINGS (0 high, 2 low)

The op="status" implementation is correct, genuinely read-only, well-tested, and the budget raise follows the documented pattern. Two LOW doc-sync findings: the op-set comment sweep missed `src/agent/factory.rs:939` and `src-tauri/src/ipc/spawn.rs:453`, which still say "op = diff | log | show". Both are one-line comment fixes.

**Scope & method:** the full uncommitted diff via `git_read op="diff"` (7 tracked files, +167/−20, plus the untracked `.coding/plans/65c6b70f.md` bookkeeping), `git log` for branch context, full reads of `src/tool/agent/git_read.rs` + `git_read_tool.rs`, the factory budget-test block (lines 1700–2075), the frontend `argLabel` branch (`toolCardPaths.ts:917–941`) + its test file, the PLAN.md section, and `src/agent/approval.rs` (`is_git_read_only`); repo-wide literal searches for `git_status`, `diff, log, show`, and `git_read` over `**/*.rs` and `**/*.md`, plus README.md and `docs/*.md` sweeps. Read-only throughout — the parent's green suites were cross-checked by inspection, not re-run.

## (a) Read-only guarantee — PASS

- `GitStatusTool::execute` runs the **fixed argv** `["status", "--short"]` through the shared `run_git_read` (git_read.rs:312) — the exact same helper as log/show: direct argv (no shell), `kill_on_drop(true)`, `stdin(Stdio::null())`, and the pre-existing `cfg(windows)` CREATE_NO_WINDOW block (untouched, the sanctioned pattern).
- `execute` ignores `_args` entirely — **no user-controlled argv ever reaches git**, so there is no option-injection surface (extra fields like `{"op":"status","path":"x"}` are silently ignored, consistent with the forgiving style of the siblings).
- Verified the task's claim: `is_git_read_only` (src/agent/approval.rs:160–182) already classifies `"status"` as read-only for the `git` tool (line 173: `"status" | "diff" | "log" => true`) — the new op is consistent with the repo's established classification.
- `git status` does refresh the index stat-cache (touches `.git/index` mtime) — the same internal git behavior as `git diff`, which already rides this tool; no staging, fetch, or ref mutation is possible. `SafetyLevel::AutoRun` is justified; `GitReadTool::safety()` stays AutoRun.

## (b) Summary computation — PASS

- Empty stdout → `lines` empty → `"(working tree clean — no changes, nothing untracked)"`. Whitespace-only lines are filtered (`!l.trim().is_empty()`).
- Untracked counting: `starts_with("??")` — in porcelain short format the first two chars are XY status codes, so only untracked entries match; `lines.len() - untracked` cannot underflow (untracked ⊆ lines). Renames/conflict entries correctly count as changed.
- **Cap interaction (the subtle case):** the summary is computed from `result.data["stdout"]` — the FULL, uncapped stdout — while `result.output` is capped at 100 KiB by `cap_tool_output` (src/tool/agent/mod.rs:61–68). So the count stays accurate even when a huge tree truncates the listing, and the appended line lands after the `"... (truncated ...)"` note. Correct and informative.
- Non-repo directory → git exits non-zero → `result.success == false` → the summary block is skipped and git's own `fatal: not a git repository` passes through. (No dedicated test, but the siblings have no non-repo test either — consistent.)
- `result.output.trim_end()` is safe: `run_git_read` always prefixes the `$ git status --short` echo line, so no leading-newline artifact; `str::lines()` strips CRLF, so Windows output parses identically.

## (c) Unknown-op error — PASS

- `"unknown op '{other}' — valid: diff, log, show, status; for write operations (commit/merge/push/…) use the `git` tool"` — the `git` tool is indeed the write-capable `NeedsApproval` sibling (registered with core-operations gating at factory.rs:935–938; module docs confirm commit/merge/push). The pointer is conditional ("for write operations"), so it doesn't mislead for an unknown *read* op like `blame`. This satisfies the backlog's ask (name the substitute that ends the read-loop).
- Dispatch normalizes `op.trim().to_ascii_lowercase()` (pre-existing), so `"Status"` / `"STATUS "` route correctly.

## (d) Tests genuinely exercise the new path — PASS

- `status_op_lists_dirty_tree_and_summarizes`: real `repo()` fixture — modified `a.txt` + untracked `b.txt` → asserts `"M a.txt"`, `"?? b.txt"`, `"(1 changed, 1 untracked)"`. Exercises the dispatch arm + delegate + summary end-to-end; **fails without the fix** (op:"status" → unknown-op error, `success=false`).
- `status_op_reports_clean_tree`: commits everything → asserts `"(working tree clean"` and no `"?? "`.
- `unknown_or_missing_op_errors_with_the_valid_set` tightened to assert the full four-op set **and** the `` `git` tool `` pointer.
- Frontend pin: `argLabel('{"op":"status"}', "git_read")` → `"status"` — verified against the branch at toolCardPaths.ts:923–941: op="status" skips the show/log cases, `path` empty → bare-op fall-through `return op`. No code change was needed there; the pin locks the behavior.
- Placement is right: the new tests live in `git_read_tool.rs` (whose established style spawns git via `repo()`), while `git_read.rs`'s own test module deliberately spawns no git — that boundary is respected.

## (e) Budget ceiling raise — PASS

- Planning and Complete: 18_700 → 18_900 with dated cause comments (2027-02-05, backlog 1aa7e456, ~+122, measured 18_727) — exactly the documented `X → Y (date): cause; measures Z; Ceiling = measured + headroom, deliberate raise` pattern followed by every prior raise in that block. Headroom is 173 (the prior round ran at 95).
- Not papering over a regression: the ~+122 growth is the feature's own schema text (the op="status" description sentence + the enum entry) — the point of the change. git_read rides every filter, and the other four filters absorbed the same growth within existing headroom (Executing 33,507/33,900; PlanFrozen 34,793/35,200; ExecutingResearch 27,632/28,000; Reviewing 28,771/29,200) — internally consistent with the pre-change baselines recorded in the comment block (18,605 + 122 = 18,727; 33,385 + 122 = 33,507; etc.).

## (f) Constitution — FINDINGS (see L1/L2)

- **Doc sync:** PLAN.md git-bridge line updated; module docs in both Rust files updated; both frontend comments updated; README.md and docs/ contain no git_read mentions (verified — nothing to update there). But two op-set comments were missed → L1, L2 below.
- **Multi-platform:** no new platform-specific code; the `cfg(windows)` block is pre-existing and untouched; tests use `Command::new("git")` (cross-platform).
- **File-tools-first:** no shell-based file mutation anywhere in the diff (`std::fs::write` in tests is sanctioned test code).
- **Warning-free:** no unused imports or dead code introduced (`GitStatusTool` is consumed by git_read_tool.rs; the test module's `use std::process::Command` pre-existed at line 126).

## (g) Bugs / security / edge cases — PASS

No option injection (no user argv), no arithmetic underflow, no panic paths (the `data` access is Option-chained and defensive), serde ignores unknown fields consistently with the siblings. The schema description "clean tree → an empty listing" is a hair imprecise (the summary line is always appended, so the output is never fully empty) — cosmetic wording only, not counted.

## Findings

### L1 (low) — stale op-set comment at src/agent/factory.rs:939

The registration comment still reads `// git_read is the read-only view into git (op = diff | log | show):` — now inaccurate with status as the fourth op. This is the exact doc-sync class the change applied everywhere else (PLAN.md, two module docs, two frontend comments); the sweep missed this one. One-line fix: `op = diff | log | show | status`.

### L2 (low) — stale op-set comment at src-tauri/src/ipc/spawn.rs:453

The `REVIEWER_BASE_TOOLS` entry comment still reads `// One read-only view into git (op = diff | log | show) — the reviewer's` — same staleness, same one-line fix. (The adjacent sentence "the reviewer's only way to see uncommitted changes" remains true and now covers status too — reviewers gain the op for free once the binary is rebuilt, since `git_read` is in the reviewer allow-list.)

## Informational (not counted — no action required for this plan)

1. **Live smoke test:** this reviewer's own `git_read op="status"` call returned the OLD error (`unknown op 'status' — valid: diff, log, show`) — the running app binary predates the uncommitted change. Expected (the change is not yet built); the new dispatch arm is covered by the unit tests. The op goes live on rebuild.
2. **Pre-existing staleness, out of this diff's scope:** agent.md:124 (reviewer-surface description) and the SPEC memory record `2026-08-23-strict-reviewer-surface-failed-reviewer-protocol.md` still name the pre-unification tools `git_diff`/`git_log`/`git_show` — stale since the earlier git_read unification, not since this change. Worth a future doc/memory pass.

## Verification status

Parent-reported and cross-checked for internal consistency by inspection (read-only reviewer; suites not re-run): `cargo test --workspace` green (2,522 passed / 0 failed / 21 ignored, exit=0, warning-free under `#![deny(warnings)]`); frontend `npm test` green (90 files, 1,246 tests); budget test green with all six filters under ceiling. The measured figures in the dated comments match the arithmetic of the recorded baselines.
