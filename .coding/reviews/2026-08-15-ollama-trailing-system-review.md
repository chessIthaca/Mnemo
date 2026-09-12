# Review: Ollama trailing-system-message fix (provider-conditional placement)

Date: 2026-08-15
Scope: ALL uncommitted working-tree changes (`git status` / `git diff HEAD`) — `src/agent/turn.rs`, `src/agent/tests.rs`, plus the incidental `.coding/backlog.json` / `.coding/plans/stack.json` bookkeeping and the untracked plan file.

## Verdict

**No findings.** The diff is clean and implements the stated goal correctly.

## Per-focus-point verification (checked against the actual code, not just the diff)

1. **Local path (turn.rs:386, 397–403, 456–471, 529–532).** `is_local` folds `volatile_tail` into `head_content` via `h.push_str(&volatile_tail)` and writes it to `messages[0]`; both trailing pushes and both pops are skipped. `build_volatile_tail` always emits the `# WORKFLOW STATE` header (prompt.rs:270), so the leading system message genuinely carries the workflow state and no system message ever follows the user message — satisfying Ollama's "system message must be at the beginning" constraint. The footer is entirely absent on Local. ✔

2. **OpenAI path.** When `is_local == false`, `head_content` passes through unchanged, the tail + `CONTEXT_FOOTER` are pushed in the original order and popped after the request — byte-for-byte the pre-change behavior. The existing test `volatile_tail_then_stable_footer_appended_and_popped` (tests.rs:1200–1286) still asserts footer-last and that both transients are popped. ✔

3. **`volatile_tail` ownership / warning-free.** `let head_content = if is_local { ... }` (397) shadows the `String` from line 343. `push_str` takes `&str` (a borrow), so on the Local path `volatile_tail` is never read again — but that is fine because the only other use is inside the `if !is_local` block (456–471), which is unreachable when `is_local`. Rust's unused-variable lint only fires when a variable is *never* used on any path, so no warning. On the OpenAI path `volatile_tail` is moved into the push at line 459. Consumed exactly once on each path — never both, never dropped-unused. `let mut h` is required for `push_str` (no unneeded `mut`). Compiles clean under `#![deny(warnings)]`. ✔

4. **Push/pop symmetry on every exit path.** The pops (529–532) run before `let stream = stream_result?` (560) and before the Interrupt/Cancel early return (536–551), and are guarded by the identical `!is_local` condition as the pushes — symmetric on success, provider error (retry-exhausted `Err`), interrupt, cancel, compact, clear, and channel-close. `complete_with_retry` takes `messages: &[Message]` (dispatch.rs:482–487) and returns `BoxStream<'a, LlmEvent>` borrowing `&'a Arc<dyn LlmClient>` (the provider), not the messages slice — so popping after the pinned future resolves is sound, matching the pre-existing safety comment at turn.rs:443–448. ✔

5. **Consistent `is_local` within an iteration.** The provider is snapshotted once per turn (turn.rs:79–86, with the mid-flight-swap-applies-next-turn comment at 56–59), and `is_local` is computed once per loop iteration from that snapshot (386). The same flag drives fold, push, and pop — no torn state. ✔

6. **Tests meaningful.** `local_provider_tail_folded_into_leading_system_message` (tests.rs:1288+) observes the actual wire messages: last captured message == "hi" (not the footer, not the tail), head contains both "plan-first workflow" and "# WORKFLOW STATE", no footer in the head or anywhere in the persistent vec, and `messages.len() == 3` with System/User/Assistant roles — proving nothing transient leaked. `CapturingProvider::new()` (1041) still delegates to `new_with_tails()` → `build(ProviderKind::OpenAI)`, so every pre-existing test (constitution_reread_after_agent_md_edit, etc.) keeps the OpenAI default and is unaffected. `ProviderKind` is imported at tests.rs:7 — no missing or unused imports introduced. ✔

7. **Constitution compliance.** Doc comments on the new `kind` field (tests.rs:1035–1037), `new_with_tails` (1046–1047), `new_local_with_tails` (1052–1054), and `build` (1059–1060). No `#[allow(...)]` suppressions added anywhere in the diff. No unused imports/fields/dead code/unneeded `mut`. The pre-existing `fn new()` at 1041 has no doc comment, but it is unchanged by this diff and sits in `#[cfg(test)]` mock helpers — not a new item. ✔

## Incidental (non-source) changes

`.coding/backlog.json` (new backlog item #33 describing this very change) and `.coding/plans/stack.json` (active-plan pointer swap + merge_to_main skill state removed) are bookkeeping artifacts of the workflow tool itself, plus the untracked plan file `.coding/plans/1cd07d18-….md`. Not code; no correctness/security impact.

## Nit (observation, not a finding — no change required)

`build_stable_head`'s doc comment (prompt.rs:115–122) still says the head "is the only content placed in `messages[0]`". On the new Local path the volatile tail is also folded into `messages[0]`, so that sentence is now OpenAI-specific. prompt.rs was deliberately out of scope for this change and the comment documents the OpenAI cache strategy rather than behavior, so it is not misleading in a way that affects the implementation — flagging only for awareness.
