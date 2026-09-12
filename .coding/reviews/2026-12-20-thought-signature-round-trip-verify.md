## Verdict: PASS

Both round-1 findings (L1 docs, L2 tests) from `.coding/reviews/2026-12-20-thought-signature-round-trip-review.md` are correctly resolved in commit `35b4d98` on `wt/agenticcoding`. The thought-signature round-trip implementation remains correct, well-tested, and warning-free (1800 tests, 0 failures, zero warnings under `#![deny(warnings)]`).

## Finding L1 (docs) — RESOLVED ✓

**Prior issue:** PLAN.md invariant (3) stated signatures "persist across model switches mid-conversation" without qualification, overstating survival through summarizing switches (compaction replaces signed turns with unsigned summary `Message`s carrying `provider_meta: None`, dropping their signatures).

**Fix verified:** PLAN.md line 198, invariant (3), now reads:

> (3) **conversation-scoped survival** — signatures persist across model switches mid-conversation when the context fits the new model (a summarizing switch — e.g. to a smaller context window — replaces signed turns with an unsigned summary, dropping their signatures by necessity; the implementation fails safe, never fabricating).

The caveat accurately describes the compaction/summarization edge case without overclaiming:
- **Qualifies the claim** — "when the context fits the new model" correctly scopes persistence to non-summarizing switches. ✓
- **Names the exception** — "a summarizing switch — e.g. to a smaller context window — replaces signed turns with an unsigned summary" matches the verified behavior (`src/agent/context.rs` summary messages carry `provider_meta: None`). ✓
- **No overclaim** — "dropping their signatures by necessity" frames the loss as inherent to compaction, not a defect. ✓
- **Fails-safe note** — "the implementation fails safe, never fabricating" matches the prior review's root-cause analysis (summary messages have `provider_meta: None`, never a fabricated signature). ✓

The caveat correctly sets reader expectations. **Resolved.**

## Finding L2 (tests) — RESOLVED ✓

**Prior issue:** No end-to-end test at the request-builder level asserting that messages with `provider_meta: None` produce a request body containing no `thought_signature` key (the no-fabrication guarantee was only covered at the helper level).

**Fix verified:** Test `build_request_json_omits_thought_signature_when_no_meta` added at `src/provider/openai.rs:3427-3468`, sitting directly after the echo-when-present test (`build_request_json_echoes_tool_call_provider_meta`).

The test:
1. Builds an `OpenAiClient` (OpenAI kind, `gemini-3` model).
2. Constructs a single assistant `Message` with **both** `provider_meta: None` (message-level) and a `ToolCall` with `provider_meta: None` (per-tool-call level) — exercising both echo scopes.
3. Calls `client.build_request_json(&[msg], &[], None)` — the full request-builder path (not a helper unit test).
4. Asserts `body["messages"][0].get("thought_signature").is_none()` — no key on the message object. ✓
5. Asserts `body["messages"][0]["tool_calls"][0].get("thought_signature").is_none()` — no key on the tool_call entry. ✓

**Would it fail if `apply_provider_meta` fabricated keys?** Yes. `apply_provider_meta` is the sole echo gate in `build_request_json` (the body is built manually, not via `Message::Serialize`). If a future regression inserted a `thought_signature` key when `provider_meta` is `None` (e.g. a default value, or a refactor that unconditionally emits the key), `.get("thought_signature")` would return `Some(...)`, `.is_none()` would be `false`, and both assertions would fail. The test correctly guards the no-fabrication invariant end-to-end. ✓

The test is a standard `#[test]` in the `openai.rs` test module, compiled and run by `cargo test` (confirmed green: 1800 tests, 0 failures). **Resolved.**

## Observations (out of scope — no verdict impact)

- **Committed scratch files:** Commit `35b4d98` includes a `.coding/tmp-shard/` directory (~30 tiny `.txt` files containing implementation fragments like `apply_provider_meta`, `capture_provider_meta`, field-block snippets). These are leftover working shards from the implementation session (the session's bug record documents fragment-decomposition workarounds for payload corruption). They are inert — no build/test impact, sidecar directory — but are committed junk that should be cleaned up (`git rm -r .coding/tmp-shard/` + add to `.gitignore`) in a follow-up housekeeping commit. Not a finding: outside the two-fix verification scope and unrelated to the feature's correctness.

## Scope note

This re-review was scoped to verifying the two round-1 fixes only (per the task brief). The full feature implementation (capture→accumulate→package→echo pipeline, security, serialization) was already reviewed as correct in the prior round and is unchanged. Both fixes are correct, complete, and the build remains green.
