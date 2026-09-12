## Verdict: PASS

Round-2 re-review of plan 7b77cf0c / backlog b804012f (auto-delegate symbol and memory searches inside `search`/`search_read`) on `wt/agenticcoding`. Scope: commit 6cf10d5 (`git show --stat`) plus the current tree — all round-1 fixes are committed. Round-1 report: `.coding/reviews/2026-12-28-auto-delegate-search-review.md` (FINDINGS, 1 high, 3 low).

**Summary.** All four round-1 findings are genuinely fixed, each with the prescribed mechanism and a regression test. The fixes introduced no new bugs: the Mutex state write moves the key exactly once and only when a delegation fires, and the steering reorder cannot affect any pre-existing C3/C5 test (none of their fixtures carry the `AUTO-DELEGATED` first line). Docs match the fixed behavior; the change is multi-platform neutral.

### H1 (high) — escape key now recorded on BOTH delegation arms — FIXED

- `src/tool/agent/search.rs:1218–1228`: `*delegation_state.lock().unwrap() = Some(key);` is the **first statement** inside `if let Some(block) = delegated` — above the fast-path return (`memory_delegated || args.glob.is_none()`) and above `prepended = Some(block)`. Comment cites review 2026-12-28 H1.
- `src/tool/agent/search_read.rs:218–228`: identical shape, same comment.
- Tests: `glob_narrowed_delegation_escapes_on_the_repeat` in `search.rs:2768–2797` and the twin in `search_read.rs:936–966`. Both pin: first call prepends `AUTO-DELEGATED to the code graph` with results riding below (`a.rs:1` / `=== a.rs`); the immediate repeat is a plain glob-narrowed search with **no** `AUTO-DELEGATED` and results present.

**Mutex state-write ordering (requested check) — sound.** In both closures: `key` is constructed before `spawn_blocking` (`search.rs:1157`, `search_read.rs:172`), cloned for the escape comparison (`:1158` / `:173`), then moved into the closure and written exactly once at `:1223` / `:223` — inside the `if let Some(block) = delegated` guard, so the write fires **only when a delegation actually happened**. On the escape path both blocks are `None`, `delegated` is `None`, and the state is not rewritten — the key persists, keeping the escape sticky (third identical call still escapes, as documented). No use of `key` after the move (the crate builds warning-free under `#![deny(warnings)]`, so a use-after-move or unused binding cannot compile); the lock is a `std::sync::Mutex` inside `spawn_blocking` with no `.await` in the critical section.

### L1 (low) — auto-switch now precedes the C3 escalation loop — FIXED

- `src/agent/steering_stats.rs:447–467`: the delegated-answer auto-switch block (SearchNudge detected **and** first line contains `AUTO-DELEGATED` → switch counted) now runs **before** the C3 escalation loop (`:468–499`). Comment cites review 2026-12-28 L1.
- Regression assertion in `delegated_answer_registers_as_a_switch_and_never_gates` (`:819–834`): one ignored advisory nudge (fired=1, below `ESCALATION_THRESHOLD`=2 at `:314`) followed by a delegated answer (fired=2, but the auto-switch sets switched=1 **before** the C3 check, so `fired >= 2 && switched == 0` is false) → `take_escalations(1).is_empty()`. Exactly the round-1 failure sequence, now asserted empty.

**Reorder safety (requested check) — no existing test can regress.** The auto-switch triggers only when the output's first line contains `AUTO-DELEGATED` (`:456–461`). I checked every pre-existing C3/C5 fixture in the file (`:718`, `:749`, `:765`, `:781`, `:794`, `:915`, `:932`, `:1015`, `:1024`, `:1097`, `:1115`, `:1132`, `:1156`, `:1173`) — all are advisory outputs (`'hello' is an indexed symbol — graph_context…`, TIPs, consolidation notes) with no `AUTO-DELEGATED` first line, so the inserted block is a no-op for them. The only test using an `AUTO-DELEGATED` fixture is the new one. Metric coherence also holds: memory delegations carry no `is an indexed symbol` marker, so they are neither fired nor switched (documented design — KnownMemoryHit stays uuid-only).

### L2 (low) — edge_summary "+N more" uses the true kind-total when capped — FIXED

- `src/tool/agent/codegraph.rs:162–196`: when `rows.len() >= CONTEXT_GROUP_CAP` (imported at `:38` from `crate::codegraph::query`), the remainder is `ctx.outgoing_total`/`ctx.incoming_total` (the true kind-total) `.saturating_sub(3)`; otherwise `rows.len().saturating_sub(3)` as before. The comment honestly documents the residual over-count when other edge kinds exist — which still steers to the 360° view. Matches the round-1 prescribed fix exactly.

### L3 (low) — coverage gaps closed — FIXED

- (a) `search_read`'s memory test `memory_targeted_query_delegates_and_skips_the_read` (`search_read.rs:881–933`) now includes the escape repeat (`:922–932`): repeating the same memory query runs the plain search+read with no `AUTO-DELEGATED` block.
- (b) `a_different_delegating_query_replaces_the_escape_key` (`search.rs:2800–2832`) pins the full stickiness contract: delegate A → delegate B → repeat A delegates **again** (key replaced, not cleared) → B re-delegates → B's own immediate repeat escapes.
- Informational (not a finding): (b) has no `search_read` twin. The wiring is line-for-line identical in both files (shared `search::DelegatedKey`, same state pattern), and `search_read`'s escape path is already covered by its glob twin + the memory escape repeat, so the residual risk is copy-paste drift only. If a future change touches `search_read.rs`'s delegation block, add the twin then.

### Additional checks

- **Docs sync.** `README.md:53` describes the full contract including the escape hatch ("re-issuing the SAME query (pattern+glob+literal) skips delegation … sticky until a different query delegates; a delegated answer counts as a steering switch, so the C5 intercept gate never punishes it") — now true of the code on both arms (H1's point was that the docs already promised this). Module docs (`search.rs:14–32`, `search_read.rs:12–17`), schema descriptions (`search.rs:1108–1114`), and the knowledge-file amendments (DECISION 2026-12-28 sticky-escape record, the 2026-08-29/2026-09-01 spec amendments, the two HOW records) all match the implemented behavior. The frameless-tool-cards knowledge file flagged in round 1 rides this commit.
- **Multi-platform neutrality.** No `cfg(windows)`, no Windows-only APIs, paths, or shell syntax in any changed file; tests use `tempdir` + forward-slash globs; path rendering normalizes `\` → `/`. Clean.
- **Tests.** I have no shell in this review surface, so I could not re-run `cargo test`; the reported run (1911 passed, 0 failed, warning-free under `#![deny(warnings)]`) is consistent with my code-level verification — every new assertion matches the fixed behavior, and no pre-existing fixture is affected by the reorder (checked above).

**Conclusion.** All round-1 findings are fixed as prescribed, with regression tests; no new issues introduced. Ready to land.
