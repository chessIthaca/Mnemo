+++
title = "076f601e streamed answer stayed plain until the next request (fixed 0d16ab7)"
created = "2027-01-11"
+++

Symptom: the answer streamed plain and STAYED plain after the turn (chat + thinking box) until the next request. Root cause (two): (1) the answer mirror in activityLog rendered plain forever — the log is not cleared at turn end (2026-08-22 user request) and ActivityLine rendered every entry plain; (2) a late text_delta after finished re-populated streamingText (running=false), and the next started cleared it unflushed — silent tail drop. Fix (wt/stream-plain 0d16ab7, plan 3d40e529): answer entries render via chat Markdown once !running; late deltas settle into the transcript via settleTextIntoTranscript (agentState.ts) in both delta writers. Regression: useAgentStore.test.ts "regression (backlog 076f601e)" (red: streamingText stayed " world"); useAgentEvents.dispatch.test.ts "late delta flush after finished" + InflightBar.test.ts "settled answer mirror" cover the same scenario. Red→green evidence in the commit message.
