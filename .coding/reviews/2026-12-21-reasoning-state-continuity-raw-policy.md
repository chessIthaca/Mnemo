## Verdict: FINDINGS (2 high, 2 medium, 2 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan 70910b7d
("Reasoning-state continuity: raw-payload source of truth + ProviderPolicy").

The raw/view separation, Anthropic content-array assembly (signatures +
redacted_thinking preserved, block order kept), cross-vendor strip (both
OpenAI-flat and Anthropic content-array shapes), open-tool-loop retreat edge
cases, the serialization-bug fail-loud classifier (Rule 6), and multi-platform
neutrality are all **correct**. Two HIGH regressions share one root cause: the
OpenAI builder echoes `raw.clone()` unconditionally for assistant turns, but
the turn loop mutates structured fields (the null-turn "(no output)"
placeholder and the bad-JSON sanitized args) *without* updating `raw` — so the
verbatim raw overrides the fix. Neither path is exercised with real raw
because `MockProvider` never emits `RawAssistantDelta`. Two MEDIUM findings
concern spec/doc fidelity (ProviderPolicy params are inert; the
`provider_meta`/`reasoning_content` migration bridge was not removed). Two LOW
findings are design observations.

## HIGH findings

### H1 — Null-turn regression: raw echo bypasses the "(no output)" placeholder

**Files:** `src/provider/openai.rs` `build_request_json` (~L1300-1320);
`src/agent/turn.rs` null-turn packaging (~L1501).

**Root cause.** `build_request_json` does, for every assistant message:

```rust
if m.role == Role::Assistant {
    if let Some(raw) = &m.raw {
        return raw.clone();          // unconditional — no content/tool_calls guard
    }
}
```

On a **null turn** (model returns `Finish` with no text and no tool calls) the
streamed raw is `{ "role": "assistant" }` with **no `content` key**: the first
delta carries `content: null` / `""`, which `merge_scalar` skips, and no
content deltas follow. The turn loop then substitutes
`assistant_text = "(no output)"` into the structured `content` field and pushes
`Message { raw: raw.take(), ..Message::assistant_text("(no output)") }` — so
`m.content = "(no output)"` but `m.raw = Some({"role":"assistant"})` (no content).

**Impact.** `validate_request_messages` (openai.rs:1835) checks the structured
`content` field — it sees `"(no output)"` and **passes** — but the wire body is
the empty raw `{ "role": "assistant" }` (no content, no tool_calls). The
provider 400s ("assistant message has no content and no tool calls"). That 400
is **not** a serialization bug (not in the `is_serialization_bug` classifier),
so `complete_with_retry` retries it 3× — all fail identically — and the session
wedges. This **regresses the previously-fixed null-turn bug** (the "(no output)"
placeholder exists precisely to prevent this; see prior fix review
2026-11-23).

**Why tests miss it.** The null-turn tests use `MockProvider`, which does **not**
emit `LlmEvent::RawAssistantDelta`, so `raw = None` and the field-based path
(content = "(no output)") is exercised. The bug only manifests with a real
provider stream. (Confirmed: `RawAssistantDelta` is emitted only by
`openai.rs::parse_sse_chunk` and `anthropic.rs::message_stop`.)

**Anthropic secondary concern.** The Anthropic builder echoes
`raw.get("content")` even when it is an empty array `[]` (a null turn with no
content blocks → `content_blocks` is empty → raw = `{role, content: []}`). It
does **not** fall through to field construction, so the placeholder is bypassed
there too — though Anthropic null turns are rarer and whether an empty content
array is rejected is less certain than the OpenAI case.

**Fix (pick one).** (a) Cleanest: in the null-turn packaging site, set
`raw = None` so the builder falls through to field construction (which carries
the "(no output)" content). (b) Mirror the Anthropic guard in the OpenAI
builder: fall through to field construction when raw lacks **both** `content`
and `tool_calls`. (c) Inject the placeholder into raw. **Add a regression
test** that feeds a real OpenAI delta stream (`RawAssistantDelta` with
`{role:"assistant", content:""}` then `finish_reason:"stop"`) and asserts the
re-serialized request body contains `"(no output)"` (not an empty assistant
message). The test must fail without the fix.

---

