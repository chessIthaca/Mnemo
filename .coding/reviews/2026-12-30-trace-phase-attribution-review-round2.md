## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of the four round-1 fixes in commit 28cd957 (HEAD, clean tree — confirmed via git status/diff, both empty). Three of four fixes are correct and complete (Low 1 stall stamps, Low 3 comments, Low 4 docs — verified against the real code paths, not just diff hunks). The round-1 **Low 2 fix is incomplete**: 2 of the 10 claimed no-chunk connect sites were not actually stamped (openai `stream_stopped`, anthropic `ParseError`), and a third same-class site (anthropic repetition else) that round 1's own list missed remains unstamped — the partial fix also introduced an openai↔anthropic mirror divergence on the ParseError/repetition arms, the exact inconsistency class round-1 Low 1 flagged. All three residual sites are the same one-line fix. Rename completeness, attribution rules, regression tests, and the frontend 9-segment math all re-verified clean.


## Findings

### Low 1 — Round-1 Low 2 fix incomplete: 3 no-chunk branches still never stamp connect_ms

The fix claim stated all 10 listed sites now stamp `log.set_connect_ms(*id, request_start.duration_since(record_created).as_millis() as u32)` before the existing `set_ttft_ms`, each with the "No chunk: connect = record→headers, ttft = headers→error." comment. Verified present and correct at 8 sites — openai.rs: Error-event (1315-1322), consumer-dropped (1364-1369), repetition (1409-1414), ParseError (1480-1485), Err(e) (1523-1528); anthropic.rs: Error-event (1655-1662), consumer-dropped (1691-1696), Err(e) (1754-1759). All 8 use the correct fixed record→headers expression (`request_start.duration_since(record_created)`, the same one the timeout arm uses — NOT `record_created.elapsed()`), stamp connect before ttft, and carry the comment. But three sites were missed:

- **`src/provider/openai.rs:1454-1459`** — the `stream_stopped` no-chunk else still stamps only `set_ttft_ms`. Claimed fixed; round-1 Low 2 explicitly listed it.
- **`src/provider/anthropic.rs:1718-1723`** — the `ParseError` no-chunk else still stamps only `set_ttft_ms`. Claimed fixed; round-1 Low 2 explicitly listed it.
- **`src/provider/anthropic.rs:1537-1542`** — the repetition no-chunk else still stamps only `set_ttft_ms`. NOT in round-1's site list (the finding's own inventory missed it) — same class, same one-line fix.

