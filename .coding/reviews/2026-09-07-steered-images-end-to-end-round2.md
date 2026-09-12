## Verdict: PASS

Round-2 verification of commit 8c2612a ("fix: carry steered images end-to-end through the steer pipeline (dabfe373)", HEAD of `wt/agenticcoding`) against the round-1 findings in `2026-09-07-steered-images-end-to-end-review.md` (1 high, 2 low). All three findings are genuinely resolved in the committed code, and the commit as a whole is sound: the payload threads end-to-end with no stale text-only shape left anywhere, every injection site routes through the shared `build_user_content` path, CancelSuggestion's text-match is preserved, the regression tests exist and exercise the changed paths, and the docs are synced. No new blocking issue.

Method note: `cargo` could not be run in this review environment, so the HIGH 1 compile fix was verified statically — every src-tauri site touching the changed shapes was enumerated by exhaustive literal search (10 `AgentCommand::Suggestion` sites, 2 `SuggestionInjected` sites) and each checked against the widened root-crate definitions. The commit message records both Rust suites + frontend green (root 2039, src-tauri 233 + 4, frontend 1003 + build).

## Round-1 finding verification

### HIGH 1 — src-tauri compile fallout: RESOLVED

The root-crate shapes the app crate must compile against are all as claimed: `SteerPayload { pub text: String, pub images: Vec<String> }` with `From<&str>` and `From<String>` impls (src/runtime/channels.rs:62-85), `AgentCommand::Suggestion(SteerPayload)` (channels.rs:99), `AgentEvent::SuggestionInjected { text, images }` (channels.rs:222), `SerializableAgentEvent::SuggestionInjected { text, images }` (channels.rs:494-497) with the serialization mapping (channels.rs:709-710). Every round-1 site is fixed exactly as prescribed:

- console.rs:775 and ipc/events.rs:866 — both now `AgentCommand::Suggestion(text.into())`; `text` is a `String` from the unchanged `turn_resolve` helpers and `From<String> for SteerPayload` exists, so both compile.
- console.rs:271 — `AgentEvent::SuggestionInjected { text, .. }` (binds text, `..` absorbs `images`); `text` is used in the render line.
- ipc/events.rs:1378-1381 — the fixture gained `images: vec![]`.
- The five test pattern sites (console.rs:2425, 2481, 2532; events.rs:1572, 1682) all bind `AgentCommand::Suggestion(payload)` and assert on `let text = &payload.text;` — `text` is `&String`, so `.contains(...)` and `{text}` formatting are valid. The two wildcard arms (events.rs:1601, 1624 — `Suggestion(_)`) are shape-agnostic.
- `send_suggestion` (src-tauri/src/ipc/agent.rs:241-291) gained `images: Vec<String>` and builds `AgentCommand::Suggestion(mnemo::runtime::SteerPayload { text, images })` directly.

Exhaustive sweeps confirm nothing was missed: exactly 10 `AgentCommand::Suggestion` occurrences in src-tauri (the 2 send sites, 1 IPC construction, 7 test arms — all listed above) and exactly 2 `SuggestionInjected` occurrences (the console match and the events fixture) — every one consistent with the new shapes. No other src-tauri code constructs or matches the widened variants.

### LOW 1 — `arePropsEqual` steer arm: RESOLVED

frontend/src/lib/messageEquality.ts:60-72 — the `steer` case now compares `images` by reference and `imagesEvicted` by strict equality before falling through to the text comparison, mirroring the user arm (lines 47-54), with a comment explaining the budget-flip rationale. A `capTranscriptImages` eviction of a steer entry now correctly fails the memo check and re-renders the row as the "N images unloaded" chip.

### LOW 2 — doc comment placement: RESOLVED

src/runtime/agent.rs:142-156 — `AgentTask::build_user_content` with its own 5-line doc comment now sits ABOVE `run_turn_with_retry`'s doc comment (lines 158-178), which is contiguous and immediately precedes `async fn run_turn_with_retry(` (line 179). `run_turn_with_retry`'s docs are whole again (retry semantics, `AfterTurn` meanings, steer soft-stop chain) and `build_user_content`'s rendered docs no longer open with turn-retry text.

## Commit-as-a-whole verification (no findings)

