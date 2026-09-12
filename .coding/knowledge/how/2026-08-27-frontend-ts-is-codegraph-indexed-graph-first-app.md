+++
title = "frontend TS IS codegraph-indexed — graph-first applies to React/lib work too"
created = "2026-08-27"
+++

Empirically verified 2026-12 (graph_search during plan 195b2eb1, trace-legend toggles): the code knowledge graph indexes frontend TypeScript — `TraceStats` (frontend/src/components/views/TraceStats.tsx::TraceStats) and `phaseTipRows` (frontend/src/lib/traceStats.ts) both resolve with ids. Earlier assumption that the codegraph might be Rust-only (memory of plan 76d06893 was truncated at "rs→Ru…") was wrong.

Consequences for workflow:
- For frontend symbol questions (where is X defined, who renders/imports it), graph_search → graph_context replaces grep + whole-file reads — same as Rust.
- graph_impact before editing shared frontend exports (lib/*.ts functions imported by components + tests) is required and works there too.
- Caveat: graph line numbers lag uncommitted working-tree edits (locator, not truth — always read the file before editing).
- `search` stays correct for: string literals, UI copy/comments, config keys, doc-debt greps in README/PLAN.md; memory_search stays the mandatory pre-planning semantic step (it surfaced the palette-duplication review finding reused in plan 195b2eb1).