### H2 — Bad-JSON sanitization bypassed by raw echo

**Files:** `src/agent/turn.rs` bad-JSON retry packaging (~L1431);
`src/provider/openai.rs` `build_request_json`.

**Root cause.** When tool-call arguments are malformed JSON, the turn loop
builds `sanitized_calls` (each malformed `arguments` → `"{}"`) and pushes:

```rust
messages.push(Message {
    raw: raw.take(),                              // <-- verbatim, malformed args
    ..Message::assistant(text.clone(), sanitized_calls)   // <-- sanitized args
});
```

But `raw` was assembled from the verbatim streamed deltas via
`merge_tool_call_delta`, so `raw.tool_calls[i].function.arguments` still holds
the **original malformed** string. The builder echoes `raw.clone()` → sends the
malformed arguments, **not** the sanitized `"{}"`.

**Impact.** The sanitization's entire purpose — avoid the 400 "Unterminated
string" / invalid JSON in arguments (the comment at turn.rs:1409-1415 says so
explicitly) — is defeated. The retry request 400s before the model even
responds, so the bad-JSON self-correction path (the model re-tries with valid
args) is broken. `validate_request_messages` does not parse tool-call
arguments, so it does not catch this locally.

**Why tests miss it.** Bad-JSON tests use `MockProvider` (no raw → field path
with sanitized args).

**Fix.** When sanitizing, set `raw = None` (so the builder uses the sanitized
`tool_calls`), **or** rewrite `raw.tool_calls[i].function.arguments` to match
`sanitized_calls`, **or** have the builder fall through to field construction
when a raw tool-call's `arguments` fails `serde_json::from_str`. **Add a
regression test** with a real stream whose tool-call argument deltas
concatenate to invalid JSON, asserting the re-serialized request body carries
`"{}"` (not the malformed string). Must fail without the fix.

**Shared root cause (H1+H2).** The OpenAI builder echoes raw unconditionally
for assistant turns, but the turn loop mutates structured fields (placeholder
text, sanitized args) expecting the builder to use them. The Anthropic builder
is partially protected (it only echoes `raw.content`, falling through when
content is absent) but still bypasses the placeholder for empty-content null
turns. The robust fix is to make the builder fall through to field
construction whenever raw and the structured fields have diverged (empty
content / invalid args), or to clear `raw` at every packaging site that
substitutes structured fields.

## MEDIUM findings

### M1 — ProviderPolicy params are inert; builders hardcode behavior (Rule 2 partially unmet; docs overstate)

**Files:** `src/provider/policy.rs` (fields `include_params`, `template_kwargs`,
`reasoning_field`, `signatures_portable_across_models`; fn `vllm_reasoning_field`);
`src/provider/openai.rs` `build_responses_request_json` (~L1535); `PLAN.md`;
`README.md`.

The spec (Rule 2) and the updated PLAN.md/README.md claim "a ProviderPolicy
data object per provider drives all reasoning behavior … adding a provider is a
data change, not a new builder branch." In the actual code, `for_kind_and_model`
is consulted in **exactly one** place: `strip_cross_vendor_reasoning` (for
vendor detection via `is_cross_vendor`). The remaining policy fields are never
read outside `policy.rs`:

- `include_params` is **hardcoded** in `build_responses_request_json`
  (`body["include"] = json!(["reasoning.encrypted_content"])`) instead of read
  from `policy.include_params`.
- `template_kwargs`, `reasoning_field`, `signatures_portable_across_models` —
  no builder reads them.
- `vllm_reasoning_field()` — never called at runtime (only in `policy.rs`
  unit tests).

Not a correctness bug (the hardcoded values are correct for the current
providers), but the data-driven goal is unmet and the docs overstate it. Adding
a provider still requires builder edits, not just a policy entry.