**Payload threading end-to-end.** InputBar's steer branch captures `const images = attachedImages` (InputBar.tsx:275) BEFORE the draft/tray clear (279-280) and passes them to `addSteer(agentId, input, images)` + `sendSuggestion(agentId, input, images)`; `sendSuggestion` (tauri.ts) has an `images: string[] = []` default so every JS caller is covered. Backend: `Suggestion(SteerPayload)` → `StopReason::fold` normalizes both steer-carrying commands (`Suggestion(p)` and mid-stream `Prompt { text, images }` — the same bug class, fixed by the same arm) into `fold_steer`, which preserves the accumulation and grace-window promotion semantics (Interrupt → InterruptWithSteers, Compact → CompactWithSteers); `buffered_steers` is `Vec<SteerPayload>`; `AfterTurn::Compact(Vec<SteerPayload>)` threads through `run()` → `compact_context`.

**Injection sites — all via the ONE image path.** `AgentLoop::build_user_content` (loop_impl.rs:1586+) implements the exact prompt-arm triage (no images → text; multimodal or no vision client → `Parts` + `ImageUrl` blocks; text-only + vision → `VisionDescribe`/`describe_image_data_url`/`VisionDescribed` per image with descriptions folded). Every injection site routes through it: `run_turn_with_retry`'s Steer arm (agent.rs:202-233), `drive_turns`' Compact arm (345-366), the between-turns Suggestion arm (652-694), the `/compact` re-injection (913-967), and turn.rs's `handle_pending_swap` + `maybe_compact` re-injections — the latter three keeping the documented split (text-only steer → system message, unchanged; image-bearing steer → user message with image blocks; Prompt → `build_user_content`). The Prompt arm's former 70-line inline image logic was replaced by delegation to the same helper (dedup, no behavior change).

**SuggestionInjected carries images** at all five emission sites (agent.rs:217, 352, 683, 929; turn.rs:1548), through serialization (channels.rs:709) with a JSON roundtrip test (channels.rs:1131-1158).

**CancelSuggestion text-match preserved:** `fold` retains by `p.text != s`, `drop_cancelled_steers` by `!cancelled.contains(&p.text)`, the provider-request buffer by `p.text != s` — all pinned by `fold_cancel_by_text_still_drops_image_bearing_steer`, and the limitation is documented in both updated SPECs.

**Frontend carry-through:** `suggestion_injected` event type widened (types.ts) with all constructors updated (store tests ×2, ipc fixture); `reduceSuggestionInjected` pushes the steer transcript entry with images (spread only when non-empty); `SteerEntry.images?` + bubble ImageIcon/count indicator; click-to-edit loads `steer.images ?? []` back into the tray; `capTranscriptImages`'s `bearsImages` covers user + steer entries; Message.tsx's steer case renders thumbnails + the eviction chip mirroring the user case.

**Regression tests present and exercising the changed paths:** `midturn_image_input_injects_user_message_with_image_block` (agent.rs:3329 — mid-stream image-bearing Prompt through the fold, multimodal provider), `steer_with_image_injects_user_message_with_image_block` (agent.rs:4596 — asserts both the `SuggestionInjected` images AND the follow-up request's `ImageUrl` part), `non_multimodal_with_vision_describes_steered_image` (agent.rs:4672-4751), `fold_carries_images_with_steers_in_order`, `fold_prompt_mid_stream_carries_images`, `fold_cancel_by_text_still_drops_image_bearing_steer` (loop_impl.rs), `suggestion_injected_roundtrips_json` (channels.rs), and the frontend "suggestion_injected: carries the steer's images into the transcript entry" store test. The grace-window test (src/agent/tests.rs) and context.rs buffering test were updated to the payload shape.

**Docs synced:** new SPEC `2027-01-07-steered-images-ride-the-steer-payload-end-to-end.md` is accurate against the code (pipeline, events/UI, cancel limitation, test list all check out); the cancel-steer SPEC and pasted-images SPEC correctly updated; BUG knowledge file + plan file committed; the stale "Steers are text-only" InputBar comment replaced; channels.rs/loop_impl.rs doc comments match the new shapes.

**Multi-platform neutrality:** clean — no Windows-only APIs, paths, or shell syntax anywhere in the diff; all new code is platform-neutral Rust/TS.

**Warning-free plausibility:** no unused imports, bindings, or dead code introduced (the `..`-bound `images` in console.rs:271, every `payload` binding is used, `fold_steer` is called from both fold arms, the `steer_with_images` test helper is used).

## Non-blocking observations

- The `.coding/backlog.jsonl` hunk in this commit only attaches the provider-error checkpoint note to item 950e81f3 (status still `pending`) — item completion is the parent workflow's finish-time bookkeeping, not a code concern.
- The src-tauri suite's green status rests on the commit message's recorded run plus this review's static exhaustiveness check; if any doubt remains, a follow-up `cargo test` inside `src-tauri` (or `--workspace`) re-run is cheap confirmation.
