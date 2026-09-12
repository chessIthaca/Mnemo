# Review: Prompt cache-hit optimization (R1/R2/R3) — split stable head from volatile tail

**Date:** 2026-08-13
**Reviewer:** read-only background subagent
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD` + `git status --short`).
**Plan:** Prompt cache-hit optimization (R1/R2/R3) — `.coding/plans/7512c6f7-2ffb-423d-9988-957d80929505.md`

## Files changed
- `src/agent/prompt.rs` — split `build_system_prompt` into `build_stable_head` + `build_volatile_tail` + back-compat wrapper; 3 new tests.
- `src/agent/turn.rs` — `messages[0]` holds stable head only; volatile tail pushed as trailing system msg after conversation history, popped before `?`.
- `src/tool/mod.rs` (R2) — `schemas()` output sorted by name; 1 new test.
- `src/memory/mod.rs` (R3) — recall sort gains deterministic title tiebreaker.
- `.coding/plans/1a378eea-*.md` + `.coding/plans/stack.json` — bookkeeping (B4 plan checkbox + stack swap). Untracked: `.coding/plans/7512c6f7-*.md` (this plan's file).

## Verdict
**No findings requiring a fix.** All focus-area invariants hold. One low-severity informational note (not a defect).

---

## Correctness

### C1 — Volatile tail is ALWAYS popped (no leak into persistent messages) ✓
**Critical invariant verified.** Push at `src/agent/turn.rs:342-348`, pop at `src/agent/turn.rs:350`, `?` at `src/agent/turn.rs:351`. The pop runs unconditionally before the `?`, so it executes on BOTH the Ok and Err paths of `complete_with_retry`.

Traced **every** exit path in the loop body that follows the pop (all operate on `messages` WITHOUT the tail):
- `return Ok` Cancel — turn.rs:514 ✓ (after pop)
- `return Ok` soft-stop mid-stream — turn.rs:573 ✓
- `return Ok` interrupted — turn.rs:597 ✓
- `return Err` stream error (no output) — turn.rs:613 ✓
- `return Ok` MAX_RETRIES bad JSON — turn.rs:653 ✓
- `continue` bad-JSON retry — turn.rs:711 ✓
- `return Ok` no tool calls — turn.rs:725 ✓
- `return Ok` soft-stop after tool batch — turn.rs:895 ✓
- `return Ok` MAX_RETRIES tool errors — turn.rs:925 ✓
- implicit loop continuation — turn.rs:934 ✓

**Summarization never sees the tail:** the summarization block (turn.rs:174-232) runs at the TOP of the loop iteration, ~168 lines BEFORE the tail push (turn.rs:342). The tail is pushed and popped entirely within the "stream the completion" phase, after summarization + auto-recall + system-message setup. Confirmed.

**No off-by-one:** `messages.push(...)` then `messages.pop()` — symmetric single push/single pop. The tail is the last element when popped (nothing is pushed between push and pop). Confirmed.

### C2 — Tail push/pop doesn't deadlock or double-borrow ✓
The workflow mutex (`self.workflow.lock().await`) is scoped to the block at `turn.rs:292-298` and dropped at the block's close (line 298), **before** the tail push at line 342. No lock is held across the push / `complete_with_retry` / pop sequence. The `messages` Vec is a local (`&mut messages`), not behind a lock — no double-borrow possible. Confirmed.

### C3 — Summarization still clones the stable head correctly ✓
`context.rs:77` (`summarize`) and `context.rs:157` (`summarize_with_interrupt`) both do `let system = &messages[0];` then `result.push(system.clone())` (context.rs:126). With `messages[0]` now holding the smaller stable head (preamble + constitution, no workflow/memories), the result is still a valid `system + summary + recent` list and the head is preserved verbatim. The head being smaller is semantically fine — summarization only needs the constitution/preamble as the persistent system context; the volatile workflow/memories were never part of the summarized prefix anyway (they were regenerated each turn). **No change to context.rs is needed** — confirmed by reading both functions (context.rs:66-130, 142-169). ✓

### C4 — Schema sort (R2) doesn't change WHICH tools are filtered ✓
`src/tool/mod.rs:355-380`: the filter (`if !included { continue }`) runs at line 363, INSIDE the `for` loop and BEFORE the `out.sort_by(...)` at line 378. The sort only reorders the already-filtered `out` Vec — the same set of tools is returned, just in alphabetical order. The `plan_mutations_allowed` retain() at `turn.rs:322-329` runs after `schemas()` returns and operates on the sorted Vec via `retain` (which preserves relative order of kept elements) — still correct. ✓

### C5 — Recall title tiebreaker (R3) ✓
`src/memory/mod.rs:592-597`: `b.score.partial_cmp(&a.score).unwrap_or(Equal).then_with(|| a.memory.title.cmp(&b.memory.title))`. Primary sort is score descending (`b` vs `a`); `then_with` only breaks ties. NaN safety preserved — `partial_cmp` returns `None` for NaN, `unwrap_or(Equal)` falls back, and `then_with` is only evaluated when the primary comparator returns non-Equal (so a NaN-vs-NaN pair yields Equal and the title tiebreaker runs, which is deterministic). `Memory.title` is `String` (types.rs:58), so `.cmp()` is a total order — no panic. ✓

### C6 — `complete_with_retry` retry loop sees the tail across all 3 attempts ✓
`src/agent/dispatch.rs:409-419`: the retry loop calls `provider.complete(messages, ...)` up to 3 times. `messages.pop()` (turn.rs:350) runs only AFTER `complete_with_retry` returns (the whole retry sequence), so all 3 attempts see the tail on `messages`. Correct — the tail must be present for every attempt so the model has the workflow context on retries. ✓

### C7 — Stream lifetime does not borrow `messages` ✓
`LlmClient::complete` (`src/provider/mod.rs:317-322`) returns `BoxStream<'_, LlmEvent>` where `'_` is `&self` (the provider), NOT the `messages: &[Message]` slice. The messages are serialized into the request body inside `complete` and the borrow ends when the future resolves. Therefore `messages.pop()` after `.await` is sound — the borrow checker accepts it (and `cargo test` passes with zero warnings, confirming compilation). The inline comment at turn.rs:339-341 documents this correctly. ✓

---

## Bugs
**No findings.**

- The tail message cannot be visible to the model as a persistent conversation turn: it is pushed and popped within a single loop iteration and never written to the persistent `messages` Vec that survives across iterations/turns (verified in C1).
- No off-by-one in push/pop (C1).
- Retry semantics correct (C6).

---

## Security
**No findings.**

- No new secret-logging introduced. The tail contains workflow state + recalled memories — this content was already present in the system prompt before this change (just relocated from `messages[0]` to a trailing message). No sensitive data is newly exposed; if anything the surface is identical.
- No new file reads, network calls, or shell invocations added.
- The schema sort and recall tiebreaker are pure reordering — no security implications.

---

## Constitution compliance
**No findings.**

- **Doc comments on new public functions:** `build_stable_head` (prompt.rs:42-49), `build_volatile_tail` (prompt.rs:71-77), and the `build_system_prompt` back-compat wrapper (prompt.rs:100-107) all carry doc comments. ✓
- **No `#[allow(...)]` suppressions added:** searched `src/` for `#[allow(` — the only match is the **pre-existing** `#[allow(clippy::too_many_arguments)]` on the two `AgentLoop` constructors in `src/agent/loop_impl.rs:174,211` (a clippy style lint, not a rustc warning, predates this change, untouched). No new `#[allow]` in any of the four changed source files. ✓
- **Warning-free build under `#![deny(warnings)]`:** the main agent reports `cargo test` passes (730 lib tests + integration tests, zero warnings). The new code introduces no unused imports/dead code — `build_system_prompt` (the wrapper) is still called by the 5 existing test sites, `build_stable_head`/`build_volatile_tail` are called by production (turn.rs) + the 3 new tests, the sort/tiebreaker additions use existing fields. ✓
- **Line-ending style:** the git `LF will be replaced by CRLF` warning is on `.coding/plans/1a378eea-*.md` (a bookkeeping markdown file), NOT on the Rust source files. The four `.rs` files show no line-ending warnings. Existing style preserved. ✓

