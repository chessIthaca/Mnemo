## Verdict: FINDINGS (0 high, 2 low)

Verification review of commit `65422bf` on `feat/max-tokens-cap-repetition-model-switch`.
This is a re-review after the main agent acted on the first review
(`.coding/reviews/2026-12-r9-r10-model-switch-review.md`, 1 high / 2 medium / 5 low).

**All 8 original findings are correctly fixed in code.** The two LOW findings below
are *missing regression tests* for two of those bug fixes — a project-constitution
compliance gap, not a code-correctness defect. The shipped code is sound.

---

### Verified correct (all 8 original findings)

**HIGH 1 — `detect_repetition` multi-byte panic — `src/provider/openai.rs:1480-1502`** ✓

The boundary checks now precede EVERY slice, exactly as recommended:
```rust
let start = text.len() - needed;
if !text.is_char_boundary(start) { return false; }   // guard BEFORE &text[start..]
let tail = &text[start..];
if !tail.is_char_boundary(window) { return false; }   // guard BEFORE &tail[..window]
let segment = &tail[..window];
```
Both panic sites from the original review (the `&text[len-needed..]` slice and the
`&tail[..window]` slice) are now guarded. The regression test
`detect_repetition_handles_multibyte_utf8_without_panic` (line 1912) uses the exact
trigger from the review (`"é".repeat(300) + "a"` = 601 bytes, start=1 mid-é → false,
no panic) AND a genuine multi-byte repetition case (100×"é"=200-byte window, 300×"é"=600
bytes = 3× window, boundaries align → true). The test would panic without the fix and
passes with it. Correct and complete.

**MEDIUM 2 — Stale deferred swap override — `src/agent/loop_impl.rs:1013-1019`** ✓ (code)

The immediate-swap path now clears `pending_swap`:
```rust
self.set_provider(provider.clone(), context_manager.clone());
*self.pending_swap.lock().expect("pending_swap lock poisoned") = None;  // ← THE FIX
*self.explicit_provider.write()... = Some((provider, context_manager));
```
`set_provider` only touches `self.provider` / `self.context_manager` (never
`pending_swap`), so the clear is necessary and correctly placed. `set_explicit_provider`
is synchronous (no await points), so the three lock operations are atomic within a
tokio task — no race window. The stale-swap-override scenario (defer B → immediate C →
`take_pending_swap` returns None, not B) is correctly handled. **However, see LOW 1
below — no regression test covers this.**

**MEDIUM 3 — `run_turn` integration test — `src/agent/tests.rs:4559-4696`** ✓

`run_turn_completes_deferred_swap_after_summarization` genuinely exercises the
summarization path, not just the deferral decision:
- Deferred swap set up (200K→2K context, `has_pending_swap()` asserted true).
- Conversation sized to exceed the 80% threshold: 20 messages × ~450 chars of diverse
  English text ≈ ~2140 tiktoken tokens > 1600 (80% of 2K). If the count were ≤1600,
  summarization would be skipped and the `contains("## Conversation summary")`
  assertion would **fail** — so the test provably exercises the summarization branch.
- The `"## Conversation summary"` format is confirmed to exist in `context.rs:186,308`
  (`"## Conversation summary\n\n{summary_text}"`), so the assertion is meaningful.
- Asserts: turn ran on NEW provider (`outcome.text == "Hello from new model!"`),
  swap completed (`max_context == 2_000`), summary present, pending swap consumed
  (`!has_pending_swap()`). The OLD provider's queue (summary response) is consumed by
  `summarize_with_interrupt`; the NEW provider's queue (turn response) by the turn.
  Correct and complete.

**LOW 4 — Dangling `CompactStarted` on interrupt — `src/agent/turn.rs:148-163`** ✓ (code)

The interrupt path (`stop_reason.is_some()`) now emits a paired `AgentEvent::Error`
("Compaction interrupted — original conversation kept, model switch completed.") before
completing the swap and returning early. This mirrors the main summarization path's
2026-08-22 interrupt fix. The message is more precise than the main path's (adds "model
switch completed") because the swap IS completed on this path (`set_provider` +
`explicit_provider` write before `return`). On interrupt,
`summarize_with_interrupt` returns the original messages, so `*messages = summarized`
(line 122) is a no-op — no data loss. **However, see LOW 2 below — no regression test
covers the interrupt path.**

**LOW 5 — `did_summarize` not set after model-switch summarization — `src/agent/turn.rs:67,123`** ✓

