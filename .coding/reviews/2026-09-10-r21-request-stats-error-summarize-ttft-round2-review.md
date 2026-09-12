## Verdict: PASS

Round-2 verification of plan a3a04af8 / backlog bd9df146 (R21) at HEAD 2416dda on wt/agenticcoding. Tree confirmed clean (`git diff HEAD` and `git status --short` both empty), so the reviewed state is exactly the committed code. All six round-1 findings are fixed correctly, the round-1 core implementation still holds, and the project constitution checks pass. No new findings; two non-blocking informational notes at the end.

### Fix verification (all six round-1 findings)

**1. HIGH — extraction script NULL-crash + tag blindness: FIXED**
`.coding/analysis/cache-hit-5-extract.py`:
- The SELECT gains `outcome, purpose` as columns 9/10 (lines 38-41).
- NULL numerics are normalized immediately after the fetch — `r[5] or 0, r[6] or 0, r[7] or 0` (lines 47-48) — so the downstream r[5]/r[6]/r[7] arithmetic can no longer TypeError on the new NULL rows.
- Rows are segmented (lines 67-69): `error_rows` (r[9]=='error'), `summarize_rows` (r[10]=='summarize'), `main_rows` (both tags None); `rows` is rebound to `main_rows` at line 71 — before `hit_of`, the R19 `REPORTING` tier computation (line 83), and the A0-A5 aggregates, so the hit/reset stats run on main-loop rows only.
- A new A6 section (lines 222-235) reports error and summarize rows per model (count + mean estimated prompt / actual prompt+completion tokens).
- The deliberate summarize-exclusion is documented in the script itself (lines 61-66 and the A6 header 222-226): a compaction's request is a fresh single-message prefix, structurally unable to hit cache, so including it would drag the conversation hit% down for a structural reason.
- `import statistics` is present (line 9); A6's `statistics.mean` calls run over non-empty lists by construction (each model comes from the tagged row set).
- The R19 decision record (`.coding/knowledge/decision/2027-01-07-r19-cache-tier-split-is-script-side-schema-flag.md`) carries the dated amendment paragraph noting the stale NULL-safety premise and the new segmentation.

**2. LOW — non-atomic table-rebuild migration: FIXED**
`src/memory/schema.rs` 241-287: the swap batch (DROP IF EXISTS request_stats_new → CREATE → INSERT…SELECT → DROP request_stats → RENAME) is wrapped in `BEGIN IMMEDIATE; … COMMIT;` (lines 251/276), with the comment (242-248) explaining that `execute_batch` does not wrap statements in a transaction of its own and that an uncommitted transaction rolls back on close. Index recreation stays outside the transaction (279-286). Correct — SQLite DDL is transactional. The migration remains gated on `outcome` column presence (229-240, idempotent) and ordered after the endpoint migration (217, so copied rows carry the endpoint); the migration test (579-636) re-verifies row survival, NULL insert, tag filtering, and idempotent re-apply.

**3. LOW — eager estimate_prompt_tokens per request: FIXED (documented accepted cost)**
`src/agent/turn.rs` 2384-2396: the comment documents the accepted O(history) walk on every request (~1-3ms + one String alloc per message on very long conversations, not covered by the L3 serialized-prefix cache) and why the lazy closure alternative is impossible — it would hold a shared borrow of `messages` across the caller's scaffolding pops, which need a unique borrow. Matches the round-1 agreed resolution.

**4. LOW — dispatch.rs error-row site untested: FIXED**
`src/agent/tests.rs` 3157-3241 `complete_with_retry_records_error_stats_per_attempt`: FlakyProvider (fail_times=2, extended with a `success_usage` field whose `complete` pushes a Usage event before Finish on the success attempt) driven through `run_turn` with a real in-memory MemoryStore and a session id. Asserts exactly 3 rows: 2 error rows (outcome='error', cached_tokens None, completion 0, prompt_tokens > 0, purpose None, model "mock-flaky") + 1 success row carrying the Usage event's values (900/40/Some(0)/Some(120)). The test fails without the fix (only the success row would exist) — a genuine regression pin exercising the changed path end-to-end.

