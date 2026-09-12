## Verdict: FINDINGS (0 high, 3 low)

# Review — commit 54354d5 (fix/trace-freeze): trace.rs lock-order fix + async trace IPC

Scope reviewed (working tree clean; inspected committed state at HEAD): `src/provider/trace.rs` (writer_main scoping, invariant docs, regression test), `src-tauri/src/ipc/trace.rs` (sync→async conversion), frontend call sites (`frontend/src/lib/tauri.ts`, `frontend/src/components/views/LlmTraceView.tsx`), registration (`src-tauri/src/main.rs:674-678`), remaining direct callers (`main.rs:746-749`, `console.rs:1472`), docs (`README.md`, `.coding/llm-trace.md`). The swept-in `.coding` files are plan/review artifacts (plans e2ba07d8/ebf23ff0 etc.) — nothing anomalous.

## Correctness — lock-order fix is COMPLETE

Full audit of every lock site in `trace.rs` across the five mutexes (`records`, `log_path`, `error_log_path`, `error_logged_ids`, `writer`):

- `writer_main` (trace.rs:924-1043): `log_path` cloned via scoped statement temporary (976-980) — guard dropped at the `;`, BEFORE `records.lock()` (987, itself scoped to the snapshot block 986-992) and before `write_records_to_file` (994). `error_log_path` same pattern (1003-1007) before all error-log I/O. The fatal nesting (log_path held → records acquired) no longer exists.
- `with_record`/`maybe_log_error` (597-718): the only nesting edges in the file are `records` → {`log_path`, `error_log_path`, `error_logged_ids`, `writer`} — the documented A→B order. The `log_path`/`error_log_path` guards there are statement temporaries (`.is_some()`), dropped at the `;`; `error_logged_ids` is a scoped block.
- `ensure_writer` takes `writer` under `records` (441, 615, 695, 715); nothing ever takes `records` while holding `writer` (`flush_file_writes` clones the sender out of the lock first, 781-787). Lock graph is acyclic — the AB-BA cycle is eliminated, not merely narrowed.
- No logic change beyond scoping: drain-time `log_enabled` check, DirtyForced-always-mirror semantics, id dedup, coalescing, flush-ack ordering, and the over-cap fresh-start rules are all preserved.

## Regression test — exercises the changed path, cannot wedge the suite

