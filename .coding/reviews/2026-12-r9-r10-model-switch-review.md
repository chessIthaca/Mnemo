## Verdict: FINDINGS (1 high, 2 medium, 5 low)

Review of all uncommitted changes on `feat/max-tokens-cap-repetition-model-switch`
(R9 max_completion_tokens cap, R10 repetition detector, model-switch summarization).
Scope: `git diff HEAD` over the 8 source files + 1 test file.

---

### HIGH

**1. `detect_repetition` panics on multi-byte UTF-8 — `src/provider/openai.rs:1473-1490`**

The char-boundary guard is placed AFTER the slice that can panic, so it is dead code
in the panic path:

```rust
fn detect_repetition(text: &str, window: usize, threshold: usize) -> bool {
    let needed = window * threshold;
    if text.len() < needed { return false; }
    let tail = &text[text.len() - needed..];          // ← PANIC #1: slices first
    if !text.is_char_boundary(text.len() - needed) {  // ← guard, too late
        return false;
    }
    let segment = &tail[..window];                     // ← PANIC #2: byte 200 may be mid-char
    tail[window..].as_bytes().chunks_exact(window).all(|c| c == segment.as_bytes())
}
```

`&text[text.len() - needed..]` (line 1478) panics whenever `text.len() - 600` is not a
UTF-8 char boundary. `&tail[..window]` (line 1485) panics whenever byte 200 of `tail` is
mid-character. The comment claims "window is always a char boundary in practice (we pass
200)" — but 200 is a *byte* offset, not a char count, so it is NOT guaranteed to be a
boundary when the text contains multi-byte UTF-8.

`response_text` accumulates `TextDelta` content from the model, which routinely contains
multi-byte chars (em dashes —, smart quotes, ellipsis …, emoji ✅🎯, arrows →, CJK). The
detector runs on **every** `TextDelta` once `response_text.len() >= 600`, so this is the
streaming hot path. A panic in the `tokio::spawn`'d stream task (no `catch_unwind`)
aborts the task, drops `tx`, and the consumer sees a truncated/errored stream — breaking
normal generation of any non-ASCII output. The guard meant to *prevent* token waste
instead *crashes* legitimate responses.

The unit tests use ASCII only (`"x".repeat(200)`, `"loop body ".repeat(20)`), so `cargo
test` passes and the bug is latent. Concrete trigger: `text = "é".repeat(300) + "a"` (601
bytes) → `text.len() - 600 = 1`, which is mid-`é` → panic.

**Fix:** check boundaries BEFORE slicing, or use non-panicking `get`:
```rust
let start = text.len() - needed;
if !text.is_char_boundary(start) { return false; }
let tail = &text[start..];
if !tail.is_char_boundary(window) { return false; }
let segment = &tail[..window];
```
Add a regression test with multi-byte content (e.g. `"é".repeat(301)`) that must return
`false` (not panic).

---

### MEDIUM

**2. Stale deferred swap overrides a later immediate swap — `src/agent/loop_impl.rs` `set_explicit_provider`**

The immediate-swap path (when `new_max >= current_max`) calls `set_provider` + writes
`explicit_provider` but does **not** clear `pending_swap`. `set_provider` (lines 958-968)
only sets `self.provider` and `self.context_manager` — it never touches `pending_swap`.
This creates a stale-swap override:

1. User switches to a **smaller** model B → deferred: `pending_swap = Some(B)`, provider
   stays A (the deferral `return`s before `set_provider`).
2. Before any turn runs, user switches to a **larger/equal** model C → immediate swap:
   `set_provider(C)`, `explicit_provider = C`, but `pending_swap` **still holds B**.
3. Next `run_turn` calls `take_pending_swap()` → returns B → summarizes + completes the
   swap to B, overriding C.

The IPC emitted `ModelChanged { model: C }` (UI shows C) but the agent actually runs on B.
The user's latest choice is silently overridden and the toolbar label is wrong. Trigger:
two model switches with no turn in between (e.g. idle user clicking the picker twice).

**Fix:** clear `pending_swap` in the immediate path:
```rust
*self.pending_swap.lock().expect("pending_swap lock poisoned") = None;
self.set_provider(provider.clone(), context_manager.clone());
```

**3. Missing `run_turn` integration test (plan step 3, test (c)) — `src/agent/tests.rs`**

Plan step 3 required test (c): "`run_turn` with pending_swap + oversized conversation →
summarize called with old provider, then swap completes." Only tests (a) and (b) —
covering `set_explicit_provider`'s deferral *decision* — were added. The riskiest path,
`run_turn`'s pending-swap consumption (summarization with the old provider, swap
completion, interrupt early-return, error recovery, event pairing), has **zero** test
coverage. This is exactly where findings #2, #4, and #5 live; a run_turn test asserting
the event sequence + final provider would have caught them.

---

### LOW

**4. Dangling `CompactStarted` on the model-switch interrupt path — `src/agent/turn.rs:91, 141-159`**

The model-switch path emits `CompactStarted` (line 91) but on interrupt
(`stop_reason.is_some()`) returns early (lines 153-158) with no paired end event. The main
summarization path was fixed in the 2026-08-22 review to emit an interrupt `Error` note
("Compaction interrupted — original conversation kept.", lines 512-524) pairing the
`CompactStarted`. The model-switch path didn't replicate this, leaving a dangling
"Compacting context…" transcript entry when the user interrupts during a model-switch
summarization. (Note: on interrupt `summarize_with_interrupt` returns the original
messages, so `*messages = summarized` is a no-op — no data loss, just the dangling UI
entry.)

