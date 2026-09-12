## Verdict: FINDINGS (0 high, 1 low)

Review of plan ee33d615 ("Anthropic conversation-history caching (3rd breakpoint)") — all uncommitted changes on wt/agenticcoding vs HEAD. **The only source file changed is `src/provider/anthropic.rs`** (verified via `git diff HEAD` + `git status`); the remaining changes are `.coding/` bookkeeping (backlog status flips, the D1 plan file's post-commit regression-test section, untracked D1 knowledge files, this plan's file) — app-managed, not source.

The core change is correct, and — the load-bearing property — **cache-correct**: the breakpoint sits on the last real block BEFORE the appended tail blocks, and the tail is re-appended only to the current final message at build time (never baked into stored history), so the cached prefix is byte-stable across turns and the next request's 20-position lookback finds the prior write. Every docs claim in the comments verifies against the live-fetched Anthropic documentation (prompt-caching + handle-tool-calls pages, fetched during this review). One LOW finding: PLAN.md's "Prompt caching strategy" section still describes the old shape and needs a sync update.

**Verification basis:** full `git diff HEAD` + `git status` inspected; `src/provider/anthropic.rs` read directly (`build_request_json` :229-427, `message_to_json` :385-570, `raw_content_is_usable` :856-885, `complete` :1155+, test module :1778-2468); turn-loop scaffolding read (turn.rs :2117-2296, the tail + CONTEXT_FOOTER push sites); PLAN.md :158-179 read; Anthropic docs fetched live (platform.claude.com/docs/en/build-with-claude/prompt-caching and /agents-and-tools/tool-use/handle-tool-calls). Implementer-recorded `cargo test --workspace` green (2278 lib + 16 + 293 + 4 + 2, zero warnings under `deny(warnings)`); not re-run by this read-only reviewer — the deny(warnings) gate makes the recorded green run the warning-free proof.

## 1. Scope verification

