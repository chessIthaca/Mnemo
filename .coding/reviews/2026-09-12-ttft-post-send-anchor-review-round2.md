## Verdict: PASS

All five round-1 findings (1 high, 4 low) are verified fixed at HEAD 778427e on `wt/agenticcoding` (working tree clean — the commit IS the tree), and every round-1 "verified correct" item re-checked at HEAD still holds. No new findings. Bug-plan checks pass: root cause documented (BUG memory `fbd9f84d` + the committed knowledge file), both regression tests present and exercising the changed path end-to-end.

Reviewed: `git show 778427e` (13 files, +440/−63) plus working-tree reads of every touched file and the surrounding arms, a repo-wide `set_ttft_ms` sweep (all 16 call sites), a repo-wide `request_start` sweep (27 matches), a repo-wide `post_sent` sweep (28 matches), and a frontend `ttft` sweep (63 matches across 13 files). Tests were not re-run (read-only reviewer); the stated green runs are cross-checked against what the tests actually assert.

## Round-1 fix verification

### H1 (high) — the three missed stream.rs sites: FIXED
- `src/provider/openai/stream.rs:540-551` (stream-guard cut arm, no-chunk else): now `log.set_ttft_ms(*id, post_sent.elapsed()...)` with the comment "ttft = POST-send→error" (:540-541).
- `:573-585` (`SseOutcome::ParseError` arm, no-chunk else): same swap (:574-575, :582-585).
- `:617-625` (body-decode `Err(e)` arm, no-chunk else): same swap (:618, :625).

All three mirror anthropic.rs exactly, as prescribed.

**Repo-wide ttft/request_start sweep (completeness)**: all 16 `set_ttft_ms` call sites enumerated — stream.rs :172 (no-chunk timeout), :301-304 (Usage arm, `first_chunk.duration_since(post_sent)`), :397-403 (LlmEvent::Error), :448-451 (receiver-drop cancel), :489-492 (repetition guard), :548, :582, :625 (the H1 trio); anthropic.rs :1446 (no-chunk timeout), :1552-1555 (error), :1589-1591 (Usage arm), :1683-1687 (error), :1722-1725 (cancel), :1748-1751 (ParseError), :1789 (decode-error); trace.rs :2341/:2344 are direct-value stamps inside a unit test (anchor-agnostic). Every one stamps from `post_sent`. The `request_start` sweep shows every remaining usage is a connect_ms expression (stream.rs :168, :312, :391, :444, :485, :544, :578, :621; anthropic.rs :1442, :1548, :1598, :1677, :1718, :1744, :1785), the destructure/field/pass-through, or a comment — **no ttft expression anywhere still stamps from request_start**. The 6 remaining "headers→first" text matches in Rust are all explanatory ("the old headers→first-chunk window measured ~0") inside the new comments/tests — not live anchors.

### L1 — types.ts doc: FIXED
`frontend/src/lib/types.ts:629-632` now reads "Time-to-first-token in ms (POST sent → first streamed chunk — the real first-token wait: upload + server prefill/queue; the record-created → headers window is connect_ms)" — mirrors trace.rs:199-204 exactly. The sibling `connect_ms` doc (:638-641) is consistent with it.

### L2 — spec amendment: FIXED
`.coding/knowledge/spec/2026-12-30-trace-phase-attribution-connect-backoff-stall-sp-2.md:9` carries the dated "Amended 2027-01-11" paragraph: the POST-send anchor at EVERY ttft site (no-chunk error arms included), the root cause (SSE headers burst with the first chunk → ~0 window), connect_ms unchanged with the by-design overlap, both regression test names, and a pointer to the BUG file. The predecessor `-sp.md` retains the old wording but is frontmatter-marked `status = "superseded"` (:4) — historical record, correctly not live; no contradicting live record remains.

### L3 — README phrasing: FIXED
`README.md:75` now reads "wait (TTFT: POST-send → first token — upload + server prefill/queue)".

### L4 — trailing newline: FIXED
The commit diff for `src/provider/openai/tests.rs` ends `+}` with no `\ No newline at end of file` marker (that marker was present in the round-1 diff and is absent now) — the file (4313 lines) ends with a newline.

## Round-1 "verified correct" items — re-verified at HEAD

