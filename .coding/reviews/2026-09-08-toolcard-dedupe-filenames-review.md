## Verdict: FINDINGS (0 high, 1 low)

Review of all uncommitted changes on `wt/toolcard-dedupe-filenames`
(`git diff HEAD` + untracked `.coding/` files) against the plan goal:
deduplicate ToolCard header file-path chips so each file appears at most
once (backlog 2a03710d).

### Summary

The fix is correct, minimal, and behavior-preserving. `dedupePaths` +
`buildPathChips` (new, `frontend/src/lib/toolCardPaths.ts:85-145`) collect
paths across ALL grouped calls, dedupe by normalized path (forward slashes,
no trailing slash, lowercased), keep the first occurrence verbatim (so its
link/casing survives), and cap at 3 + `+N` overflow from the **deduped**
list. The ToolCard chip loop (`Message.tsx:500-531`) now seeds `chips` with
`buildPathChips(calls, name)` and the per-call loop only adds label chips
(for pathless calls) + search-engine chips — the render site
(`Message.tsx:568-600`, incl. the `opensDiff = name === "file_edit"` deep-link
check) is untouched. 7 new vitest cases pin the fix.

### Review-focus verification

1. **Correctness of dedup** — PASS.
   - `dedupePaths` (`toolCardPaths.ts:105-116`): `Set`-based, pushes the
     verbatim first occurrence, skips later `seen` normals. Correct.
   - `normalizePath` (`:95-97`): `\\`→`/`, trim trailing `/+`, `.toLowerCase()`.
     `src/main.rs` / `src\\main.rs` / `Src/Main.rs` → all `src/main.rs` (collapse);
     `src/a/main.rs` vs `src/b/main.rs` → distinct (survive). Correct.
   - `buildPathChips` (`:127-145`): loops ALL `calls` accumulating `argPaths`
     per call, dedupes, `slice(0,3)`, overflow `deduped.length - 3`. Correct.
   - *Note (by-design, not a finding):* lowercasing is correct for the target
     platforms (Windows + macOS default filesystems are case-insensitive) and
     is explicitly accepted by the plan. On a case-sensitive Linux fs two
     same-named-different-case files would falsely dedupe, but that requires
     the agent to read one file with inconsistent casing across calls —
     implausible, and out of scope for the macOS/Windows neutrality check.

2. **Behavior preservation** — PASS.
   - (a) Label-only tools (git, shell, git_read, search, search_read):
     `argPaths` returns `[]` for these → `paths.length === 0` → `argLabel`
     chip pushed per call with `key: c.id` (`Message.tsx:507-509`). Unchanged.
   - (b) Search-engine chips (`index · N matches`) + `searchNotes`:
     block at `Message.tsx:514-530` is byte-for-byte unchanged; render at
     `:617-625` unchanged.
   - (c) `file_edit` deep-link: `opensDiff = name === "file_edit"` at
     `Message.tsx:572` is unchanged — only the chip *source* changed
     (`buildPathChips` vs the old per-call loop), not the render.
   - (d) >3 cap + overflow: now from the deduped list (`toolCardPaths.ts:141`),
     which is the intended fix.

3. **Chip ordering / React keys** — PASS.
   - Path chips: `path:<normalized>` + `path:overflow`; label chips: `c.id`;
     engine chips: `${c.id}:engine`. Distinct namespaces — no realistic
     collision (call ids are UUIDs/sequential, never `path:…`).
   - Ordering shifted from per-call-interleaved to all-paths-then-labels, but
     this is unobservable: path-bearing tools (read_files/file_*) never
     produce label chips (the `paths.length === 0` guard), and label-only tools
     never produce path chips. No mixed card can arise in practice.

4. **"Not shown" edge case** — acceptable (plan-accepted). A pathless/
   malformed `read_files` call → `buildPathChips` returns `[]` + `argLabel`
   returns `null` → bare header; the error summary still renders below the
   header (`Message.tsx:630-634`) when the call fails. No regression.

5. **Tests pin the fix** — PASS. 7 new `it` blocks
   (`toolCardPaths.test.ts:272-357`): 3 for `dedupePaths` (dedup, slash/case
   normalization, distinct-same-basename survives), 4 for `buildPathChips`
   (dedup across calls, dedup within one batch, cap+overflow, label-only→[]).
   The "shows each file once even when read in multiple grouped calls" test
   (`:297-308`) is the distinguishing pin: on old code `buildPathChips`/
   `dedupePaths` don't exist → import fails → test errors; with the fix it
   yields exactly one chip. The cap test (`:324-347`) correctly asserts
   `+1` from a 5-spec/4-distinct input.

6. **Constitution** — PASS.
   - No `#[allow(...)]` (N/A — TypeScript).
   - All new public exports carry doc comments (`ToolCardChip`, `dedupePaths`,
     `buildPathChips`); `normalizePath` is module-private but documented too.
   - No unused imports: `basename` still used in `argLabel`
     (`Message.tsx:416,449`); `argPaths` still used in the per-call
     label-fallback guard (`:506`); `buildPathChips` + `type ToolCardChip`
     newly imported and used (`:500`).
   - Multi-platform: `normalizePath` handles both `/` and `\` — correct for
     Windows + macOS; no platform-only APIs.
   - Build/tests reported green (584 vitest, build exit 0); not independently
     re-run by this read-only reviewer, but consistent with the diff.

### Findings

#### LOW 1 — Stray untracked `.coding/` files from a different plan should not be swept into this commit

Two of the four untracked `.coding/` files belong to a **different** line of
work (the branch-policy auto-fork fix), not the toolcard-dedupe bug:

- `.coding/plans/e228e92a-4b5d-485e-962d-600e3f07f1e5.md` — describes
  `work_branch_name` / `ensure_work_branch` / create_plan auto-fork (unrelated).
- `.coding/reviews/2026-09-08-branch-policy-auto-fork-fix-verification.md` —
  review report for that branch-policy work (unrelated).

The two that DO belong here:
- `.coding/knowledge/bug/2026-08-26-read-files-toolcard-duplicate-missing-filenames.md`
  (this bug's record) and
- `.coding/plans/ea71a9d1-5b8b-41e5-845d-dd20290e20a3.md` (this plan).

**Fix:** when committing, stage only the toolcard-dedupe source changes
(`Message.tsx`, `toolCardPaths.ts`, `toolCardPaths.test.ts`), the backlog
flip (`.coding/backlog.jsonl`), and the two related `.coding/` records
above. Exclude `e228e92a-…md` and the branch-policy review report from this
commit (or commit them deliberately as a separate bookkeeping commit) so
the toolcard-dedupe commit stays focused and the stray records don't
desynchronize from their own (possibly unmerged) branch. (Mirrors the
incidental-file note in the 2026-08-25 gitread-stuck-card review.)

### Verdict

**FINDINGS (0 high, 1 low).** The code change itself is correct, well-tested,
and constitution-compliant — ship it after excluding the two stray
branch-policy `.coding/` files from the commit (LOW 1).
