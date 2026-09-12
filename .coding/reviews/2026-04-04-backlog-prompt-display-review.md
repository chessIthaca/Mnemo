# Review: Backlog-dispatched prompts as the goal in the agent transcript

**Date:** 2026-04-04
**Scope:** All uncommitted changes (`git diff`): `src/runtime/channels.rs`,
`src-tauri/src/ipc/commands.rs`, `frontend/src/lib/types.ts`,
`frontend/src/hooks/useAgentStore.ts` (+ plan/backlog bookkeeping files, ignored).

## Verdict: no findings (clean)

The change is correct, consistent with existing patterns, and constitution-
compliant. Details on each concern raised in the review brief follow.

---

## 1. Event ordering — PromptDispatched before Started? ✅

**Yes, reliably.** The ordering rests on a strong happens-before relationship
in the current code, not on a synchronization primitive:

- `dispatch_next_impl` (`commands.rs:1081-1090`) and `run_all_dispatch_next`
  (`commands.rs:1216-1222`) both call `manager.send(...)` (which is `try_send` —
  non-blocking, no await) then `drop(manager)` then `emit_prompt_dispatched(...)`
  **synchronously, before any subsequent `.await`**.
- `emit_prompt_dispatched` calls `app.emit(...)` synchronously (no await).
- The agent task that receives the `Prompt` command is parked on its inbox
  `recv()`. It cannot wake, process the Prompt, and emit `Started` through the
  fan-in until the current task yields at an `.await` — which happens *after*
  `emit_prompt_dispatched` has already run.
- The forwarder (`events.rs:61-213`) only emits `Started` to the frontend after
  it reads it from `fanin_rx` and calls `app.emit` — strictly later than the
  direct `app.emit` in `emit_prompt_dispatched`.

So `PromptDispatched` is emitted to the webview before the agent task is even
scheduled. Tauri delivers `app.emit` calls in call order (same channel the
existing `emit_child_finished` pattern relies on), so the frontend sees
`prompt_dispatched` before `started`. The user message lands at the top of the
turn. ✅

