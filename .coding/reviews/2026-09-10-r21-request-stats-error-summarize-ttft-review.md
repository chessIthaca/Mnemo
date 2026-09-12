## Verdict: FINDINGS (1 high, 5 low)

Review of plan a3a04af8 / backlog bd9df146 (R21) — all uncommitted changes on wt/agenticcoding (10 source files + .coding bookkeeping). The core implementation is correct and well-tested: the schema rebuild is idempotent and correctly ordered after the endpoint migration, the aggregation queries are verified NULL-safe, the StatsRow refactor preserves successful-stream behavior exactly, and the recording sites are right. The one HIGH: the R19 extraction script (.coding/analysis/cache-hit-5-extract.py) deterministically crashes on the new NULL cached_tokens rows — the item's own instruction 4 ("the aggregates extraction can separate outcomes") is unmet. Five LOW findings follow.

### 1. HIGH — the R19 extraction script crashes on the new NULL `cached_tokens` rows (unmet acceptance criterion)

- **Location:** `.coding/analysis/cache-hit-5-extract.py` — line 56 (`hit_of`: `r[5] / r[2]`), line 65 (`REPORTING = {m ... if any(r[5] > 0 ...)}`), line 77 (`sum(r[5] for r in rs)`), line 107 (`r[6] > 0`); the SELECT at lines 38-41 reads `cached_tokens` (r[5]) and `ttft_ms` (r[6]) from `request_stats`.
- **Symptom:** R21 writes error rows with `cached_tokens` NULL (and `ttft_ms` NULL). In Python 3, `None > 0`, `None / int`, and `sum()` over `None` all raise `TypeError`. The first extraction run over a DB containing even one error row dies with a traceback before emitting any aggregate.
- **Impact:** the standing cache-analysis tool — R19's deliverable, referenced by the decision memory `2027-01-07-r19-cache-tier-split-is-script-side-schema-flag` and the executive summary — is broken by this change. The R19 review explicitly relied on "cached_tokens is INTEGER NOT NULL (src/memory/schema.rs:106), so the `> 0` test is NULL-safe"; R21 invalidates that premise without touching the script. Backlog instruction 4 ("the aggregates extraction (.coding/analysis/cache-hit-5-extract.py pattern) can separate outcomes") is unmet: the extraction cannot even run, let alone separate outcomes.
- **Fix:** NULL-guard the cached/ttft reads (`(r[5] or 0)`, `(r[6] or 0)`), add `outcome, purpose` to the SELECT, and segment: cache-hit/reset aggregates should filter `outcome IS NULL` (error rows carry no cache signal — including them drags hit% down and inflates reset counts); emit a separate error-row section (count, estimated prompt tokens by model). Decide explicitly whether `purpose='summarize'` rows stay in the hit aggregates (they are real cache rows — a fresh single-message prefix, guaranteed miss) or get their own section, and document the choice. Once fixed, amend the R19 decision record's stale NULL-safety premise (`memory_amend`).

### 2. LOW — the table-rebuild migration is not atomic (crash window silently loses all request_stats)

- **Location:** `src/memory/schema.rs` — the R21 rebuild block (`DROP TABLE IF EXISTS request_stats_new; CREATE ...; INSERT ... SELECT ...; DROP TABLE request_stats; ALTER TABLE ... RENAME`), all inside one `execute_batch`.
- **Symptom:** rusqlite's `execute_batch` does not wrap statements in a transaction — each autocommits individually. A crash/power-loss between `DROP TABLE request_stats` and `ALTER TABLE ... RENAME` leaves the DB with only `request_stats_new` (holding all the data). On the next open, `apply_schema`'s `CREATE TABLE IF NOT EXISTS request_stats` creates a fresh EMPTY new-shape table → the `outcome` gate sees the new shape → the rebuild (and its `DROP TABLE IF EXISTS request_stats_new` cleanup) never runs → all historical rows are orphaned in `request_stats_new` and silently lost.
- **Impact:** analytics data only, and the window is microseconds — but the fix is one line.
- **Fix:** wrap the rebuild in an explicit transaction (`BEGIN IMMEDIATE; ... COMMIT;` in the same batch string, or `conn.execute_batch("BEGIN")` … commit after) — SQLite DDL is transactional, making the swap atomic. The index-recreation batch can stay outside.

### 3. LOW — the unconditional O(history) token estimate per request re-introduces the L3 cost class

