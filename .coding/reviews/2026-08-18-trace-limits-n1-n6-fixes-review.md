# Review: mem/perf/threading fixes (N1–N6) + configurable in-memory trace limit

**Scope:** all uncommitted changes at HEAD (`git diff HEAD` + `git status`). Rust workspace (`src/`, `src-tauri/`) + React/TS frontend (`frontend/src/`). Implements the six findings of `.coding/reviews/2026-06-14-mem-perf-threading-review.md` plus a configurable in-memory trace memory limit (`[trace]` config → `LlmRequestLog` budget + request-body cap → Settings UI).

**Verdict: no blocking findings.** All six findings are correctly fixed, the new budget/cap machinery is sound (termination, accounting, lock discipline, wire compat all verified), and constitution requirements (doc comments, no `#[allow]`, regression tests) are met. 2 Low + 4 Info findings below, none of which block the commit.

---

## Findings

### Low

**L1 — Trace detail/compare poll never stops when `get_llm_request` returns `null`** — `frontend/src/components/views/LlmTraceView.tsx:625-641` (detail effect) and `:663-681` (compare effect).
The poll now stops on `d?.is_complete` — the N1 fix works for every record that exists. But `get` returns `null` for ids that were never recorded or were evicted from the 32-record ring, and a `null` result never stops the interval. Two reachable cases: (a) compare mode with `selectedId - 1` never recorded/evicted — the compare poll fires a tiny IPC every 1.5 s for as long as compare stays on; (b) the selected record itself gets evicted while 32 newer requests stream past. Cost per tick is now a null response (bytes), so the N1 win (stop re-cloning ~2 MiB/tick) is intact — this is a residual nano-leak, not a regression. Since ids are never reused and the ring only loses records, a `null` result can never become non-null: stopping on `d === null` would be a strict improvement. Errors mid-fetch correctly keep polling (transient IPC errors recover) — that part is right.

**L2 — Stale doc comment on `list()`** — `src/provider/trace.rs:452-457`.
`list()`'s doc still says "Records whose payloads were evicted are absent." After this change, payload-evicted records ARE present in the list (only their `response_raw` is gone — that's the whole point of `response_evicted` on `LlmRequestSummary`). "Evicted" now means ring-eviction for absence and payload-eviction for the flag; the sentence conflates them. One-line doc fix.

### Info

**I1 — `cap_request_json` serializes the body on the caller's thread, which is the async request path** — `src/provider/trace.rs:346-349` (call site in `start`) + `:714-746`; called from async context at `src/provider/openai.rs:495`.
Every request now pays one full `serde_json::to_string` of its body (typically 100–800 KB ≈ 1–2 ms) to check the cap, and an over-cap body pays ~2 serializations per convergence pass (~4–5 passes for a 2 MiB body → roughly 10–40 ms of sync CPU) — on a tokio worker, in `complete()`. Bounded, rare (only oversized bodies pay multi-pass), and small next to the network call it precedes; not worth blocking the commit. If it ever matters, hoist `start`'s cap application into a `spawn_blocking` or serialize once and cut against the measured length.

