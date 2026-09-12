## Verdict: FINDINGS (1 high, 2 low)

Review of ALL uncommitted changes on `wt/agenticcoding` for plan dabfe373 ("Steering with an image drops the image"). The root-crate (mnemo) and frontend halves of the change are correct, complete, and well-tested — payload threading end-to-end, fold semantics, CancelSuggestion text-match, vision fallback, and the summarization re-injection split all verified sound. However, the `mnemo-app` crate (`src-tauri`, which depends on the local root crate via `path = ".."`) was NOT updated and does not compile: six unchanged sites still use the old `Suggestion(String)` / `SuggestionInjected { text }` shapes. The claimed `cargo test` run covered only the root package. Two minor issues: `arePropsEqual`'s steer arm misses the images/imagesEvicted comparison, and a doc comment got misattached in `src/runtime/agent.rs`.


---

## HIGH 1 — `mnemo-app` (src-tauri) does not compile: six sites still use the old text-only shapes

`src-tauri` depends on the local root crate (`src-tauri/Cargo.toml` line 12: `mnemo = { path = "..", features = ["browser", "embeddings"] }`). The widened `AgentCommand::Suggestion(SteerPayload)`, `AgentEvent::SuggestionInjected { text, images }`, and `SerializableAgentEvent::SuggestionInjected { text, images }` break these **unchanged** src-tauri sites (none are in the diff):

1. `src-tauri/src/console.rs:775` — `mgr.send(parent_id, AgentCommand::Suggestion(text))` where `text` is a `String` (from `turn_resolve::completion_suggestion_text` / `reviewer_failure_suggestion_text`, both return `String` — unchanged in `src/runtime/turn_resolve.rs:226,248`). → E0308 (expected `SteerPayload`, found `String`).
2. `src-tauri/src/ipc/events.rs:866` — same construction, same `String` source. → E0308.
3. `src-tauri/src/console.rs:271` — `AgentEvent::SuggestionInjected { text } =>` pattern missing the new `images` field. → E0027.
4. `src-tauri/src/ipc/events.rs:1378` — `SerializableAgentEvent::SuggestionInjected { text: "s".into() }` construction missing `images`. → E0063.
5. `src-tauri/src/console.rs:2425-2431, 2480-2488, 2530-2534` (tests) — pattern binds `text: SteerPayload`, then calls `text.contains(...)` and formats `{text}`. `SteerPayload` has no `contains` method and no `Display` impl. → E0599 / E0277.
6. `src-tauri/src/ipc/events.rs:1569-1573, 1678-1693` (tests) — same (`text.contains("child")`, `text.contains("no report")`, `{text}`).

**Why the claimed test pass missed it:** the root `Cargo.toml` is a workspace (`[workspace] members = ["src-tauri"]`), but plain `cargo test` at the root builds/tests only the root package `mnemo` — the `src-tauri` member is compiled only by `cargo test` inside `src-tauri` (or `--workspace`). The reported "cargo test passed (2039+16 tests)" was root-only. The project's own convention (backlog items) requires verifying **both** Rust suites; the app crate build (`cargo tauri build` / `cargo test` in src-tauri) fails until this is fixed.

**Fix (mechanical):**
- console.rs:775 and events.rs:866 → `AgentCommand::Suggestion(text.into())` (`From<String> for SteerPayload` exists for exactly this).
- console.rs:271 → `AgentEvent::SuggestionInjected { text, .. }` (or bind `images` too).
- events.rs:1378 → add `images: vec![]`.
- The five test pattern sites → bind the payload and assert on `text.text` (e.g. `AgentCommand::Suggestion(p) => { let text = &p.text; ... }`), or destructure.
- Then run the src-tauri suite (`cargo test` with manifest `src-tauri/Cargo.toml`) — its steer-notification tests (child-finished / failed-reviewer protocol) must pass.

## LOW 1 — `arePropsEqual` steer arm doesn't compare `images` / `imagesEvicted`

`frontend/src/lib/messageEquality.ts:60-61` — the `steer` case returns `pe.text === ne.text` only. The `user` arm (lines 47-54) deliberately also compares `images` and `imagesEvicted`, with a comment explaining why: `capTranscriptImages` can flip an entry from `images` to `imagesEvicted` and the memoized `Message` must re-render. This change makes steer transcript entries bear images (`reduceSuggestionInjected` pushes them; `Message.tsx:336-368` renders thumbnails + the eviction chip) and extends the byte budget to them (`bearsImages` includes `"steer"` in `agentState.ts`) — but the memo gate wasn't widened. When the 32 MiB budget evicts a steer entry's images, `arePropsEqual` returns "equal" and the row keeps rendering the full data-URL thumbnails instead of the "N images unloaded to save memory" chip — the exact staleness the user-arm handling was added to prevent. Fix: mirror the user arm's `images`/`imagesEvicted` comparison in the `steer` case.

## LOW 2 — Doc comment misattached: `build_user_content` inserted mid-way through `run_turn_with_retry`'s docs

