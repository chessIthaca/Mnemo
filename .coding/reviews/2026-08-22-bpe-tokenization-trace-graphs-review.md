# Review: BPE tokenization fix + Trace-tab stats graphs

Branch: `fix/bpe-tokenization-trace-graphs`. Reviewed the full working tree (`git diff HEAD` + untracked files): `src/agent/context.rs`, `src/agent/turn.rs`, `src/runtime/channels.rs`, `frontend/src/components/views/LlmTraceView.tsx`, `frontend/src/components/views/TraceStats.tsx` (new), `frontend/src/lib/traceStats.ts` (new), `frontend/src/lib/traceStats.test.ts` (new), `frontend/vitest.config.ts`, `README.md` (+ `.coding/` bookkeeping files, out of scope).

Summary: the design is sound and the BPE per-message formula (4-token overhead + content + tool-call name/args) is preserved exactly from the old `try_tiktoken_count`. The turn-loop rewiring preserves emission order/values. One real correctness bug in the delta accounting: the standard fresh-session flow (user-first message list) triggers turn.rs's insert-at-0 system-head branch, which shifts indices and causes a permanent double-count of the last pre-insert message. Findings below.

---

## HIGH

### H1. `TokenAccounting` double-counts when a system head is *inserted* at index 0 (fresh sessions, post-`/clear` turns)

**Where:** `src/agent/context.rs:454-487` (`update`), `src/agent/context.rs:490-506` (`full_pass`), interacting with `src/agent/turn.rs:515-526` (the `messages.insert(0, ...)` branch).

**What happens:** `update` takes the incremental path only when `counted_len > 0`, the list didn't shrink, and `messages[0]` is `Role::System`. When the head is *not* System, `full_pass` records `counted_len = n` and `head_content = None` (context.rs:500-505). The turn loop then executes its head-prepend branch at turn.rs:515-526 — the *normal first-iteration flow* in production: `AgentRuntime.messages` starts as `Vec::new()` (src/runtime/agent.rs:71), user prompts are pushed as `Role::User` (agent.rs:202), and the unit tests confirm user-first lists (src/agent/tests.rs:149-156). The insert shifts every pre-existing message one index right.

On the next `update`, `messages[0]` is now System, so the incremental path runs:
- Head swap (context.rs:472-477): `head_content = None` → correctly adds `count(S)` to the system bucket (no subtraction needed — correct).
- Suffix loop (context.rs:480-484): `messages[self.counted_len..]` = `messages[n..]` — but the message at index `n` of the new list is the **old last message** (pre-insert index `n-1`), which was already counted in the previous full pass. It is counted a **second time**.

