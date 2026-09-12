# Review — Notification sounds (complete ding / needs-input ping / doom on triple-error stop)

Branch `feat/notification-sounds` (uncommitted). Reviewed ALL uncommitted changes per `git diff HEAD` + `git status --short`: the 16 modified files plus the 4 untracked new files (`frontend/src/lib/sounds.ts`, `sounds.test.ts`, `components/settings/sections/SoundsSection.tsx`, `SoundsSection.test.ts`). Backend flow verified against `src/agent/turn.rs`, `src/runtime/agent.rs`, `src/agent/mod.rs`, `src/workflow/mod.rs`.

## Findings

### 1. CORRECTNESS (must fix) — Provider-exhaustion doom never fires: `Started` is re-emitted between provider retries and resets the streak

The real provider-retry event stream is not what the reducer (or its test) models:

- `run_turn` emits `AgentEvent::Started` first thing on **every call** (`src/agent/turn.rs:87-89`).
- `run_turn_attempt` retries the **entire** `run_turn` per attempt (`src/runtime/agent.rs:150-160`), up to `MAX_PROVIDER_TURN_ATTEMPTS = 3` (`agent.rs:21`), emitting `Error { retrying: true }` between attempts (`agent.rs:169-179`) and a final `Error { retrying: false }` after the third (`agent.rs:209-217`).
- So the stream the frontend actually receives on provider exhaustion is: `Started → Error(retrying) → Started → Error(retrying) → Started → Error(final)`.
- `reduceStarted` resets `consecutiveToolErrors = 0` unconditionally (`frontend/src/hooks/agentEventReducer.ts:163-165`), so each re-`Started` wipes the streak. At the final error the streak is **1**, not 3 → `reduceError` (`agentEventReducer.ts:819-821`) never requests `sound: "doom"` on this path.

This is exactly one of the two "three errors and the agent just stops" scenarios the backlog item asked for (the other — the tool-error abort path — works correctly, see Verified below).

The test **pins an impossible sequence**: `"retrying errors extend the streak (provider-retry path) → doom on the third"` (`frontend/src/hooks/useAgentStore.test.ts:1005-1015`) feeds three `error` events with **no interleaved `started` events**, so it passes while the real path is broken. Per the project's defect-test rule, the regression test must reproduce the real backend sequence (with the `started` events between the retrying errors) and fail without the fix.

Suggested fix (frontend-only): in `reduceStarted`, reset the streak only when the agent was **not already running** (`!agent.running`). Provider retries keep `running = true` (a `retrying: true` error does not clear it), so the re-`Started` between attempts would stop resetting the streak, and the final error lands at streak 3 → doom. Caveat to decide when fixing: the steer soft-stop path also runs a follow-up turn whose `Started` may arrive while `running` is still true — the backend's `tool_error_count` is per-`run_turn` and resets there; either mirror that too (reset on the steer boundary) or document the accepted divergence.

### 2. LOW (semantic divergence) — Frontend streak counts user denials that the backend deliberately excludes

The backend does **not** count user denials / interrupts toward `tool_error_count` (`src/agent/turn.rs:1223-1233`, via `is_user_denial_tool_output` at `turn.rs:1545-1551` — "user denied", "interrupted while awaiting approval", …), because they are deliberate safety choices, not a stuck model. The frontend `reduceToolResult` increments on **every** `success: false` result (`agentEventReducer.ts:404-409`). Consequence: a DenyAll batch (3+ denied calls, all forwarded as failed `tool_result` events — `turn.rs:1298-1306`) inflates the frontend streak to 3, and any later unrelated final error in the same turn then plays doom even though the backend never hit its cap. Superset counting → occasional spurious doom, never a missed one. The reducer receives `event.result.output`, so mirroring the denial-substring check is straightforward if you want exact parity.

### 3. LOW (doc accuracy / minor UX) — `skill_end` returning to Complete re-dings; "every ding means an actual plan just closed" overclaims

`WorkflowStateChanged` is emitted after **every successful** workflow tool call with the post-call state (`turn.rs:1331-1366`) — it is not transition-gated on the backend. A skill that starts and ends in Complete (e.g. `merge_to_main` — explicitly described at `turn.rs:1384-1388`; `skill_start` is gated to Complete+Planning, `factory.rs:609-610`) re-enters `Complete` at `skill_end`, emitting `state: "complete"` a second time → a second ding for the same already-closed plan. Arguably fine UX ("agent hit complete state again"), but the comment on `reduceWorkflowStateChanged` (`agentEventReducer.ts:630-634`) claims transition-only semantics that don't hold. If undesired, suppress the sound when the previously recorded `workflowStates[agentId]` was already `complete` (frontend-side transition detection).