**Fix.** Either wire the builders to read from `ProviderPolicy` (consult
`include_params` in `build_responses_request_json`, etc.), or soften the
PLAN.md/README.md wording to match reality ("ProviderPolicy captures the
contract and drives cross-vendor detection; full builder consultation of
include/template/reasoning-field params is a follow-up"). If the params are
intentionally forward-looking, mark them as such in the module doc.

### M2 — `reasoning_content` / `provider_meta` retained as redundant parallel sources (Rule 1 partial; plan step 4 incomplete)

**Files:** `src/provider/mod.rs` (`Message`/`ToolCall` structs);
`src/provider/stream.rs` (`merge_provider_metadata`, `take_message_provider_meta`);
packaging sites in `src/agent/turn.rs`.

Plan step 4 specified "REMOVE `reasoning_content` and `provider_meta` from
Message and ToolCall — raw subsumes them." They were instead retained as a
"migration bridge" (stream.rs comment). Consequence: `Message::provider_meta`
and `ToolCall::provider_meta` are still **set** at every packaging site but
**never read** by any builder (the old `apply_provider_meta` was removed).
`reasoning_content` is still set on synthetic messages. This is redundant with
`raw` and partially violates Rule 1's "raw is the single source of truth."

Not a correctness bug (the builder uses `raw` for assistant turns). It is
technical debt that bloats serialized messages and leaves a second, stale
source of provider metadata that can drift from `raw`.

**Fix.** Complete the migration — remove `provider_meta` from `Message`/
`ToolCall` and the `merge_provider_metadata`/`take_message_provider_meta`
bridge — or, if the bridge must stay, document why and track its removal in a
follow-up. (No `#[allow(dead_code)]` is needed today because the fields are
`pub` on a `pub` struct in a lib crate, so they do not warn — but they are
dead at runtime.)

## LOW findings

### L1 — Cross-vendor strip is a permanent, one-way mutation

**Files:** `src/agent/turn.rs` (`strip_cross_vendor_reasoning` call ~L876);
`src/provider/mod.rs` (`strip_cross_vendor_reasoning`).

`strip_cross_vendor_reasoning` mutates the live `messages` (`&mut [Message]`)
in place — setting `reasoning_stripped = true` and stripping `raw`. This
persists across turns. A transient vendor A→B switch for a single turn
**permanently** strips all prior A-turns' reasoning: even if the user switches
back to A, the reasoning is gone (`raw` was mutated). The `reasoning_stripped`
flag makes the operation idempotent (no re-processing), which is good. The spec
does not require restoration, so this is arguably correct — but it is a
surprising, irreversible quality degradation from a one-turn model switch.

**Fix.** Document the one-way nature in the module doc. (Snapshotting raw
before stripping to allow restoration is likely out of scope and would re-introduce the very state the strip exists to remove.)

### L2 — Responses API input conversion does not echo raw (stateful path relies on server-held state)

**Files:** `src/provider/openai.rs` `message_to_responses_input` (L1581).

`message_to_responses_input` converts messages to Responses-API input using
**structured fields** (`m.content`, `m.tool_calls`), not `raw`. For assistant
turns that fall *after* the `previous_response_id` anchor, reasoning is
therefore not preserved verbatim. In normal operation this is fine: the anchor
is always the latest assistant turn with a `response_id`, so no assistant
reasoning is in the input. But if an assistant turn after the anchor lacks a
`response_id` (the stream omitted `response.completed`, or the parser missed
it), that turn's reasoning is silently dropped from the request input.

Edge case only; the stateful path is opt-in (`use_responses_api: false` at every
construction site). `message_to_responses_input` does handle all four roles
(System→developer, User, Assistant→assistant/function_call, Tool→
function_call_output) correctly.

**Fix.** Document the assumption ("every assistant turn receives a
`response_id` from `response.created`/`response.completed`"), or handle the
missing-`response_id` case by falling back to `raw` echo for that turn.

## Verified correct (no finding)

- **Raw echo is byte-identical.** `build_request_json` returns `raw.clone()` for
  assistant turns — identical to the stored raw. The `raw_assembles_*` and
  `build_request_json_echoes_raw_byte_identical` tests confirm key order and
  values are preserved (serde `Map` preserves insertion order).
- **Anthropic content-array assembly preserves block order + signatures.**
  `content_blocks` is a `Vec` indexed by block `index`; `content_block_start`
  places each block, deltas accumulate in place, `message_stop` emits the whole
  array. `build_raw_content_block` captures `signature` verbatim for `thinking`
  blocks and `data` verbatim for `redacted_thinking`; `tool_use.input` is
  reassembled from `input_json_delta` fragments and parsed to an object at
  `message_stop` (tool-call args, not reasoning — sanctioned).
- **Cross-vendor strip handles both shapes.** `strip_reasoning_from_raw`
  removes `reasoning_content`/`reasoning`/`thought_signature` (OpenAI flat),
  strips `thinking`/`redacted_thinking` blocks from the content array
  (Anthropic), and clears `thought_signature` from each `tool_call` — while
  retaining `text` and `tool_use` blocks. Idempotent via `reasoning_stripped`.
- **Open-tool-loop retreat edge cases.** `retreat_past_open_tool_loop` scans
  backward for the loop start (last `User` message) or a closing text-only
  assistant; the `cut <= 1` guard skips compaction for the degenerate
  all-open-loop conversation; forward orphan-tool-advance and backward retreat
  operate on different boundaries and do not conflict; `cut == start` is a
  no-op (loop start preserved).
- **No `raw == None` panic.** Both builders guard with `if let Some(raw)` and
  fall through to field construction.
- **No double-take of `raw`/`response_id`.** The stop-reason path returns at
  turn.rs:1292; `acc.take_raw()`/`take_response_id()` execute exactly once per
  turn (either the stop path or the normal path, never both). `Option::take`
  is used at the branch sites.
- **Security — opaque reasoning fields never inspected/parsed/truncated.**
  `merge_delta_into_raw`/`merge_scalar` *reassemble* fragmented deltas
  (concatenate strings) — sanctioned verbatim reconstruction. `strip_*` *removes*
  fields — sanctioned by Rule 5. `view_from_raw` *reads* for display —
  sanctioned (Rule 7 UI waived). No code parses signature contents or truncates.
- **Serialization-bug fail-loud (Rule 6).** `is_serialization_bug` matches the
  four spec patterns (thought_signature+missing, reasoning_content+must-be-
  passed-back, expected-thinking+found-text, modified-prior-content);
  `dispatch.rs:664` surfaces a "SERIALIZATION BUG (not retried)" error with
  `retrying: false` and returns immediately (no retry). `is_non_retryable`
  delegates to it. Tests cover all four patterns plus negatives (503, Tool,
  Config errors).
- **Multi-platform neutrality.** No Windows-only APIs, paths, or shell syntax in
  library/app code. All changes are pure Rust (serde_json, provider logic).
  `use_responses_api` is platform-neutral.
- **Doc comments.** All new public items (`ProviderPolicy`, `StatefulKey`,
  `Vendor`, `for_kind_and_model`, `vllm_reasoning_field`, `TurnView`, `view`,
  `strip_cross_vendor_reasoning`, `is_serialization_bug`, the stream helpers,
  the Responses-API functions) carry doc comments. No `#[allow(...)]` added.

## Process note — build verification

As the reviewer I have no shell and could not execute `cargo test`. I
statically verified there are no obvious warning sources under
`#![deny(warnings)]`: all new `pub` items are documented; no unused imports or
variables introduced; `pub` items on a `pub` struct in a lib crate do not
trigger `dead_code` (so the inert policy fields and `vllm_reasoning_field` do
not warn, though they are dead at runtime — see M1). The implementer must
confirm `cargo test` passes warning-free before committing. Every
`OpenAiClientConfig` construction site I could locate carries
`use_responses_api: false` (client_factory.rs, main.rs dummy, vision.rs test,
provider_integration.rs test, and the openai.rs test helpers); a missing field
would be a compile error, not a silent warning.

## Summary of required actions before commit

1. **H1** — Fix the null-turn raw bypass (clear `raw` at the null-turn packaging
   site, or guard the OpenAI builder). Add a real-stream regression test.
2. **H2** — Fix the bad-JSON raw bypass (clear `raw` when sanitizing, or rewrite
   raw's tool-call args, or guard the builder). Add a real-stream regression
   test.
3. **M1** — Wire builders to `ProviderPolicy` params, or soften the PLAN.md/
   README.md "data change" claim.
4. **M2** — Remove the `provider_meta`/`reasoning_content` migration bridge, or
   document why it stays.
5. **L1/L2** — Document the one-way strip and the Responses-API `response_id`
   assumption.
6. Re-run `cargo test` (warning-free) after fixes; commit to `wt/agenticcoding`
   with this report.