- **OpenAI anchor plumbing**: `PreparedRequest.post_sent` (openai.rs:436-443, documented), placeholder `post_sent: record_created` in `prepare_request` (:596-598, commented), stamped at openai.rs:616-620 immediately before the FIRST POST; the reasoning_effort retry (:689-728) rebuilds the body and re-POSTs without touching `post_sent` (verified — no assignment in the retry block, so a retry's wait lands in ttft as designed); `spawn_stream` destructures it (:760) and passes it into `StreamTask` (:822). The repo-wide `post_sent` sweep (28 matches) confirms :620 is the only stamp and the stream loop the only reader — the open_stream error paths (:631-642, :717-727) stamp connect from `record_created` and return `Err` before `spawn_stream`, so the placeholder is provably never read before its stamp.
- **Anthropic anchor**: `let post_sent = Instant::now()` at anthropic.rs:1286, immediately before `.post(&url)` (:1287-1299); a local moved into the spawned loop task — no struct, no locks.
- **connect_ms untouched everywhere**: all connect expressions still `request_start − record_created`; the pre-headers error paths still stamp `record_created.elapsed()` (openai.rs:637, :719-721; anthropic.rs:1307, :1331 — re-read at HEAD). The instruction-2-vs-5 contradiction stays resolved exactly as documented in the plan and the spec amendment.
- **No collateral changes**: the cancel arms keep `log.cancelled` (D1 classification) and only swap the ttft anchor; the parser `ttft_ms: None` sites (sse.rs:114/:217, anthropic.rs:798, runtime/agent.rs:1056/:1103, dispatch.rs:829, turn.rs, trace.rs:602) are placeholders enriched by the stream loop — anchor-agnostic, correct.
- **Regression tests**: `ttft_measures_post_send_to_first_chunk` (openai/tests.rs:4246-4313, `DelayedFirstChunkSseServer`) and `anthropic_ttft_measures_post_send_to_first_chunk` (anthropic.rs:3191-3300, `DelayedAnthropicSseServer`) both drive `complete()` end-to-end over real TCP (bind 127.0.0.1:0), assert `ttft_ms >= 200` (red under the old anchor by construction — the server holds headers AND the first chunk ~300ms after the POST, so the old headers→first-chunk window measured ~0) and `connect_ms >= 200` (pinning connect's unchanged record→headers window). Deterministic margins (≥300ms hold vs ≥200 thresholds).
- **Docs**: trace.rs both field docs (:199-204 record, :354-356 summary), LlmTraceView PhaseTimes doc (:57-69) + span title (:114), types.ts (:629-632), README (:75) — all state the POST-send anchor; traceStats.ts (:130, :188) is value-agnostic (names no anchor).

## Bug-plan checks

- **Root cause documented**: BUG memory `fbd9f84d` (semantic, live) + `.coding/knowledge/bug/2027-01-11-ttft-ms-dead-anchor-stamped-at-headers-arrival-n.md` — committed in 778427e (symptom → root cause → fix + both regression test names; its "at every ttft site" claim is now true post-H1).
- **Regression tests present in code**: both, as above; the committed plan file (`.coding/plans/25e8771c.md`) records the regression test name.
- **Commit contents**: all 13 files (code + tests + docs + knowledge + plan + round-1 review + app-managed backlog bookkeeping) landed on `wt/agenticcoding` — never main.

## Constitution checks

- **Documentation sync**: complete — every surface that named the anchor is updated (trace.rs, types.ts, LlmTraceView, README, spec amendment, BUG file).
- **Multi-platform neutrality**: test servers bind `127.0.0.1:0` via tokio — cross-platform; no `cfg(windows)`, no platform APIs, no Windows-only paths in the diff.
- **File-tools-first**: no shell-based file mutation in the changeset; the backlog.jsonl change is app-managed bookkeeping (status pending→in_flight + plan_id + commit note).
- **Security**: no surface changes; `post_sent` is a `Copy` `Instant` moved through structs — no new locks, no lock-order changes.
- **Tests at fix time** (cross-checked, not re-run — read-only): `cargo test ttft_measures` 2 passed; workspace 2308+16+297+4+2+2+0 passed / 0 failed, warning-free under `#![deny(warnings)]`; vitest 79 files / 1097 tests; `tsc --noEmit` clean — all consistent with the verified code.

## Observations (no action required)

1. The spec amendment paragraph opens "Amended 2027-01-11: AMENDED 2027-01-11 (plan 25e8771c…" — a doubled prefix from the amend tooling. Purely cosmetic; the content is correct and complete.
2. The connect/wait overlap (connect ⊇ ttft up to the record→post_sent sliver, so the stacked phase chart double-counts the prefill window) remains, exactly as documented in the amendment — the plan's instruction-2 resolution, accepted by design.
3. The predecessor spec `-sp.md` retains the old headers→first-chunk wording but is frontmatter-marked `superseded` — correct memory hygiene; history, not live spec.
