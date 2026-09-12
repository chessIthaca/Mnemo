# Review — R5: byte-stable context footer for prompt-cache prefix reuse

**Date:** 2026-04-16
**Reviewer:** read-only subagent
**Scope:** ALL uncommitted working-tree changes (`git diff HEAD` + untracked):
`src/agent/prompt.rs`, `src/agent/turn.rs`, `src/agent/tests.rs`, plus
bookkeeping/analysis under `.coding/` (plan files, `stack.json`, `cache-hit-2-*`).

## Plan goal (as given)
The provider (DeepSeek) reuses the request byte-prefix only when the request's
LAST message is byte-identical to the previous request's last message. Fix:
append a byte-stable `CONTEXT_FOOTER` as the FINAL system message after the
volatile tail, so tail changes cost only the tail+footer to reprocess instead
of the whole cached context.

## Verification of each focus area (checked against the actual code, not just the diff)

### 1. Footer is a FIXED literal — PASS
`src/agent/prompt.rs:113`
```rust
pub const CONTEXT_FOOTER: &str = "<context footer — cache-stable sentinel, ignore>";
```
A plain `&str` constant. No `format!`, no timestamps, no interpolation, no
volatile content. Length ≈ 49 chars (within the asserted `<= 60`).

### 2. Push/pop order, no leak on any exit path — PASS
`src/agent/turn.rs:350-367`
- Push order: volatile tail first (350-356), then footer (357-363) → footer is
  the LAST message. ✅
- Pop order: footer first (365), then tail (366). ✅
- Both pops are unconditional statements executed **before** `let stream = stream?;`
  (367). The `?` is applied to the already-bound `stream` result after the pops,
  so an `Err` from `complete_with_retry` (all retries exhausted / MAX_RETRIES)
  still pops both. ✅
- Stream errors, interrupts, soft-stop: all occur inside the stream-consumption
  loop at line 384+, i.e. after the pops already ran. ✅
- No `?`/early-`return` exists between the pushes (350-363) and the pops
  (365-366). The `.await` on `complete_with_retry` only yields; it cannot return
  early. ✅
- Borrow safety: `complete_with_retry<'a>(&self, provider: &'a Arc<dyn LlmClient>,
  messages: &[Message], ...)` (`src/agent/dispatch.rs:401`) binds the returned
  `BoxStream<'a>` to `provider`, while `messages` is an anonymous shorter
  `&[Message]`. So the returned stream does not borrow `messages`, and popping
  after the call is sound. ✅
- No path retains tail/footer in the persistent `messages`. ✅

### 3. build_system_prompt back-compat wrapper unchanged — PASS
`src/agent/prompt.rs:183-197`. The diff only rewrites the doc comment; the body
(`prompt.push_str(&build_volatile_tail(caps, workflow, memories));` at 197) is
unchanged and deliberately excludes the footer. Callers (`search build_system_prompt`)
are all in `prompt.rs`'s own tests — no production caller affected.

### 4. Exactly one push site per request, no double-push — PASS
`search volatile_tail|CONTEXT_FOOTER|complete_with_retry` shows a single push of
`volatile_tail` (turn.rs:350) and a single push of `CONTEXT_FOOTER` (turn.rs:357),
both in the single turn-loop iteration before the one `complete_with_retry` call.
Each loop iteration re-pushes after the prior iteration's pops; no double-push.

### 5. Tests are meaningful — PASS
- `context_footer_is_stable_literal` (`prompt.rs:574`): asserts non-empty, no
  `WORKFLOW STATE` / `RECALLED MEMORIES` markers, `<= 60` chars, and that the
  footer is absent from both `build_stable_head` and `build_volatile_tail`
  outputs. The `assert_eq!(CONTEXT_FOOTER, CONTEXT_FOOTER)` is trivially true
  but harmless. ✅
- `volatile_tail_then_stable_footer_appended_and_popped` (`tests.rs:1181`): runs
  a real `run_turn` against `CapturingProvider`, asserts the provider's received
  LAST message equals `CONTEXT_FOOTER`, the head contains no footer, and after
  the turn the persistent `messages` contains neither the footer nor any
  `# WORKFLOW STATE` content. This genuinely observes the on-the-wire messages
  and verifies the pop. ✅
- `CapturingProvider::new()` is unchanged in behavior (delegates to
  `new_with_tails`, discards the tails handle), so existing tests are unaffected. ✅

### 6. Constitution compliance — PASS
- No `#[allow(...)]` suppressions added anywhere in the diff.
- `pub const CONTEXT_FOOTER` and `pub fn new_with_tails` both carry doc comments.
- Build is warning-free under `#![deny(warnings)]` per the plan's green
  `cargo test` (752 lib + integration).

## Bookkeeping / non-source changes
`.coding/plans/*.md`, `.coding/plans/stack.json`, and `.coding/analysis/cache-hit-2-*`
are plan bookkeeping and the analysis report that motivated R5. Nothing alarming;
`stack.json` reflects the new plan on the stack. The only git note is a benign
LF→CRLF warning on one plan file (consistent with the repo's line-ending policy).

## Minor observations (NOT findings — no action required)
- The `assert_eq!(CONTEXT_FOOTER, CONTEXT_FOOTER)` in `context_footer_is_stable_literal`
  is tautological (comparing the const to itself). It does not verify "two calls
  produce identical bytes" since a const has no call semantics. It is harmless and
  the other assertions carry the real guarantees. Not flagged as a finding because
  the const is definitionally byte-stable and the other assertions cover the
  documented constraints.
- On an actual panic inside `complete_with_retry` (which the dedicated test
  `complete_with_retry_returns_err_not_panic` proves does not happen), the pops
  would be skipped — but a panic unwinds out of `run_turn` entirely, so no
  persistent-state leak results. Not a finding.

## Findings by severity

**Correctness:** none.

**Bugs:** none.

**Security:** none.

**Constitution compliance:** none.

## Verdict
**No findings.** The diff is clean and correct. The footer is a fixed literal,
pushed after the tail and popped before any error propagation on every exit path,
the back-compat wrapper is behaviorally unchanged, there is exactly one push site
per request, both new tests meaningfully observe the wire messages, and no
`#[allow]` suppressions were added. The change matches the plan goal.
