# Review: git_diff output is never truncated (review-critical exception)

- **Branch:** `fix/git-diff-uncapped` — commit under review: `6742665` ("git_diff output is never truncated (review-critical exception)"), parent `main`
- **Plan:** `0330f650` (3 steps, all complete) — make `git_diff` exempt from `cap_tool_output` (100 KiB tool-output cap), with a regression test proving a >100 KiB diff arrives complete
- **Reviewer constraints:** read-only; `cargo test` could NOT be executed (the reviewer toolset has no shell). Static analysis only — see Caveat at the end.

## Scope reviewed

- `src/tool/agent/git_diff.rs` (the only source file in the commit)
- Uncommitted leftovers (`git diff HEAD`): only `.coding/plans/0330f650-7424-4ebf-aa54-62997935e072.md` (step 3 checkbox) and `.coding/plans/stack.json` (`reviewed:false`) — plan-machinery state, expected pre-review bookkeeping, no findings.
- Downstream paths: tool dispatch / agent loop (`src/agent/turn.rs`), memory recording (`src/memory/mod.rs`), context compaction (`src/agent/context.rs`), tool registry (`src/agent/factory.rs`), sibling capping sites.

## Verdict

**No High or Medium findings.** One Low (informational) nit and one caveat (test execution). The change is correct, minimal, and genuinely protected by a meaningful regression test.

## Check results

### 1. Correctness — cap removed, no downstream truncation (✅)

- `src/tool/agent/git_diff.rs:21-28` — the `use crate::tool::agent::cap_tool_output;` import is gone (removed import confirmed; no unused-import warning risk). `git_diff.rs:132` assigns `combined` straight to `ToolResult.output`; no capping anywhere in the file (remaining `cap_tool_output` mentions at lines 16, 128, 238, 276 are doc/comment/test-string only).
- The truncation marker string `"truncated: output exceeded size limit"` originates ONLY from `cap_tool_output` (`src/tool/agent/mod.rs:61`) — no other producer exists (searched repo-wide), so the test's marker assertion checks the real cap.
- All other `cap_tool_output` call sites are in sibling tools that must stay capped: `src/tool/agent/git.rs:131`, `src/tool/agent/shell.rs:192`, `src/tool/agent/web_fetch.rs:210`. No generic dispatch-level cap exists: `GitDiffTool` is registered directly at `src/agent/factory.rs:599` (`registry.register(Box::new(...))`, no wrapper).
- Tool-result consumption is verbatim: `src/agent/turn.rs:1287-1295` clones `result.output` into the Tool message with no truncation.
- Two adjacent mechanisms were examined and do NOT defeat the fix:
  - `src/memory/mod.rs:1266` caps the output stored in *working memory* (`MAX_WORKING_CONTENT_CHARS`) — that is the FTS/recall snippet, explicitly documented as such (lines 1262-1265); the full output still goes to the LLM tool message. Irrelevant to the reviewer seeing the diff.
  - `src/agent/context.rs` summarization compacts *old* conversation turns generically when the whole conversation exceeds a token threshold (and keeps recent messages verbatim). That is not a per-tool-result truncation; the git_diff result is delivered in full at the moment it is produced, like every other message. Out of scope for this plan.

### 2. Safety — process spawning unchanged (✅)

`src/tool/agent/git_diff.rs:46-79`: discrete argv (no shell) for all three invocations (`["diff","HEAD","--stat"]`, `["diff","HEAD"]`, `["status","--short"]`), `kill_on_drop(true)` still present (line 52), `CREATE_NO_WINDOW` under `#[cfg(windows)]` (lines 56-60), `current_dir(project_root)`. The commit only removed the capping line; no regression to the spawn path.

### 3. Regression test quality (✅)

`src/tool/agent/git_diff.rs:241-300` `large_diff_is_not_truncated`:
- **Genuinely exceeds the cap:** committed file = 30,000 × `"line {i}\n"` (~270 KB); working tree = 30,000 × `"line {i}x\n"`. `git diff HEAD` therefore emits ~30,000 `-` lines + ~30,000 `+` lines (one merged hunk) ≈ 700 KB+ ≫ the 100 KiB cap — independent of the small `--stat` section. The `output.len() > 100 * 1024` assertion (line 295-299) verifies this empirically, not just by construction.
- **Fails if capping is re-added:** under `cap_tool_output` all three of the marker assertion (line 284-287), the tail assertions (`+line 29999x` / `-line 29999`, line 290-294), and the length assertion would fail. Each assertion independently guards the regression.
- **Robust/fast:** deterministic (no timing, no sleeps, no network); sub-second git ops on a small repo; `tempfile::tempdir()` cleans up on drop; `git config user.email/name` set before commit so it runs without a global identity; git identity/`core.autocrlf` variants don't affect the assertions (file is written directly, no checkout conversion involved; the trailing newline is present in both versions, so no "\ No newline" marker noise). Uses `std::process::Command` + `tempfile` like its siblings.
- The tail assertions are unambiguous: `+line 29999x` and `-line 29999` appear only as the diff's last +/- lines, not in headers or the stat bars.

### 4. Multi-platform neutrality (✅)

Test uses only `std::process::Command` + `tempfile` (identical to sibling tests). The only `#[cfg(windows)]` code is the pre-existing `CREATE_NO_WINDOW` block, which mirrors the sanctioned `GitTool::run_git` pattern (`src/tool/agent/git.rs`) and compiles to nothing on macOS/Linux. No Windows-only APIs or paths added.

### 5. Constitution / code hygiene (✅)

- Module doc (lines 15-19), schema description (lines 100-101), inline NOTE (lines 126-128), and test doc (lines 237-240) all accurately state the exception, the rationale, and the "do not re-add capping" warning. The intra-doc link `[`cap_tool_output`](super::cap_tool_output)` resolves (`pub(crate)` in the parent module).
- No `#[allow(...)]` added; no dead code (the removed import was the only candidate; `truncate_to_boundary` remains used by `cap_tool_output` and `read_files`).
- `name_and_category` test and registration (`factory.rs:599`, expected-name list `factory.rs:1216`) unchanged — no drift between registry and tests.

## Findings

### Low (informational, optional)

- **L1 — `git_diff.rs:243-259`:** the new test duplicates the git-init/config boilerplate instead of calling the existing `repo_with_change()` helper, even though the plan text said "build on repo_with_change() pattern". The duplication is deliberate-looking and consistent with the file's existing style (`empty_repo_shows_no_changes`, lines 199-235, does the same), and `repo_with_change()` is hard-wired to a 1-line fixture so reuse would require parameterizing it. Not worth a fix; noted for awareness only.

### Caveat — test execution

- The reviewer is read-only (no shell), so `cargo test` was not run. Static analysis strongly indicates `tool::agent::git_diff::tests::large_diff_is_not_truncated` will pass (the tool returns the concatenation of three `git` invocations with no cap applied; the assertions match that output shape), and no warning sources were found (`#![deny(warnings)]`). **The main agent must run `cargo test` at the repo root (and confirm green, including the new test) before committing** — per the closing sequence this happens anyway after findings are addressed.
