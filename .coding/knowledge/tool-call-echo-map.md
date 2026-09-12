# Tool-call echo map — what echoes where (audit for backlog c8531c34, 2026-12-29)

**Question:** "Are there any tool calls that don't echo into the console? I see
lots of read_files but not many graph calls and search calls."

**Answer: No.** Every tool call the model makes echoes into *both* display
surfaces — the GUI chat transcript (a ToolCard) and the `-console` REPL (a
`-> name` line) — with no name-based suppression anywhere in the chain. The
scarcity of `graph_*`/`search` cards relative to `read_files` is not
suppression; see "Why graph/search cards are rare" below.

## The echo chain (verified, code-referenced)

1. **Emission — every call announced.** `src/agent/turn.rs:1104-1121` forwards
   every `LlmEvent::ToolCallStart { index, id, name }` to the fan-in as
   `AgentEvent::ToolCallStart` — unconditionally, mid-stream, with no
   name/category filter (backlog 63cbc20f).
2. **Emission — every call closed.** `AgentEvent::ToolResult` is emitted from
   six sites in `turn.rs` covering every outcome: the real result (success or
   error) at `turn.rs:1816`, plus synthetic closers for announced-but-not-run
   calls (turn stopped / hard stop / cancel / compact / malformed-args retry
   and terminal) at `turn.rs:1329, 1432, 1537, 1664, 1975`. An announced card
   can never spin "running" forever.
3. **Serialization — nothing dropped.** `src/runtime/channels.rs:570-625`
   (`AgentEvent::into_serializable`) maps every event variant through to the
   IPC boundary; the adapter (`src-tauri/src/ipc/events.rs:375-508`) is
   generic plumbing (watchdog/workflow side-channels + emit) with no
   tool-name filtering.
4. **GUI chat — every call carded.**
   `frontend/src/hooks/agentEventReducer.ts:486` (`reduceToolCallStart`)
   creates a transcript entry for every tool call. Consecutive same-name calls
   merge into one card (lines 520-527, `MAX_CALLS_PER_TOOL_CARD` cap; a failed
   call breaks the chain); `shell`, `search`, `search_read`, and the browser
   tools never merge (`neverGroups`, line 545 — backlogs daa38cbe + 24ddb845).
   `capTranscript` rolls the oldest entries off in very long sessions — a
   display cap, not suppression.
5. **Console REPL — every call printed.** `src-tauri/src/console.rs:241`
   (`render_event`): `ToolCallStart` renders `-> {name}` for every tool (line
   252); `ToolResult` renders via `render_tool_result` (line 254). The Silent
   set is exactly `{Started, ToolCallArgDelta, ContextUsage, PromptDispatched,
   Phase}` — no tool call is Silent. `handle_event` (lines 1175-1181) routes
   every fanned-in event through `render_event`.