`did_summarize` is now declared at the top of `run_turn` (line 67, before the model-switch
block at line 72) and set to `true` at line 123 after `*messages = summarized`. The flag
is consumed at line 1109 to gate the `cached ≈ min(prev, curr)` cache heuristic — when
true, it reports 0 cached tokens instead of inflating. The comment at line 278 documents
the rationale. On the error path (summarization failed, line 104-120), `did_summarize` is
also set to true; this is correct because the provider still changes (swap completes
regardless), invalidating the cache prefix — reporting 0 is conservative and accurate.
On the interrupt path, the function returns early (line 173) before `did_summarize` is
read, so the true value is harmless. The MEDIUM 3 integration test exercises this path
(summarization succeeds, `did_summarize = true`), so the fix is covered indirectly.
Correct and complete.

**LOW 6 — Doc says "chars / 4" but uses byte length — `src/provider/mod.rs:268-275`** ✓

The doc comment now reads "byte-based heuristic (bytes / 4 + 4 per message + tool-schema
bytes / 4)" and explains that multi-byte UTF-8 overestimates more. The implementation
uses `String::len()` (bytes), matching the doc. (Minor cosmetic note: the local variable
is still named `chars` and the inline comment at line 299 says "chars/4", but this is a
pre-existing naming choice, not part of this change, and the substantive doc comment is
now accurate.)

**LOW 7 — Repetition detector false-positive risk — `src/provider/openai.rs:117-128`** ✓

The `REPETITION_THRESHOLD` doc comment now includes a "Known limitation" paragraph
documenting that legitimate 600+ byte runs of identical 200-byte segments (markdown rules,
table separators, ASCII art, repeated data records) would be aborted, accepted as a
tradeoff, with a future allowlist refinement noted. Matches the review's request.

**LOW 8 — 1024 floor can still exceed context window** ✓

Accepted per plan, no code change required.

---

### No new issues introduced

- No `#[allow(...)]` added; no unused imports/variables; all new public items documented.
- No platform-specific APIs (pure cross-platform Rust).
- No README/PLAN/endpoints.toml docs affected (backend-internal mechanics; the 64K→32K
  default is a fallback, not a user-facing config knob).
- The `did_summarize = true` on the model-switch error path is correct (provider changes
  → cache prefix invalidated → reporting 0 is accurate, not an over-report).
- `set_explicit_provider` is synchronous; the three lock operations in the immediate path
  are atomic within a tokio task — no new race.

---

### LOW findings (missing regression tests)

**LOW 1 — No regression test for MEDIUM 2 (stale deferred swap override) — `src/agent/loop_impl.rs:1013-1019`**

The project constitution requires: "For every defect (bug) you fix, add a regression test
that reproduces the defect and asserts the fix. The test must fail without the fix and pass
with it." MEDIUM 2 is a bug fix (stale deferred swap silently overrides a later immediate
swap), but no test covers the deferred-then-immediate sequence. The two existing
`set_explicit_provider` tests each do a *single* swap:
- `set_explicit_provider_immediate_when_context_same_or_larger` (line 4414): one immediate
  swap, no prior deferred swap — removing the `pending_swap = None` line would not fail it.
- `set_explicit_provider_deferred_when_context_smaller` (line 4482): one deferred swap,
  then `take_pending_swap` — also doesn't test the override-clearing.

**Required test:** defer a swap (smaller-context model B), then call `set_explicit_provider`
with a larger/equal-context model C (immediate path), then assert `!has_pending_swap()`
and `provider().model() == C`. Without the fix, `has_pending_swap()` would be true (stale
B) and `take_pending_swap()` would later return B, overriding C.

**LOW 2 — No regression test for LOW 4 (interrupt `CompactStarted` pairing) — `src/agent/turn.rs:148-163`**

LOW 4 is a defect fix (dangling `CompactStarted` event on the model-switch interrupt path),
but no test triggers an interrupt during model-switch summarization. The MEDIUM 3
integration test (`run_turn_completes_deferred_swap_after_summarization`) does not send an
interrupt command, so it exercises only the non-interrupt path — the paired `Error` event
added at lines 153-163 is untested.

**Required test:** set up a deferred swap + oversized conversation (as in the MEDIUM 3
test), push an interrupt/steer command into `cmd_tx` before calling `run_turn`, then assert
the turn returns early with `stop_reason: Some(...)` and that a paired `Error` event
("Compaction interrupted...") was emitted on `fanin_rx` (matching the `CompactStarted`).
Without the fix, the `CompactStarted` would be dangling (no paired end event).

---

### Note on verification

I could not run `cargo test` (read-only reviewer). The main agent confirmed 1497 tests
pass with 0 warnings. The HIGH 1 regression test and MEDIUM 3 integration test are
well-constructed and would fail without their respective fixes. The two LOW findings above
are test-coverage gaps, not code defects — the shipped code is correct.
