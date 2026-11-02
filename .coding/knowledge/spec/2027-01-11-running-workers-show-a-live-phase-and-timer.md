+++
title = "running workers show a live phase and timer"
created = "2027-01-11"
+++

While a Mnemo2 worker runs, its mission-control row and the end of its thread show a status line with a pulsing dot: Starting…, Thinking…, Writing…, Running <tool>…, or Waiting on you (solid dot, no pulse). The row timer counts from the worker's creation (createdAtMs); the thread timer counts from the current turn's `started` event. The phase comes from mnemo2/src/lib/workerActivity.ts, a pure reducer over TeamEvents (applyWorkerEvent) and status snapshots (applyWorkerStatus). store.ts applies it for every worker, not only those whose thread is open. The shared component is mnemo2/src/components/ActivityStatus.svelte, also used by the lead chat's Thinking row; it honours prefers-reduced-motion. Rows hide the live line below 1100px, like the cost line. After a Mnemo2 restart, a running worker shows Starting… without a turn timer until its next event.
