# Review — non-blocking trace/file-log writes (background writer thread)

**Scope:** all uncommitted changes per `git diff HEAD` + `git status`:
- `src/provider/trace.rs` (main change: async file mirroring via background writer thread)
- `src-tauri/src/main.rs` (exit-hook flush)
- `.coding/plans/stack.json`, untracked `.coding/plans/e565c2e4-*.md` (workflow bookkeeping — not code, not reviewed further)

## Findings

### Correctness
None.

### Bugs
None.

### Security
None. Verified specifically:
- Traces mirror: `write_records_to_file` (src/provider/trace.rs:644-661) clones each record and applies `redact_json` (request body) + `redact_text` (raw response, error text) **before** serialization/disk — identical redaction set to the old synchronous path; no other code writes `log_path`.
- Provider-error log: `redact_text` runs on the caller thread (src/provider/trace.rs:440) *before* the line enters the channel; `Msg::ErrorLine` only ever carries pre-redacted text and the writer appends it verbatim (src/provider/trace.rs:600). No path persists unredacted content. (Known pre-existing limitation, unchanged by this diff: `redact_text`'s JSON-key regex can miss a key embedded inside a JSON-*escaped* error string, e.g. `\"api_key\":\"…\"` in the serialized line. The old code had the exact same behavior — not a regression.)

### Constitution compliance (minor)
1. **Doc-comment grammar + private intra-doc link on a public method** — src/provider/trace.rs:498-503, `pub fn flush_file_writes`: "This is the seams between the asynchronous mirroring…" should be "These are the seams…" (or "This is the seam…"). The doc also intra-doc-links `[`writer_main`]`, a private fn, from a public item — that is a rustdoc `private_intra_doc_links` warning under `cargo doc` (not a rustc warning, so `#![deny(warnings)]` / `cargo test` is unaffected), and the link is invisible in rendered public docs anyway. Suggest rewording to reference the writer descriptively (e.g. "the background writer thread") without the bracket link.

No other findings.

## Invariant verification (all checked, all hold)

1. **Public API unchanged** — all 14 pre-existing public methods (`new`/`start`/`set_status`/`set_usage`/`append_response`/`finish`/`fail`/`list`/`get`/`clear`/`set_log_file_path`/`set_error_log_path`/`set_logging_enabled`/`logging_enabled`) plus `Default` keep identical signatures; the only addition is the planned `flush_file_writes`. Moving fields into private `Shared` is safe: no external site constructs `LlmRequestLog` by literal (all 28 uses go through `LlmRequestLog::new()`).
2. **Redaction before disk** — see Security above.
3. **No deadlock** — full lock-order audit: hot path nests only `records → {writer, error_log_path, error_logged_ids}` (trace.rs:389-398, 413, 424); the writer thread nests only `log_path → records` (trace.rs:559-575) and takes `error_log_path` standalone (trace.rs:584); the writer thread never acquires the `writer` mutex; `flush_file_writes`/`ensure_writer` never hold another lock while blocking. No opposite-order nesting exists anywhere → no cycle. The send while holding `records` (trace.rs:396) is safe: `std::sync::mpsc` senders never block on an unbounded channel. First-spawn race is empty — no sender exists before the spawn, so the fresh thread blocks in `recv()` holding no locks.
4. **flush_file_writes can't hang** — no-writer early return (trace.rs:507-510); writer-mutex released before `ack_rx.recv()` (trace.rs:505-515); if the writer thread panicked (lock poison), all senders drop → `tx.send` errs → the `is_ok()` guard skips `recv`. Called from `set_logging_enabled(false)` with no other lock held.
5. **Error-log dedup unchanged** — `error_logged_ids.insert` still gates on the caller before enqueue (trace.rs:424-432); repeated `fail()` enqueues at most one line.
6. **Old gating preserved** — writer re-checks `log_enabled` at write time (trace.rs:558), so a `Dirty` enqueued while enabled but drained after disable is skipped; `set_logging_enabled(false)` deliberately flushes *before* flipping the flag so the file is complete at toggle-off.
7. **Tests deterministic** — every file-content assertion is preceded by `flush_file_writes()`; the coalescing test is robust to arbitrary drain interleavings because every drain is an id-keyed upsert (row count stays 1). The `!path.exists()` assertions occur only while logging was never enabled → no message ever sent → no writer spawned.
8. **Warning-free / docs** — no `#[allow]`; `Arc`/`Mutex`/`Path` imports all still used; every `Msg` variant constructed and matched; every `Shared` field read; new public fn has a doc comment. (Reviewer could not run `cargo test` — read-only; verified by inspection.)

## Batched-writer correctness details verified
- Flush semantics: the drain loop (`recv` then `try_recv` to empty) guarantees everything queued before a `Flush` is processed before its ack — error lines and mirror writes included.
- `write_records_to_file` upsert: `lines[i]` index aligned with `records[i]`; duplicate file rows for one id cannot arise (both old and new writers replace-all-matches / guard with `replaced[]`); batch ids are deduped in `writer_main`.
- Evicted-id skip (trace.rs:569-575) and the whole-batch `unwrap_or_default()` on serialization failure (trace.rs:657-661) are documented, and the latter is unreachable in practice (every field type serializes infallibly into `serde_json`) — acceptable as written.
- Writer exits cleanly when the last `LlmRequestLog` drops (channel close); the thread holds `Arc<Shared>`, not the handle — no Arc cycle, no leak.

## src-tauri exit hook (src-tauri/src/main.rs:433-452)
- `IpcState` is `manage`d in all three setup arms (Ready L187, NeedsProject L253, Err L316) before the event loop starts, so `state::<IpcState>()` cannot miss at `RunEvent::Exit`.
- `.trace` in the managed state is `brain.trace.clone()` (L208) — the **same** instance providers write to; the fallback arms' fresh logs have no paths configured, so the flush is a no-op there. Correct instance.
- Ordering: flush (cheap local I/O) precedes the `block_on(browser.close())` kill — the right order (a hang/panic in the browser kill can no longer lose the log drain).
- Limitation (inherent, same as the pre-existing browser kill): a hard process kill (`TerminateProcess`) never delivers `RunEvent::Exit`. Not fixable at this layer.

## Bookkeeping diff
`.coding/plans/stack.json` swap + the untracked new plan file are workflow state, not source. No action.