- **Location:** `src/agent/turn.rs` `request_stream` (~:2336-2341): `estimate_prompt_tokens(messages, tool_schemas)` computed before `complete_with_retry` on every request, success or failure.
- **Symptom:** the estimate is only consumed by the mid-stream Error arm, but it is computed always. `estimate_prompt_tokens` walks every message via `message_estimate_chars` → `as_text()`, which **returns `String` by value** (a clone/join per message per call — `src/provider/mod.rs:598-610`), and `tools_chars` serializes every tool schema's parameters JSON (`t.parameters.to_string()`) — the exact per-iteration O(history) walk that L3 (backlog 0f3ad0f5, marked done in this same diff) just eliminated for the request builder via the serialized-prefix cache ("prefix_chars cached, tail chars fresh, tools_chars fresh — walk skipped on hits").
- **Impact:** ~1-3ms plus N String allocations per request on 503-message conversations — small in absolute terms, but it partially regresses the just-shipped L3 win on every successful request, which is the hot path L3 exists to protect.
- **Fix options:** compute lazily (only when a stream actually errors — e.g. capture the estimate behind a closure/`OnceCell` taken before the scaffolding pop, or record the error row in `run_turn` where `messages` is still in scope), or reuse the builder's split estimate (cached prefix chars + fresh tail). If the precompute stays, document the accepted cost in the comment.

### 4. LOW — the dispatch.rs error-row site (the motivating trace-255 path) has no test coverage

- **Location:** `src/agent/dispatch.rs` `complete_with_retry` Err arm (the new `record_stats_row` call); tests in `src/agent/tests.rs`.
- **Symptom:** `run_turn_records_error_request_stats` uses `MockProvider::single(vec![LlmEvent::Error { .. }])` — `complete()` succeeds and the *stream* errors, exercising only the `consume_stream` Error arm. The `complete()` → `Err` path (a request that died before any SSE chunk — exactly the trace-id-255 BadGateway example that motivated the item, and the per-attempt recording across retries) is untested: the existing `complete_with_retry_*` tests pass `session_id: None` and assert no stats rows.
- **Fix:** add a test with a provider whose `complete()` fails (the retry-mock pattern from `complete_with_retry_emits_one_retrying_error_per_failure`) plus a memory store, asserting one error row per failed attempt (e.g. 2 failures + 1 success → 2 error rows + 1 success row), `cached_tokens` NULL, `outcome='error'`, estimated prompt tokens > 0.

### 5. LOW — summarizer-side error/interrupt requests still record no row (scope gap vs the backlog text)

