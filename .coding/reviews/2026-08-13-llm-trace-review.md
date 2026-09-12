# Review — LLM Trace tool page (request/response JSON inspector)

**Date:** 2026-08-13
**Reviewer:** read-only subagent
**Scope:** ALL uncommitted working-tree changes (`git diff HEAD` + untracked):
`src/provider/trace.rs`, `src/provider/openai.rs`, `src/provider/client_factory.rs`,
`src/provider/mod.rs`, `src/model_resolver.rs`, `src-tauri/src/ipc/trace.rs`,
`src-tauri/src/ipc/{agent,settings,state,mod}.rs`, `src-tauri/src/main.rs`,
`src/agent/{factory.rs,tests.rs}`, `src/runtime/agent.rs` (test-timeout widening),
`frontend/src/components/views/LlmTraceView.tsx`,
`frontend/src/{components/layout/Sidebar.tsx, hooks/agentState.ts, hooks/rightPanelViews.tsx, lib/tauri.ts, lib/types.ts}`,
`.coding/llm-trace.md`, `.coding/plans/*`.

Reviewed against the plan's goals: record every `/chat/completions` request (exact
request JSON + raw response) into an Arc-shared ring buffer, expose it via IPC, and
render list/detail/compare UI for cache-prefix debugging.

---

## Correctness findings

### 1. [High] `append_response` can panic on a non-char-boundary slice — and poison the log for good
`src/provider/trace.rs:220-224`:

```rust
let remaining = MAX_RAW_RESPONSE_BYTES.saturating_sub(existing_len);
if text.len() > remaining {
    r.response_raw.get_or_insert_with(String::new).push_str(&text[..remaining]);
    r.response_truncated = true;
```

`remaining` is an arbitrary byte count (`cap - existing_len`), not a UTF-8 char
boundary of `text`. When the 2 MiB cut lands inside a multi-byte character of the
current chunk, `&text[..remaining]` **panics** ("byte index is not a char boundary").
This is reachable in practice: chunks are `String::from_utf8_lossy` output
(openai.rs:509), and any response crossing 2 MiB (long reasoning traces) whose
cut point falls mid-character triggers it — multi-byte chars near the cut are
common in real content.

Two aggravating factors:
- The panic happens **while holding the `records` Mutex** (via `with_record`,
  trace.rs:267-272), so it **poisons** the log. Every subsequent
  `.expect("LlmRequestLog lock poisoned")` panics: `start()` (trace.rs:182) —
  which runs on the caller path of `complete()` (openai.rs:393-395), i.e. a
  single unlucky truncation takes down subsequent LLM requests app-wide — plus
  `list()`/`get()`/`clear()` (the Trace tab itself).
- The unit test only uses ASCII (`"x".repeat(MAX_RAW_RESPONSE_BYTES + 10)`,
  trace.rs:356-357), so a mid-char cut is never exercised.

Fix: cut at a char boundary — `text.floor_char_boundary(remaining)` (stable
since Rust 1.73) or a `char_indices()` walk — and add a test where a multi-byte
char straddles the cap.

### 2. [Medium] The fallback `finish("stop")` overwrites the real finish reason in every normal stream
`src/provider/openai.rs:627-630`:

```rust
// If the stream ended without a Finish event, emit one.
if let Some((log, id)) = &trace_ctx {
    log.finish(*id, "stop");
}
```

This runs **unconditionally** whenever the loop breaks. But in every well-formed
OpenAI SSE stream a real `LlmEvent::Finish` was already logged by the `other =>`
arm (openai.rs:571-574) when `parse_sse_chunk` saw `finish_reason`
(openai.rs:939-947). The fallback therefore overwrites `tool_calls` / `length` /
`content_filter` with `"stop"` — the trace record (and the UI's "finish: …",
LlmTraceView.tsx:163, and the Response header at :415) shows `stop` for virtually
every successful request. It's also nondeterministic: if the consumer drops the
receiver right after the real Finish, `tx.send(...).is_err()` returns early
(openai.rs:586-588), the fallback never runs, and the real reason survives.
Either way the displayed finish reason is wrong for non-`stop` finishes — which
defeats part of the feature's purpose (surfacing `length` truncations, etc.).
Fix: track whether a Finish was already logged (local `bool`) and only run the
fallback log when it wasn't.

### 3. [Low] Compare mode can transiently pair the new request with the *previous request's* previous
`frontend/src/components/views/LlmTraceView.tsx:620-640` (prev effect) + `:643-659`
(compare memo). When `selectedId` changes, `prevDetail` is **not** cleared at
effect start (the detail effect does `setDetail(null)` at :600, the prev effect
only nulls it in the `!compareOn || selectedId === null` branch). So after
selecting B while A was selected, the memo runs with `detail = B` and
`prevDetail = A-1` until the B-1 fetch resolves — wrong per-message badges and a
wrong "first k of n messages identical" prefix line for one IPC round-trip.
Fix: `setPrevDetail(null)` at the start of the prev effect.

### 4. [Low] Consumer-dropped streams leave a record "in-flight" forever
`src/provider/openai.rs:586-588`: `if tx.send(event).await.is_err() { return; }`
exits without a terminal trace update. The record keeps `finish_reason: None`,
`error: None`, and the UI renders "Waiting for response bytes…"
(LlmTraceView.tsx:421) next to HTTP 200 indefinitely (until evicted) — a dropped
receiver (cancelled turn, caller abort) is indistinguishable from a live stream.
Consider logging a terminal state ("stream aborted") on receiver-drop.

---

## Memory finding