---

## Bookkeeping changes (not part of the plan's source changes — reviewed for completeness)
- `.coding/plans/1a378eea-b3a4-4137-8930-bb7f8863c651.md`: step 4 checkbox flipped `[ ]` → `[x]` (B4 plan marked complete). Consistent with the B4 work being done.
- `.coding/plans/stack.json`: active plan id swapped from `1a378eea-...` (B4) to `7512c6f7-...` (this cache-hit plan). `"reviewed":true` preserved. Correct — reflects the plan stack transition.
- Untracked `.coding/plans/7512c6f7-2ffb-423d-9988-957d80929505.md`: this plan's own file (steps 1-5 checked, step 6 unchecked — the review/commit step in progress). Consistent.

No issues with the bookkeeping changes.

---

## Informational note (low severity — NOT a defect, no fix required)
The volatile tail is now sent as a **trailing `Role::System` message after the conversation history** (turn.rs:342-348). This is a slightly unusual message ordering (system messages conventionally lead). The OpenAI Chat Completions API accepts system messages at any position, and DeepSeek (the provider in the trace data) handles it correctly — the trace analysis in `.coding/analysis/cache-hit-analysis.md` and the projected ~98.5% hit rate assume exactly this placement (tail after history = cache-immune). This is the intended design and is correct. Flagged only for awareness: if a future provider is added that rejects non-leading system messages, this placement would need revisiting. No action needed now.

---

## Summary
The R1/R2/R3 changes are correct, complete, and constitution-compliant. The critical invariant (volatile tail always popped, never leaking into the persistent conversation) holds on every exit path. The schema sort and recall tiebreaker are pure reordering with no semantic change. No `#[allow]` suppressions, no warnings, doc comments present, line-ending style preserved. **No findings requiring a fix.**
