+++
title = "Mnemo2 tool cards show the turn's elapsed time, call time in the tooltip"
created = "2026-11-05"
+++

Tool cards (mnemo2/src/components/ToolActivity.svelte) show the time since the turn's first tool call, not the call's own duration, and no "Completed" word; a failed call reads "Failed · 2:43". Turn starts come from turnStartTimes(entries) in mnemo2/src/lib/elapsed.ts, which resets at a lead-chat user message or a worker-thread lead message. The tooltip reads "Turn time X · this call Y". The per-call startedAtMs/endedAtMs stamps stay per-call (an earlier bug made batch timers look cumulative by accident); the cumulative view is derived in the UI only. The user asked for it because cards read "Completed 0:00".