- **Location:** `src/agent/context.rs:351` (`provider.complete(...).await?` — an Err propagates, no row), `:387` (`LlmEvent::Error { .. } => break` — no row), `:419-422` (an interrupt arriving after the Usage event returns `usage=None` — the captured usage is discarded).
- **Symptom:** a failed compaction request (the summarizer's own BadGateway/reset — the same error class that motivated R21) leaves no row; an interrupt in the small window after the summarizer's Usage event drops a real billed usage.
- **Impact:** error coverage is main-loop-only. Backlog instruction 1 is unqualified ("Error requests: write a request_stats row on the error path"); the plan narrowed scope to dispatch.rs + consume_stream. Not a bug in what was built — but the mega-prompt's *failure* mode stays invisible, which is half the original blind spot.
- **Fix (follow-up-sized):** record an error row (`purpose='summarize'`, `outcome='error'`) on the summarizer's Err/Error paths, and return the captured usage even on the interrupt path (or record it before returning).

### 6. LOW — Usage-then-Error on one stream double-counts the request

- **Location:** `src/agent/turn.rs` `consume_stream` — the Usage arm records a success row; the Error arm's `if !had_error` guard records an error row. Nothing prevents both for the same stream.
- **Symptom:** if the connection dies after the usage chunk but before the clean stream end (the SSE pump then emits Error), one request produces both a success row and an error row — counted twice in the aggregates (once as success, once as error).
- **Impact:** rare (the window is one chunk), but it skews exactly the error-rate metric this feature adds.
- **Fix:** track whether usage was recorded this stream (a local bool beside `had_error`) and skip the error row (or tag it distinctly) when usage already landed.

## Constitution checks

- **Documentation sync — no update required, with one stale-doc exception (finding 1).** README.md does not itemize the Stats view or the request_stats schema (established at plan 07d2dc07: "README does not itemize the Stats view (nothing goes stale)"), and PLAN.md carries no request_stats schema contract. The module docs that DO describe the table are all updated in this change: the schema.rs table comment (outcome/purpose/NULL-cached semantics), the types.rs field docs, and `record_stats_row`'s doc comment. The one stale doc is the R19 decision record's "cached_tokens is INTEGER NOT NULL → NULL-safe" premise — folded into finding 1's fix. `.coding/llm-trace.md` describes the trace view's cached_tokens, not request_stats — unaffected.
- **Multi-platform neutrality — clean.** SystemTime/UNIX_EPOCH, `eprintln!`, `tokio::spawn`, rusqlite, SQLite PRAGMA — all portable. No Windows-only APIs, paths, or shell syntax anywhere in the diff.
- **File-tools-first policy — clean.** No shell-based file mutation (`Set-Content`/`sed -i`/python scripts) in the change; everything is ordinary Rust source edits.
- **No `#[allow]` — clean.** None present; the parent reports `cargo test --workspace` green under `deny(warnings)` (2270 lib + 16 integration + 293 + 4 + 2), which proves zero warnings. I could not run tests myself (read-only reviewer); correctness below is verified by inspection.
- **Security — clean.** All new SQL is static DDL or parameterized (`params![]`); `request_stats_rows` binds `session_id`; the migration contains no user input. No injection surface, no secrets.

## Verified-correct highlights (what I checked beyond the findings)

- **Migration:** fresh-DB path creates the new shape directly (the `outcome` gate then skips the rebuild); legacy path rebuilds AFTER the endpoint migration so the copy carries `endpoint`; idempotent on re-run; the migration test covers row survival, the NULL insert, and idempotency; the 3 indexes are recreated after the swap; `PRAGMA table_info` column-1 read is correct.
- **Aggregates:** `session_stats` / `project_stats` / per-day / `session_list` all read `SUM(cached_tokens)` as `Option<i64>.unwrap_or(0)` — NULL-safe (mod.rs:2168/2221/2254); `COUNT(*)` now includes error/summarize rows, which is the item's intended visibility.
- **Plumbing:** `complete_with_retry` has exactly one production caller (`request_stream` — graph-verified); the `session_id` param is threaded from the turn's authoritative id everywhere, and the manual-compact path (runtime/agent.rs) uses the AgentTask's own session id. The once-per-stream guard (`if !had_error`) correctly handles providers emitting multiple Error events. The scaffolding-inclusive estimate basis matches the builder's cap basis (messages + tool schemas as sent).
- **Serde/IPC:** `RequestStats` does not cross IPC (no tauri command returns raw rows; `request_stats_rows` is store-internal/test-only), so the `cached_tokens: Option<u32>` change cannot break the frontend — `ModelBreakdown.cached_tokens` is a SUM (u64), unaffected. `outcome`/`purpose` use `skip_serializing_if` like `endpoint`.
- **Tests:** the summarize test's token math is sound (~80K BPE tokens > the 64K threshold at fill 0.5, under the 96K ceiling so keep_recent stays 6; 9 messages > keep_recent+1 so `summarize_with_interrupt` doesn't early-return); the memory-store test pins the NULL-vs-Some(0) distinction and tag separation.

## Notes (no action required)

- **In-app Stats view semantics:** error rows now blend into session/project totals — prompt-token totals include our *estimate* for failed requests, and the cost estimate (uncached = prompt − cached) bills error-row prompts at full price. For mid-stream deaths that is correct (the provider billed the prompt); for pre-send failures (auth/DNS) it overestimates. Intended per the item's goal ("visible in the cache/latency aggregates") — flagging so it is a known semantic, and it is another reason the extraction (finding 1) should segment by outcome.
- **Backlog bookkeeping riding in this diff** (R21 → in_flight, L3 0f3ad0f5 → done, qwen-3.6 35ade189 → deleted_at) is app-managed state from prior/companion work — out of scope, no issue.
- The 100ms sleeps in the two new agent tests (fire-and-forget spawn) follow the existing `run_turn_records_request_stats_on_usage` pattern — acceptable, though they are the usual mild flake surface under CI load.

**Summary for the parent:** fix finding 1 before merge (the extraction script NULL guards + outcome segmentation — it is the item's own acceptance criterion); findings 2-6 are LOW and can be fixed in this round or queued as follow-ups (2 and 6 are one-liners; 3 is a perf nicety; 4-5 are coverage/scope gaps). Re-run `cargo test --workspace` after any fix; no doc updates beyond finding 1's decision-record amendment are needed.