**Caveat (INFO, not a finding):** the ordering depends on *no `.await` being
inserted between `send` and `emit_prompt_dispatched`*. The comment at
`commands.rs:1086-1089` captures the intent ("Emitted right after the Prompt
command so it lands before the agent's `Started` event") but not the scheduling
reasoning. A future maintainer who inserts an `.await` there could let the agent
wake and emit `Started` first. Even then the impact is cosmetic (user message
appears below the first streaming chunk instead of above it) — the
`flushStreamingText` call in the reducer handles pre-existing streaming text
gracefully. No action required.

## 2. Direct `app.emit` vs. fan-in — correct and consistent ✅

Emitting directly via `app.emit` (bypassing the manager's fan-in sender) is the
right choice and is **consistent with `emit_child_finished`** (`events.rs:272-284`),
which also synthesizes a `SerializableAgentEvent` and emits it directly. Both
`PromptDispatched` and `ChildFinished` are *forwarder-synthesized* events (not
emitted by the agent itself), so routing them through the agent's fan-in channel
would be unnecessary. The direct emit is what *ensures* PromptDispatched lands
before the agent's own `Started` (which does go through the fan-in). ✅

## 3. Reducer mirrors `handleSend`'s transcript append ✅

`reducePromptDispatched` (`useAgentStore.ts:922-932`) constructs the user entry
identically to `handleSend` (`InputBar.tsx:200-207`):

| Field        | `handleSend`                          | `reducePromptDispatched`              |
|--------------|---------------------------------------|---------------------------------------|
| `kind`       | `"user"`                              | `"user"`                              |
| `text`       | `input`                               | `event.text`                          |
| `images`     | `...(images.length > 0 ? { images } : {})` | `...(event.images.length > 0 ? { images: event.images } : {})` |

The only difference is the reducer calls `flushStreamingText(next)` first —
correct and defensive: a backlog dispatch could in principle arrive while
streaming text exists (though `dispatch_next_impl` guards on `busy`), and
flushing ensures the prompt lands below any in-flight assistant text rather
than clobbering `streamingText`. `handleSend` doesn't flush because the main
input is only sent when the agent is idle. ✅

The `TranscriptEntry` type (`types.ts:176-177`) is `{ kind: "user"; text: string; images?: string[] }`
— the reducer's conditional spread matches the optional `images` field exactly. ✅

## 4. No double-append risk ✅

The agent itself never emits an event that adds a *user* transcript entry. The
only two paths that append a `kind: "user"` entry are:

1. `handleSend` (main input) — optimistic append before `sendPrompt`.
2. `reducePromptDispatched` (backlog dispatch) — event-driven append.

These are mutually exclusive: a prompt originates either from the main input or
from the backlog dispatch, never both. The `run_all_dispatch_next` path also
sends `AgentCommand::Suggestion(RUN_ALL_STEER)` before the Prompt, but
`suggestion_injected` appends a `kind: "steer"` entry (`useAgentStore.ts:907`),
not a `user` entry — no conflict. ✅

## 5. `useAgentEvents.ts` dispatch path ✅

`prompt_dispatched` is a non-text event, so in `useAgentEvents.ts:160-181` it
takes the `else` branch: `flushBuffers()` (flushes any buffered `text_delta`
fragments to the store via `appendStreamingText`), then `handleAgentEvent(payload)`.
`handleAgentEvent` → `applyAgentEvent` → `case "prompt_dispatched"` arm
(`useAgentStore.ts:1026`). The pre-dispatch `flushBuffers()` ensures the
reducer's `flushStreamingText` sees all accumulated text before appending the
user entry. Flows through correctly. ✅

The `Message.tsx:59` switch is on `entry.kind` (transcript entry kind), not
event kind — the `user` entry is already rendered (`Message.tsx:60-79`), so no
frontend rendering change is needed. ✅

## 6. Roundtrip test ✅

`prompt_dispatched_roundtrips_json` (`channels.rs:459-485`) is correct and
mirrors the existing `child_finished_roundtrips_json` pattern:

- Constructs `AgentEvent::PromptDispatched`, calls `into_serializable()`.
- Asserts `sender.is_none()` — correct (only `ApprovalRequest` carries a
  oneshot).
- Serializes and checks `"kind":"prompt_dispatched"` (matches
  `#[serde(tag = "kind", rename_all = "snake_case")]` at `channels.rs:166`).
- Deserializes back and asserts `text` + `images` fields match.

The test validates the serialization format that production uses. ✅

## 7. Constitution compliance ✅

- **Windows paths / PowerShell:** No shell commands or Linux paths in the diff.
- **Doc comments on public functions:** `emit_prompt_dispatched` is a *private*
  `fn` (no `pub`) — the constitution's "all public functions must have doc
  comments" rule doesn't strictly apply, but it has a doc comment anyway
  (`commands.rs:916-920`). The new public enum variants
  (`AgentEvent::PromptDispatched`, `SerializableAgentEvent::PromptDispatched`)
  both have doc comments (`channels.rs:111-119`, `197-199`). ✅
- **Never commit to main:** No commit in this diff (review only). ✅
- **Run tests before marking complete:** Roundtrip test added; the implementer
  must run `cargo test` before closing the plan (reviewer's reminder, not a
  finding against the diff).

---

## Notes (INFO — not findings, no action required)

**N1. `AgentEvent::PromptDispatched` + its `into_serializable` arm are only
exercised by the test, not production.** Production constructs
`SerializableAgentEvent::PromptDispatched` directly in `emit_prompt_dispatched`
(`commands.rs:928`), bypassing `AgentEvent::PromptDispatched` and its
`into_serializable` arm (`channels.rs:292-295`). This mirrors the `ChildFinished`
pattern (`notify_parent_on_completion` at `events.rs:262` constructs
`SerializableAgentEvent::ChildFinished` directly; `AgentEvent::ChildFinished`'s
`into_serializable` arm is likewise test-only). Consistent design choice — the
internal enum variant exists for completeness so the forwarder would handle it
correctly if the agent ever emitted it through the fan-in. Not a bug.

**N2. Ordering is a soft (scheduling-based) guarantee.** See §1 caveat above.
Correct in the current code; the only failure mode is a future `.await`
inserted between `send` and `emit_prompt_dispatched`, and even then the impact
is cosmetic.
