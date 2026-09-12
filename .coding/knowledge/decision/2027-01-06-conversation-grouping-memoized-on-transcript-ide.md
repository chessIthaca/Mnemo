+++
title = "Conversation grouping memoized on transcript identity — O(n) filter+group+chunk skipped on streaming-text-only frames"
created = "2027-01-06"
+++

DECISION (mem-perf review HIGH 2, plan eda9a42a, wt/agenticcoding): Conversation no longer recomputes the O(transcript) filter+group+chunk pass on every streaming frame. The visible-entry filter, the prompt-starts-a-turn grouping, and the per-turn chunkRuns are folded into ONE `const turns = useMemo(() => {...}, [state.transcript, showToolActivity, showKnowledgeActivity])` returning TranscriptEntry[][][] (turn → chunks → entries); the render maps over the pre-chunked turns (`turn.map((run, ri))` — chunkRuns is no longer called in the render body).

Why it works: Conversation re-renders every rAF streaming flush (MainPanel passes the whole agent object down), but appendStreamingText (useAgentStore.ts:1020-1036) carries agent.transcript BY REFERENCE while text streams into state.streamingText — so the memo hits across streaming-text-only frames and the common case skips the entire O(n) pass. Verified by the reviewer: every transcript write path clones (pushTranscriptMessage, agentEventReducer, capTranscript, applyToolCallArgDeltas) — no in-place mutation, so the memo never goes stale. Tool-arg streaming still recomputes (per-batch transcript clone) — accepted; the common case is text streaming.

Deps are exactly [state.transcript, showToolActivity, showKnowledgeActivity]: the toggles re-filter when flipped; chatThreadLine/chatTurnTint/chatHoverTimestamps correctly stay OUT (render-only concerns). Pinned by Conversation.test.ts source-contract assertions (`const turns = useMemo(`, the exact deps string, `turn.map((run, ri)` present, `chunkRuns(turn).map(` absent). Review: PASS, 0 findings (.coding/reviews/2027-01-06-conversation-grouping-memoization-review.md). Residual O(n) element creation per frame stays with the queued stable-keys (HIGH 3) and transcript-windowing (b2cb83b6) items.