**5. LOW — summarizer-side error/interrupt requests unrecorded: FIXED**
- `src/agent/context.rs`: the mid-stream `LlmEvent::Error` arm returns `Err(Error::Provider(error))` (387-395) instead of `break` — a truncated summary can no longer silently replace the conversation; the interrupt path returns the captured usage (line 432) so an abandoned-but-billed summary still lands its row.
- All three callers record a purpose='summarize' error row with the full-conversation estimate on Err: turn.rs `handle_pending_swap` (940-956), turn.rs `maybe_compact` (1761-1777), runtime/agent.rs `compact_context` (1045-1061). The success paths record the forwarded usage rows (turn.rs 964-979 and 1807-1822; runtime/agent.rs 1070+).

**6. LOW — Usage-then-Error stream double-count: FIXED**
`src/agent/turn.rs` consume_stream: `usage_recorded` is declared at line 2517 (with the round-1 LOW 6 rationale in the comment), set in the Usage arm after the success row lands (2656), and the Error arm's record is guarded by `if !had_error && !usage_recorded` (2700) — a stream that reports usage and then dies records exactly one success row, never both.

### Round-1 core implementation — still holds

- **Schema:** the fresh CREATE TABLE request_stats has nullable `cached_tokens` + `outcome`/`purpose` (schema.rs 101-115, with the updated table comment 91-100 documenting the NULL-vs-0 semantics); the rebuild migration is idempotent (gated on `outcome` presence; the test re-applies `apply_schema`) and ordered after the endpoint migration.
- **Aggregations NULL-safe:** session_stats (mod.rs 2151-2172), project_stats per-model (2204-2226) and per-day (2242-2256) all read `SUM(cached_tokens)` as `Option<i64>.unwrap_or(0)` and guard ttft/generation with `CASE WHEN … IS NOT NULL`.
- **StatsRow/record_stats_row refactor:** record_stats_row (turn.rs ~2262-2316) stamps id/session/model/endpoint/created_at and fire-and-forget spawns the write with an error log on failure; every recording site (dispatch.rs 818, turn.rs ×5, runtime/agent.rs ×2) goes through it.
- **Estimate wiring:** request_stream computes the scaffolding-inclusive estimate and returns `(stream, estimated_prompt_tokens)` (2395-2396; doc comment 2318-2328); run_turn destructures it (499) and passes it to consume_stream (511-523); the Error arm uses it (2705).
- **Store layer:** record_request_stats is a 13-column INSERT (mod.rs 2059-2089); request_stats_rows reads all 13 back with Option handling (2091-2128); RequestStats (types.rs 514-556) documents the None-vs-Some(0) semantics on every tagged field. The store-level test `request_stats_rows_separates_outcome_and_purpose_tags` (memory/tests.rs 1998-2113) pins the separation.

### Constitution checks

- **Documentation sync:** schema table comment, RequestStats field docs, record_stats_row/request_stream doc comments, and the R19 decision-record amendment are all updated. No README/PLAN.md change is needed (round-1 established the stats schema is internal; nothing here alters that).
- **Multi-platform neutrality:** all changes are portable (SystemTime/UNIX_EPOCH, tokio, rusqlite transactions; the Python script uses relative paths + a sqlite3 read-only URI). No Windows-only APIs.
- **File-tools-first:** no shell-based file mutation anywhere in the commit.
- **No #[allow]:** zero `#[allow(…)]` attributes in src/ (searched; the only matches were string literals/identifiers like "allow-list").

### Test run

This read-only reviewer has no shell tool and cannot execute cargo test. The parent reports `cargo test --workspace` green (2271 lib + 16 integration + 293 + 4 + 2, zero warnings under deny(warnings)); that claim is inspection-consistent — the new tests exist, assert the claimed behavior, and no type mismatches or dead code were spotted — but it was not independently re-executed here.

### Informational notes (non-blocking, no action required)

1. Script line 57: `models` is computed from ALL rows before the `rows = main_rows` rebinding, so a model with only error/summarize rows would appear in the A1 per-model table with n=0. All guards handle empty lists (`if prompts else 0`, `pct` with b=0, `pctl` on empty) — cosmetic only, no crash.
2. consume_stream records one success row per Usage event; a provider emitting two Usage events in one stream would land two rows. Pre-existing round-1 design (providers emit one usage chunk), unchanged by these fixes — noted for completeness only.
