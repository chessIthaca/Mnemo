## Verdict: FINDINGS (0 high, 1 low)

The image-parsing announcement (AgentEvent::VisionDescribe/VisionDescribed + collapsible transcript card) is correct, fully paired on all reachable paths, well tested end-to-end (backend fanin assertions, reducer tests, IPC fixtures, console render test), documented in README, and platform-neutral. One low finding: a dangling running card if the agent task dies between the two events — same exposure as existing tool/memory cards, no interrupt path exists otherwise.


## Scope reviewed

`git diff HEAD` (12 modified files) + 4 untracked files (`frontend/src/lib/ipc-fixtures/event-vision-describe.json`, `event-vision-described.json`, `.coding/knowledge/bug/2026-08-27-silent-pause-on-image-prompts-vision-fallback-de.md`, `.coding/plans/3c8469c3.md`). Plan goal: announce the text-only-provider vision fallback (each pasted image described via the vision model BEFORE `run_turn`, where `AgentEvent::Started` fires) as a collapsible "image parsing" card showing the vision query + response.

## Correctness — verified

**Event pairing (the core invariant).** `src/runtime/agent.rs` Prompt arm (text-only + vision branch): `VisionDescribe { index: i+1, total, query }` is sent immediately before `describe_image_data_url(...).await`; `VisionDescribed { index, total, success: outcome.is_ok(), description }` is sent after, on BOTH the `Ok` (description) and `Err` (error text) paths — there is no `return`/`break`/early-exit between the two sends, and `Stop` acts via `cmd_rx`/`drive_turns`, not mid-arm. Every VisionDescribe gets exactly one VisionDescribed on all in-process paths. ✓

**Announced query matches the actual query.** The arm announces `image_tools::DEFAULT_DESCRIBE_PROMPT` and calls `describe_image_data_url(vision, data_url, None)`; `None` resolves to `DEFAULT_DESCRIBE_PROMPT` inside the helper (`src/tool/agent/image_tools/mod.rs:127-130`). No mismatch. ✓

**Reducer index-matching + fallback.** `reduceVisionDescribe` replaces a still-running entry with the same 1-based index (provider retry) instead of stacking — covered by a dedicated no-duplicate test. `reduceVisionDescribed` matches by `running && index`, falling back to the last running vision entry; because the backend describes images sequentially, at most one vision entry is ever running, so the fallback cannot mis-pair. A fully orphaned `vision_described` is dropped deliberately (documented in the doc comment). Both reducers run `capTranscript`. ✓

**Memo comparator completeness.** `arePropsEqual` case `"vision"` compares all six entry fields (`index`, `total`, `query`, `description`, `success`, `running`); the card's local `expanded` state is correctly excluded. `MessageImpl` has the `case "vision"` and no other frontend site switches exhaustively over `TranscriptEntry` (verified: only Message.tsx renders entries). ✓

**`spawn_capturing_task` signature ripples.** All three call sites updated: two bind `mut fanin_rx` and assert the events, the multimodal test binds `_fanin_rx` (no unused-binding warning under `#![deny(warnings)]`). No other callers exist (search-verified). ✓

**usize→u32 casts.** `index`/`total` are image-attachment counts; `as u32` in `into_serializable` cannot overflow in practice. TS side uses `number`. ✓

**No other Rust match sites break.** The only exhaustive match is `console.rs render_event` — new arms added with a render test (the test comment correctly notes a missing arm here previously broke the bin crate for CompactStarted). The console driver match ends `_ => {}` (console.rs:1256); `watchdog_note` ends `_ => return None` (events.rs:87); the forwarder's bookkeeping matches end `_ => {}` (events.rs:618, 744). `truncate` exists and is char-boundary-safe (console.rs:205, test at 1849). ✓

**Tests fail without the fix.** Backend: the two extended agent tests loop on `fanin_rx.recv()` with a 20s timeout waiting for the new events — without the emission they time out. Reducer/IPC-contract tests dispatch `vision_describe`/`vision_described` — without the switch arms the events hit the unknown-kind default and no `vision` entry appears. Console test fails to compile without the render arms (non-exhaustive match). Store tests reset via `beforeEach(resetStore)` (useAgentStore.test.ts:56), so the `toHaveLength(1)` assertions are not polluted. ✓

**Fixtures.** `event-vision-describe.json` / `event-vision-described.json` match the `tests/contract_fixtures.rs` samples field-for-field (index 1, total 2, query "Describe this image in detail." / description "a photo of a cat", success true) and the wire kind tags are the serde-derived snake_case names asserted by the new channels.rs round-trip tests. ✓

## Security — verified

Event payloads carry only the query text and the description/error text — the description is already injected into the user prompt, so this exposes nothing new to the renderer; no image bytes (no data URLs) cross the event channel. Console output truncates both fields char-safely. No new exfiltration surface. ✓

## Constitution checks

- **(a) Documentation sync** — README gains a precise feature bullet (announcement card, live spinner, expandable query + response, error in red). Module/item docs are thorough on both enums, both reducers, the `TranscriptEntry` variant, and the agent.rs fallback comment block. PLAN.md's `AgentEvent` sketch (PLAN.md:440-469) is explicitly illustrative ("beyond the original spec (shipped)") and already omits other shipped variants (CompactStarted, memory events), so no update required there. ✓
- **(b) Multi-platform neutrality** — pure Rust + TypeScript; no platform APIs, paths, or shell syntax. ✓
- **(c) Warning-free / docs** — all new public items carry doc comments; no `#[allow]` added; no unused imports/bindings apparent (the one unused receiver is `_`-prefixed). Note: I am read-only (no shell) — `cargo test` + frontend tests must be run by the main agent per the closing sequence; static review found no warning or compile-error sources. ✓
- **(d) Regression coverage** — see "Tests fail without the fix" above. ✓

## Findings

### LOW 1 — Dangling running card if the agent task dies mid-vision-call

`src/runtime/agent.rs:444-490` (the new emission block). The pairing invariant holds for every in-process path, including vision-call failure — but if the task itself dies between the two sends (a panic inside `describe_image_data_url`, or task abort), the transcript keeps an "image parsing" card spinning forever: no reducer sweep finalizes `running` vision entries on `error`/`exited`. This is the same exposure existing tool cards and memory cards already have (no finalize-all-running sweep exists anywhere in `agentEventReducer.ts`), and the plan documents the invariant as "no interrupt path exists between them", which is accurate for non-fatal paths. Suggested mitigation (consistent with the 2026-12 dangling-`CompactStarted` precedent): in `reduceVisionDescribed`'s no-match branch, or in the `exited`/final-`error` reducer, mark any still-running `vision` entries done with `success: false` and a "(interrupted)" description. Cosmetic, ultra-rare — acceptable to fix with a few lines or to document as a known limitation.

## Non-findings (considered, rejected)

- **capTranscript evicting a running vision entry before its `vision_described` arrives** — the 1000-entry cap cannot fill between a describe and its described because the fallback runs back-to-back before the turn's event stream starts. Unreachable.
- **PLAN.md `AgentEvent` sketch staleness** — pre-existing, illustrative-only sketch that already omits shipped variants; not introduced by this diff.
- **`.coding/backlog.jsonl` noise** — the diff includes a backlog status flip to `done` for the originating item plus unrelated backlog churn; intended bookkeeping, mergeable via the union driver.
