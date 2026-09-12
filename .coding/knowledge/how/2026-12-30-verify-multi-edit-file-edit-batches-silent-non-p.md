+++
title = "verify multi-edit file_edit batches — silent non-persistence + stale-index false negatives"
created = "2026-12-30"
+++

2026-12-30, plan 0e71e4f9 step 2: a single batch of 14 file_edit calls (3 files × 3-6 edits each) ALL reported "edited" but NONE persisted for those three files (agentEventReducer.ts, agentState.ts, useAgentStore.ts body edits) — discovered only via a follow-up tree-walk search + direct reads; a later same-size batch spread across 7 files (1-3 edits per file) persisted fine. Mitigations: (1) keep same-file edit counts low per batch (≤3) or one file per batch for critical multi-region renames; (2) ALWAYS verify after a large multi-edit batch — re-read the edited regions or run a literal search for a NEW identifier; (3) the search tool's INDEX path returns false negatives right after edits (engine:index serves pre-edit content; only the tree-walk fallback is fresh — a "no matches" immediately after an edit means nothing). Related known issues: search reliability (backlog 92a41825), emission fragility (backlog e8b39d72 H5).