**5. `did_summarize` not set after model-switch summarization — `src/agent/turn.rs`**

The main summarization path sets `did_summarize = true` (line 446) to gate the
`cached ≈ min(prev_prompt, curr_prompt)` cache heuristic (the prefix changed, so there is
no large shared-prefix cache). The model-switch path rewrites `*messages` (line 116) but
never sets `did_summarize` — it can't, because `did_summarize` is declared at line 262,
*after* the model-switch block (lines 62-183). So the first request after a model-switch
summarization applies the cache heuristic despite a changed prefix, potentially
over-reporting cached tokens for one request. Fix: declare `did_summarize` before the
model-switch block and set it there.

**6. `estimate_prompt_tokens` doc says "chars / 4" but uses byte length — `src/provider/mod.rs:268-294`**

The implementation uses `m.content.as_text().len()`, which is **byte** length
(`String::len()`), not char count. For ASCII this is identical; for non-ASCII (CJK, emoji)
bytes > chars, so it overestimates more than the doc's "overestimates slightly" implies.
Still safe for capping (an overestimate only tightens the cap), but the doc is inaccurate.
Either update the doc to "bytes / 4" or use `.chars().count()`.

**7. Repetition detector false-positive risk on legitimate repeated content — `src/provider/openai.rs` (`REPETITION_WINDOW=200`, `REPETITION_THRESHOLD=3`)**

window=200/threshold=3 fires on any 600+ byte run of an identical 200-byte segment at the
tail. Legitimate content can trigger this: a 600+ char markdown horizontal rule
(`---…`), wide table separators, ASCII-art dividers, or repeated identical data records
would be aborted mid-generation. Accepted tradeoff per the plan, but worth documenting as
a known limitation (and a candidate for an allowlist of high-repetition patterns).

**8. The 1024 floor can still exceed the context window — `src/provider/openai.rs:1258-1263`, `src/provider/anthropic.rs:243-248`**

When the prompt is within ~1024 of `max_context`, the `.max(1024)` floor overrides the
sub-1024 cap, so `input + 1024 > max_context` and the request can still fail at the
provider with a context-length error. Accepted per the plan ("never floors below 1024"),
but the cap does not fully prevent failures on near-limit prompts — it only prevents the
*output* from blowing the window, not an already-oversized prompt.

---

### Verified correct (focus points)

- **R9 cap formula** — `max_output_tokens.max(4096).min(max_context - prompt_est - 1024).max(1024)`
  applied identically in both `openai.rs` and `anthropic.rs`. The conservative
  `prompt_est` (overestimate) only tightens the cap, never loosens it. Floor at 1024
  confirmed (never below). Fixes the reported 14 failures (deepseek 131K+131K>262K → now
  32K output; qwen 131K>65K → now 32K).
- **`Cow` deref** — `estimate_prompt_tokens(&messages, tools)` in openai.rs: `messages`
  is `Cow<[Message]>` after the Local-sanitize branch; `&Cow<[Message]>` deref-coerces to
  `&[Message]` via `Cow: Deref<Target=[Message]>`. Correct. (anthropic.rs passes the raw
  `&[Message]` param directly — also correct.)
- **R10 stream wiring** — `response_text` accumulates only `TextDelta` (not
  `ReasoningDelta`/tool calls); the guard fires only when `is_text_delta`; the stream is
  broken with `LlmEvent::Error` + `return` (no panic in the success path, `tx` properly
  used). The `is_text_delta` borrow ends before the `tx.send(event)` move — correct.
- **Model-switch deferral** — `new_max < current_max` (strict, not `<=`) ✓; deferred path
  stores all three fields (`provider`, `context_manager`, `model`) ✓; `take_pending_swap`
  uses `.take()` (clears mutex) ✓; `run_turn` snapshots the OLD provider/context_manager
  for summarization, then completes the swap via `set_provider` + `explicit_provider`
  write (mirrors the non-deferred `set_explicit_provider` path, avoids re-triggering
  deferral) ✓; `pending_swap` is `std::sync::Mutex`, locked briefly with no `await` while
  held → no deadlock ✓; `SwapOutcome` enum used correctly in `set_model` (NotFound /
  Swapped / Deferred) ✓; both constructors (`new`, `with_constitution_source`) init
  `pending_swap: None` ✓.
- **No `#[allow(...)]`** in new code (the pre-existing `#[allow(clippy::too_many_arguments)]`
  on the two constructors is not part of this change). All new public items have doc
  comments. No platform-specific APIs (pure cross-platform Rust).
- **Documentation sync** — backend-internal mechanics; no README/PLAN/endpoints.toml
  examples affected (the 64K→32K default is a fallback, not a user-facing config knob).
  No multi-platform concerns.

### Note on verification

I could not run `cargo test` (read-only reviewer). The HIGH finding (#1) compiles cleanly
and passes the ASCII-only unit tests, so a green `cargo test` does NOT rule it out — it
must be fixed and a multi-byte regression test added before commit.
