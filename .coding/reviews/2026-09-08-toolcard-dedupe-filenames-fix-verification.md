## Verdict: FINDINGS (0 high, 1 low)

Focused verification review of the LOW 1 fix from
`.coding/reviews/2026-09-08-toolcard-dedupe-filenames-review.md` (stray
branch-policy `.coding/` files swept into the toolcard-dedupe commit by a
blanket `git add`). The fix was a commit-hygiene fix — selective staging so
commit `30820ec` on `wt/toolcard-dedupe-filenames` carries only the 7
toolcard-dedupe files, with the orphaned branch-policy verification report
landed separately on its own branch. Verified against the four requested
checks; the LOW 1 fix itself is correct and fully verified — the single
finding is a residual untracked knowledge file in the working tree.

### Check-by-check verification

**1. Commit `30820ec` contains no stray branch-policy files — PASS.**

`git show --stat 30820ec` lists exactly 7 files, all toolcard-dedupe-related:

- `frontend/src/components/chat/Message.tsx`
- `frontend/src/lib/toolCardPaths.ts`
- `frontend/src/lib/toolCardPaths.test.ts`
- `.coding/backlog.jsonl`
- `.coding/knowledge/bug/2026-08-26-read-files-toolcard-duplicate-missing-filenames.md`
- `.coding/plans/ea71a9d1-5b8b-41e5-845d-dd20290e20a3.md`
- `.coding/reviews/2026-09-08-toolcard-dedupe-filenames-review.md`

Neither stray branch-policy file appears: no `.coding/plans/e228e92a-…md` and
no `.coding/reviews/2026-09-08-branch-policy-auto-fork-fix-verification.md`.
The selective-staging fix achieved exactly what LOW 1 asked for.

**2. Working tree is clean — PARTIAL (see LOW 1).**

`git diff HEAD` is empty — no uncommitted tracked changes. However
`git status --short` shows one untracked file:

```
?? .coding/knowledge/how/2026-08-26-git-tool-commit-auto-stages-everything-use-shell.md
```

This is NOT a stray branch-policy file — the LOW 1 concern is fully resolved:
both stray files are gone from this branch (neither untracked nor in the
commit). It is a 6-line HOW procedural knowledge record documenting that the
`git` tool's commit subcommand runs `git add -A` unconditionally, overriding
selective staging — the very lesson the LOW 1 selective-staging fix relied on
(its body references "excluding stray .coding/ files," i.e. the LOW 1
scenario). It is not a duplicate of anything tracked on
`wt/branch-policy-auto-fork`: `decdf6b`'s 13 files do not include it, and
`a44a8a4` adds only the verification report. Per the project constitution,
`.coding/knowledge/` files travel with git and should be tracked — this one
was left untracked when the bug record and plan were committed, so the tree
is not clean. → LOW 1.

**3. The verification report landed on its own branch — PASS.**

`git show a44a8a4` is commit "Add verification PASS report for the
detached-HEAD fix (L1)" adding exactly one file,
`.coding/reviews/2026-09-08-branch-policy-auto-fork-fix-verification.md`
(74 insertions, new file). Its message states it lands "on its own branch"
on top of `decdf6b`. `git show --stat decdf6b` confirms `decdf6b` is the
"One wt/* branch per agent directory (auto-fork, no auto-accumulation)"
commit on `wt/branch-policy-auto-fork` (13 files, including the tracked
`.coding/plans/e228e92a-4b5d-485e-962d-600e3f07f1e5.md` — matching the claim
that the redundant untracked `e228e92a` copy was removed because it is already
tracked there). So `a44a8a4` sits on `wt/branch-policy-auto-fork` atop
`decdf6b`, exactly as described.

**4. The code change itself is unchanged from the original review — PASS.**

The fix for LOW 1 was purely selective staging — no source edited. Confirmed:

- `frontend/src/lib/toolCardPaths.ts`: `normalizePath` (lines 95-97),
  `dedupePaths` (105-116), `buildPathChips` (127-145), `ToolCardChip`
  (87-91) — all present at the exact line numbers the original review cited.
- `frontend/src/components/chat/Message.tsx:500`:
  `const chips: ToolCardChip[] = buildPathChips(calls, name);` — the chip
  loop seeds with `buildPathChips`; the per-call loop (505-531) only adds
  label chips (pathless calls, 507-509) + search-engine chips (514-530),
  exactly as the original review described. The `file_edit` deep-link render
  is untouched.
- `git diff HEAD` is empty (no uncommitted tracked changes) and `30820ec` is
  the branch tip (`git log` shows it at HEAD), so the committed source is
  identical to the working-tree source the original review approved. The
  original review report itself is committed inside `30820ec` alongside the
  source, describing the same logic with the same line numbers — internally
  consistent.

### Findings

#### LOW 1 — Untracked HOW knowledge file should be committed

`.coding/knowledge/how/2026-08-26-git-tool-commit-auto-stages-everything-use-shell.md`
is untracked on `wt/toolcard-dedupe-filenames`. It is a legitimate procedural
knowledge record (6 lines) documenting the `git` tool's unconditional
`git add -A` — the reason the LOW 1 fix had to use selective shell staging in
the first place (its text references "excluding stray .coding/ files," i.e.
the LOW 1 scenario). It is not a duplicate of anything tracked on
`wt/branch-policy-auto-fork`. Per the constitution, `.coding/knowledge/`
files travel with git and should be tracked; this one was left behind when
the bug record and plan were committed in `30820ec`.

**Fix:** commit the HOW knowledge file on `wt/toolcard-dedupe-filenames` (a
small knowledge commit, mirroring the `7f818e8 Knowledge: …` pattern in the
log) so the working tree is clean and the procedural learning travels with
the branch. No source change, no test impact.

### Verdict

**FINDINGS (0 high, 1 low).** The LOW 1 commit-hygiene fix is correct and
fully verified — the commit is clean (7 files, no stray branch-policy
files), the branch-policy verification report landed separately on its own
branch, and the source is unchanged from what the original review approved.
The only residual is one untracked HOW knowledge file (a procedural record
born of this very fix) that should be committed to clean the tree.
