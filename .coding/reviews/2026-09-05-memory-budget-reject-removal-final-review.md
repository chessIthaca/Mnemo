## Verdict: PASS

Third (final) review pass of the uncommitted staged changes on the working
branch. The change removes the hard character-budget reject (`budget_violation`)
from `memory_write`/`memory_supersede` and instead encourages short memories
through (a) wiring the configurable `MemorySearchConfig` budgets into the
indexer/finish_capture auto-truncation and (b) a soft prompt guideline. The
core behavior change was verified sound in the first review (0 high findings);
this pass focused on confirming the documentation is now fully synced after the
three doc-comment fixes made since the rereview.

### The three fixes — verified against current file text

1. **L8 — `src/memory/types.rs:410`** (`MemorySearchConfig` struct doc):
   now reads *"the store holds a snapshot that recall and the indexer read per
   call"* (was *"recall and `memory_write` read per call"*). ✅ Correct and
   accurate — the config is read by recall (decay/caps) and the indexer (digest
   budgets), not by `memory_write`.

2. **`frontend/src/lib/tauri.ts:404`** (TS DTO field doc): now reads
   *"Digest auto-truncation lengths (chars) for derived/captured digests."*
   (was *"Digest content budgets (chars) enforced by memory_write."*). ✅
   Correct — the budgets now drive auto-truncation of derived (indexer) and
   captured (finish_capture) digests.

3. **`src/config/general.rs:44`** (`GeneralConfig.memory` field doc): now reads
   *"Read live by recall + the indexer from the store's snapshot."* (was
   *"Read live by recall + `memory_write` from the store's snapshot."*). ✅
   Correct.

**Bonus (not in the fix list, correctly done):** the Rust DTO mirror
`src-tauri/src/ipc/settings.rs:963` (`MemorySearchDto.plan_budget` doc) was
updated to the identical *"Digest auto-truncation lengths (chars) for
derived/captured digests."* text, keeping the Rust↔TS DTO docs in sync. ✅

### Project-wide stale-phrase sweep — clean

Searched `src/`, `frontend/`, `README.md`, `PLAN.md` for every stale phrase:
`enforced by memory_write`, `budget-enforced`, `budget_violation`,
`over budget`, `tool-level budget`, `budget bypass`, `recall and memory_write`,
`memory_write read per call`, `memory_write…budget`, plus a broadened pass for
any `budget…(reject|error|enforc|bypass|violation)` / `over-budget` /
`tool-level budget` variant.

- **Specific phrases:** all 28 matches are confined to `.coding/plans/*.md` and
  `.coding/reviews/*.md` (immutable historical records — the only acceptable
  location). **Zero** matches in `src/`, `frontend/`, `README.md`, or `PLAN.md`.
- **Broadened enforcement-language pass (src/ .rs):** 2 matches, neither stale:
  - `src/provider/trace.rs:1314` — *"over-budget payload"* refers to the LLM
    trace log's *memory* budget (`memory_budget_mb`), unrelated to memory_write.
  - `src/tool/memory/mod.rs:1205` — the *new* test comment accurately
    describing the removal ("With the hard budget reject removed…").
- **Broadened pass (frontend/ .ts):** 0 matches.

The documentation is fully synced. No stale description of the removed
`memory_write`/`memory_supersede` budget enforcement remains in any active
source file.

### No new issues introduced

- **No dead code.** `is_knowledge_type` is still computed and used at
  `src/tool/memory/mod.rs:155` (knowledge-backed write routing) after the
  budget-check block was deleted — the variable was correctly retained. The
  `MemorySearchConfig` import was correctly dropped from `src/tool/memory/mod.rs`
  (its only uses — `budget_violation` and the deleted
  `budget_override_from_live_config_is_honored` test — are gone).
  `MemoryRecordType::content_budget()` is retained as the default source for
  `MemorySearchConfig::default()` and the `unwrap_or` fallback at every call
  site.
- **Tests consistent with the new behavior.** Two new regression tests assert
  the post-change semantics and would fail under the old code:
  `db_only_typed_write_accepts_long_content` (DB-only typed write now accepts
  >budget content — old code rejected it) and
  `lowered_config_budget_truncates_derived_digest` (a lowered
  `decision_budget: 100` truncates the derived digest to ≤100 chars while
  preserving the pointer — old code used hardcoded budgets and ignored the
  config). Three obsolete reject-asserting tests were correctly deleted. The
  prompt test `stable_head_carries_memory_records_block` was updated: budget
  markers (`≤500`/`≤300`/`≤600`/`≤400`) replaced by a `compact and pointer-first`
  assertion.
- **Prompt complete.** `MEMORY_RECORDS` now leads with the soft guideline
  ("Keep typed records compact and pointer-first… The derived index
  auto-truncates digests to stay compact.") and drops the per-type budget
  numbers; the pointer-first / hygiene / branch-status paragraphs are intact.
- **Docs synced end-to-end.** `PLAN.md`, `README.md` (two sites), module doc
  comments (`types.rs`, `mod.rs`, `general.rs`, `finish_capture.rs`,
  `indexer.rs`), tool schema descriptions (`memory_write`/`memory_supersede`),
  the system prompt, and both the TS and Rust IPC DTOs all describe
  auto-truncation rather than enforcement. No file still claims `memory_write`
  enforces a budget or that over-budget errors.
- **Multi-platform neutral.** Pure Rust logic + doc-comment edits + a TS DTO
  doc. No Windows-only APIs, paths, or shell syntax; no `cfg(windows)` additions.

### Conclusion

All findings from the prior two reviews are resolved, the three doc-comment
fixes (plus the Rust DTO mirror) are correct, and a comprehensive project-wide
sweep confirms no stale budget-enforcement language remains in active source.
No new issues were introduced. The change is documentation-complete and
behaviorally sound.