`src/runtime/agent.rs:163-177` — the new `AgentTask::build_user_content` was inserted **between** the last line of `run_turn_with_retry`'s doc comment ("…the steer becomes a user message that drives the next turn.") and the `async fn run_turn_with_retry(` it documents. With no blank line separating them, the entire contiguous `///` block (run_turn_with_retry's docs + build_user_content's own docs) now attaches to `build_user_content` — its rendered docs open with ~20 lines about turn-retry/`AfterTurn` semantics — and `run_turn_with_retry` is left with no doc comment at all. Fix: move the insertion point above the start of `run_turn_with_retry`'s doc comment (or blank-line the two blocks apart).


---

## Verified correct (no findings)

**1. Payload threading end-to-end (root crate + frontend):**
- `SteerPayload { text, images }` with `From<&str>`/`From<String>` — all root-crate construction sites compile via `.into()` (approval.rs:663, context.rs:1498, runtime/mod.rs:399/420, loop_impl tests) or direct payload construction (ipc/agent.rs:287).
- All injection sites thread images: `run_turn_with_retry` Steer arm (agent.rs:~210), `drive_turns` Compact arm (~323), between-turns Suggestion arm (~680), `/compact` summarization re-injection (~870-960), `request_stream` select! (turn.rs:~2051-2075: Suggestion pushes payload, Prompt pushes `{text, images}`, CancelSuggestion retains by text), `handle_pending_swap` (turn.rs:~795-825) and `maybe_compact` (turn.rs:~1500-1584) re-injection, `StopReason::fold` + `fold_steer`, `drop_cancelled_steers`, `buffered_steers` → `StopReason::Steer(buffered_steers)` (turn.rs:407-408).
- `AfterTurn::Compact(Vec<SteerPayload>)` threading: constructed from `drive_turns`' return, consumed in `run()`'s Compact arm → `compact_context(steers)` → re-injection. Types line up.
- Frontend: InputBar steer branch captures `attachedImages` before draft-clear, passes to `addSteer` + `sendSuggestion`; pending-steer bubble shows ImageIcon + count; click-to-edit loads `steer.images` back into the tray; `reduceSuggestionInjected` pushes the steer transcript entry with images; `capTranscriptImages` covers steer entries via `bearsImages`; Message.tsx steer case mirrors the user case; `sendSuggestion` has an `images: string[] = []` default so all JS callers are covered; all `suggestion_injected` constructors (store tests, ipc fixture) updated — TS stays type-consistent.

**2. Fold semantics:** grace-window promotion arms preserved exactly (`fold_steer`: `Interrupt` → `InterruptWithSteers`, `Compact` → `CompactWithSteers`, steers vector carried); `CancelSuggestion` retains by exact `p.text != s` / `!cancelled.contains(&p.text)` — text-match unchanged; a mid-stream `Prompt` now folds WITH its images (same bug class, fixed).

**3. Vision fallback not bypassed:** every injection site routes through `AgentLoop::build_user_content` (loop_impl.rs:1603) — the ONE path: no images → plain text; multimodal or no vision client → `Parts` with `ImageUrl` blocks; text-only + vision → `VisionDescribe`/`describe_image_data_url`/`VisionDescribed` per image with descriptions folded into text. The summarization re-injection split (text-only steer → system message, unchanged; image-bearing steer → user message with image blocks; Prompt → `build_user_content`) is consistent across all three sites (agent.rs /compact, turn.rs model-swap, turn.rs auto-compact).

**4. Regression tests:** `midturn_image_input_injects_user_message_with_image_block` (verified failing pre-fix), `steer_with_image_injects_user_message_with_image_block`, `non_multimodal_with_vision_describes_steered_image` (CannedDescriber fires, description folded, NO image block), `fold_prompt_mid_stream_carries_images`, `fold_cancel_by_text_still_drops_image_bearing_steer`, frontend "suggestion_injected: carries the steer's images into the transcript entry" — all exercise the changed paths. Root cause documented: BUG memory (46f4a170) + `.coding/knowledge/bug/2027-01-07-steered-images-dropped-text-only-steer-pipeline.md`.

**5. Documentation sync:** new SPEC `2027-01-07-steered-images-ride-the-steer-payload-end-to-end.md` is accurate and complete; the cancel-steer and pasted-images SPECs correctly updated; the stale "Steers are text-only" InputBar comment replaced; channels.rs/loop_impl.rs doc comments updated to the new shapes. No other stale "text-only" claims found.

**6. Multi-platform neutrality:** clean — no Windows-only APIs, paths, or shell syntax anywhere in the diff; all new code is platform-neutral Rust/TS.

**7. Warning-free build:** no unused imports/dead code introduced in the root crate (full paths used, no import churn); the root suite passing warning-free is plausible. The src-tauri compile break (HIGH 1) is the only build concern — after fixing it, run the src-tauri suite to confirm it too is warning-free (`#![deny(warnings)]` at both crate roots).