6. **GUI result text — the ToolCard is the only surface.** (Corrected
   2027-01-05: this entry previously claimed an "Output tab" logged every
   result — that tab was removed in commit 3ff4845, "remove Output
   right-panel tab + dead toolOutputLog plumbing".) `reduceToolResult`
   (`agentEventReducer.ts:619+`) updates the transcript entry's result in
   place; the expanded ToolCard (`Message.tsx` `CallDetail`) renders the
   result text. The `-console` REPL and the Trace tab (raw wire JSON) are
   debug surfaces and never filter.

## The exceptions (by design — not suppression)

1. **Agent-activity cards behind `show_tool_activity=false` (GUI chat only;
   render-time filter).** One `[ui] show_tool_activity` toggle — default
   **`false`** (`src/config/general.rs:322-330`; backlog db489070, 2026-12-30)
   — gates ALL agent-activity cards at RENDER time: tool calls, memory
   reads/writes (`MEMORY_TOOLS` = {`memory_write`, `memory_search`,
   `memory_consolidate`, `memory_update`, `memory_supersede`,
   `memory_delete`}, `agentEventReducer.ts:329`), vision image-parsing, and
   skill announcements. `Conversation.tsx:107` skips these entries via
   `isActivityEntry` (`agentState.ts`) when the toggle is off; the
   transcript STORE always contains every entry (the model's context echo
   is built server-side and is unaffected), and the console still logs
   every tool result. The `-console` REPL ignores this config
   (`render_event` is a pure function) — every call always prints
   `-> {name}` there. (Supersedes the old memory-only
   `show_memory_activity` reducer suppression, default `true`, removed
   2026-12-30.)
2. **Auto-delegated graph/memory lookups inside `search`/`search_read`**
   (backlog b804012f). When the pattern is symbol-shaped or a memory hunt, the
   search tool resolves it internally — `symbol_delegation_block`
   (`src/tool/agent/codegraph.rs:106`) / `memory_delegation_block`
   (`src/tool/agent/search.rs:1057`) — and returns the answer inline
   ("AUTO-DELEGATED to the code graph — ..."). The internal lookup emits no
   `ToolCallStart`: it is a sub-operation of the search call, so no
   `graph_search` card appears even though graph work happened.
3. **Sub-agent tool calls.** A `spawn_agent` child's tool calls echo into the
   child's own transcript (its own agent stream); the parent's chat/console
   sees only the `spawn_agent` call and `ChildFinished`
   (`src-tauri/src/ipc/events.rs:445-475`).
4. **Internal harness operations are not tool calls** and echo via their own
   dedicated events: auto-recall -> `MemoryRecalled` (console.rs:287),
   compaction -> `CompactStarted`/`Compacted`, vision ->
   `VisionDescribe`/`VisionDescribed`.
5. **AUTO-DELEGATED steering note behind `show_delegation_notes=false` (GUI
   chat only; render-time filter).** One `[ui] show_delegation_notes` toggle
   — default **`false`** (backlog 0458f797, 2027-01-05) — hides ONLY the
   AUTO-DELEGATED steering header that `search`/`search_read` emit when a
   symbol-shaped query auto-delegates to the code graph or memory (see
   exception 2), in BOTH emission shapes: the fast path (memory hunt, or
   symbol hunt with no glob) returns the block raw — the header line
   starts with `AUTO-DELEGATED`; the prepend path (symbol hunt narrowed
   by a glob, riding above normal results) wraps it as
   `note: AUTO-DELEGATED …`. Display-layer filter, exactly the
   `show_tool_activity` philosophy: the tool result text — the model's
   context, including the "re-issue this exact search" escape-hatch hint —
   is untouched (the escape hatch works server-side regardless of display);
   only the ToolCard's rendering hides the line. Both ToolCard surfaces
   filter: the collapsed card's amber note chip (`Message.tsx`, via
   `isDelegationNote` — prepend shape only; the fast-path block carries
   no engine marker so no chip fires there) and the expanded output
   `<pre>` (via `stripDelegationNotes`,
   `frontend/src/lib/delegationNotes.ts` — both shapes). The delegated
   answer (def:/callers:/full 360° lines) always stays visible; other
   notes (e.g. the content-index staleness note) are unaffected. The
   `-console` REPL and the Trace tab never filter.

## Why graph/search cards are rare next to read_files

1. **read_files is the workhorse.** Agents read files constantly, and
   consecutive `read_files` calls merge into one card with many file-path
   chips — a few cards represent many calls.
2. **Search auto-delegation absorbs symbol lookups.** A symbol-shaped
   `search` runs the graph lookup inside itself (invisible), so no
   `graph_search` card appears. Worked example: the 2026-12-29 audit session
   behind this file issued many `search` calls that auto-delegated to the
   graph/memory and zero direct `graph_*` calls.
3. **Direct `graph_*` chains** (`graph_search` -> `graph_context` -> ...) happen
   mainly during explicit symbol archaeology, which is rarer than file
   reading.

## If you want the delegated lookups visible (not implemented — user decision)

- Emit a synthetic transcript marker when auto-delegation fires, or
- surface the "AUTO-DELEGATED ..." prefix on the search card's chip.

The result text already carries the prefix; only the card header would need
it.
