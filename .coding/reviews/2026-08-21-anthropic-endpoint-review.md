# Review: Anthropic Messages API endpoint + agent.md review-expectations (plan 91f7ea8f)

Scope: ALL uncommitted changes on `feat/anthropic-endpoint` (19 modified files +
untracked `src/provider/anthropic.rs`, which I read in full). Reviewed the new
AnthropicClient (request builder, SSE parser, stream loop, trace mirroring,
24 unit tests), the kind dispatch, the IPC model-listing path, the frontend
plumbing, docs (README/PLAN/agent.md), and multi-platform neutrality.

Overall: a careful, well-tested implementation that mirrors `openai.rs`
faithfully. The SSE parser, request builder, 401/403 body suppression, and the
Anthropic-vs-OpenAI dispatch gate are all correct and well covered by tests.
The findings below are ordered by severity; none are crashes, but two are
functional inconsistencies that should be fixed.

---

## Medium

### M1. IPC model pickers don't mirror the Anthropic key preference — `ANTHROPIC_API_KEY` missing (src-tauri/src/ipc/models.rs:82-87, 146-151)

`resolve_api_key` (client_factory.rs:38-50) resolves keys for an Anthropic-kind
endpoint as `stored → ANTHROPIC_API_KEY → ANTHROPIC_AUTH_TOKEN → "dummy"`. But
both `list_models` and `list_vision_models` still use the fixed chain
`stored → OPENAI_API_KEY → ANTHROPIC_AUTH_TOKEN → "dummy"` for every kind.

Consequence: a user who configures an Anthropic-kind endpoint with **no stored
key** but with `ANTHROPIC_API_KEY` set in the environment gets a working chat
provider (the factory path resolves the key correctly) while the Settings →
Endpoints "Pick from server" / "Test connection" and the Vision picker send
`"dummy"` as the key → 401, with a confusing error. The doc comment at
models.rs:42-47 ("Resolution (mirrors the provider's key fallback chain in
[`build_client`])") claims parity that doesn't exist.

Fix: when the resolved kind is `"anthropic"`, fall back to
`ANTHROPIC_API_KEY` before `ANTHROPIC_AUTH_TOKEN` (or make the chain
kind-aware in both commands). Note: the new client_factory test
`anthropic_config_resolves_key_via_anthropic_env_preference` only exercises
the stored-key path (its comment explicitly avoids the env surface), so the
`ANTHROPIC_API_KEY` preference branch itself is untested — add an env-var
test for `resolve_api_key` (Anthropic kind with `ANTHROPIC_API_KEY` set must
beat an unset `OPENAI_API_KEY`), and ideally a models.rs-level check.

### M2. Parallel tool results produce consecutive `user` messages — non-canonical wire shape (src/provider/anthropic.rs:306-322)

Each `Role::Tool` message is serialized as its own `user`-role message with a
single `tool_result` block. The turn driver pushes N tool messages for N
parallel tool calls (turn.rs:1287-1295), and the new client declares
`supports_parallel_tools: true` (mod.rs:90) — so a parallel batch yields
`assistant(tool_use xN)` followed by N consecutive `user(tool_result)` messages.

The canonical Messages-API shape for a parallel batch is ONE user message
containing all N `tool_result` blocks. The API may merge consecutive
same-role turns server-side (Anthropic documents combining consecutive user
turns), but this is exactly the kind of edge that produces intermittent 400s
or subtle semantic drift on Anthropic-compatible gateways, which this feature
explicitly targets. There is no test covering two adjacent `Role::Tool`
messages.

Fix: in `build_request_json`, coalesce adjacent `Role::Tool` messages into a
single `user` message whose content array carries all `tool_result` blocks
(preserving order). Add a test: two tool messages → one user message with two
`tool_result` blocks.

---

## Low

### L1. EndpointCard models cache key omits `kind` — stale list after a kind flip (frontend/src/components/settings/sections/EndpointCard.tsx:141, 175)

`fetchServerModels` early-returns when `modelsCacheKey === ${base_url}\n${apiKey}`
and state is ready. Flipping the kind dropdown (openai → anthropic) on a card
leaves the key unchanged, so "Pick from server" keeps showing the previous
kind's cached list (fetched with the wrong auth headers) until an explicit
refresh. `testConnection` always refetches, but it then *re-arms* the same
kind-less key, so the stale state persists afterward. VisionSection.tsx:85
already includes kind in its cache key — the omission here is an inconsistency
that proves intent. Fix: `const key = `${endpoint.base_url}\n${apiKey}\n${endpoint.kind}``
in both fetchServerModels and testConnection.

### L2. Fallback `Finish(Stop)` is delivered to the consumer after the real Finish — overwrites the real reason (src/provider/anthropic.rs:922-931)

