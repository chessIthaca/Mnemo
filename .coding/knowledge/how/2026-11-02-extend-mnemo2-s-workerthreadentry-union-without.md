+++
title = "extend Mnemo2's WorkerThreadEntry union without breaking its consumers"
created = "2026-11-02"
+++

Mnemo2's worker thread entry union (WorkerThreadEntry in mnemo2/src/lib/types.ts) is consumed by narrowing sites beyond WorkerThread.svelte. When adding a new entry kind:

- mnemo2/src/preview/previewScenes.test.ts `isMessage` narrows with `Exclude<WorkerThreadEntry, ToolEntry>` and then reads `.from` — a new kind without `from` breaks svelte-check there; exclude it from the predicate (as done for the "error" kind).
- mnemo2/src/components/WorkerPanel.test.ts's failed-worker banner test queries the error text with a singular getByText and stays green only because WorkerThread skips the failure block when compact is set (the panel's blocked banner already carries the error with its way-out actions) — guard new thread-end blocks with !compact so each surface shows the text once.
- Live "error" entries (WorkerErrorEntry) are live-only: workerThreadFromRecord never emits them, so they vanish when the snapshot's saved length moves; they are not part of durable transcripts.
- The transient retrying-error note in applyWorkerLiveEvent (workerThread.ts) is dropped by any non-"error" live event (dropRetryNotes), and a final error (retrying=false) nulls streamingId so nothing appends to the half-written message afterwards.
