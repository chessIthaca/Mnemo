# `search_read` Combo Tool — Review

**Date:** 2026-04
**Reviewer:** read-only subagent (spawned review)
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD` + untracked files), focused on the `search_read` plan.
**Branch:** feature branch (reviewing pre-commit)

## Files reviewed

- `src/tool/agent/search_read.rs` (NEW) — the `SearchReadTool`.
- `src/tool/agent/read_files.rs` — visibility promotions (`pub(crate)`) of `read_one`, `truncate_to_boundary`, `ReadSpec`, `DEFAULT_MAX_LINES`, `DEFAULT_MAX_BYTES`, `TOTAL_BYTE_CAP`.
- `src/tool/agent/search.rs` — visibility promotions of `is_ignored_component`, `should_search`.
- `src/agent/factory.rs` — registration + expected-tool-names test.
- `src/tool/agent/mod.rs` — module declaration + doc entry.
- Unrelated bookkeeping in `.coding/backlog.json`, `.coding/plans/*.md`, `.coding/plans/stack.json` (status flips / plan-stack swap — not code, out of scope for correctness).

## Verification performed

- `cargo test --lib search_read` → 7/7 pass.
- `cargo test --lib expected_tool_names_registered_when_fully_wired` → pass.
- Read `sandbox.rs` (`validate`/`root`/`check_inside`), `approval.rs` (`needs_approval`/`is_project_scoped`), `safety_rules.rs` (`is_safe`/`key_argument`/`signature`), `dispatch.rs` (approval flow), `tool/mod.rs` (`ToolFilter`/`never_auto_for`), and the mirrored `search.rs` to confirm reuse + integration semantics.

## Findings

### Correctness — no findings

The matched-file collection loop (`search_read.rs:124-159`) is correct on all the points called out:

1. **Dedup + stop at max_files + keep counting:** `seen` (HashSet) + the `matched_files.len() < max_files` guard ensure only the first `max_files` distinct matched files are collected for reading. `files_matched` (line 153) and `total_matches` (line 148) increment for *every* matched file/line regardless of whether the read-list is full, so the summary line accurately reports the full match counts even when only 5 are read. Verified by `caps_files_at_max` (asserts "7 files" + "reading top 5").
2. **`max_files` clamping** (`search_read.rs:97-101`): `.unwrap_or(MAX_FILES_TO_READ).min(MAX_FILES_TO_READ).max(1)`. For `None`→5, `0`→`max(1)`=1, large→`min(5)`=5. `usize` cannot be negative. Correct.
3. **Reuse correctness:** `ReadSpec { start_line: None, max_lines: None }` (`search_read.rs:178-182`) → `read_one` uses `start=1`, `max=DEFAULT_MAX_LINES` (2000) with per-file byte cap + truncation notes. Full-file read with the standard caps. Correct.
4. **Total byte cap** (`search_read.rs:193-196`): applied to the joined output via `truncate_to_boundary`, mirroring `read_files.rs:151-154`. Correct, char-boundary-safe.

### Bugs — no findings

- No panics: `strip_prefix` uses `unwrap_or_else` (line 174-177); `read_one` never propagates errors (inline error sections); `spawn_blocking` join failure maps to `ToolResult::error`. No `unwrap` on fallible I/O.
- No off-by-one: line numbering is delegated to `read_one` (1-indexed, `start + i + 1`), unchanged.
- No race: single-threaded `spawn_blocking` closure; `Sandbox` is `Clone` (cheap `PathBuf`).

### Security — no findings

Path handling is sound and defense-in-depth:

- The glob walk (`search_read.rs:118-119`) mirrors `search.rs:149-150` exactly (`format!("{}/{}", root, glob_pattern)`). A glob like `**/..` *could* enumerate paths outside the project root, but:
  - `should_search` (line 136) skips ignored dirs and >1 MB files (same as `search`).
  - **Crucially, every read path is routed through `read_one` → `sandbox.validate`** (`read_files.rs:166`), which canonicalizes and enforces `starts_with(root)` (`sandbox.rs:137-143`). A matched file outside the root would be rejected by `validate` and surface as an inline `=== <path> (error) === path validation failed: ...` section — never read. No path-traversal vector reaches file I/O.
- The absolute→relative conversion (`search_read.rs:173-177`) is only for the *header display* + the relative form `validate` expects; `validate` re-canonicalizes regardless, so a malformed relative string cannot bypass the check.
- `SafetyLevel::AutoRun` + `ToolCategory::Agent` is correct for a read-only combo tool; it is visible in Planning/Complete (read-only allow-list via `safety == AutoRun`, `tool/mod.rs:196,256`) and available in Executing/Reviewing.

### Constitution compliance — no findings

- **Doc comments:** all new public items are documented — `SearchReadTool` struct (line 42), `new` (line 49), `MAX_FILES_TO_READ` (line 20), `SearchReadArgs` + its fields (lines 26-39). The `Tool` trait impl methods inherit the trait's docs (no per-impl doc required by convention; matches `search.rs`/`read_files.rs`). Module-level `//!` doc present (lines 1-6).
- **Line endings:** the `git diff` CRLF warnings on `read_files.rs` are pre-existing (the file tools normalize to the file's detected style automatically); no new mixed endings introduced by this diff.
- **No commits to main:** this is a pre-commit review on a feature branch; no merge/push in the diff.

## Low-severity consistency notes (not blocking, no fix required)

These are **not bugs** — `search_read` is `AutoRun`, so neither code path is reached for it — but they are minor inconsistencies with the sibling `search` tool, noted for completeness:

1. `src/agent/approval.rs:125` — `is_project_scoped` has `"search" => true` but no `"search_read"` arm. Functionally moot: `needs_approval` returns `false` for `AutoRun` tools before reaching `is_project_scoped` (`approval.rs:74-76`), so `search_read` is never evaluated here. Adding `"search_read" => true` would keep the allow-list + doc-comment consistent with `search`.
2. `src/safety_rules.rs:420` — `key_argument` maps `"search" => "pattern"` but not `"search_read"`. Functionally moot: safety rules only short-circuit `NeedsApproval` tools that reached the approval branch (`dispatch.rs:159-164`); `AutoRun` tools never get there. A `search_read` rule would be inert even if added. Noting only for symmetry.

Neither affects correctness, security, or behavior. The diff is clean and safe to commit.

## Conclusion

**No blocking findings.** The `search_read` tool is correct, sandbox-safe, well-documented, and consistent with the existing `search`/`read_files` tools it reuses. The two consistency notes above are optional polish (matching `search` in the `is_project_scoped` allow-list and `key_argument` map) and do not affect any runtime behavior because `search_read` is `AutoRun`.
