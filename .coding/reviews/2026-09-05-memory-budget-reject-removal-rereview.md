## Verdict: FINDINGS (0 high, 1 low)

Re-review of the uncommitted change on the working branch after the main
agent fixed the 7 low documentation-sync findings from the original review
(`.coding/reviews/2026-09-05-memory-budget-reject-removal-review.md`).

The change removes the hard character-budget reject (`budget_violation`)
from `memory_write`/`memory_supersede` and instead encourages short memories
through (a) wiring the configurable `MemorySearchConfig` budgets into the
indexer/finish_capture auto-truncation and (b) a soft prompt guideline.

## The 7 original findings — all FIXED

Each was verified against the **actual current file text** (not just the
diff), and a project-wide search for the stale phrases
(`enforced by \`memory_write\``, `budget-enforced`, `budget_violation`,
`over budget`, `tool-level budget`, `budget bypass`) confirms the only
remaining matches live in `.coding/plans/*.md` and `.coding/reviews/*.md`
(immutable historical records — not docs to sync). Zero remain in `src/` or
active docs.

1. **L1 — `src/memory/types.rs:99-103` — FIXED.** The enum doc now reads
   "Typed records are pointer-first (the gist + a path/commit pointer; the
   file carries the detail) and are filterable via `memory_list`…". The old
   "carry content budgets (enforced by `memory_write`)" is gone.

2. **L2 — `src/memory/types.rs:117-118` — FIXED.** The `Review` variant doc
   now reads "Usually indexer-written (Phase 2 derived records)." The stale
   "but the prefix is budget-enforced for authored writes too" sentence is
   gone.

3. **L3 — `src/memory/types.rs:428-430` — FIXED.** The `plan_budget` field
   doc now reads "Digest auto-truncation lengths (chars) for derived/captured
   digests, read by the indexer and finish-capture. Defaults mirror
   [`MemoryRecordType::content_budget`]."

4. **L4 — `src/memory/mod.rs:184-187` — FIXED.** The trait-method doc now
   reads "read by recall (decay/caps) and the derived-index/finish-capture
   auto-truncation."

5. **L5 — `src/memory/mod.rs:413-415` and `:466-468` — FIXED.** Both the
   `search_config` field doc and the inherent `memory_search_config` method
   doc now read "Read per recall/index so a Settings save takes effect
   without a restart."

6. **L6 — `src-tauri/src/ipc/settings.rs:963` — FIXED.** The DTO doc now
   reads "Digest auto-truncation lengths (chars) for derived/captured
   digests."

7. **L7 — `src/tool/memory/mod.rs:145-147` — FIXED.** The dangling sentence
   "The tool-level budget therefore applies only to DB-only rows." is gone.
   (The original review's *optional* suggestion to merge the two adjacent
   "Knowledge-backed" comment blocks at `:145-147` and `:150-154` was left
   undone — acceptable, as it was explicitly optional and the two blocks
   describe distinct concerns; neither is stale or incorrect.)

## No new issues introduced by the doc edits

The doc edits are comment-only (no code, no signatures, no tests touched).
The README.md / PLAN.md / prompt.rs / finish_capture.rs / indexer.rs edits
were part of the original change (already verified sound) and remain
consistent with the new "auto-truncation" language. The new test
`lowered_config_budget_truncates_derived_digest` and the replacement test
`db_only_typed_write_accepts_long_content` are unchanged and still
meaningful.

## Remaining finding (LOW — same class as the original 7)

### L8 — `src/memory/types.rs:410` — stale `MemorySearchConfig` struct doc

The struct-level doc comment on `MemorySearchConfig` still reads:

> "the store holds a snapshot that recall and `memory_write` read per call,
> so a Settings save takes effect without a restart."

`memory_write` no longer reads the config snapshot — the `budget_violation`
call (the only config read on the write path) was removed by this very
change. After the change the config is read by **recall** and the
**indexer/finish-capture** auto-truncation, not by `memory_write`. This is
the exact same class of stale doc as L4 (which attributed config reads to
`memory_write`/`memory_supersede` and was fixed); this struct-level comment
makes the same stale attribution and was missed by both the original review
and the fix pass.

**Suggested fix** (mirrors the L4 wording): change "recall and
`memory_write` read per call" to "recall and the indexer read per call" —
e.g.

> "the store holds a snapshot that recall and the indexer read per call, so
> a Settings save takes effect without a restart."

No runtime, correctness, or build impact — a documentation-sync cleanup of
the same kind the original review was catching.

## Summary

All 7 original findings are correctly and completely fixed, and no new
issues were introduced by the doc edits. One additional stale doc comment
of the same class remains at `src/memory/types.rs:410` (the
`MemorySearchConfig` struct doc still claims `memory_write` reads the
config). Fixing that one line completes the documentation-sync cleanup.
