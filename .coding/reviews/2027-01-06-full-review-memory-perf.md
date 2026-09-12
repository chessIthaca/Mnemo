## Verdict: FINDINGS (3 high, 3 low)

Full-codebase memory/perf review of Mnemo at HEAD 9a0d5b9 (wt/agenticcoding), 2027-01-06. Axis: wasted memory + performance — the display-update pipeline (agent events → IPC → store → React) and the model-interaction path (Rust provider/agent/memory), plus general memory waste.

One-line summary: the streaming **data** path is well-engineered (rAF batching, 64 KiB buffer caps, pooled reqwest clients, bounded channels, capped trace/repetition buffers), but the React **render** path re-renders the entire app shell on every streaming frame, Conversation recomputes O(transcript) grouping per frame without memoization, and index-based keys force full-list reconciliation plus row-state misattribution once the transcript hits its 1000-entry cap; pasted images are additionally retained as full-size base64 with no byte budget.


---

## HIGH 1 — App subscribes to the whole agent object: entire app shell re-renders on every streaming frame

**Evidence:**
- `frontend/src/App.tsx:96-98` — `const activeState = useAgentStore((s) => (s.activeAgent != null ? s.agents[s.activeAgent] : undefined));` — selects the whole per-agent state object.
- `frontend/src/App.tsx:721` — its ONLY consumer: `{activeState && <InflightBar state={activeState} agentId={activeAgent} />}`.
- `frontend/src/App.tsx:678-740` — App's render tree mounts `<Sidebar />`, `<MainPanel />`, `<InflightBar />`, `<InputBar />`, `<StatusBar />`, `<RightPanel />`; none are memoized (the only `memo(` in the component tree is `Message.tsx:472`).

**Why it matters:** During streaming, every rAF flush (up to 60 fps) creates a new agent-object identity in the store (`appendStreamingText` spreads the agent per flush). App's selector returns that new object → App re-renders → React re-renders ALL children: Sidebar, InputBar (textarea + toolbar + image thumbnails), StatusBar, RightPanel (whatever view is active), MainPanel. The entire app tree reconciles per streaming frame — the dominant streaming-jank source on long transcripts and low-end hardware, and it competes with user typing in InputBar during streaming.

**Fix:** Move the subscription down: `InflightBar` already receives `agentId` — have it subscribe itself (`useAgentStore((s) => s.agents[agentId])`) and delete App's `activeState`. App then stops re-rendering during streaming entirely; the shell components re-render only when their own slices change. (Complementary hardening: wrap `Sidebar`/`InputBar`/`StatusBar` in `React.memo` so any future App-level re-render can't drag them along.)

## HIGH 2 — Conversation recomputes O(transcript) filter/group/chunk work in the render body on every streaming frame

**Evidence:**
- `frontend/src/components/chat/Conversation.tsx:140-145` — `const visible = state.transcript.filter(...)` runs in the render body, no `useMemo`.
- `frontend/src/components/chat/Conversation.tsx:152-159` — the `turns` grouping loop runs in the render body.
- `frontend/src/components/chat/Conversation.tsx:207` — `chunkRuns(turn)` re-runs per turn per render.
- `frontend/src/components/chat/Conversation.tsx:163-220` — `renderEntry` creates new React elements for every visible entry on every render.

**Why it matters:** Conversation re-renders on every streaming frame (its `state` prop changes identity per rAF flush). Each render re-filters and re-groups up to 1000 transcript entries and re-creates their React elements — O(n) work at up to 60 fps ≈ 60k element allocations/sec in a long session, even though the memoized `Message` rows skip deep rendering. This is exactly the "UI jank during long transcripts" scenario: the per-frame cost scales with transcript length, not with the delta size.

**Fix:** Wrap the computation in `useMemo` keyed on `[state.transcript, showToolActivity, showKnowledgeActivity]` — `state.transcript` keeps its array identity across streaming-text-only frames (the store spreads the agent object but preserves the transcript array reference), so the common case (text streaming into the last turn) skips the entire O(n) pass. Compute `chunkRuns` per turn inside the same memo (or precompute runs alongside `turns`).

## HIGH 3 — Index-based React keys: full-list reconciliation + row-state misattribution once the transcript hits its 1000-entry cap

**Evidence:**
- `frontend/src/components/chat/Conversation.tsx:198-200` — `turns.map((turn, ti) => <div key={ti}` (turn-index key).
- `frontend/src/components/chat/Conversation.tsx:207-210` — `chunkRuns(turn).map((run, ri) => <div key={ri}` (run-index key).
- `frontend/src/components/chat/Conversation.tsx:217` — `run.map((e, i) => renderEntry(e, i))` (entry-index key within run).
- `frontend/src/components/chat/Conversation.tsx:220` — `renderEntry(run[0], ri)`.
- `frontend/src/hooks/agentState.ts` — `capTranscript` keeps the LAST 1000 entries: once at cap, every append drops the oldest entry and shifts every index.

**Why it matters:** In a long session at cap, each append shifts the first turn's entry indices → the `Message` components at those keys receive different `entry` props → `arePropsEqual` fails → those rows deeply re-render; and — worse — local row state (an expanded tool card, a copied flag) silently transfers to a DIFFERENT entry, because React reuses the component instance at the key while the underlying data shifted. Turn-count changes shift all turn keys, forcing O(n) reconciliation of the whole list per append. The memoized `Message` plus stable entry object references limit the deep-render damage, but the reconciliation cost and the state-jumping are user-visible precisely in the long-transcript scenario this review targets.