`message_stop` emits the real `Finish{reason}` + `Usage` exactly once (parser is
correct). But when the stream then ends, the loop's fallback sends a second
`Finish{reason: Stop}` to the consumer unconditionally (`finish_logged` gates
only the trace mirror, not the consumer event). The consumer assigns
`finish_reason = reason` on every Finish (turn.rs:781-783), so the last one
wins: an Anthropic stream that stops with `tool_use` or `max_tokens` is
reported to the turn as `Stop`. Concrete consequence: the "Tool call was
truncated (hit the token limit)" retry branch (turn.rs:1007, keyed on
`FinishReason::Length`) can never fire for Anthropic streams, and
`AgentEvent::Finished`/`TurnOutcome.finish_reason` report Stop for tool turns.
This mirrors the pre-existing `openai.rs` behavior (openai.rs:884-888), so it
is not a regression — but the new client advertises `reliable_finish_reason:
true` (mod.rs:91), making the mismatch meaningful. Fix: track a
consumer-facing `saw_finish` flag set in the event arm (next to
`finish_logged`) and skip the fallback send when a real Finish was already
emitted — a strict improvement that also fixes the OpenAI path identically.

### L3. "Supports reasoning effort" + effort dropdown are shown for Anthropic-kind endpoints but silently ignored

`anthropic_client_config` has no `reasoning_effort` field, and `build_client`
(client_factory.rs:131-139) drops the `reasoning_effort` argument on the
Anthropic branch. The EndpointCard (EndpointCard.tsx:396-454) renders the
checkbox + dropdown for every kind, and the setting persists to
`endpoints.toml`, where it is dead config for Anthropic endpoints. Either
disable/hide the effort controls when `endpoint.kind === "anthropic"`, or
document the no-op in the UI. (Anthropic's `thinking` parameter is a separate
feature — out of scope — but the current state silently misleads.)

### L4. Stale doc comments

- src-tauri/src/ipc/settings.rs:498 — `/// Map [`EndpointKind`] to the serde wire form ("openai" / "local").` — missing `"anthropic"` (the arm below handles it).
- src-tauri/src/ipc/settings.rs:1283-1284 test comment — same `("openai"/"local")` staleness (trivial).
- src/provider/anthropic.rs:373-374 — `message_start` doc says it captures "the model id"; the code doesn't capture it. Either drop the claim or capture it (nothing consumes it today; drop the claim).

---

## Checks that passed (no findings)

- **SSE parser correctness**: `event:`-name tracking across chunks is sound —
  the incomplete-frame test (`retains_incomplete_trailing_line`, anthropic.rs:1442)
  proves a chunk-boundary-split `event:` line survives in the buffer and is
  re-read on the next call; `data:` frames are attributed correctly; unknown
  events ignored; `message_start`/`message_delta` usage capture and
  `message_stop` Finish+Usage emission are exactly-once per event; `stop_reason`
  mapping (end_turn/stop_sequence→Stop, tool_use→ToolCalls, max_tokens→Length,
  other��Other) is correct and tested.
- **Request builder**: system hoisting correct (concatenated with `\n\n`, blank
  texts dropped, field omitted when empty, no system messages left in
  `messages`); `max_tokens` required and floored at 4096; `tool_choice` mapping
  (Auto→omit, Function→`{type:"tool",name}`, Required→`{type:"any"}`) matches
  the Messages spec; tool `input` parsed from the stored JSON string with `{}`
  fallback; `strict` never leaks into the Anthropic shape; image blocks
  (base64 data URL → `source.type: base64` with media-type prefix + `image/png`
  fallback, plain URL → `source.type: url`) and multimodal=false stripping are
  correct; tool results ride on `user`-role messages (correct protocol choice;
  the multi-result coalescing gap is M2).
- **Kind dispatch**: every construction site (main.rs, config_io.rs
  `resolve_model_provider`, settings.rs re-sync, memory_maintenance.rs,
  `build_provider_for`, `context_manager_for_model`) goes through `build_client`;
  the only remaining `build_openai_client` call is the OpenAI/Local branch
  inside it. No path can build an `OpenAiClient` for an Anthropic endpoint;
  `build_vision_client` rejects the Anthropic kind with a warning + `None`.
  Dispatch-gate test present.
- **Error paths / security**: 401/403 response bodies suppressed in BOTH the
  user-visible error (`provider_error`) and the trace log (trace records
  "unauthorized (body suppressed)") — no key echo can reach the Trace tab;
  `x-api-key` is a header (never in URLs/bodies/errors); redirect
  `Policy::none()` on both the Messages client and `fetch_models_anthropic`
  (cross-host redirect key-leak closed); stream stall/decode errors include
  headers + truncated partial data but never the key; no key in `fetch_models_*`
  error strings.
- **Constitution**: no `#[allow(...)]` added; all new public items have doc
  comments; regression tests exist for every claimed behavior (24 in-file
  anthropic tests + factory dispatch/vision-reject tests + settings DTO +
  endpoints.toml parse + patch validation). The `rejects_unknown_kind` test was
  correctly re-keyed to `"claude"` since `"anthropic"` is now valid.
- **Multi-platform neutrality**: all new code is portable Rust (std/reqwest/
  tokio/serde) + plain TS; no Windows-only APIs, paths, shell syntax, or
  `cfg(windows)` additions anywhere in the change.
- **Docs sync**: README.md (feature list + config paragraph) and PLAN.md (LLM
  client + provider strategy rows + ProviderKind enum) updated for the
  anthropic kind; agent.md gains the "## Review expectations" section exactly
  as specified; module docs in anthropic.rs, client_factory.rs, models.rs, and
  tauri.ts all updated. No `endpoints.toml` example exists in the repo to
  update. (Residual staleness is L4.)
