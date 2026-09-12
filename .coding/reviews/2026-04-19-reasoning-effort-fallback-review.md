# Review: reasoning_effort self-healing fallback + embedder display name

Scope: ALL uncommitted changes — `src/provider/openai.rs`, `src/memory/embedder.rs`,
`.coding/backlog.json`, `.coding/plans/stack.json`, new plan file `.coding/plans/e7925f98-*.md`.

## Verdict

**No High or Medium findings.** The implementation matches the plan spec exactly and is
correct on every point in the review focus. Two Low/informational observations follow,
neither requiring a code change (both are deliberate design decisions documented in the
code/plan).

## Verification against the review focus

### Correctness (`src/provider/openai.rs` `complete()`, lines ~468-598)

- **Retry bounded to exactly one extra request** — YES. The retry happens once inside
  `if effort_rejected` (line 570); the retried response is checked with
  `if !response.status().is_success()` (line 590) which either falls through to the
  success path (line 599) or returns a terminal error via `fail_response` (line 593).
  There is no recursion and no loop, so even a hypothetical repeated rejection cannot
  retry twice. The comment at lines 533-541 accurately documents this ("The retry cannot
  recur: the field is gone from the body").
- **Fires only on (status==400 AND body mentions reasoning_effort AND config effort set)** —
  YES, exactly at lines 567-569. `build_request_json` adds `body["reasoning_effort"]`
  only when `self.config.reasoning_effort.is_some()` (lines 972-973, verified), so the
  config check is equivalent to "the field was actually sent". Tests B and C pin the two
  negative cases.
- **Response body read exactly once** — YES. `response.text().await.unwrap_or_default()`
  (line 566) is the only read of the first response; the non-fallback error path (line 596)
  builds the error from the already-read `body_text` via `fail_response` →
  `provider_error`, never re-consuming the response. The GLM code-1214 diagnostics are
  preserved (test B asserts `"messages parameter is illegal"` survives into the error).
- **401/403 body suppression preserved (trace + user-visible error)** — YES. The
  `fail_response` closure (lines 548-563) replicates the old trace suppression exactly
  (`"HTTP {status} — unauthorized (body suppressed)"` for 401/403) and delegates the
  user-visible error to `provider_error` (lines 419-430), which never includes the body
  for 401/403. Byte-for-byte the previous behavior on the non-fallback path.
- **Trace records only the final outcome** — YES. The success path never calls
  `log.fail`; after the fallback retry succeeds it records only `log.set_status(id, 200)`
  (lines 599-601). A retry transport failure traces once with status 0 (lines 583-586),
  and a final (retried) failure traces once via `fail_response`.
- **Retried request actually omits the field** — YES. `body.as_object_mut().and_then(|o|
  o.remove("reasoning_effort"))` (line 571) mutates the same `body` value that is
  re-serialized by `.json(&body)` in the retry POST (line 577); test A asserts
  `bodies[1].get("reasoning_effort").is_none()`.
- **Closure borrow/lifetime** — SOUND. `rec_id` is `Option<u64>` (`TraceLog::start`
  returns `u64`, `src/provider/trace.rs:340`); `u64` is `Copy`, so the closure captures
  it without moving, `log: &Arc<LlmRequestLog>` binds by match ergonomics, and `&trace` /
  `&url` are shared borrows → the closure is `Fn`, callable at both call sites, and
  `trace`/`rec_id` remain usable afterwards (lines 599, 619; NLL). `TraceLog::fail`
  takes `&self, id: u64, status: u16, error: &str` (trace.rs:445), matching the call
  at line 560.

### Tests (end of `mod tests`, lines 2723-2961)

- **Regression quality** — Test A (`complete_retries_once_without_reasoning_effort_on_400_rejection`)
  FAILS on the old code: `complete()` returned `Err` on the 400, so
  `.expect("complete must succeed via the fallback retry")` panics. Tests B and C guard
  the retry condition's negatives (unrelated 400 → no retry; effort omitted → no retry).
- **StubServer HTTP parsing** — CORRECT for these requests: head read until `\r\n\r\n`;
  case-insensitive `content-length:` parse; body read loop handles the case where the
  head read already pulled body bytes; response written with correct content-length +
  `connection: close` + shutdown → clean EOF. `reqwest` sends a small JSON body with
  `Content-Length` (no `Expect: 100-continue`, no chunked encoding), so the parser sees
  exactly the advertised shape. `data: [DONE]` is skipped by the SSE parser
  (line 1305), and the stream loop emits a fallback `Finish` at EOF (lines 815-824), so
  test A's `finished == true` / `errored.is_none()` assertions hold.
- **Flakiness** — LOW RISK: `127.0.0.1:0` ephemeral port (no collision); the accept loop
  serves responses strictly in order and the client's two POSTs are strictly sequential
  (the retry only starts after the 400 is received); request bodies are pushed to the
  `Arc<Mutex<Vec<..>>>` BEFORE the response is written, so `server.received()` never
  races the drain. Each test owns its server; a panicking test drops the spawned task
  with the tokio test runtime.

### Security

No new attack surface: the retry POSTs to the same `url` with the same
`Authorization: Bearer {api_key}` header — the key still goes only to the configured
`base_url`. 401/403 bodies (which may echo the key) remain suppressed in both the trace
and the user-visible error, on the original path and the retried path.

### Constitution

- No `#[allow(...)]` added; `let mut body` / `let mut response` are genuinely needed.
- New items carry doc comments; all are private test-module items.
- Regression tests for the defect present (test A reproduces the 400 rejection and
  asserts the self-healing behavior; B/C pin the no-retry cases).
- `check_response` is still used (openai.rs:206, vision.rs:187/234), so no dead code
  under `#![deny(warnings)]`. Plan step 2 ran `cargo test --lib provider::openai` green
  (zero warnings).

### Bookkeeping

- `.coding/backlog.json` — item #37 removed. The item bundled two asks: the
  browser-debugging restart hint and the embedder display-name rename. The rename is
  implemented in this diff; the browser-debugging half appears already addressed (a
  "…enable 'Agent browser inspection' … and restart the app" message exists at
  `src/browser/mod.rs:687`), and the plan file explicitly scopes item #37 as the
  resolved item. Removal is justified.
- `.coding/plans/stack.json` + the new `e7925f98-*.md` plan file — expected harness
  state (active plan). Nothing wrong.

## Findings

1. **Low (informational — no change required, deliberate design)** — Trace record on the
   fallback-success path shows the ORIGINAL request body (with `reasoning_effort: "max"`)
   but `http_status: 200` from the retried request; the trace never indicates a fallback
   retry occurred (`openai.rs:500` records the pre-removal body; line 600 sets the
   retried status). This is exactly what the plan specifies ("trace records only the
   final outcome") and is documented in the code comment at lines 538-541. If more
   fidelity is ever wanted, the retried body could be pushed into the record, but that
   is out of scope for this plan.

2. **Low (informational)** — The rejection test is a case-insensitive substring match
   (`openai.rs:569`); a 400 whose body merely *mentions* `reasoning_effort` for an
   unrelated reason would trigger one wasted request. Benign and bounded: the retried
   response's error is still reported correctly, and the retry cannot recur. This is
   the condition the plan specified verbatim.

## Minor observations (not findings)

- StubServer's reason phrase is hardcoded `"Bad Request"` for any non-200 status
  (openai.rs ~2775 in test module) — cosmetic only; HTTP/1.1 parsers ignore reason
  phrases.
- `buf[head_end..head_end + content_length]` would panic if a client closed mid-body
  (openai.rs test module) — unreachable with the well-behaved test client, so not a
  real risk.

## Conclusion

The diff is clean. The self-healing fallback is correct, bounded, and preserves all
existing diagnostics and security behavior; the tests genuinely exercise the network
path and would catch a regression of the fix; the embedder rename is a trivial string
change; bookkeeping is consistent. No blocking findings.