**Consequence:** from iteration 2 of every turn that entered with a user-first list (i.e., every fresh session's first turn — the common case, since tool-using first turns loop multiple times — and every turn after `/clear`), `breakdown.user` and the returned total are permanently inflated by one message's tokens. `used` emitted in `ContextUsage` disagrees with `ContextManager::count_tokens_and_breakdown` — the byte-for-byte parity the struct's doc comment claims (context.rs:414-418) — and the summarize threshold check at turn.rs:218 can fire slightly early. The next turn self-heals (a new `TokenAccounting` is created per turn, turn.rs:142), which is why this is bounded rather than accumulating session-wide.

**Why the tests miss it:** all four new context.rs tests start with a `Role::System` head or truncate a System-first list — none exercises a non-System head followed by growth. `context_usage_event_carries_cached_token_count` (src/agent/tests.rs:504-…) uses a single-response mock, so only iteration 1's (pre-insert) emission is observed.

**Fix suggestion:** in `full_pass`, when `messages.first()` is not `Some(Role::System)`, leave `counted_len = 0` (or set a flag) so the *next* `update` re-runs a full pass instead of trusting an index-shifted prefix — the head-insert transition happens exactly once per turn, so the cost is one extra full pass in turn 1. Alternatively, reset the accounting in turn.rs immediately after the `messages.insert(0, …)` at turn.rs:515-526. Add a regression test: `update([User "hi"])` → simulate the insert (push a System at index 0 via `insert`) + append an assistant message → `update` → assert total/breakdown equals `count_tokens_and_breakdown`. That test fails on the current code and passes with the fix.

**Related robustness note:** the incremental path's correctness relies on an undocumented invariant — the counted prefix must be *index-stable* (only in-place replacement of `messages[0]` is safe). An insert at index 0 while the head is already System (not done in turn.rs today) would trigger the same suffix double-count. Worth stating in the `TokenAccounting` doc comment.

---

## LOW

### L1. Fallback (non-BPE) total semantics changed vs HEAD

**Where:** `src/agent/context.rs:89-91` + `count_message_tokens` None arm (context.rs:532-540).

Old `count_tokens` fallback (`char_based_estimate`) computed `floor(Σ chars / 4)` once over the whole list; the new code sums per-message `floor(chars / 4)` (matching the old `count_tokens_by_role` fallback). Results differ when fractional remainders add up (e.g., two 2-char messages: old total 1, new total 0). This only matters if `try_tiktoken_bpe()` returns `None` — practically unreachable (the rank table is embedded), and the new behavior is *more* self-consistent (total = sum of buckets), but the doc comment's "byte-for-byte identical" claim (context.rs:414-418) is strictly false for the fallback path. No action strictly required; consider one clarifying sentence.

### L2. README overstates the complexity bound

**Where:** `README.md:51` — "so context counting stays O(1) per loop iteration".

Each `update` re-encodes the changed head plus the appended messages — O(head + new messages), not O(1). For Local-kind providers the volatile tail is folded into the head (turn.rs:508-511), so the head re-encode happens every iteration there. Minor wording nit; suggest "stays proportional to the appended messages (plus the rebuilt system head)".

### L3. `cacheHitPct` is unclamped — bar can exceed 100%

**Where:** `frontend/src/lib/traceStats.ts:69-72` + `TraceStats.tsx:223-225`.

`Math.round((cached / prompt) * 100)` can exceed 100 when a provider reports `cached > prompt`; the bar width (`width: ${r.hitPct}%`) is clipped visually by `overflow-hidden`, but the label renders e.g. "125%". Pre-existing behavior (the old local `cachePct` was identical), so not a regression — cosmetic only.

### L4. Chart row labels collide for same-second requests

**Where:** `frontend/src/lib/traceStats.ts:61-63` (`fmtLabel` → `toLocaleTimeString()`).

All three charts label rows only by local HH:MM:SS; several requests within the same second get identical labels with no other visible identifier. Cosmetic.

### L5. Missing regression test for the insert-at-0 transition

Covered under H1 — the constitution's regression-test rule needs a test that reproduces the user-first → insert → append sequence and fails on the current code.

---

## CLEAN AREAS (verified, no findings)

- **BPE formula parity:** `count_message_tokens` (context.rs:521-542) is byte-for-byte identical to the old `try_tiktoken_count` BPE path (4 + `encode_with_special_tokens` of content, tool-call names, arguments) and to the old `count_tokens_by_role` BPE path. `count_tokens_and_breakdown`'s total = sum of the four buckets, same as the old per-message sum.
- **Head-swap math for in-place replacement:** `system.saturating_sub(head_count) + new_head_count` (context.rs:474-475) is correct given the invariant that only the head was replaced; `saturating_sub` is a safe guard. Other System messages (summary message at index 1 after compaction) survive untouched.
- **Shrink/rewrite detection:** `messages.len() < counted_len` → full pass (context.rs:460) is correct; empty-list and empty→non-empty transitions are handled (counted_len 0 → full pass).
- **turn.rs rewiring:** update runs before the summarize check (turn.rs:211); the summarize branch resets + recomputes *after* `*messages = summarized` **and** the buffered-suggestion pushes (turn.rs:289-293), so the emitted `ContextUsage` reflects the post-rewrite list — same position as the old recompute. The else branch reuses the computed breakdown; there are no message mutations between `update` (turn.rs:211) and the emission (turn.rs:333) in the non-summarize path — the head rebuild (turn.rs:515-529) and the volatile-tail/footer push/pop (turn.rs:568-585, 661-664) happen *after* emission and net out before the next `update` (pops run before every early return). `Compacted` guard (`used < token_count`) semantics preserved; `did_summarize` is still used (turn.rs:847) — no dead code, no `#[allow]`.
- **Overflow:** per-message counts are u32 exactly as the old per-role path did; totals are summed in usize; no realistic u32/usize overflow. `as u32` casts of totals in turn.rs are pre-existing patterns.
- **`Copy` on `ContextBreakdown`** (channels.rs:45): all-u32 struct; additive trait; no callers relied on move semantics.
- **Frontend correctness:** division-by-zero guarded everywhere (`maxPrompt = Math.max(1, …)`, `totalGenMs > 0`, `prompt === 0 → null`); null/undefined handled (`?? 0`, `usage` null checks); React keys are stable (`r.id`); no hooks misuse; `TraceStats` returns `null` on empty rows. `LlmTraceView.test.ts` only imports `shouldStopPolling`, so removing the local `cachePct` breaks nothing (no remaining `cachePct` references anywhere). Test helper `row()` in traceStats.test.ts includes every required `LlmRequestSummary` field, and `ToolCall { id, name, arguments }` in the Rust test matches `src/provider/mod.rs:239-246`.
- **vitest registration:** `src/lib/traceStats.test.ts` added to `frontend/vitest.config.ts:46`. ✓
- **Constitution compliance:** all new pub items have doc comments (`TokenAccounting` + `new`/`reset`/`update`, `count_tokens_and_breakdown`, `traceStats.ts` exports, `TraceStats`); no `#[allow(...)]`; regression tests exist for the BPE-parity fix (H1 notes the one missing case). Build hygiene by inspection is warning-free.
- **Documentation sync:** `README.md` updated (both the stats graphs and the incremental accounting); `PLAN.md`'s context-management section describes counting generically and does not reference the removed `last_token_count`/`appended_since_last_count` heuristic — not stale. No doc finding.
- **Security:** the stats UI renders only derived numbers and local wall-clock timestamps — no `base_url`, request bodies, or secrets. The existing "Log to file" plaintext warning is unchanged.
- **Multi-platform neutrality:** nothing Windows-only in the Rust or frontend changes (`toLocaleTimeString`, flexbox, pure TS helpers). No finding.