`git diff HEAD --stat`: `.coding/backlog.jsonl` (4 lines — this plan's backlog item fe3fcd55 marked "interrupted by the user, returned to queue"; the D1 item 959b669f flipped in_flight→done), `.coding/plans/814f27c9.md` (+3 — the D1 regression-test section appended by the finish gate after the D1 commits), `src/provider/anthropic.rs` (282 lines — the change). Untracked: `.coding/knowledge/bug/814f27c9.md` + `.coding/knowledge/spec/2027-01-07-user-cancels-are-stamped-cancelled-not-provider.md` (D1 finish artifacts) and `.coding/plans/ee33d615.md` (this plan). **The only source file changed is `src/provider/anthropic.rs` — the task's claim verifies.** The `.coding/` delta is expected bookkeeping, not review scope.

## 2. Correctness of the relocation + breakpoint placement

Verified by direct code walk of `build_request_json` (:229-427), `message_to_json` (:385-570), `raw_content_is_usable` (:856-885), and the turn-loop scaffolding (turn.rs :2117-2296):

- **Empty `tail_blocks`**: the append loop pushes nothing; the breakpoint still lands on the last real block. Pinned by `breakpoint_lands_on_final_block_when_no_volatile_tail`. ✔
- **The `expect("message content is always an array")`**: safe — every entry in `messages_json` comes from `message_to_json`, whose every return path emits `"content": <array>` (user: blocks vec with the empty-text fallback; assistant raw-echo: `Value::Array(sanitized)`; assistant field-built: blocks vec, errors when empty; tool: single-block array). `rest.is_empty()` is rejected earlier (:277-281), so `messages_json.last_mut()` is always `Some`. ✔
- **Assistant-final messages**: `last_mut()` is role-agnostic; the raw-echo tests confirm the breakpoint lands on the assistant's last markable block. In production the final non-system message is always user/tool (the scaffolding pushes the tail AFTER the final user message, turn.rs :2295-2296), so assistant-final + non-empty tail never co-occurs via the agent loop — and appending text blocks to an assistant message is valid shape regardless. ✔
- **All-thinking final message**: `markable` = `None` → the breakpoint is skipped entirely (no panic, no wrong stamp). This also resolves the 2026-12-21 review's L2 (the old `attach_ephemeral_cache_control` could stamp a thinking block). Pinned by the updated `build_request_json_echoes_raw_content_array_verbatim`, which now explicitly asserts thinking/redacted_thinking carry no `cache_control`. ✔
- **Borrow/move**: `markable` (an `Option<&mut Value>` from `content.iter_mut().rev().find(...)`) is consumed by the `if let`; NLL releases the borrow before `content.push(b)`. Compiles; tests green. ✔
- **Blank system texts** are skipped (`trimmed.is_empty() → continue`), so no empty text blocks are ever appended (docs: "Empty text blocks cannot be cached"). ✔
- **Coalescing interaction**: the carrier fold guard checks `last["content"][0]["type"] == "tool_result"` — the FIRST block; appended tail blocks at the end don't affect it. A text message after a carrier still starts its own message (`tool_results_separated_by_text_do_not_coalesce` stays green). ✔

## 3. The critical cache-correctness property (why this produces reads, not just writes)

The design's load-bearing invariant, verified against the docs' "How automatic prefix checking works" + "Example: Lookback in a growing conversation":

- Request N's wire: `[..., U_N: [text_N (BREAKPOINT), tail_N...]]`. The cached entry = hash of the prefix ending at `text_N` — the tail blocks sit AFTER the breakpoint, excluded from the cached prefix.
- Request N+1's wire: `[..., U_N: [text_N], A_N: [...], T_N: [...], U_{N+1}: [text_{N+1} (BREAKPOINT), tail_{N+1}...]]` — the tail is re-appended only to the CURRENT final message; `U_N` appears with content `[text_N]` only, byte-identical to request N's cached prefix.
- So the walk-back from request N+1's breakpoint (docs: "it walks backward one block at a time, checking whether the prefix hash at each earlier position matches something already in the cache") finds request N's write at `text_N`. The distance is [T_N tool_result run (1 position)] + [A_N blocks (thinking 1 + text 1 + tool_use run 1 ≈ ≤3)] + [U_{N+1} earlier real blocks (usually 0)] ≈ ≤5 positions — well inside the 20-position window even for large parallel batches (runs coalesce). The code comment's enumeration ("assistant reply + coalesced tool_use / tool_result runs + new content") is accurate — the previous request's tail blocks do NOT intervene, because they are never baked into stored history.

This is exactly the docs' documented multi-turn pattern ("During each turn, the final block of the final message is marked with cache_control so the conversation can be incrementally cached"), adapted so the breakpoint sits just before our varying suffix — the docs' "static prefix + varying suffix" rule: "place the breakpoint at the end of the static prefix, not on the varying block."

## 4. Docs-claims verification (fetched live during this review)

From platform.claude.com/docs/en/build-with-claude/prompt-caching:

- **20-position lookback**: "The lookback window is 20 blocks. The system checks at most 20 positions per breakpoint, counting the breakpoint itself as the first." ✔
- **Run coalescing**: "a run of consecutive tool_use blocks counts as one position, and so does a run of consecutive tool_result blocks, so a turn with many parallel tool calls doesn't push the previous request's entry out of the window on its own." ✔
- **Thinking blocks**: "Thinking blocks cannot be cached directly with cache_control." ✔ (`redacted_thinking` isn't named; treating it as unmarkable is the correct conservative reading — same block family.)
- **Automatic caching**: "Add a single cache_control field at the top level of your request. The system automatically applies the cache breakpoint to the last cacheable block" — and the "Common mistake" section: "Automatic caching hits the same trap: it places the breakpoint on the last cacheable block, which in this structure is the one that changes every request." The code comment's rejection rationale is accurately represented: with our shape the last cacheable block is the varying tail → a fresh write every turn and never a read. ✔
- **Breakpoint budget**: 3 explicit breakpoints (system head, last tool, final message) ≤ the 4 allowed. ✔
- **Minimum cacheable size**: silent no-op below threshold, "no error is returned" — no size guard needed (plan point 7 correct). ✔

From platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls:

- **tool_result + text ordering — VALID, explicitly documented**: "In the user message containing tool results, the tool_result blocks must come FIRST in the content array. Any text must come AFTER all tool results." — with a ✅ example showing exactly `[tool_result, text]`. The relocation produces precisely this shape (tool_results first, tail text blocks after). ✔
- The one caveat — "If the assistant turn also called a server tool that has no result block yet, the user message must contain only tool_result blocks" — is unreachable here: this client never enables server tools (only client tool schemas ride the tools array), so no unresolved `server_tool_use` blocks can appear in echoed history. Informational only.

## 5. Other code paths depending on the old shape — none found

- `estimate_prompt_tokens(messages, tools)` (call site :382): operates on the Message list, not the built body — basis unchanged; the plan explicitly kept it unchanged. ✔
- **Trace recording**: `complete()` passes the built body to `log.start(...)` verbatim (size-capped, display-only). A search for `["system"]` across `src/` finds exactly three matches — the builder's own write (:407) and two test assertions (:2052, :2282). No code reads the body's system shape. ✔
- **Sanitized-args / interrupted-output paths**: operate on `Message.raw`, not the built body. ✔
- **Other providers**: `build_request_json` takes `&[Message]` (read-only — the input is never mutated); openai.rs/vision.rs build their own bodies; the OpenAI `PrefixCache` is Message-list-based and excludes the trailing system run — unaffected. ✔
- `message_to_json`'s `unreachable!("system messages are hoisted before mapping")` still holds (system messages are filtered out of `rest` before the mapping loop). ✔

## 6. Test quality

- The replaced test (`volatile_tail_relocates_into_final_message_and_breakpoint_lands_on_last_real_block`) pins the full contract: system = [head] with breakpoint 1; earlier messages carry no `cache_control`; final message = [real block (breakpoint 3), relocated tail (uncached)]. ✔
- The carrier test pins the mid-turn shape (coalesced tool-result carrier, breakpoint on the LAST tool_result, tail after it) — matching the docs' tool-use caching example. ✔
- The no-tail and thinking walk-back cases are pinned (the latter via the updated raw-echo test with explicit no-`cache_control` assertions on thinking/redacted_thinking). ✔
- `breakpoint_stays_within_lookback_window_on_long_tool_history` is a placement proxy — the 20-position walk itself is provider-side and cannot be asserted client-side; the by-construction argument (breakpoint on the final message's last real block + run coalescing) is sound, and the test comment is honest about what it asserts. Observation, not a finding.

## 7. Security

`cache_control` values are hardcoded literals; placement is purely structural (last markable block); no user-derived content flows into placement decisions. The relocated tail is the same content that previously rode in `system` — no new exposure. The trace records the body as before. Clean.

## 8. Constitution checks

- **Multi-platform neutrality**: pure Rust JSON construction; no OS-specific code, paths, or shell syntax. ✔
- **File-tools-first**: no shell-based file mutation in the diff. ✔
- **Warning-free**: implementer-recorded `cargo test --workspace` green (2278 lib + 16 + 293 + 4 + 2, zero warnings under `deny(warnings)`). ✔
- **Documentation sync**: FINDING (below).

## Findings

### LOW 1 — PLAN.md "Prompt caching strategy" section is stale (documentation sync)

**Where:** `PLAN.md` :158-163 (the "Prompt caching strategy" bullet list).

PLAN.md still describes the old design: "(2) Anthropic Messages API system block splitting with `cache_control: {"type": "ephemeral"}` on the stable head while volatile tail (steps, progress, memories) is uncached". After this change the volatile tail is no longer in `system` at all — it is relocated into the final message's content as the uncached varying suffix, and a 3rd breakpoint caches the conversation history (tools + head + history + the current turn's real content). The 2026-12-21 prompt-caching review's informational note anticipated exactly this moment ("once H1 resolves and caching behavior settles, add one line to PLAN.md's provider-strategy section"); H1 — the volatile-tail-defeats-history-breakpoint problem — is precisely what this change resolves, so the update is now due.

**Fix:** update bullet (2) to the new shape (head-only `system` with breakpoint 1; volatile tail relocated into the final message's content as trailing text blocks; breakpoint 3 on the final message's last real block; automatic caching evaluated and rejected per the docs' varying-block trap).

**Related, same fix batch (knowledge records, not PLAN.md):** `.coding/knowledge/spec/2026-12-21-multi-provider-prompt-caching-and-waiting-reduct.md` line 14 ("Message-history breakpoints deliberately NOT set: volatile tail mutates every turn inside system") is now stale — the finish-time SPEC/DECISION memory should supersede or amend it, and should also record the plan's "Record as DECISION" item (the automatic-caching rejection rationale) so the old "deliberately avoided" claim doesn't survive as live knowledge.

## Informational notes (no action required)

1. **Prompt-layout semantic shift (intentional):** all later system content (compaction summary, re-injected suggestions, workflow state, memories, CONTEXT_FOOTER) now renders AFTER the current user message on the Anthropic path, instead of before the history in `system[1..]`. This matches the OpenAI path's trailing-system layout (cross-provider consistency actually improves) and gives the state blocks recency. Intended per the plan.
2. **Empty-text fallback edge:** if a non-multimodal endpoint strips all image blocks from the final user message, its last real block is the `""` text fallback; the docs say empty text blocks cannot be cached — a silent no-op (no error), graceful degradation on a pre-existing edge that predates this change.
3. **Runtime verification dependency:** the acceptance criterion (cache_read_input_tokens > 0 growing with the conversation) is only observable once the separate "Parse Anthropic cache usage fields" backlog item (648051bf) lands — the plan documents this honestly; this review's verification is necessarily request-body-shape-based, which is the correct scope for the landed change.

## Verdict rationale

The relocation is mechanically correct (all probed edge cases safe), the breakpoint placement is the documented multi-turn pattern adapted to a varying suffix, every docs claim in the comments verifies against the live-fetched documentation, no other code path depends on the old `system[1..]` shape, tests pin the new contract meaningfully, and the constitution checks pass except for the PLAN.md doc-sync gap (LOW 1). Fix LOW 1, re-run `cargo test --workspace`, and this is ready to commit.
