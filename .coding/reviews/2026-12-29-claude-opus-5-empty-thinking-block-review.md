## Verdict: FINDINGS (0 high, 1 low)

Review of plan 37f3bfda (bug bd5eb19d — claude-opus-5 400 "each thinking block must contain thinking"), all changes uncommitted on the working tree: `src/provider/anthropic.rs` (helper + capture retain + echo sanitize + 2 regression tests), `.coding/analysis/claude-opus-5-1022-shape.txt` (new), `.coding/knowledge/bug/2026-12-29-...-fi.md` (new), old knowledge file marked superseded, backlog bd5eb19d → in_flight.

### Verified correct

**1. Fix correctness — all requested edge cases traced against the code:**
- *Content not an array* (anthropic.rs:432-441): `as_array()` → None → `sanitized` = `[]` → falls through to field construction. Identical to pre-fix behavior (`raw_content_is_usable` returned false for non-arrays). ✓
- *Empty array*: sanitized empty → H1 fall-through, exactly as before; the H1 comment was correctly updated to say "(sanitized) array". ✓
- *All-valid array*: sanitized is a block-by-block clone of the original → `raw_content_is_usable` verdict unchanged → echo verbatim, JSON-identical to the pre-fix output. (Allocation is equivalent to the old `content.clone()` — no perf regression.) ✓
- *Array with ONLY an empty thinking block*: echo-side sanitized empty → H1 fall-through to field construction (a local descriptive error if the message also has no text/tool calls — strictly better than the pre-fix wasted 400 round-trip); capture-side retain empties → no `RawAssistantDelta`. ✓
- *redacted_thinking with valid data*: `content_field_empty("data")` → false → preserved (Rule 4). ✓
- *Non-string `thinking`/`data` values* (null, number): `as_str()` → None → `unwrap_or(true)` → dropped — correct, such blocks are invalid on the wire. Whitespace-only `thinking` is kept (non-empty passes Anthropic's validator) — conservative, Rule-1-preserving. ✓
- *Layering*: sanitize-then-usability (`raw_content_is_usable` now runs on the sanitized array, anthropic.rs:453-460) is the right decomposition — it keeps the H1 empty-array fall-through semantics intact instead of conflating "drop block" with "fall through".
- *Capture-side placement* (anthropic.rs:727): retain runs after tool_use input parsing — disjoint block types, order safe. The `if !content.is_empty()` guard (line 728) matches the existing `raw_none_when_no_content_blocks` semantics.
- *No bypassing echo path*: `view_from_raw` (mod.rs:389) is read-only UI/planning; `strip_reasoning_from_raw` (mod.rs:533) removes thinking blocks entirely (cross-vendor); openai.rs:1536 is the flat OpenAI shape. The Anthropic content-array wire path is unique to this builder. ✓

**2. Rule 4 trade-off** — verified: `is_always_invalid_thinking_block` (anthropic.rs:871-886) returns true ONLY for thinking blocks with missing/empty/non-string `thinking` and redacted_thinking blocks with missing/empty/non-string `data`. A valid thinking block (non-empty thinking, any signature state) is never filtered. The doc comment (860-870) accurately documents the always-invalid vs. valid distinction and the historical "Expected thinking or redacted_thinking, but found text" failure mode.

**3. Regression quality** — both tests genuinely pin the fix (traced against the pre-fix code):
- `build_request_json_drops_empty_thinking_block_from_echoed_history` (1830-1872): pre-fix, `raw_content_is_usable` passes the 3-block array (it never checked thinking content) → verbatim echo → assert fails. ✓
- `raw_drops_empty_thinking_block_that_never_received_content` (2747-2779): pre-fix, message_stop emits [thinking(""), text] → assert fails. ✓
- Existing coverage all present and still pinning their behaviors: `build_request_json_echoes_raw_content_array_verbatim` (1793, full-array equality incl. valid thinking + redacted_thinking), `build_request_json_retains_thinking_signatures_across_tool_calls` (1875), `assistant_reasoning_is_not_echoed_as_an_unsigned_thinking_block` (2372), `raw_captures_thinking_signature_text_and_tool_use` (2702), `raw_preserves_redacted_thinking_block` (2782).

**4. Bug-plan checks** — regression tests exercise both changed paths (echo path in the assistant mapper; message_stop finalization); root cause documented in `.coding/analysis/claude-opus-5-1022-shape.txt` (including the honest note that the incident rows rotated out of provider-errors.jsonl and the shape was reconstructed from the error path + capture code — the right adaptation of the DeepSeek adjudication method); BUG memory correctly superseded (881fef2d now [superseded], FIXED record c3f9dbd9 live; knowledge-file supersedes chain 2026-12-24 → 2026-12-29 consistent).

**5. Constitution** — doc comment on the new helper is thorough and accurate; code style matches the file (free helper placement after `raw_content_is_usable`, comment conventions, test structure); pure Rust logic with no platform assumptions (multi-platform neutral); green `cargo test` under `#![deny(warnings)]` (1919 + 16 / 0) proves zero warnings. No README/PLAN.md update needed — internal provider fix, no user-facing config or feature change.

### Findings

**1. LOW — untested branch: capture-side retain emptying the whole array emits no RawAssistantDelta, but no test pins it.**
`src/provider/anthropic.rs:727-735` — the designed behavior "if the retain empties the array, no RawAssistantDelta is emitted at all" (stated in the plan and in `.coding/analysis/claude-opus-5-1022-shape.txt` lines 53-55) has no regression test. `raw_drops_empty_thinking_block_that_never_received_content` keeps a text block alive, so the `content.is_empty()` true-case at line 728 is never exercised; `raw_none_when_no_content_blocks` (2811) covers the different no-blocks-at-all path. If a future edit reorders the retain or makes the emission unconditional, nothing fails.
Fix: add a test mirroring `raw_none_when_no_content_blocks` — a stream whose ONLY content block is a thinking block-start (with signature) that never receives a thinking_delta, asserting no `RawAssistantDelta` event is emitted (Finish/Usage still are).
