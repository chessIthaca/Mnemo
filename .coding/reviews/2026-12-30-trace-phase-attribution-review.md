## Verdict: FINDINGS (0 high, 4 low)

Review of ALL uncommitted changes on `wt/agenticcoding` (git diff HEAD + untracked) for plan 6e734ccb "Trace graphs: split waiting out of send (connect/backoff/stall)". The core attribution model is implemented correctly and consistently across backend + frontend: the rename is complete, the phase boundaries match the spec, the stall timer and StallTracker math are right, and the regression tests exercise the changed paths. Four low findings: a mirrored stall-stamping inconsistency between the two providers' mid-stream error arms, missing `connect_ms` on the no-chunk branches of the in-stream terminal paths, three stale inline comments contradicting the new ttft/connect semantics, and stale user-facing docs (README.md + docs/) — the task brief's claim that README.md contains no phase-chart mentions is incorrect.

## Findings

### Low 1 — Mid-stream error arms: `stall_ms` stamped in one arm per provider, missed in the mirrored arm

The attribution model says mid-stream errors → generation + stall. Each provider got exactly one of the two mid-stream error arms; the sibling was missed:

- `src/provider/openai.rs` — the SSE **Error-event arm** stamps it (set_generation_ms + set_stall_ms, ~line 1301-1309), but the **repetition-detection arm** (~line 1376-1388) stamps only `set_generation_ms` — no `set_stall_ms`.
- `src/provider/anthropic.rs` — the **repetition arm** stamps it (line 1534), but the **Error-event arm** (lines 1638-1654) stamps only `set_generation_ms`.

Result: on the missed arm, byte-silence gaps stay lumped into `generation_ms` — the exact misattribution class this plan fixes, on a secondary error path. Fix: add the one-line `log.set_stall_ms(*id, stall_tracker.ms());` next to the existing `set_generation_ms` in openai's repetition arm and anthropic's Error-event arm (mirroring the arms that already do it).

### Low 2 — In-stream terminal paths never stamp `connect_ms` on their no-chunk branches

Transport errors, HTTP errors, and the read-timeout path all correctly stamp connect now. But the in-stream terminal arms — SSE Error event, repetition detection, consumer-dropped, ParseError, reqwest body error (both providers), and openai's stream_stopped — stamp `ttft_ms` (headers→error) when no chunk ever arrived while leaving `connect_ms` None:

- openai.rs: Error-event else-branch (~1310-1317), consumer-dropped (~1349-1353), repetition else (~1382-1386), stream_stopped else (~1422-1426), ParseError else (~1445-1449), Err(e) else (~1481-1482)
- anthropic.rs: Error-event (~1645-1650), consumer-dropped (~1671-1675), ParseError (~1693-1697), Err(e) (~1727-1728)

On those records the record→headers window is unattributed and the frontend `totalMs` under-counts (connect contributes 0). Nothing is misattributed — a window is simply missing — and these are rarer paths than the ones that were fixed, hence low. Fix: stamp `log.set_connect_ms(*id, request_start.duration_since(record_created).as_millis() as u32)` alongside the `set_ttft_ms` in each no-chunk branch (same expression the timeout path uses), or consciously accept and document the gap.

### Low 3 — Stale inline comments contradicting the new semantics at the anchor points

The plan fixed the wrong "POST → first chunk" TTFT description in the public docs, but three inline copies of the old semantics remain at the exact anchor variables:

- `src/provider/openai.rs:957-960` — "Time the request was sent (POST issued) — the anchor for TTFT … Captured here … so it covers the full POST→first-chunk latency including connection setup." `request_start` is captured *after* the response headers arrive; it is now the connect/ttft boundary (connect = record_created→request_start, ttft = request_start→first chunk).
- `src/provider/anthropic.rs:1347-1348` — same stale text.
- `src/provider/openai.rs:1221-1222` — "TTFT = POST → first chunk" in the Usage-event arm; it is headers → first chunk now.

Fix: reword to match the new semantics (e.g. "Headers-arrived anchor: connect ends here, ttft/generation start here").

### Low 4 — Documentation sync: README.md and docs/ still describe the old chart

The task brief stated README.md/PLAN.md "contain no phase-chart mentions". PLAN.md is clean, but README.md is not, and two docs/ files carry the old segment list:

- `README.md:61` — "…compact (auto-compaction call), **send**, wait (TTFT), reasoning … error records (connect timeout, stream stall, HTTP error) carry their elapsed time in the **wait/generation buckets** too" — the segment is now connect (+ new backoff/stall), and the error-attribution sentence describes exactly the misattribution this plan fixed (errors now stamp connect).
- `README.md:62` — "stacked phase times (prep/compact/**send**/wait/reason/generate/tools)" — should be prep/compact/backoff/connect/wait/reason/generate/stall/tools.
- `docs/brand.md:152` — "prep green, compact red, **send cyan**" — the cyan segment is now `connect`; backoff (amber) and stall (violet) are new legend entries.
- `docs/why-mnemo-deck.md:856, 860` — "prep, compact, **send**, TTFT …" and the example line "send **3m 11s** …" — same staleness.

Per the project constitution (documentation sync is a review check; a feature that ships with stale docs is an incomplete change), update these to the 9-segment model and the new error-path story. `.coding/llm-trace.md` was checked and is generic enough to remain accurate.

## Verified correct (checked against the real code paths)

