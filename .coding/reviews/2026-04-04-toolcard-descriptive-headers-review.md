# Review: Descriptive ToolCard headers for read_files / search / search_read

**Reviewer:** read-only reviewer subagent (spawn_agent)
**Date:** 2026-04-04
**Scope:** ALL uncommitted changes in the working tree (`git status` / `git diff HEAD`).
**Plan:** Make the ToolCard header in the agent window more descriptive for `read_files` and `search`/`search_read` (previously bare "▶ read_files" / "▶ search" because `argLabel` only handled shell/spawn_agent/skill_start/git + a top-level `path`/`file`).

## Files reviewed

- `frontend/src/components/chat/Message.tsx` (substantive — two new `argLabel` branches, lines 279-309)
- `.coding/plans/f84531d3-…md` (bookkeeping — step 9 checkbox flipped `[ ]`→`[x]`)
- `.coding/plans/stack.json` (bookkeeping — stack pointer swapped to new plan id, `reviewed:false`)
- `.coding/plans/302153bc-…md` (new untracked plan file)

## Method

Read the full `Message.tsx` file and the `git diff`. Cross-checked the new
branches against the actual Rust tool argument schemas to confirm field names
and shapes:
- `read_files` → `ReadFilesArgs { files: Vec<ReadSpec> }`, `ReadSpec { path: String, start_line?, max_lines? }` (`src/tool/agent/read_files.rs:52-65`).
- `search` → `SearchArgs { pattern: String, glob: Option<String>, literal: bool }` (`src/tool/agent/search.rs:68-73`).
- `search_read` → `SearchReadArgs { pattern: String, glob: Option<String>, literal, max_files }` (`src/tool/agent/search_read.rs:28-40`).

Traced the cap-at-3 / "+N" logic for 0, 1, 3, 4, 5 files and the
pattern/glob suffix conditions by hand.

## Findings by severity

### Correctness
**No findings.**

- **`read_files` array shape:** Correctly reads `parsed.files` as an array of `{path, …}` specs and extracts `.path` from each — matches `ReadSpec` exactly. The `Array.isArray(files)` guard returns `null` for missing/non-array/`null` `files` (graceful). Per-element guard `f && typeof f === "object" && typeof f.path === "string"` filters out non-object entries and entries whose `path` is missing or non-string; the subsequent `.filter((p): p is string => !!p)` drops any empty-string paths. Empty result → `null`.
- **Basename extraction:** `p.replace(/\\/g, "/").split("/")` normalizes Windows backslashes to forward slashes before splitting, so both `src\foo\bar.rs` and `src/foo/bar.rs` yield `bar.rs`. The `|| p` fallback covers the trailing-slash edge case (`"foo/"` → last segment is `""` → falls back to `"foo/"`), which is harmless since such paths are already filtered to non-empty.
- **Cap-at-3 + "+N":** Verified for all requested counts:
  - 0 files → early `return null` (line 294).
  - 1 → `"n1"`.
  - 3 → `"n1, n2, n3"` (no suffix).
  - 4 → `"n1, n2, n3 +1"`.
  - 5 → `"n1, n2, n3 +2"`.
  All correct.
- **`search`/`search_read` branch:** `pattern` is read with a `typeof === "string"` guard and `.trim()`; an empty/whitespace-only/missing/non-string pattern returns `null`. The `glob` suffix (`"${pattern}" in ${glob}`) appears only when `glob` is a non-empty string after trim; otherwise the bare `"${pattern}"` form is returned. Both field names match the `search` and `search_read` schemas.
- **Label-format convention:** `argLabel` returns the bare label content (e.g. `"mod.rs, agent.rs"`); the caller wraps it in `(…)` via `labelStr` (`labels.join(", ")` → ` (label)`, line 350) and renders `displayName(name) + labelStr` (line 376). So the header reads `read_files (mod.rs, agent.rs)` / `search ("WorkflowState")` / `search ("WorkflowState" in **/*.rs)` — exactly the intended convention. The new branches `return` explicitly and never fall through to the common `path`/`file` fallback.

### Bugs
**No findings.**

- **TypeScript types:** `parsed` is `Record<string, unknown>`. `Array.isArray(files)` narrows `unknown` → `any[]`, so the per-element `f` is `any`; the `(f as Record<string, unknown>)` / `(f as Record<string, string>)` casts are defensive and harmless (no type error, no unsoundness — the `typeof` runtime check is what actually guards access). `parsed.pattern` / `parsed.glob` are `unknown` and narrowed by `typeof === "string"` before use. No `any`-escape or unsafe cast.
- **No mutation of inputs:** the branches only read from `parsed`; no side effects.

### Security
**No findings.**

- The label strings are rendered as React JSX text children (`{displayName(name)}{labelStr}`, line 376; also `CallDetail` line 426), not via `dangerouslySetInnerHTML`, so React auto-escapes them. A `pattern` or `path` containing `<`, `"`, or `{` cannot inject markup. No new network, filesystem, or credential handling.

### Constitution compliance
**No findings.**

- **Frontend-only change:** no Rust touched, so `cargo test` is unaffected (no Rust compile/test surface changed).
- **No commit to main:** nothing committed; reviewer is read-only.
- **Line-ending preservation:** the `Message.tsx` diff is clean (no CRLF noise). The lone `LF will be replaced by CRLF` git warning is on `.coding/plans/f84531d3-…md` (a bookkeeping file), not the source file — and the file tools normalize line endings automatically per the constitution.
- **Doc comments:** `argLabel` already has its `/** … */` doc (line 234); the new branches carry explanatory inline comments consistent with the surrounding branches.
- **Bookkeeping files:** `stack.json` is valid single-line JSON (no trailing newline — matches prior format); the plan `.md` checkbox flip and new untracked plan file reflect normal workflow progression, not corruption.

## Notes (INFO, not findings)

- **Comma ambiguity across multiple calls in one ToolCard:** `labelStr` joins multiple calls' labels with `", "` (line 350). A single `read_files` call's label already contains `", "` (e.g. `"a.rs, b.rs, c.rs"`), so two `read_files` calls would render `read_files (a.rs, b.rs, c.rs, d.rs, e.rs)` — the boundary between calls is not visually delimited. This is purely cosmetic (the expanded `CallDetail` view shows per-call labels at line 426) and mirrors how the existing path fallback could already concatenate multiple single-file labels. Not worth changing.
- **Frontend type-check:** since this is a TypeScript change, the closing sequence should also run `npx tsc --noEmit` (in addition to the constitutionally-required `cargo test`) to confirm the frontend compiles. The change is type-safe by inspection, but a `tsc` pass is the cheap belt-and-suspenders confirmation. (The constitution's literal `cargo test` requirement is satisfied trivially since no Rust changed.)

## Overall verdict

**SHIP — no findings.** The two new `argLabel` branches are correct, type-safe, and consistent with the existing label convention. They gracefully handle missing/malformed arguments (returning `null` so the header falls back to the bare tool name), correctly extract basenames across both path separators, and the cap-at-3 + "+N" and pattern/glob-suffix logic both behave as specified for all edge cases. Cross-checked against the actual Rust tool schemas — field names and shapes match exactly.
