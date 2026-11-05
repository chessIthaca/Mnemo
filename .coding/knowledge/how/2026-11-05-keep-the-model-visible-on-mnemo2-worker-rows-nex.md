+++
title = "keep the model visible on Mnemo2 worker rows next to non-shrinking chips"
created = "2026-11-05"
+++

In mnemo2/src/components/WorkerRow.svelte the name line (.worker-name) is one flex row. Any chip there with flex: 0 0 auto (like .route-chip) wins over the ModelBadge (min-width 0, max-width 130px), which then shrinks to nothing, so the row appears to show the Laya rating instead of the model. Rule: the name line holds only the name, the Audit/High-risk badges and the ModelBadge (flex 0 1 auto); the Laya routing and nudge chips live on their own .worker-route line under it, rendered only when a chip exists. Tests in WorkerRow.test.ts pin that .worker-name contains .model-badge and no .route-chip. Add new per-worker chips to .worker-route, not to the name line.