Impact if ever reached: connect_ms stays None, the record→headers window is unattributed, and the frontend totalMs under-counts — the exact gap round-1 Low 2 described. Mitigating (why low, matching round 1's severity): all three branches are practically unreachable — each fires only from a parsed SSE outcome (Error event, TextDelta for repetition/stream_stopped, ParseError) or an in-loop send of a parsed event, and the `Ok(bytes)` arm sets `first_chunk` before any parsing, so `first_chunk` is always `Some` when these arms run. The genuinely reachable no-chunk paths — the read-timeout arm and the reqwest `Err(e)` arm — are correctly fixed in both providers. However, the partial fix introduced a cross-provider mirror divergence: openai's ParseError and repetition else-branches now stamp connect while anthropic's don't — the same inconsistency class round-1 Low 1 was about.

**Fix:** add the same block to the three else-branches (comment + `log.set_connect_ms(*id, request_start.duration_since(record_created).as_millis() as u32)` before the existing `set_ttft_ms`), mirroring the sibling arms. No test change required (the branches are unreachable by construction; the existing regression tests cover the reachable paths).

## Verified correct (round-2 checks against the real code, not diff hunks)

1. **Round-1 Low 1 fix (stall_ms in mirrored arms)** — present and correct. openai repetition first-chunk branch stamps `set_generation_ms` + `set_stall_ms(*id, stall_tracker.ms())` (1397-1405); anthropic Error-event first-chunk branch stamps both (1642-1650). The previously-correct arms (openai Error-event 1302-1310, anthropic repetition 1531-1536) are unchanged. All four mid-stream error arms now stamp generation + stall.
2. **Round-1 Low 2 fix** — 8 of 10 sites correct (see finding for the 3 misses, one of which was never in round-1's list). Timeout arms (openai 1077-1087, anthropic 1430-1440) already stamped connect+ttft and are unchanged.
3. **Round-1 Low 3 fix (comments)** — openai.rs:957-961 and anthropic.rs:1347-1350 now describe the headers-arrived boundary (connect = record_created→request_start, ttft = request_start→first chunk, generation = first→last) — accurate against the actual stamps. The openai Usage-arm comment (1222-1226) now says "TTFT = headers → first chunk" and correctly explains generation = first→now.
4. **Round-1 Low 4 fix (docs)** — README.md:61 lists the full 9-phase model with windows and the error-attribution sentence now says connect/wait/generation buckets; README.md:62's segment list is the 9 segments; docs/brand.md:152-154 adds backoff orange + stall red and renames send→connect — the new/renamed entries match the actual PHASE_SEGMENTS palette (backoff `bg-orange-400/80`, connect `bg-cyan-500/80`, stall `bg-red-400/80`); docs/why-mnemo-deck.md:856-861 lists the 9 phases and relabels the aggregate send 3m 11s → connect 3m 11s.
5. **Rename completeness re-verified** — `send_ms|sendMs|totalSendMs|set_send_ms`: 101 hits, ALL in `.coding/` historical files (analysis report, spec, provider-errors.jsonl logs); zero in src/, src-tauri/, or frontend/.
6. **Attribution rules consistent** — transport errors (openai 860, 927) and HTTP errors (openai 901, anthropic 1330) stamp `record_created.elapsed()` (correct — pre-headers, the whole window is connect); the Usage/success path stamps connect via `request_start.duration_since(record_created)` + stall; timeout no-chunk = connect+ttft, with-chunks = finalize + stall + generation.
7. **Regression tests all present** — `complete_with_retry_attributes_backoff_sleeps_to_next_attempt` (src/agent/tests.rs:2554), `mid_stream_stall_flips_phase_to_waiting` (tests.rs:1020), `http_error_stamps_connect_bucket_and_parked_backoff` (openai.rs:5909), `transport_failure_stamps_connect_bucket` (openai.rs:5956), `stall_tracker_accumulates_gaps_over_threshold_only` / `stall_tracker_finalize_adds_trailing_gap_before_timeout` / `stall_tracker_no_chunks_measures_nothing` (provider/mod.rs:1081/1103/1114). Frontend traceStats.test.ts covers the 9-segment model: `phaseMsByKey` keys == PHASE_SEGMENTS keys, generate = gen − reason − stall (8400−2000−400 = 6000 asserted), backoff/connect/stall extraction, and the 9-key chartVisibility map.
8. **No new issues from the fixes** — the added code is only stamp calls + comments; indentation/line-wrapping follows the surrounding rustfmt style; no logic changes beyond the stamps; the 401/403 body-suppression behavior is untouched.

## Constitution checks

- **Tests / warning-free build**: the main agent reports cargo test 1933+16 passed (exit=0) and frontend vitest 782 passed + tsc clean after the fixes. I could not re-run the suites myself (read-only reviewer, no shell) — the main agent must re-run both after fixing the round-2 finding.
- **Doc comments**: the fixes add no new public symbols; round-1's doc-comment verification stands.
- **Multi-platform neutrality**: the fixes stamp u32 timings only; no platform-specific code.
- **Documentation sync**: PASSES now (round-1 Low 4 fixed correctly).
- **Line endings**: no CRLF/LF mixing introduced.

## Observations (no action required)

- The ParseError / consumer-dropped / stream_stopped first-chunk branches stamp generation_ms without stall_ms, consistently in both providers — pre-existing and outside round-1 Low 1's stated scope (the Error-event + repetition pairs). Same-class residual if the "mid-stream errors → generation + stall" model is ever extended to those arms.
- openai.rs:1224's comment says "`last_chunk` is always `now` here" in backticks though no variable named `last_chunk` exists — conceptually correct (the chunk being processed arrived at `now`); trivial wording nit.
- brand.md's "generate magenta" for `bg-violet-500` and "prep green" for `bg-teal-400` are loose hue names — pre-existing wording, unchanged by this commit.