**Fix:** Give each `TranscriptEntry` a stable `id` at creation (monotonic counter in the reducer), and key by content at all three levels: entry key = `entry.id`, run key = first entry's id, turn key = first entry's id. Entries already carry `ts` (used for the hover tooltip at Conversation.tsx:167-169), so per-entry identity plumbing already exists.


---

## LOW 4 — Pasted images retained as full-resolution base64 data URLs: no downscale, no byte budget

**Evidence:**
- `frontend/src/components/layout/InputBar.tsx:120` — `reader.readAsDataURL(file)`; no canvas downscale/compression anywhere in the paste path.
- `frontend/src/components/layout/InputBar.tsx:328` — the data URLs are embedded in the transcript entry (`...(images.length > 0 ? { images } : {})`).
- `frontend/src/hooks/agentState.ts` — the transcript cap is entry-COUNT only (1000); there is no per-entry or total byte budget.

**Why it matters:** A 4K screenshot is ~5-10 MB as base64. A multi-hour vision-heavy session retains every pasted image at full size in the zustand store AND in the DOM (the entry-count cap never evicts them by size). This is the largest open-ended RAM contributor on the display side — the "RAM growth over a multi-hour session" scenario.

**Fix:** Downscale on paste via canvas (cap the long edge, e.g. ~1568 px — matching common vision-API input limits — with JPEG re-encode), and/or evict image payloads from entries beyond a rolling byte budget (render a placeholder chip for evicted images).

## LOW 5 — `appendStreamingText` rebuilds the full accumulated string on every rAF flush

**Evidence:**
- `frontend/src/hooks/useAgentStore.ts` — `appendStreamingText` / `appendStreamingReasoning` compute `agent.streamingText + text` (a fresh full-length string per flush).

**Why it matters:** For a 100 KB response streamed over ~60 flushes, that is ~3-5 MB of transient string copying per response. The reducer comments document this as an accepted micro-opt; it is bounded by `SANE_MAX_OUTPUT_TOKENS` (32 K tokens) and the 64 KiB rAF-buffer cap keeps flush sizes sane. Allocation churn, not a leak.

**Fix:** Only if profiling ever shows it: accumulate chunks in an array and join on finalize (the streaming display can render the chunk list), amortizing to O(n).

## LOW 6 — Per-chunk `RawAssistantDelta { delta: d.clone() }` in the SSE parser

**Evidence:**
- `src/provider/openai/sse.rs:323` — `events.push(LlmEvent::RawAssistantDelta { delta: d.clone() });` — every SSE chunk's delta JSON map is cloned for the raw-echo/fallback-replay path.

**Why it matters:** Thousands of small clones per response — allocation churn, not growth (the accumulator merges into a single map, `src/provider/stream.rs:242-279`, freed at request end). Minor.

**Fix:** Move the parsed delta value into the event where the non-raw path does not also need it, or clone only the keys the raw echo requires.


---

## Verified clean (no action needed)

**Display-update pipeline:** rAF batching with the 64 KiB occlusion cap + 40 ms fallback and a single module-level Tauri listener (useAgentEvents.ts — no StrictMode double-listener, buffers registered for `/clear` cleanup); transcript capped at 1000 entries / activity log at 300 (agentState.ts); `Message` memoized with identity-based `arePropsEqual` (Message.tsx:472) and streaming text rendered as plain text — no markdown re-parse per delta; Markdown lazy-loaded + memoized on text (Markdown.tsx); MainPanel uses a narrow `useShallow` tab selector; RightPanel mounts only the active tab and unmounts when hidden — GraphView/LlmTraceView/MemoryDebugView/PlanProgress polls run only while their tab is visible; InflightBar's 1 s timer runs only while a turn is active (InflightBar.tsx:98-102); scroll throttled with trailing-timer cleanup (Conversation.tsx:118-132); BacklogView uses narrow selectors, no polling; all `setTimeout` uses are one-shot UI feedback with ref-based cleanup where it matters.

**Model-interaction path (Rust):** single pooled `reqwest::Client` per provider client, built once (openai.rs:163-281, anthropic.rs:174) — no per-request client rebuilds; SSE buffer drained in place via `String::drain` (sse_util.rs) — no O(n²) re-copy; bounded event channel (128, openai.rs:686); trace log response/request caps + ring eviction + `spawn_blocking` for record starts (trace.rs:17-22); repetition-guard buffer soft-capped (stream.rs:406-426) so `detect_repetition` examines a bounded tail only; live usage overlay throttled to 500 ms; incremental `TokenAccounting` replaced the per-iteration full-history token recount (turn.rs:267-271); memory consolidation is session-end triggered, not a background loop; tool outputs capped at source (file_read.rs:22-28: 500 lines / 100 KB; git.rs `cap_output`); context compaction (`compact_old_tool_results` + summarize) bounds history growth.

---

## Summary

| Severity | Title | Location |
|---|---|---|
| HIGH | App-level whole-agent subscription re-renders entire app shell per streaming frame | App.tsx:96-98, 721 |
| HIGH | Conversation recomputes O(transcript) filter/group/chunk per frame (no useMemo) | Conversation.tsx:140-159, 207 |
| HIGH | Index-based keys → full-list reconciliation + row-state misattribution at the 1000-entry cap | Conversation.tsx:198-220; agentState.ts |
| LOW | Pasted images retained full-size base64 (no downscale, no byte budget) | InputBar.tsx:120, 328 |
| LOW | `appendStreamingText` O(n) string concat per rAF flush | useAgentStore.ts |
| LOW | Per-chunk `RawAssistantDelta` delta clone | openai/sse.rs:323 |
