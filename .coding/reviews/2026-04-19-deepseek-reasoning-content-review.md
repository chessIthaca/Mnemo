# Review: DeepSeek `reasoning_content` echo fix

**Plan goal.** DeepSeek thinking-mode models reject any request whose prior assistant messages lack the `reasoning_content` field (HTTP 400 `invalid_request_error`). The app consumed `reasoning_content` from SSE one-way (into a UI "thinking" display) but never stored or echoed it, so every multi-turn request against a DeepSeek reasoning model failed — blocking skill-start and provider-switch flows. The fix adds a `reasoning_content: Option<String>` field to `Message`, accumulates `ReasoningDelta` text during the stream, stores it on the four assistant-output construction sites, and makes the OpenAI request serializer always emit the key on assistant-role messages (empty string when absent).

**Files reviewed.** `src/provider/mod.rs`, `src/agent/turn.rs`, `src/provider/openai.rs`, `src/agent/context.rs`, `src/memory/consolidation.rs`, `src/runtime/agent.rs`, `src/agent/tests.rs`, `tests/provider_integration.rs`, `tests/workflow_integration.rs`, plus bookkeeping. Reviewed via `git diff HEAD` and full-file reads of every production `Message` construction site and the serializer.

---

## Correctness — VERIFIED

**All four assistant-output sites carry the reasoning.** A `Role::Assistant` sweep across the repo confirms the only production sites that build a *new* assistant message from the current turn's output are in `src/agent/turn.rs`, and all four receive `assistant_reasoning.clone()`:

| Site | Line | Role | reasoning_content |
|------|------|------|-------------------|
| stop-signal partial output | turn.rs:870 | Assistant | `assistant_reasoning.clone()` ✓ |
| bad-JSON re-inject | turn.rs:977 | Assistant | `assistant_reasoning.clone()` ✓ |
| no-output placeholder | turn.rs:1020 | Assistant | `assistant_reasoning.clone()` ✓ |
| tool-calls path | turn.rs:1040 | Assistant | `assistant_reasoning.clone()` ✓ |

No assistant-output construction site was missed. The remaining `Role::Assistant` matches in the repo are test fixtures (`agent/tests.rs`, `openai.rs` tests — correctly `None`), role-display helpers (`openai.rs:860`, `context.rs:467`), token-breakdown arms, or the serializer gate itself. `context.rs` summarization builds summary/request messages with `None` — correct: those are fresh synthesizer calls whose own reply will be re-captured next turn, not prior-model echoes.

**Reasoning captured at the right place.** `reasoning_text` is accumulated only on `LlmEvent::ReasoningDelta` (turn.rs:698-703), alongside `text`, inside the single stream-consume `select!` loop. `assistant_reasoning` is packaged once after the loop (turn.rs:850-854): `None` when empty, `Some` only when real reasoning streamed. This is exactly the right scope — per-turn, reset each loop iteration, attached only to the message that records that turn's own output. `DeltaAccumulator::feed` (stream.rs:56) ignores `ReasoningDelta`, so there is no double-capture.

**Serde round-trip is coherent.** Stored struct: `#[serde(default, skip_serializing_if = "Option::is_none")]` (mod.rs:138) — `None` ⇒ key omitted from the in-memory/serialized conversation JSON, `Some` ⇒ round-trips through serialize/deserialize intact (`default` back-fills `None` on legacy data, so old transcripts deserialize cleanly). Request serializer: assistant-role messages always emit the key (`openai.rs:923-925`), via `m.reasoning_content.clone().unwrap_or_default()` — `Some("thinking...")` echoes verbatim, `None` emits `""`. This is the DeepSeek contract: the key must be *present*; empty string is accepted, absent is not. An assistant message carrying reasoning from a prior turn survives: stream → `Some` stored → serde round-trip → serializer echoes it. ✓

**Only assistant messages get the key.** User/tool/system messages have no such key added (verified by test 3 and the role gate). DeepSeek does not want the field on non-assistant roles, and other OpenAI-compatible providers would see a spurious field on those roles — correctly avoided.

## Bugs — none blocking; two notes (non-actionable as defects)

1. **`assistant_reasoning.clone()` per push.** The value is cloned at each of the four sites, but only one site executes per turn (they are mutually exclusive branches: stop-signal early-return, bad-JSON `continue`, no-output early-return, or tool-calls fall-through). There is at most one assistant push per stream, so the `.clone()` is never wasted across sites within a single turn — it exists only because each branch is written independently. No issue.

2. **Context summarization drops reasoning on the folded messages (by design, and safe).** When `context.rs` summarizes/compacts, the folded prior turns are replaced by a summary message built with `reasoning_content: None`. On the next request the serializer emits `""` for that message (it is not an assistant message from a reasoning model — it's a system/summary message, which correctly omits the key entirely). The *recent* (un-summarized) assistant messages retain their stored `Some(...)` and echo correctly. So dropping reasoning on summarization does **not** re-trigger the DeepSeek 400: the messages that must carry real reasoning (recent assistant turns) still do; the folded ones become non-assistant or carry `""`, both accepted. Confirmed safe.

**Security.** One optional string field, echoed only to the same provider endpoint that produced it. No new attack surface, no logging of the field, no path where reasoning text influences tool dispatch or approval. Clean.

**Constitution compliance.**
- Doc comment on the new public field: present (mod.rs:133-137) ✓
- Regression tests for the defect: 3 present in `openai.rs` — `build_request_json_echoes_assistant_reasoning_content` (Some echoes verbatim), `build_request_json_assistant_without_reasoning_emits_empty_string` (None ⇒ present-but-empty `""`), `build_request_json_non_assistant_messages_omit_reasoning_content` (system/user/tool omit key). These directly reproduce the defect (missing key / dropped reasoning) and assert the fix. ✓
- No `#[allow(...)]` added ✓
- Warning-free build: `cargo test` reported 1041 passed / 0 failed, exit 0; under `#![deny(warnings)]` a green run proves zero warnings ✓

---

## Findings

**No findings.** The diff is correct, complete, and faithful to the plan. All assistant-output sites carry the captured reasoning; the serde round-trip and always-emit serializer are coherent; the three regression tests cover the exact defect (Some echo, None→empty-present, non-assistant omission); non-assistant and synthetic messages correctly use `None`; context summarization's dropping of reasoning on folded messages is safe and does not re-trigger the 400. Constitution requirements (doc comment, regression tests, no `#[allow]`, warning-free green test run) are all satisfied. Ship it.