1. **Rename completeness** — `send_ms`/`sendMs`/`totalSendMs`/`set_send_ms`: zero matches in src/, src-tauri/, or frontend/ (all 101 hits are `.coding/` historical files: logs, analysis reports, and the spec's own rename note). Backend fields, `set_usage` param, `From` mapping, frontend types, chart segments, summary strip, chips, and all tests are consistently renamed.
2. **`record_created` anchor move** (both clients) — captured *after* the `spawn_blocking` `log.start()` (so "record creation (just above)" is accurate and the prep sliver = entry_at→record_created genuinely includes body build + trace-log start) and *before* the POST. `connect_ms = request_start − record_created` (POST in flight → headers). prep ∪ connect covers entry→headers with no invisible gap; the stamp block's µs of local work landing inside connect is negligible. The prep sliver is added only inside `if let Some(ms) = parked_prep` — matches the documented model (retry attempts after the first correctly get no prep).
3. **backoff_ms** — parked in `complete_with_retry` right after each sleep, before `delay_ms *= 2` (so 1000/2000, matching the test); the `if attempt < 2` guard means every park is followed by a `complete()` call that consumes it — no lingering-park edge even when all attempts fail. `SwappableProvider` is a plain slot (no `LlmClient` impl), so `record_backoff_ms` dispatches on the real client — no forwarding gap. Default no-op in the trait; both clients park + stamp on the next record.
4. **connect/ttft/stall on success + primary error paths** (both providers) — transport error and HTTP error (including both openai reasoning_effort-fallback retry error arms) stamp `set_connect_ms(record_created.elapsed())`; the fallback's success-path connect correctly includes the first failed round trip (documented). Stream timeout with no chunk stamps connect (record→headers) + ttft (headers→timeout); with chunks it finalizes the tracker, stamps stall + generation. Usage event stamps `set_usage(..., Some(connect_ms))` + `set_stall_ms` — and since the usage chunk is itself a byte arrival (`on_chunk` runs before parsing), the trailing gap before it is counted and lies inside generation_ms (stall ⊂ generation holds).
5. **StallTracker** — threshold strictly `>`, gap math and `finalize` (trailing gap, no last_chunk update) correct; no-chunk finalize is a no-op; three unit tests cover exactly these. Doc comments present on the struct and all methods.
6. **turn.rs stall timer** — `tokio::pin!` + `Sleep::reset` via `as_mut()` is the correct API usage; re-armed on every stream event (before `acc.feed`, so processing time can't eat the window); the cmd arm deliberately does not re-arm (documented); on fire it emits Waiting only when `stream_phase.take()` was Some (no spurious duplicate Waiting during the pre-first-chunk window, where the phase is already Waiting) and re-arms afterwards. The paused-clock test's expected `[Sending, Waiting, Streaming, Waiting, Streaming]` matches the actual emission sequence (Waiting is emitted pre-request at turn.rs:896-903).
7. **Frontend** — 9 disjoint segments; `answerMs = max(0, gen − reason − stall)`; `totalMs = (prep−compact) + compact + backoff + connect + wait + gen(inclusive) + tools` (test: 10177ms = "10.2s"); PhaseTimes/UsageCard chips p c b n w r g s t; `genShown` clamping is a strict superset of the old behavior; chartVisibility derives from the new PHASE_SEGMENTS keys.
8. **Tests** — all five named regressions exist and exercise the changed paths: `complete_with_retry_attributes_backoff_sleeps_to_next_attempt` (paused clock, asserts parks [1000, 2000]), `mid_stream_stall_flips_phase_to_waiting` (11s stream silence vs the 10s timer, paused clock), `http_error_stamps_connect_bucket_and_parked_backoff` (StubServer 500; connect Some, ttft None, backoff 1500), `transport_failure_stamps_connect_bucket` (127.0.0.1:1 — loopback connection-refused is immediate on both Windows and macOS, no firewall prompt for loopback traffic, so not flaky or slow), `stall_tracker_*` ×3, plus the updated frontend segment-math tests.
9. **Security** — the new code stamps only u32 timings; no new string/content handling; the 401/403 body-suppression in the touched error paths is preserved. No secrets enter trace records; no injection surface.

## Constitution checks

- **Warning-free build**: cargo test reported green pre-review (1933+16, exit=0) under `#![deny(warnings)]`, which proves zero warnings; frontend tsc clean + 782 vitest passed. I could not re-run the suites myself (read-only reviewer) — the main agent must re-run both after fixing the findings.
- **Doc comments**: all new public functions (StallTracker + methods, `record_backoff_ms`, `set_connect_ms`/`set_backoff_ms`/`set_stall_ms`) have doc comments; frontend functions carry JSDoc.
- **Multi-platform neutrality**: no Windows-only APIs or paths in the changed code; the dead-port and StubServer tests bind loopback only and behave identically on macOS.
- **Line endings**: no CRLF/LF mixing introduced (diff shows clean line endings throughout).
- **Documentation sync**: FAILS — see Low 4.

## Observations (not findings — no action required for this plan)

- The turn-level retry layer (`run_turn_attempt`, 1s/2s/4s backoff in src/runtime/agent.rs) is outside this plan's stated scope; its sleeps remain unattributed between turn-attempt records. Same invisible-gap class — a good future backlog item.
- The stream-end-without-Usage path stamps no timings at all (connect_ms only lands via `set_usage`) — pre-existing behavior, unchanged by this plan.
- Untracked `.coding/knowledge/bug/cb91d6ea.md` belongs to a different plan (slash-command asymmetry); make sure its inclusion in this commit is intentional. The spec + plan files (`.coding/knowledge/spec/2026-12-30-trace-phase-attribution-…`, `.coding/plans/6e734ccb.md`) should be committed with the change.
- `.coding/backlog.jsonl` marks item 40bc1a65 in_flight under plan 6e734ccb — resolve it when finishing.
