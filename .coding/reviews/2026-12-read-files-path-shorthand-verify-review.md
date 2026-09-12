## Verdict: PASS

Verification review of commit `5c37673` ("fix: read_files ToolCard shows file
name for single-file `path` shorthand") on branch `fix/read-files-path-shorthand`.
This is the follow-up to the first review
(`.coding/reviews/2026-12-read-files-path-shorthand-review.md`), which found
1 HIGH + 2 LOW findings. All three are confirmed addressed, and the fix itself
is correct.

---

### Commit scope — clean (6 files, all expected)

`git show 5c37673` lists exactly:

| File | Change |
|------|--------|
| `.coding/plans/3b675749-90c9-4dce-93fa-dbad4d603a52.md` | new (plan) |
| `.coding/reviews/2026-12-read-files-path-shorthand-review.md` | new (first review report) |
| `frontend/src/lib/toolCardPaths.ts` | modified — `argPaths` fall-through |
| `frontend/src/components/chat/Message.tsx` | modified — `argLabel` fall-through |
| `frontend/src/lib/toolCardPaths.test.ts` | modified — +2 tests |
| `frontend/src/components/chat/messageArgLabel.test.ts` | modified — +6 tests |

No other files are touched.

---

### Finding verification

#### HIGH 1 — Stray revert of merged R9/R10 work (`src/provider/*.rs` + `package-lock.json`) — FIXED ✓

The three Rust provider files (`src/provider/anthropic.rs`,
`src/provider/mod.rs`, `src/provider/openai.rs`) and `package-lock.json` are
**NOT present** in the commit diff. They were restored to HEAD before
committing, so the reviewed-and-merged R9 `max_completion_tokens` cap is
intact and does not ride along on this unrelated card-label fix. ✓

#### LOW 1 — `argLabel` path-shorthand branch had no unit test — FIXED ✓

`messageArgLabel.test.ts` now contains a new
`describe("argLabel — read_files")` block with **6 tests**:

1. `shows basenames for the files array shape` → `"a.rs, b.rs"`
2. `caps at 3 names with +N overflow` → `"a.rs, b.rs, c.rs +2"`
3. `falls back to the top-level path for the single-file shorthand` → `"main.rs"`
4. `files array wins over a top-level path when both present` → `"a.rs"`
5. `returns null when files is empty or has no valid paths` → `null` (two cases)
6. `returns null when neither files nor path is present` → `null`

Test #3 directly exercises the fixed `argLabel` shorthand path. ✓

#### LOW 2 — Erroneous `backlog.jsonl` flip to `failed` — FIXED ✓

`.coding/backlog.jsonl` is **NOT present** in the commit diff. It was restored
to HEAD, so the backlog item that motivated this fix is not prematurely marked
`failed`. ✓

---

### Fix correctness — traced end-to-end against full source

Both functions were read in full (not just the diff hunk) to confirm the
fall-through reaches the common `path`/`file` fallback and that no unintended
block is entered.

#### `argPaths` (`frontend/src/lib/toolCardPaths.ts:48-73`)

- `files` is an array → enters `if (Array.isArray(files))` (line 50), returns
  the mapped/filtered paths via early `return` (line 51-57). **Identical to the
  pre-fix array behavior** — the `return` was simply moved inside the `if`.
  ✓ unchanged
- `files` absent / not-an-array → `Array.isArray(files)` is false, the inner
  `if` body is skipped, and there is **no `return`** after it, so control
  exits the `read_files` block (line 59). The next statement is
  `if (toolName === "write_review_report")` (line 63) — **false** for
  `read_files`, so it is skipped (no fall-through into the
  `write_review_report` block). Control reaches the common fallback
  (lines 69-73): extracts top-level `path`/`file` and returns `[path]` or `[]`.
  ✓ reaches common fallback

#### `argLabel` (`frontend/src/components/chat/Message.tsx:374-416`)

- `files` is an array → enters `if (Array.isArray(files))` (line 376), builds
  the basename label with `+N` overflow and returns it (lines 377-385).
  **Identical to the pre-fix array behavior.** ✓ unchanged
- `files` absent / not-an-array → inner `if` skipped, no `return`, exits the
  `read_files` block (line 387). Falls past the `search`/`search_read` block
  (line 392, `toolName` mismatch), then `graphCallLabel("read_files", …)`
  (line 402) returns `null` for a non-graph tool so its `if (graph !== null)`
  guard is skipped, and control reaches the common fallback (lines 407-415):
  extracts top-level `path`/`file` and returns `basename(path)` or `null`.
  ✓ reaches common fallback

#### Edge cases (all correct)

- Empty `files: []` → array branch → `[]` / `null`. Unchanged. ✓
- `files: [{foo:"bar"}]` (array, no valid paths) → array branch, `names.length === 0` → `[]` / `null`. Does **not** fall through to top-level `path` — intended "files wins" semantics, unchanged. ✓
- Both `path` and `files` present → `Array.isArray(files)` true → `files` wins, top-level `path` ignored. Mirrors the Rust tool. Covered by precedence tests in both suites. ✓
- `files` present but non-array (`files:null`, `files:"foo"`) → falls through to `path`/`file`. This is the new fix. ✓
- Missing/blank `path` after fall-through → `[]` / `null`. ✓

#### No fall-through into `write_review_report` for `read_files`

Confirmed: the `write_review_report` block in `argPaths` (line 63) is guarded
by `toolName === "write_review_report"`, which is false for `read_files`. In
`argLabel` there is no `write_review_report` block at all. The concern is
unfounded. ✓

---

### Test presence + old-code failure analysis

**`toolCardPaths.test.ts` (+2 tests):**
- `read_files single-file path shorthand falls back to the top-level path`:
  `argPaths('{"path":"src/main.rs"}', "read_files")` → `["src/main.rs"]`.
  Old code did `if (!Array.isArray(files)) return [];` → returned `[]`.
  **FAILS on old code.** ✓ true regression test for the fix.
- `read_files files array wins over a top-level path when both present`:
  `argPaths('{"path":"ignored.rs","files":[{"path":"a.rs"}]}', "read_files")` → `["a.rs"]`.
  Old code returned `["a.rs"]` (array branch unchanged). **Passes on old code**
  — this is a precedence guard documenting unchanged "files wins" semantics, not
  a regression test for the fix. Correct and expected.

**`messageArgLabel.test.ts` (+6 tests):**
- `falls back to the top-level path for the single-file shorthand`:
  `argLabel('{"path":"src/main.rs"}', "read_files")` → `"main.rs"`.
  Old code did `if (!Array.isArray(files)) return null;` → returned `null`.
  **FAILS on old code.** ✓ true regression test for the fix.
- `shows basenames for the files array shape`, `caps at 3 names with +N
  overflow`, `files array wins over a top-level path when both present`,
  `returns null when files is empty or has no valid paths`,
  `returns null when neither files nor path is present` — all exercise the
  unchanged array branch or the unchanged empty/missing edge cases, so they
  **pass on old code** as well as new. These are guard/edge-case tests that
  document intended behavior and protect against future regressions — correct
  and expected, not a deficiency.

Both halves of the fix (argPaths and argLabel) now have a dedicated
shorthand-fallback regression test that fails on the old `return []` /
`return null` code and passes with the fix. ✓

---

### Constitution

- **Multi-platform neutrality:** Frontend-only change (TypeScript/React), no
  platform APIs, paths, or shell syntax. ✓
- **Doc sync:** A ToolCard header display fix needs no README/PLAN/endpoints
  update. The inline code comments in both functions were updated to document
  the shorthand fall-through. ✓
- **Security:** Display-only; paths originate from already-trusted tool args
  and render as text/links. No new attack surface. ✓

---

### Summary

All three findings from the first review are addressed: the stray R9/R10
revert and `package-lock.json`/`backlog.jsonl` bookkeeping changes were
excluded from the commit (HIGH 1, LOW 2), and `argLabel` now has direct unit
tests covering the shorthand fallback (LOW 1). The fix itself is correct —
both `argPaths` and `argLabel` fall through to their common `path`/`file`
fallback when `files` isn't an array, the array case is unchanged, and there
is no fall-through into the `write_review_report` block. The shorthand
regression tests fail on the old code and pass with the fix. No findings.