### 4. LOW (doc link) — Wrong path in the `consecutiveToolErrors` doc comment

`frontend/src/hooks/agentState.ts:196-201` links `[DOOM_ERROR_STREAK](../lib/sounds)`, but `DOOM_ERROR_STREAK` is exported from `./agentEventReducer`, not `lib/sounds`. Point the link at the right module.

## Verified clean (no findings)

- **Reducer purity**: sound is requested only via `Effects.sound`; `agentEventReducer.ts` imports only the `SoundKind` type — no audio calls. Reducers stay pure; the dispatcher owns playback.
- **Dispatcher wiring** (`useAgentStore.ts:999-1016`): zustand `set` is synchronous, so reading the flags from `getState()` after the `set` is fresh — no staleness race. `playSound` can't throw in practice (`getCtx` fully guarded; scheduling on a live context doesn't throw; `close()` is never called). One execution per event — no double-play.
- **Tool-error abort path (primary doom scenario) works end-to-end**: failed tool executions ARE forwarded to the UI as `tool_result` events carrying `success: false` (`turn.rs:1298-1306`); 3 consecutive failures within one `run_turn` → frontend streak 3 → the final abort error (`turn.rs:1493-1505`) lands at streak 4 ≥ 3 → doom fires. The malformed-JSON path (`turn.rs:984-1010`) also reaches doom (2 retrying errors + 1 final = 3 error events inside one turn). `DOOM_ERROR_STREAK = 3` mirrors `MAX_RETRIES = 3` (`src/agent/mod.rs:38`).
- **Reset semantics**: success resets (`reduceToolResult`), `started` resets, streak includes the current error — consistent for the within-turn paths (finding 1 covers the cross-attempt hole).
- **Input ping**: `approval_request` / `user_question` each fire once per blocked decision (DenyAll latches so no repeat requests); double-ping not a concern.
- **Settings round-trip is complete and safe**: old `config.toml` without the keys parses via defaults (`sounds_default_on_and_round_trip` covers `""` and partial `[ui]`); `GetSettingsUi` + golden fixtures updated on both sides (`contract_fixtures.rs`, `dto-get-settings.json`); `SettingsSaveDto` is a serde-default `Option` patch so ChatSection's save (`ChatSection.tsx:75-78`, chat keys only) cannot clear the sound flags; SoundsSection persists all three through store setters + patch (`SoundsSection.tsx:108-134`, same store-before-IPC pattern as ChatSection); `App.tsx:252-262` hydrates unconditionally inside the existing `if (settings.ui)` guard, matching the documented no-localStorage-mirror design.
- **Audio implementation** (`sounds.ts`): one lazy module-level `AudioContext` (window lifetime — not a leak; never closed); `resume()` on suspended with `.catch`; all exponential ramps start from `0.0001` (never a zero-value ramp error); `osc.stop(t1 + 0.02)` on every note; gains ≤ 0.12; doom span 520 ms < 600 ms. Preview buttons intentionally ungated (audition + user-gesture unlock) — matches the stated design.
- **Constitution**: doc comments on all new public Rust and TS items; no `#[allow(...)]` introduced; TS strict-clean (exhaustive switch in `soundEnabled`); `vitest.config.ts` correctly includes `sounds.test.ts` explicitly (line 35) and `SoundsSection.test.ts` via the `src/components/settings/**/*.test.ts` glob (line 13).
- **`.coding/` churn** (backlog flip of item 57 to in_flight, stack.json swap, new plan file) is expected bookkeeping, not a defect. (Note: unrelated backlog items 62/63 were appended and #54's typo fixed in the same file — pre-existing rider churn, flagging only for awareness.)

## Summary

One must-fix correctness defect (finding 1 — doom is silent on the provider-exhaustion path because retry re-`Started` events reset the streak, and its test validates an impossible event sequence). Findings 2–4 are minor parity/doc issues. Everything else — purity, dispatcher wiring, the tool-error doom path, the settings round-trip, audio synthesis, and constitution compliance — checks out.
