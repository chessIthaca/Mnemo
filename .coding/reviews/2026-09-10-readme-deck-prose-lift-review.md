## Verdict: FINDINGS (0 high, 3 low)

Plan 7fd51434's README lift is faithfully implemented and factually sound: all 8 described edits landed, the lifted prose traces cleanly to `docs/deck_content.py`, every independently checkable number checks out (781 reviews and 440 knowledge records counted exactly; 608 plans consistent; commit/test counts parent-verified and date-attributed), markdown integrity and tone are clean, and no tracked doc contradicts the new claims. The three LOW findings: (1) the new workflow paragraph's `finish`-gate sentence can read as a general gate when the regression-test gate is bug-plan-specific; (2) an out-of-scope uncommitted change — a knowledge spec file condensed to a digest-style body — will ride into this plan's commit unless handled first; (3) plan step 3's "Rework is the work you do twice" one-liner never landed.


## What was verified

**Proof numbers (README.md:28)**
- **781 review reports** — counted `.coding/reviews`: exactly 781 entries. (A handful are diagnosis/verification docs rather than spawned-reviewer reports, but the count follows the deck's own refresh convention — slide 19 notes: `ls .coding/reviews/*.md | wc -l`.)
- **440 knowledge records** — counted `.coding/knowledge` recursively: bug/ 155 + decision/ 99 + how/ 47 + spec/ 135 + 4 top-level `.md` = exactly 440.
- **608 plans** — `.coding/plans` lists 611 entries; the parent's `-Filter *.md` count of 608 implies 3 non-`.md` sidecars (e.g. `stack.json`) — consistent.
- **1,303 commits / ≈2,450 Rust tests / ≈1,070 frontend tests** — parent-verified via the deck's refresh commands (root 2158+16, src-tauri 293+4+2 = 2473 ≈ 2,450); not independently re-runnable read-only, and all figures are date-attributed ("As of January 2027"), so drift is hedged by design.
- **781 > 608** — "more reviews than plans — findings trigger another round" is internally consistent, and the many `-round2`/`-round3` files in `.coding/reviews` confirm the mechanism.
- **"~160,000 lines in its first 23 days"** — matches deck slide 19 (~160k = 121k Rust + 39k TS, 23 days from first commit), framed as a historical claim, not a current one.

**Architecture claims (README.md:32-40, 108)**
- "In Planning no write tools exist — absent from the request schema, not discouraged" — PLAN.md:332-333 confirms write tools/shell/`complete_step` are omitted from the `tools` array in Planning. ✓
- "Bug plans are locked to the reproduce → root-cause → fix → verify skeleton" — PLAN.md:358-363 (skeleton lock; `update_plan` refuses replacement and append). ✓
- The regression-test finish gate is bug-plan-specific — see Finding 1.
- "Shell/build noise never enters the window", deferred tool families, byte-stable tool array for prompt caches — match README:50/67 and PLAN.md's caching strategy. ✓
- "The spec is a file on disk, not a chat message" + crash-resume — matches PLAN.md:337-341. ✓

**Deck fidelity** — every lifted line traces to `docs/deck_content.py`: the five failure modes (slide 2 + notes), the allocation punchline, the "smaller / less capable / faster" tone in both the conviction paragraph and the routing bullet, memory-as-defect-control ("the cost is not the retyping…" — slide 9), "editing with a map, not a torch" + "the right file, first try" (slides 15/20), spec-on-disk (slide 5), the context bullet (slide 6), the harness punchline (slide 5), and the 2am/deadline + schema-not-prompt + rails sentences (slides 5/8 notes). ✓

**Markdown & tone** — bold/italic/backtick pairs balanced in every edited region; list integrity intact (failure-mode list, conviction bullets, Key ideas bullets); the two previously-broken bullets (old lines 22 and 26) are now grammatical closed statements; em-dash density and assertive register match the README's existing voice. ✓

**Documentation sync** — PLAN.md's test counts sit under a dated "Current status (2026-08-11)" header (historical snapshot — no contradiction with the date-attributed January numbers). `docs/why-mnemo-deck.md` self-declares its ON SLIDE / SPEAKER NOTES sections stale with `deck_content.py` as the single source of truth, so its line-414 "cheap, fast" remnant is marked-stale by the file's own header — no action needed. Module docs are unaffected. The README's own Key ideas bullet (line 36) correctly scopes the regression-test gate to bug plans. ✓

**Multi-platform neutrality** — prose only; no platform-specific assumptions introduced ("the laptop closes" is generic). ✓

**Scope** — no screenshots; Key features / Building / Configuration untouched (diff confirms: only the three narrative sections plus the workflow paragraph changed). ✓


## Findings

### 1. LOW — README.md:108 — the `finish`-gate sentence can read as a general gate

"`finish` is blocked until the verify step names a regression test, validated against the code graph." The gate is **bug-plan-specific**: implementation plans' `finish` is gated on the reviewer report, and research plans have no gate at all (PLAN.md:354-369; the README's own line 36 scopes it correctly). As written, the sentence follows "Bug plans are locked to the … skeleton," so "the verify step" has a bug-plan antecedent — but the unqualified sentence-initial `finish` invites the general (false) reading. This is the exact check the plan flagged for review. Minimal fix — insert three words:

> For bug plans, `finish` is blocked until the verify step names a regression test, validated against the code graph.

### 2. LOW — out-of-scope uncommitted change will ride into this commit: knowledge spec file condensed to a digest

`.coding/knowledge/spec/2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md` (modified, −19/+1): the detailed body — the per-bullet status⇄plan-lifecycle contract and four amendment paragraphs — was replaced by a one-paragraph digest ending in a self-referential `path .coding/knowledge/spec/2027-01-07-…md` pointer. That is the derived-index digest format written into the truth file, violating the repo's own invariant (README:56 — knowledge files are complete and unbounded; the derived index holds the budgeted digests with path pointers). The predecessor (`2026-12-06-…md`, also touched) now delegates "Canonical contract: the 2027-01-09 amendment" to this file — which carries that amendment as a single sentence. Timestamps indicate the write happened around the cace17a6 session's finish, not from this plan. Before committing: restore the full body from HEAD (`git show HEAD:.coding/knowledge/spec/2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md`) and re-apply the cace17a6 amendment as an appended "Amended 2027-01-09 (plan cace17a6): …" paragraph — the file's established pattern — or confirm the condensation was deliberate; if deliberate, at minimum drop the trailing self-`path` line. Related: two already-committed knowledge files carry the same self-referential digest tail (`spec/2026-12-21-multi-provider-prompt-caching-…:16`, `bug/2027-01-07-serving-layer-strips-…:7`) — candidate backlog item for the memory tooling.

### 3. LOW — plan step 3's "Rework is the work you do twice" one-liner (deck slide 11) never landed

Plan step 3 lists five deck one-liners to weave into Key ideas; four landed (harness-vs-chat, spec-on-disk, forgotten-decision-is-a-defect, map-not-a-torch). The rework slogan is absent — the concept is present as failure mode #4 ("**Rework**: a defect found after merge costs multiples…"), and the parent's task summary already reflects the drop, so this may be a conscious omission. If not, the minimal fix folds it into the failure-mode bullet: `- **Rework** — the work you do twice: a defect found after merge costs multiples of one found before commit — …`. Otherwise a one-line justification suffices.

## Tree state (advisory — no action required)

The working tree also carries other sessions' uncommitted side-car changes that this plan's closing commit will sweep in if committed wholesale: `.coding/backlog.jsonl` (+1 well-formed pending item — the load_tools schema-rendering request), untracked `.coding/knowledge/bug/cace17a6.md` and `.coding/knowledge/how/2027-01-07-load-tools-…md` (standard finish-captured BUG / HOW formats), and this plan's `.coding/plans/7fd51434.md`. All are legitimate mergeable side-car content — just commit them knowingly rather than blindly.
