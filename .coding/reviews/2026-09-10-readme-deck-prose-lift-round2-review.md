## Verdict: PASS

Round-2 verification of plan 7fd51434 ("README: lift the deck's sharper prose") at HEAD 13bffb8 on wt/agenticcoding: all three round-1 LOW findings are correctly fixed, the advisory backlog item (63f882c9) exists and is well-formed, the commit contains exactly the expected six-file set with the restored spec file correctly absent from its diff, and the working tree is clean. No new findings.


## Fix verification (round-1 findings)

### Finding 1 (LOW) — README.md:108 finish-gate scoping — FIXED ✓
The sentence now opens exactly as round 1 prescribed: "…Bug plans are locked to the reproduce → root-cause → fix → verify skeleton. **For bug plans, `finish` is blocked** until the verify step names a regression test, validated against the code graph." The unqualified sentence-initial `finish` (the general/false reading) is gone; the gate is now explicitly bug-plan-scoped, consistent with the README's own Key-ideas bullet (line 36: "the verify step must record the regression test name (validated against the code graph at finish)" — inside the bug-plan bullet) and with PLAN.md's bug-plan-only regression-test gate. Verified in both the worktree read and the commit diff.

### Finding 2 (LOW) — condensed knowledge spec file — FIXED ✓
`.coding/knowledge/spec/2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md` is fully restored (24 lines):
- **The complete per-bullet status⇄plan-lifecycle contract**: `in_flight` (l.9), `done` (l.10), `failed` (l.11), every-other-turn-end (l.12), halt-for-approval (l.13), checkpoint-failure (l.14), escape hatches (l.15), plus the `backlog_status`/`CantResolve` bullet (l.16).
- **All four dated amendment paragraphs**: 2027-01-07 run-all orphans (l.18), 2027-01-07 amplifier fix (l.20), 2027-01-07 done-orphan guard (l.22), 2027-01-09 single-dispatch intervention (l.24).
- **No self-referential digest tail** — the file ends on the 2027-01-09 amendment paragraph; no `path .coding/…` line anywhere.
- **Unmodified vs HEAD**: `git status --short` and `git diff HEAD` are both empty (worktree == HEAD), and the file does not appear in 13bffb8's diff — the condensation never landed in any commit; the restore correctly produced no diff.
- **Consistency**: the superseded predecessor (`2026-12-06-…md`, l.22) delegates "Canonical contract: the 2027-01-09 amendment" to this file — and the successor now carries that amendment in full (l.24), so the delegation is sound (at round 1 it pointed at the digest).

### Finding 3 (LOW) — "Rework is the work you do twice" one-liner — FIXED ✓
README.md:16 (failure mode #4) reads exactly the prescribed fold: "- **Rework** — the work you do twice: a defect found after merge costs multiples of one found before commit — and an agent that certifies its own work maximizes the number you find late." Verified in worktree and commit diff — the deck slide-11 one-liner has landed.

## Advisory follow-up — backlog item 63f882c9 ✓
`.coding/backlog.jsonl:143` (committed in 13bffb8): id `63f882c9-01fc-42db-b615-fdbb4527c9fb`, status `pending`, well-formed — headline ≤100 chars ("memory_update rewrites a knowledge truth-file's body with digest content — data loss"), blank line, then a body carrying all five required elements: problem+repro (the −19/+1 incident, the violated README invariant, the two committed digest-tail files), fix direction (three concrete options: row-only update / dated amendment append / refuse-with-error, plus repairing the two committed files), acceptance (a regression test pinning that memory_update cannot shrink a full-body file; both digest-tail files repaired; cargo test green), and related pointers (the round-1 review, the README invariant, the row-vs-file contract). It accurately captures the finding-2 root cause. The sibling item cc52264b (load_tools schema rendering) also landed — backlog.jsonl is +2 in the commit, matching the commit message.

## Commit-content verification (b)
13bffb8 is HEAD (git log confirms) and contains **exactly** the expected six files, nothing more:
1. `README.md` — the lift plus both fix edits
2. `.coding/backlog.jsonl` — +2 (cc52264b, 63f882c9)
3. `.coding/knowledge/bug/cace17a6.md` — finish-captured BUG record, standard format (front matter, symptom, regression-test name `intervention_keeps_a_single_dispatch_item_in_flight`, plan path + branch @ 02dd4cd unmerged hint)
4. `.coding/knowledge/how/2027-01-07-load-tools-confirmation-is-final-never-re-call-l.md` — standard HOW format
5. `.coding/plans/7fd51434.md` — the plan file, all 5 steps checked
6. `.coding/reviews/2026-09-10-readme-deck-prose-lift-review.md` — the round-1 report, verbatim

The restored spec file correctly has **no** diff in the commit. The commit message enumerates the side-car content knowingly — exactly what round 1's advisory asked ("commit them knowingly rather than blindly"). The working tree is clean: nothing left uncommitted, no stray changes swept in.

## Regression check (a)
The round-1 → HEAD delta is two one-line README prose edits plus a git restore of a `.coding` knowledge file; the commit touches **no Rust/TS source**, so no test outcome can change through the diff itself (README.md is not compiled). The commit message records the full suites re-run green after the fixes (root 2158+16, src-tauri 293+4+2, 0 failed), matching the parent's report; not independently re-runnable read-only, but there is no code path through which these edits could affect a test.

## Markdown integrity (c)
- **README.md:16** — the Rework bullet is item 4 of the five-item failure-mode list (ll.13–17); the `**Rework**` bold pair is balanced, em-dash usage matches the list's voice, list integrity is intact (five single-line bullets; the punchline at l.19 and the conviction paragraph follow unchanged).
- **README.md:108** — the edited sentence sits inside the workflow paragraph (blank-line separated from the numbered list above and `## Building` below); the `finish` backtick pair and the `*suggest*` italic pair are balanced; the sentence reads unambiguously bug-plan-scoped.
- No duplicate or orphaned text from the fixes anywhere in the added regions (full README diff reviewed).

## Advisory (no action required)
- The two committed digest-tail knowledge files round 1 named (`spec/2026-12-21-multi-provider-prompt-caching-*.md:16`, `bug/2027-01-07-serving-layer-strips-*.md:7`) remain as-is — correctly deferred into backlog item 63f882c9's scope rather than silently ignored.
- Minor stylistic note, not a finding: line 108 now opens two consecutive sentences with "Bug plans…" ("Bug plans are locked to… For bug plans, `finish` is blocked…"). That is the exact fix round 1 prescribed; the small repetition buys unambiguity.
