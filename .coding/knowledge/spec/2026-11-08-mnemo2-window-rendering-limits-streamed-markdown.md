+++
title = "Mnemo2 window rendering limits — streamed markdown throttle, 200-entry conversation window, lazy Graph/Trace views"
created = "2026-11-08"
+++

Mnemo2's window bounds its rendering cost in three places:
- mnemo2/src/components/Markdown.svelte throttles re-renders of APPENDED text (new source startsWith the shown one) to one per 150 ms (`STREAM_RENDER_MS`), with a trailing render so the newest text shows within 150 ms of the last chunk; any non-append change renders at once; a pending timer is cleared on destroy. It uses Date.now(), so tests drive it with vi.useFakeTimers() + advanceTimersByTimeAsync(150). No per-call-site streaming flag exists.
- LeadChat.svelte and WorkerThread.svelte render only the newest `ENTRY_WINDOW` (200) entries via mnemo2/src/lib/entryWindow.ts (`windowEntries`, `earlierEntriesLabel`); a "Show earlier messages (N hidden)" button adds 200 per click; the worker thread resets on worker switch. Neighbour classes use the next VISIBLE entry; turnStartTimes keeps the full list. A tail window, not a virtual scroller, so stick-to-bottom is untouched.
- App.svelte loads GraphView and TraceView with `{#await import(...)}` so they are separate chunks (2026-11-08 build: main JS ~839 kB, Graph ~43 kB, Trace ~66 kB). DECISION: no vite manualChunks — the bundle is local files inside the app, so splitting statically imported vendors defers no parse work; only lazy-loading a view does.
