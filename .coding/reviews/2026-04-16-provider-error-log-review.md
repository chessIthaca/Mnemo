# Code Review — Always-on provider-error log (.coding/logs/provider-errors.jsonl)

**Reviewer:** read-only reviewer
**Date:** 2026-04-16
**Scope:** all uncommitted changes (`git diff HEAD`) for plan "Always-on provider-error log".

## Changed files reviewed
- `src/provider/trace.rs` — `LlmRequestLog` gains `error_log_path` + `error_logged_ids`, `set_error_log_path`, private `maybe_log_error`, and 4 new tests.
- `src-tauri/src/main.rs` — unconditional `trace.set_error_log_path(...)` after the traces.jsonl path setup.
- `.coding/plans/*` bookkeeping (not code).

I read the full current `src/provider/trace.rs` (808 lines) and the real provider call sites in
`src/provider/openai.rs` (`fail` at :404/:414/:489/:572/:586/:597/:625, `set_status` at :421,
`set_usage` at :539, `finish` at :566/:638) to validate behavior against actual request flows,
not just the diff summary.

## Focus-area verification

1. **Terminal-error predicate (trace.rs:356-357).**
   `record.http_status.map_or(false, |s| s >= 400) || record.error.is_some()`.
   - No false positive on success: the success path (`set_status(200)` → `set_usage` → `finish`)
     leaves `error: None` and `http_status: Some(200)`, so `200 >= 400` is false and
     `error.is_some()` is false → never logged. Confirmed by `error_log_skips_successful_requests`.
   - No missed real errors:
     - HTTP >= 400: `fail(id, status.as_u16(), ...)` at openai.rs:414 — `status >= 400` arm logs.
     - Transport failure status 0: `fail(id, 0, "failed to start stream: ...")` at openai.rs:404 —
       `0 >= 400` is false but `error.is_some()` is true → logged. Confirmed by
       `error_log_transport_error_status_zero`.
     - Mid-stream errors (`fail(id, status.as_u16(), error)` at :489/:572/:586/:597/:625) set
       `error` → logged via the `error.is_some()` arm. Note these carry the *success* status
       (e.g. 200) in `http_status` — correct behavior since the OR predicate logs on `error`.
   The predicate is correct.

2. **Dedupe correctness (trace.rs:361-369).** The id is inserted into `error_logged_ids` BEFORE
   the file write, so a failed write consumes the id (a later retried `fail()` on the same id will
   not double-log). This is the documented, deliberate tradeoff (request path must never break).
   - Lock ordering: `with_record` holds `records` (Mutex) for the whole body, then `maybe_log_error`
     takes `error_log_path` then `error_logged_ids`. Order is always `records → error_log_path →
     error_logged_ids`. `set_error_log_path` takes only `error_log_path`; `set_log_file_path` /
     `set_logging_enabled` take only `log_path`. No code path ever takes these three locks in a
     different order or holds two of them simultaneously in reverse. **No deadlock possible.**

3. **Independence from traces.jsonl opt-in (trace.rs:327-335).** `maybe_log_error(r)` is called
   unconditionally after the `if self.log_enabled ...` block — it does not read `log_enabled`.
   Confirmed by `error_log_is_always_on_without_trace_checkbox` (sets only the error path, asserts
   `!logging_enabled()`, still writes). ✓

4. **Append-only write, I/O tolerated (trace.rs:370-384).** Uses
   `OpenOptions::new().create(true).append(true)` — one line appended per failure, no whole-file
   rewrite (unlike `write_record_to_file`). Parent dirs created with `create_dir_all` (errors
   discarded via `let _ =`), the open and `writeln!` results both discarded. **No unwrap/panic on
   the request path.** ✓

5. **Line content (LlmRequestSummary, trace.rs:110-153).** Serializes
   `{id, ts_ms, model, base_url, http_status, usage, finish_reason, ttft_ms, generation_ms, error,
   response_truncated}` — the compact shape with NO `request_json` / `response_raw` (those are on
   `LlmRequestRecord`, not the Summary). No request/response bodies or plaintext payloads are
   persisted. ✓

6. **Constitution compliance.**
   - No `#[allow(...)]` added anywhere in `src/provider/trace.rs` (searched — zero matches). ✓
   - `pub fn set_error_log_path` has a doc comment (trace.rs:396-400); `maybe_log_error` (private)
     also has one. ✓
   - No frontend changes required. ✓
   - Build warning-free under `#![deny(warnings)]` is asserted by the plan (cargo test green); the
     code as written introduces no dead code / unused imports that would trip it.

## Findings

### Correctness
- **No findings.** The predicate, dedupe, independence, and append-only write are all correct for
  the real call flows in `openai.rs`.

### Bugs
- **No findings.**

### Security
- **No findings.** The error line deliberately excludes request/response bodies (Summary, not the
  full Record), so no plaintext payloads or secrets are written by this log.

### Constitution compliance
- **No findings.**

## Notes (non-blocking observations, not findings)
- Mid-stream error lines carry the *success* HTTP status (e.g. 200) in `http_status` alongside the
  `error` text (openai.rs passes `status.as_u16()` into `fail`). This is pre-existing record
  behavior, not introduced by this change, and the OR predicate still logs them correctly. A reader
  of the jsonl should treat `error != null` as the authoritative failure signal rather than
  relying on `http_status >= 400` alone. Worth documenting in whatever tool consumes this file, but
  not a defect in this diff.
- `error_logged_ids` is an unbounded in-memory set that grows by one `u64` per failed request for
  the process lifetime. Bounded in practice (only failures, one 8-byte entry each) and consistent
  with the session-scoped log; no action needed.

## Verdict
**Clean — no findings.** The implementation matches the plan goal and each focus area checks out
against the actual code.