**I2 — N5 fix keeps `corpus_digest` sync-fs, just off the exit path** — `src/runtime/agent.rs:456-462`.
Moving the call inside `tokio::spawn` is literally what the review prescribed and does fix the stated defect (the agent's exit path no longer blocks; "Never blocks the caller" is now true). The reads still run sync on a tokio worker *inside* the spawned task (≤ ~20 files × 1500 chars) — wrapping in `spawn_blocking` would remove that too. Polish only.

**I3 — `src-tauri/Cargo.toml` shows modified with an empty content diff** — line-ending normalization only (git warns "LF will be replaced by CRLF"). No content change; make sure the commit doesn't accidentally include CRLF churn for it (or stage it deliberately as a no-op).

**I4 — AdvancedSection numeric-field edge UX (pre-existing pattern, extended to the new fields)** — `frontend/src/components/settings/sections/AdvancedSection.tsx:99-107, 116-119`.
Clearing the budget field yields `Number("") === 0` (or `NaN` for text): the field reads dirty, the save guard (`Number.isFinite && >= 1`) silently drops it from the patch, but the snap is still updated — so a cleared field "saves" nothing while clearing the dirty flag, and a `NaN` snap (`NaN !== NaN`) leaves the section permanently dirty. Identical behavior already existed for `fillRate`; the new fields inherit it. Cosmetic; backend validation covers anything actually patched.

---

## Requested assessments

**(a) Correctness — verified clean:**
- **`with_record` lock handling**: eviction runs after the `iter_mut` borrow ends; the id re-find after `enforce_raw_budget` is safe because eviction only drops payloads, never records. `enforce_raw_budget`/`set_memory_budget` run under the records lock with no re-entrancy (`ensure_writer` is the only nested lock and has no reverse-order acquirer; `writer_main` takes `records` only between messages, never while anything holds `writer`).
- **`cap_request_json` termination**: each pass truncates the current longest leaf to `min(cap/4, len)` + a marker of ≤ ~30 bytes; `MIN_CUT_CHARS = 32` exceeds the marker's worst-case length, so every pass strictly decreases serialized size or breaks — no growth, no infinite loop, char-boundary-safe (`chars().take`). A body of many short strings correctly falls through over-cap (documented, structure preserved).
- **Budget edge cases**: a single payload larger than the whole budget evicts every payload including the newest (sum → 0 ≤ budget) — invariant maintained, flagged `response_evicted`, and the `append_response` early-return on `response_evicted` prevents append→evict→append churn (regression test covers the self-eviction case). `set_memory_budget` stores then enforces under the lock; a racing `with_record` that read the stale budget self-heals on the next mutation.
- **`is_complete`**: maintained on every mutation; `start` initializes false. Cross-checked the provider: every terminal path in `openai.rs` calls `finish` or `fail` (status error :515/:536, stream timeout :611, parse error :719, consumer-dropped :708, fallback finish :760), so the flag is reliable — the poll-stop is not premature. Usage is emitted before Finish in the stream, so the last fetch before stopping carries it.

**(b) Security — no findings.** All four N2 sites validate through the sandbox (or derive from the project root) *before* `spawn_blocking`, with owned data; no new path escapes. `browse_markdown_file`'s picked path remains intentionally unsandboxed (the picker is the feature; documented, unchanged). Secrets handling unchanged (redaction still applied before any disk write; the new summary field carries only a bool).

**(c) Constitution compliance — pass.** All new public functions/consts/fields have doc comments (`TraceConfig` + `clamp_*`, `set_memory_budget`, `set_request_body_cap`, `GetSettingsTrace`, the updated command docs). No `#[allow]` attributes introduced. Green `cargo test --workspace` under `#![deny(warnings)]` proves the build is warning-free. Regression tests exist for every behavioral defect: budget eviction ×3, request cap ×2, `is_complete`, N3 (`error_log_dedup_set_is_bounded`), N4 (`error_log_over_cap_starts_fresh`), config round-trip/clamps. **N2's skipped regression test is acceptable**: the change is a pure thread-placement move with byte-identical observable behavior (same validation order, same error strings) — there is no behavior to assert without Tauri mock-`State` infra, and the invariant that matters (validate-before-spawn) is verifiable by inspection at each of the four sites.

**(d) Wire compatibility — verified clean.** `get_llm_request` and `set_trace_logging` became `async … Result<T, IpcError>`; Tauri unwraps `Ok` for `invoke()`, so the frontend sees identical values (`Option<Detail>` → `Detail | null`, `()` → `null`). `GetSettingsTrace` (snake_case) matches `AppSettings.trace` and the updated golden fixture `dto-get-settings.json` + `contract_fixtures.rs` on both sides. `LlmRequestDetail`/`LlmRequestSummary` gained `request_truncated`/`response_evicted`/`is_complete` on both the Rust `Serialize` and TS types (always-emitted bools, no `skip_serializing_if` asymmetry). Backward compat with on-disk rows holds: the traces.jsonl merge path parses each line as a `Value` and reads only `id`, so pre-change rows round-trip untouched.

---

## Verified clean (checked in-session, not re-reportable)

- Validation ranges in `save_settings` (1..=512 MiB, 16..=8192 KiB) match `TraceConfig` clamps; clamping applied at load *and* save; live push to `state.trace` gated on either knob being patched; MiB→bytes math cannot overflow `usize` at the clamped max (512 MiB).
- `main.rs` applies config limits after `set_error_log_path` on the Ready path; the NeedsProject/Err fallback branches keep baked-in defaults — correct, since no provider (hence no traffic) exists there; documented in-code.
- N4 over-cap path: `write+truncate` without `create` is safe because `over_cap` requires the file to exist (metadata failure maps to 0 → append+create); a delete-race drops one batch and self-heals — consistent with the writer's tolerated-I/O-failure design.
- N3 clear-at-10k lives inside the `error_path_configured` block, so the set only ever grows when inserts happen; the rare double-log after a clear is documented in-code.
- `append_response`'s new `response_evicted` guard mirrors the ring state correctly to traces.jsonl (evicted rows rewrite with `response_raw: null` + flag).
- Frontend teardown: both poll effects clear their interval via `stopPolling()` in the cleanup; the list poll (32 small summaries) intentionally continues — matches the review's "list is fine".
