## Verdict: FINDINGS (1 high, 2 low)

Review of ALL uncommitted changes on the current working branch. The plan goal
is the read_files ToolCard `path`-shorthand fix (3 frontend files). The working
tree, however, contains **8 modified files** — including a full revert of the
previously-merged R9 `max_completion_tokens` cap in the Rust providers, which is
unrelated to this plan and must not ship with it.

---

### The read_files fix itself — CORRECT (no findings)

Traced both functions end-to-end. The fall-through reaches the common
`path`/`file` fallback exactly as intended, and the array case is unchanged.

**`argPaths` (`frontend/src/lib/toolCardPaths.ts:48-59`):**
- `files` is an array → enters the block, returns mapped/filtered paths (early
  `return`). Behavior identical to before. ✓
- `files` absent / not-an-array → `if (Array.isArray(files))` is false, the
  `read_files` block is skipped. The next block is `if (toolName ===
  "write_review_report")` — since `toolName === "read_files"`, it does **not**
  match (the concern raised in the review brief is unfounded). Control reaches
  the common fallback at lines 69-73 and returns `[path]` / `[]`. ✓

**`argLabel` (`frontend/src/components/chat/Message.tsx:374-387`):**
- `files` is an array → basename label with `+N` overflow (unchanged). ✓
- `files` absent / not-an-array → skips the block, falls past the
  `search`/`search_read` block (toolName mismatch), past `graphCallLabel`
  (returns null for a non-graph tool), to the common `basename(path)` fallback
  at lines 407-414. ✓

**Edge cases (all correct):**
- Empty `files: []` → array branch → `[]` / `null`. Unchanged. ✓
- `files` array with no valid path entries (e.g. `[{foo:"bar"}]`) → array
  branch → `[]` / `null`. Does **not** fall through to `path`; this is the
  intended "files wins" semantics and is unchanged behavior. ✓
- Both `path` and `files` present → `Array.isArray(files)` true → `files`
  wins, top-level `path` ignored. Mirrors the Rust tool. Covered by the new
  precedence test. ✓
- `files` present but non-array (e.g. `files:"foo"`, `files:null`) → falls
  through to `path`/`file`. This is the new fix. ✓
- Missing/blank `path` after fall-through → `[]` / `null`. ✓

**Regression tests (`toolCardPaths.test.ts:39-50`):** two new tests exercise the
shorthand fallback and the `files`-wins precedence. Both would fail under the
old `return []` / `return null` code. Good coverage for `argPaths`.

**Security:** Frontend display only; paths originate from already-trusted tool
args and render as text/links. No new attack surface. ✓

**Constitution:** Multi-platform neutral (frontend only, no platform code). Doc
sync — a card-label display fix needs no README/PLAN update. ✓

---

### FINDINGS

#### HIGH 1 — Stray revert of the merged R9 `max_completion_tokens` cap (unrelated to this plan)

**Files:** `src/provider/anthropic.rs`, `src/provider/mod.rs`,
`src/provider/openai.rs`

The uncommitted diff reverts the entirety of commit `65422bf` ("R9
max_completion_tokens cap + R10 repetition detector + model-switch
summarization"), which was reviewed twice (see memory REVIEW records
`7bae0b67…` and `ec682b33…`) and merged to `develop` (`a2576be`) and `main`
(`d3a6e07`):

- `src/provider/mod.rs`: deletes the `estimate_prompt_tokens` helper and bumps
  both OpenAI and Anthropic `max_output_tokens` defaults `32_000 → 64_000`.
- `src/provider/anthropic.rs`: replaces the context-window-aware cap with a bare
  `let max_tokens = self.caps.max_output_tokens.max(4096);` and updates the
  assertion `32_000 → 64_000`.
- `src/provider/openai.rs`: same cap removal (`max_completion =
  self.caps.max_output_tokens.max(4096)`), and deletes the three cap-behavior
  tests (`estimate_prompt_tokens_counts_content_and_tools`,
  `max_completion_tokens_capped_when_prompt_large`,
  `max_completion_tokens_floors_at_1024_when_prompt_exceeds_context`).

This is **completely unrelated** to the read_files ToolCard fix (the plan goal)
and is **not listed** in the task's changed-files. If committed alongside the
read_files fix it would silently undo a reviewed-and-shipped bugfix (the R9 cap
that prevents input+output from exceeding the model's context window).

**Required action:** Exclude these three Rust files (and their test deletions)
from the read_files commit. If the revert is intentional, it needs its own
plan + review — it must not ride along on an unrelated card-label fix. Verify
with `git checkout HEAD -- src/provider/anthropic.rs src/provider/mod.rs
src/provider/openai.rs` before committing, then re-confirm `cargo test` is
green on the *un-reverted* base.

#### LOW 1 — `argLabel` path-shorthand branch has no direct unit test

**File:** `frontend/src/components/chat/Message.tsx:374-387`

The fix touches `argLabel` symmetrically with `argPaths`, but the new
regression tests (`toolCardPaths.test.ts`) only cover `argPaths`. `argLabel`
lives in `Message.tsx` (a React module), so it isn't exercised by the node-env
test suite — a pre-existing gap, not introduced here. The fix is correct by
inspection (identical fall-through structure), but the `argLabel` shorthand
path is unverified by tests. Consider extracting `argLabel` into the
React-free `toolCardPaths.ts` (or adding a jsdom-backed test) so both halves
of the fix are covered. Not blocking.

#### LOW 2 — Unrelated bookkeeping/metadata changes in the working tree

**Files:** `package-lock.json`, `.coding/backlog.jsonl`

- `package-lock.json`: adds `"license": "MIT"` to the root and `frontend`
  workspace entries. Benign metadata (consistent with the prior MIT relicense,
  commit `fe4de25`), but unrelated to the read_files fix.
- `.coding/backlog.jsonl`: flips backlog item `2a03710d…` ("why do some
  readfiles not shwo the file name") from `pending` to `failed` with a note
  "plan loop never ran this turn". This is the very backlog item that
  motivated this fix — marking it `failed` appears premature/incorrect since
  the fix addresses it. Confirm whether this status flip is intended before
  committing.

Neither blocks the read_files fix, but both should be consciously decided on
rather than swept into the commit.

---

### Summary

The read_files `path`-shorthand fix is correct, well-tested (for `argPaths`),
and constitution-compliant. The **blocking** issue is the stray R9 revert in
the Rust providers — it is unrelated to this plan, reverts merged/reviewed
work, and must be excluded from the commit. The two LOW findings are
test-coverage and bookkeeping hygiene.