### 5. [Low] `request_json` is uncapped — the "~64 MiB worst case" doc claim is wrong
`src/provider/trace.rs:14-16` (module doc) claims worst case ~64 MiB, but only
`response_raw` is capped at 2 MiB (trace.rs:32). `request_json` — the full message
history including tool outputs — is stored verbatim (trace.rs:172) × 32 records.
Sessions with large tool results can exceed the documented bound. Either cap the
stored request body (e.g. serialize to string with a byte cap + truncation flag)
or correct the doc.

---

## Constitution / style findings

### 6. [Low] `LlmRequestSummary` lacks doc comments
`src/provider/trace.rs:104-117` — the struct and all its fields are undocumented,
while every other public item in the module (and in `src-tauri/src/ipc/trace.rs`)
has doc comments. Not a build failure (`#![deny(warnings)]` without
`#![deny(missing_docs)]`), but it violates the "public items documented" style
rule. Add a one-line doc comment.

### 7. [Nit] `set_trace` is dead API
`src/provider/openai.rs:132-136` — no call sites anywhere (searched; only the
definition and a doc reference). Public lib fn, so no dead-code warning under
`deny(warnings)`, but either wire it in (e.g. the `settings.rs` rewire could use
it on the boxed client) or drop it.

### 8. [Nit] New files are LF in the working copy; the repo is CRLF
Git warns "LF will be replaced by CRLF" for `.coding/plans/e05b76d0-*.md`, and
the new files (e.g. `src/provider/trace.rs`) contain no `\r` while the repo is
CRLF-configured. Git's autocrlf should normalize at commit, so no mixed endings
should reach the index — informational, but worth confirming at commit time
(`git add` then check for the warning).

---

## Verified sound — no findings

- **Capture coverage is complete on every production chat path:**
  `client_factory.rs:80` (`build_openai_client` → `new_with_trace`), `main.rs:373`
  (default provider) and `:383` (dummy fallback), `ipc/agent.rs:424` (`set_model`
  swap), `ipc/settings.rs:463` (save_endpoints rewire), `model_resolver.rs:243`
  (per-context overrides via `build_provider_for`). All 16 `OpenAiClient::new`
  sites outside the factory are tests, except `vision.rs:122`, which builds a
  VisionClient with capture off — vision/embedder not captured is the documented
  design. All `ConfigModelResolver::new` / `build_provider_for` call sites
  (including tests) were updated.
- **fail/set_status ordering is correct:** transport error → `fail(id, 0, …)`
  (openai.rs:413-416); non-2xx → `fail(id, status, body)` before the `Err` return
  (424-426); only 2xx reaches `set_status` (431-433). Mid-stream errors overwrite
  with the 2xx status + error text — reasonable.
- **Eviction/threading:** `with_record` no-ops on evicted ids (trace.rs:267-272) —
  no use-after-eviction; `start` monotonic `AtomicU64`; `trace_ctx` (owned
  `Arc<LlmRequestLog>`, `u64` id, `StatusCode` is Copy) moves into the spawned
  task soundly; the trace `RwLock` is read once per request (no per-chunk cost
  when disabled); buffer and trace append the *same* lossy-converted text
  (openai.rs:509-513), so what the UI shows matches what was parsed.
- **IPC ↔ frontend contract:** command names/args match
  (`list_llm_requests`, `get_llm_request` with `id: u64` ↔ `{ id }`,
  `clear_llm_requests`); `Option<LlmRequestDetail>` serializes to `null`; the TS
  interfaces mirror the Rust `Serialize` output field-for-field (types.ts:249-314
  vs trace.rs:63-117), including `summary` (11 fields, no payloads) vs `detail`
  (13 fields).
- **Frontend lifecycle:** all three polling effects clean up (`cancelled` flag +
  `clearInterval`, LlmTraceView.tsx:565-640); the view only mounts while the tab
  is active (RightPanel.tsx:114-120 renders only the active view), so no hidden
  polling; selection is stable by id with a newest-fallback on eviction (:573-576);
  request rows keyed by `r.id`; message rows keyed by index inside a section that
  remounts through the detail-null gap (one cosmetic stale frame after a selection
  change — self-correcting, non-blocking). Compare edge cases handled: k=0 / k=n /
  removed > 0 / empty previous (PrefixLine, LlmTraceView.tsx:522-548). XSS-safe by
  construction: everything renders through React text nodes or `JSON.stringify` in
  `<pre>`; no `dangerouslySetInnerHTML` anywhere in the new view.
- **runtime/agent.rs timeout widening** (`src/runtime/agent.rs`, 9 sites,
  200 ms → 2000 ms): all nine are positive `fanin_rx.recv()` waits for events that
  must arrive, with loop deadlines (2 s / 3 s) still bounding total test duration;
  no negative timing assertions are affected. Sound fix for parallel-load flake.
- **Security:** the recorded `request_json` contains only `model`, `messages`,
  `stream`, `stream_options`, `max_completion_tokens`, `tools`, `tool_choice`,
  `reasoning_effort` (openai.rs:744-772) — no Authorization header, no `api_key`
  (the key is applied at send time, openai.rs:405, and never serialized). `base_url`
  is stored (host-level; only a concern if a user embeds credentials in the URL,
  which is the user's own config content). No file I/O, no path traversal, no
  shell, no secrets on the wire to the frontend beyond the request bodies.
- **Constitution:** `#![deny(warnings)]` at both crate roots (src/lib.rs:1,
  src-tauri/src/main.rs:7); no new `#[allow(...)]` suppressions were added (the
  two `clippy::too_many_arguments` allows at src/agent/loop_impl.rs:174/211 are
  pre-existing, untouched by this diff). No commits to main — all changes are
  uncommitted on the feature branch. Plan/bookkeeping files updated consistently
  (stack.json now references the current plan; the old plan's closing step marked
  done).