`writer_never_holds_log_path_while_waiting_for_records` (2125-2221): sends `Msg::DirtyForced` to the REAL writer thread with logging off — exactly the production path (`maybe_log_error`'s DirtyForced branch → writer mirror under a held `records`). Main thread holds `records`, probes `log_path` with non-blocking `try_lock`, and distinguishes the post-fix nanosecond clone critical section from a pre-fix sustained hold (≥10 consecutive Errs ≈ 50 ms; 1 s deadline). Helper thread drives `with_record`'s order (records → log_path); completion observed via `recv_timeout(3 s)` (2209) — a failure can never deadlock the test suite. Assertions fire only after every guard is dropped, so a failure never poisons the mutexes (the comment at 2143-2144 says exactly this). Flakiness: a false-positive inversion needs the writer descheduled >50 ms inside a ~nanosecond section; a false-negative pre-fix pass needs the writer to sleep >200 ms after a channel send — both implausible. Acceptable.

Minor comment nit (not a finding): the header says pre-fix "the AB-side thread deadlocks" — in this exact orchestration the main thread (not the helper) holds `records` during the probe, so the primary pre-fix detector is the sustained-hold probe assertion; the helper is belt-and-braces. The test fails pre-fix either way.

Bug-fix closing requirements: root cause documented thoroughly (with_record invariant 588-596, writer_main doc 915-923, test header 2127-2144); BUG: memory exists (id 5e252963, "trace.rs records↔log_path lock-order inversion + sync trace commands — fixed"). ✓

## IPC conversion — correct and complete

All five commands in `ipc/trace.rs` are `pub async fn … -> Result<_, IpcError>` using `tokio::task::spawn_blocking`: `list_llm_requests` (30), `get_llm_request` (49), `clear_llm_requests` (66), `get_trace_logging` (82), `set_trace_logging` (101). Each clones the owned `Arc<LlmRequestLog>` out of `State` BEFORE the `move` closure — no references moved in, closure is `'static`; `State` borrow does not escape. Error chain: `JoinError → String → IpcError` via `From<String>` (error.rs:47). All five registered in `main.rs:674-678` (async fns register identically via `generate_handler!`).

Frontend: `tauri.ts` wrappers (1554-1583) are pass-through `invoke()`s with unchanged TS signatures — Ok payload types unchanged, so no breakage. Every call site in `LlmTraceView.tsx` handles rejection: `getTraceLogging` (711-717), `listLlmRequests` poll (729-740), `getLlmRequest` detail/compare (776-784, 814-824), `clearLlmRequests` (854-862), `setTraceLogging` with optimistic-revert (868-874). The only remaining direct blocking callers of trace internals are intentional: `flush_file_writes` at process exit (`main.rs:746-749`, watchdog already stopped, documented at ipc/trace.rs:97-99) and the headless console runtime (`console.rs:1472`) — neither is the Tauri main thread during runtime.

## Security

No new exposure. Redaction (`redact_json`/`redact_text`) and `restrict_log_file` still run on both the mirror and error paths; forced-mirror redaction is pinned by tests (1881-1970, 2060-2123). IPC wire shapes unchanged.

## Constitution

- **Multi-platform neutrality ✓** — pure std/tokio; no `cfg(windows)`; the test uses tempfile + std only. (`restrict_permissions` is the pre-existing sanctioned cross-platform helper.)
- **Warning-free ✓ (by inspection)** — all imports used in both files (`Duration`/`Instant` consumed by the new test; all five imports in ipc/trace.rs used); no `unused_mut`, no `#[allow]`.
- **Docs sync** — two stale spots (findings L2/L3). `README.md`'s trace bullets (58-64) describe UI features only — no sync/lock claims, not stale. No `PLAN.md` trace-command claims found.

## Findings

### L1 — LOW (robustness, pre-existing pattern) — `set_logging_enabled` still holds `log_path` across disk I/O via the same if-let scrutinee temporary

`src/provider/trace.rs:753-765` — the enable branch is `if let Some(path) = self.shared.log_path.lock().expect(…).clone() { … create_dir_all(parent) … }`. Under edition 2021 the scrutinee temporary (the `MutexGuard`) lives until the end of the whole `if let` statement, so the `log_path` guard is held across the `std::fs::create_dir_all` disk I/O. This does NOT acquire `records` inside, so no AB-BA cycle exists here and the deadlock fix is complete — but it is the identical fragile pattern this commit eliminated in `writer_main`, and the new `writer_main` invariant doc (915-923) generalizes "never hold `log_path` … across disk I/O". A future edit adding a `records` access inside that block would silently reintroduce the deadlock class. Fix: hoist to a scoped `let path = self.shared.log_path.lock().expect(…).clone();` before the `if let`, mirroring 976-980. Impact today is trivial (enable-only, now on a blocking thread), hence LOW.

### L2 — LOW (docs sync) — `ipc/trace.rs` module doc says "Three commands"; the file has five

`src-tauri/src/ipc/trace.rs:8-10` — "Three commands over the shared LlmRequestLog … list … fetch one full detail … and clear the log" omits `get_trace_logging`/`set_trace_logging`, and says nothing about the property this commit made uniform: all five commands are async + `spawn_blocking` precisely because Tauri runs sync commands on the main thread. (Staleness predates this commit, but the commit rewrote every command doc in this file.)

### L3 — LOW (docs sync, pre-existing) — `.coding/llm-trace.md` internals section is stale

`.coding/llm-trace.md:131` lists only `list_llm_requests`, `get_llm_request`, `clear_llm_requests` (missing `get_trace_logging`/`set_trace_logging`), and `:119` ("Session-only — the log lives in memory and is cleared on app restart") omits the opt-in `traces.jsonl` mirror and the always-on `provider-errors.jsonl` persistence. Not introduced by this commit, but it is the user-facing trace doc the constitution names; cheap to correct while the trace subsystem is hot.
