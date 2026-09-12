# Rust Core Review — uncommitted changes in `src/`

Scope: the modified + new files under `src/` listed in the task. Reviewed for
correctness, API/design fit, safety, style, and concurrency. `cargo build` is
clean and `cargo test --lib` passes **357 passed / 0 failed** (the shell's
reported exit code of `1` is the known PowerShell pipe artifact, not a failure).

## Verdict per file

| File | Verdict |
|------|---------|
| src/runtime/correction.rs | OK |
| src/tool/agent/describe_image.rs | OK |
| src/tool/agent/spawn_agent.rs | OK |
| src/runtime/agent.rs | minor issues |
| src/runtime/channels.rs | OK |
| src/runtime/mod.rs | OK |
| src/agent/mod.rs | minor issues |
| src/agent/factory.rs | OK |
| src/config/endpoints.rs | OK |
| src/config/mod.rs | OK |
| src/memory/mod.rs | minor issues |
| src/memory/schema.rs | OK |
| src/memory/types.rs | OK (style nit) |
| src/memory/consolidation.rs | OK |
| src/provider/mod.rs | OK |
| src/provider/openai.rs | OK |
| src/provider/vision.rs | OK |
| src/tool/agent/mod.rs | OK |
| src/tool/agent/shell.rs | OK |

No critical or safety-compromising findings. Path confinement, shell handling,
and tool exposure are sound.

## Findings (by severity)

### Major

1. **Cache-token heuristic can over-report `cached_tokens`.**
   `src/agent/mod.rs` (`run_turn` Usage handler, ~line 540) and the type doc in
   `src/memory/types.rs` (`RequestStats::cached_tokens`).
   When the API reports `cached_tokens == 0`, the fallback sets
   `effective_cached = min(prev_prompt_tokens, curr_prompt_tokens)`. The stated
   assumption is "the message prefix is unchanged (the common case of appending
   tool results / a new user message)". But after a **context summarization**
   the prefix *is* rewritten, and on the first request of a brand-new session
   with a large system prompt there is no cache to hit at all — yet the
   heuristic still reports a large cached count (bounded only by `prev`). This
   inflates the cost-savings estimate and the `cached_tokens` stat shown in the
   Stats tab. Suggested fix: only apply the heuristic when the turn did **not**
   summarize (the code already knows whether `summarize_with_interrupt` ran),
   or gate it on a cheap prefix-identity signal rather than assuming it.

### Minor

2. **`session_id` for stats is read from loop state, not the `session_id`
   parameter.** `src/agent/mod.rs:559` (`let sid = self.session_id();`) inside
   `run_turn`, which already receives `session_id: Option<&str>` (used for tool
   events). The two are kept in sync only because `AgentTask` calls
   `agent_loop.set_session_id(...)` immediately after `start_session`
   (`src/runtime/agent.rs:114,230`). That's an implicit invariant across two
   structs; if a future caller passes a `session_id` to `run_turn` without first
   calling `set_session_id`, stats rows get `session_id = NULL` while tool
   events get the real id. Suggested fix: use the `session_id` parameter
   directly in the stats block (it owns the authoritative value for the turn),
   or drop the parameter and always use loop state — don't read both.

3. **`SpawnAgentTool::new` is missing a doc comment.** `src/tool/agent/spawn_agent.rs:41`.
   The project requires doc comments on all public functions; this is the only
   new public fn without one (its sibling `DescribeImageTool::new` has one).
   Suggested fix: add a `/// Create the tool with the spawner that starts agents.` doc.

4. **New public stats structs in `src/memory/types.rs` have sparse field docs.**
   `RequestStats`, `SessionStats`, `ModelBreakdown`, `ProjectStats`,
   `DayBreakdown`, `SessionSummary` each have a struct-level doc, but several
   public fields (`id`, `session_id`, `model`, `created_at`, `request_count`,
   the `SessionSummary` fields) are undocumented. Existing types in this file
   document most fields. Not a rule violation (the types themselves are
   documented), but inconsistent. Suggested fix: add brief field docs for
   consistency.

### Nit

5. **Correction trigger `"summar"` is very broad.** `src/runtime/correction.rs:58`.
   It matches any prompt containing "summar" — including benign requests like
   "summarize this file" or "add a summary field". This produces false-positive
   `user_correction` working memories. It's harmless (the module docs explicitly
   accept FPs over FNs, and these working events are distilled away at
   consolidation), but it will flag ordinary summarization requests. Optional:
   tighten to a phrase like "summary is too" / "summaries should" or drop it.

6. **Fire-and-forget stats write can be lost on shutdown.**
   `src/agent/mod.rs` (~line 570) spawns `record_request_stats` in a detached
   task with `let _ = ...`. If the process exits (or the runtime is torn down)
   right after a turn, the final request's stats row may never be written. The
   comment acknowledges this ("must never block or break the turn"), so it's a
   deliberate trade-off; noting it only because `session_stats` could then
   under-count by one request. No action needed unless exact counts matter.

## Could NOT verify

- **IPC-layer `AgentSpawner` implementation** (`src-tauri/`): the trait seam in
  `src/runtime/mod.rs` is clean, but the concrete spawn logic (agent lifecycle,
  event fan-in wiring, duplicate-name handling, what happens when a spawned
  agent errors) lives in the IPC layer, outside this review's `src/` scope.
  Concurrency correctness of the spawned background agents is therefore
  unverified.
- **Live TTFT / generation timing values** in `src/provider/openai.rs`: the
  stream-loop enrichment is unit-tested for shape (`None` from `parse_sse_chunk`,
  enriched in the loop) but real millisecond values depend on a live SSE stream;
  the always-run suite mocks the provider, so actual timing accuracy against a
  real endpoint wasn't exercised.
- **`vision.rs` `ImageDescriber for VisionClient`** delegation recurses correctly
  to the inherent method (it does — it calls the inherent `VisionClient::describe_image`,
  not the trait method, so no infinite recursion), but the HTTP path itself is
  untested here (live-connection tests are `#[ignore]`d by design).

## Positives (worth noting)

- New modules are well-documented with module-level `//!` and per-item docs.
- `describe_image` reuses the shared `Sandbox` for path validation (canonicalization,
  symlink, and traversal-safe) and is correctly `AutoRun`/read-only; `spawn_agent`
  is correctly `NeedsApproval`. Good safety posture.
- The `AgentSpawner` trait seam avoids a core→Tauri dependency; `ImageDescriber`
  mirrors the existing `LlmClient` abstraction so tests mock without network.
- Provider swap uses per-turn snapshots (`provider` + `context_manager` read once
  into locals), so a mid-flight model change can't tear a turn — sound concurrency.
- Test coverage is strong: correction detection, vision fallback (success/failure/
  multimodal), tool registration gating, reasoning_effort, pricing parsing, and
  stats aggregation are all tested.
