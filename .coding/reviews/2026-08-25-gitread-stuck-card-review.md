## Verdict: FINDINGS (0 high, 3 low)

Review: git_read stuck-running card fix + info chips (plan 233773ab, backlog 12d7ecb8) — branch `wt/toolcard-result-routing`, all uncommitted changes reviewed via `git diff HEAD` + targeted reads.

## 1. Core fix correctness — VERIFIED (no finding)

**Id-correlation in `reduceToolResult`** (frontend/src/hooks/agentEventReducer.ts:549–554): the memory-finalization loop now requires `entry.id === event.tool_call_id` in addition to `running` and `MEMORY_TOOLS`. Checked every path:

- **Foreign tool result** (the bug): `git_read`'s result no longer matches the running `memory_search` entry (ids differ) → `memoryFinalized` stays false → the tool-card loop attaches the result to git_read's call. The reported defect is dead.
- **Own result**: memory tool start stamps `id: event.id` (:430) so the entry's id equals the `tool_call_id` of its own result → finalizes with tier/title/snippet/matched and `running:false`. A single id can never exist as both a memory entry and a tool call — `reduceToolCallStart` creates exactly one or the other.
- **showMemoryActivity OFF**: start returns early, no memory entry exists (:420–422); result no-matches both loops and drops benignly — unchanged pre-fix behavior.
- **Auto-recall entries**: created already finalized (`running:false`, name `"auto-recall"` not in `MEMORY_TOOLS`, no `id`) — triple-guarded, can never match even if `tool_call_id` were somehow undefined.
- **Merge/chain logic untouched**: `canMerge` / `MAX_CALLS_PER_TOOL_CARD` / `lastCallFailed` (:442–481) apply only to tool entries; memory entries never merge (one entry per call). No regression.
- **Arg-delta routing** (:497–518): backward scan, tool branch first, memory branch gated on `entry.running && entry.index === event.index` and breaks on update — late deltas after finalization (running=false) are ignored. A stale running memory entry from an interrupted turn cannot swallow a newer tool call's deltas because the scan-from-end finds the newer entry first.
- **capTranscript eviction** of a memory entry before its result → benign no-match drop, no stuck state.

**Regression test quality** — `useAgentStore.test.ts` "regression: a foreign tool's result must not be stolen by a running memory entry (git_read stuck \"running\")" reproduces the exact scenario: three parallel starts, git_read's result FIRST, asserts the git card receives it while memory stays running, then memory finalizes on its own id with `matched: 1`, then search resolves. Pre-fix this fails at `expect(git.calls[0].result).toEqual(...)` (the memory loop consumed the result). RED→GREEN was verified by the main agent; the test directly exercises the changed code path. The arg-delta-into-memory-entry pin covers the new accumulation branch.

## 2. Matched-header regex — VERIFIED against the real emitters (finding L1 on the comment)

The Rust emitters produce exactly two header shapes (checked `src/tool/memory/mod.rs:422,424` and `src/tool/memory/retrieval.rs:205,207`):
- `"no memories matched the query"` → `no \w+ matched` matches (no end anchor, colon optional) → `matched = 0` ✓
- `"{n} memories matched:\n\n"` (always the noun "memories") → `(\d+) \w+ matched:` ✓

The zero-match trailing " the query" is tolerated because the regex has no `$` anchor; the `/m` flag anchors to line starts, and recalled content lines are indented so they can't false-match. Header-less/error outputs → `undefined` → chip hidden (tested). All good — **but** the inline comment at agentEventReducer.ts:391–394 claims typed-noun headers ("N plans/reviews/**past fixes** matched:") that (a) the emitters never produce and (b) a two-word noun like "past fixes" would **not** match `(\d+) \w+` anyway. See finding L1.

## 3. UX additions — VERIFIED (no finding)

- **`memorySearchLabel`** (toolCardPaths.ts): query quoted + `record_type`|`tier`/`prefix`/`limit N` scope suffixes, `browse · …` for query-less filtered browses, null on malformed JSON / no content. Mid-stream behavior confirmed acceptable: args stream incrementally, `JSON.parse` fails until complete → chip appears once args parse (before result), exactly like `argLabel` on ToolCards — consistent, no wrong-data flicker.
- **git_read `argLabel` branch** (Message.tsx:379–403): `log -N -- path` / `show <≤7-char commit>` / `diff path` / bare op; null on missing op or malformed JSON; called with the tool name at Message.tsx:656. Tests cover all shapes.
- **`argPaths` git_read exclusion**: pre-change, git_read's `path` fell through to the common path/file fallback and rendered a clickable link that treated a log/diff *filter* (often a directory) as an openable file — the exclusion is a correctness improvement, and the op chip carries the information now. Tested.
- **MemoryEntryCard**: query chip + right-aligned "N matched"/"no matches" merged with …/✓/✗, `shrink-0` so `ml-auto` alignment survives. `matched !== undefined && !entry.running` gate is correct (undefined for write/consolidate and while running).

## 4. Project review expectations

- **Docs sync**: module doc comments are updated thoroughly and accurately (types.ts field docs, `memorySearchLabel`, `parseMemoryEntry`, MemoryEntryCard, argLabel branch). README.md/PLAN.md/endpoints.toml do not document tool-card chip granularity; no updates needed. No finding.
- **Multi-platform neutrality**: TS-only change; no OS-specific APIs, paths, or shell syntax. No finding.
- **Security**: all parsing is display-only (try/catch JSON.parse, regex on local strings), rendered as React text nodes — no injection surface. No finding.
- **Bug-plan requirements**: root cause documented in `.coding/knowledge/bug/2026-08-25-tool-result-stolen-by-memory-entry-git-read-card.md` (symptom → root cause → fix → regression test name) + semantic memory record present; backlog 12d7ecb8 marked done with an accurate note. See finding L2 for the plan-file gap.
- **Tests**: frontend vitest 45 files / 577 tests green, `cargo test` 169+4 exit 0 — consistent with the diff (no Rust files touched).

## Findings

### L1 (low, docs-in-code) — Comment claims header shapes the emitters never produce
`frontend/src/hooks/agentEventReducer.ts:391–394`: the new comment says the header may read "N plans/reviews/**past fixes** matched:" (typed nouns). The actual emitters (`mod.rs:424`, `retrieval.rs:207`) always print `"{n} memories matched:"`, and a two-word noun like "past fixes" would not match `(\d+) \w+` anyway — so the comment both overstates coverage and implies the regex handles a shape it silently doesn't. The regex is correct for all real output; fix the comment to the two real shapes (`"N memories matched:"` / `"no memories matched the query"`) so a future maintainer doesn't rely on the typed-noun claim.

### L2 (low, bookkeeping) — Plan file's "Regression test" section records the file, not the test name
`.coding/plans/233773ab-d0cb-4fc6-8aa4-30b12c89d3e4.md` "## Regression test" contains only `frontend/src/hooks/useAgentStore.test.ts`. The bug-plan requirement (and the plan's own step 4) calls for the regression test to be recorded by name; the full name currently lives only in the BUG knowledge record. Append the test name — `regression: a foreign tool's result must not be stolen by a running memory entry (git_read stuck "running")` — to the plan's Regression test section before finish.

### L3 (low, commit hygiene) — Unrelated untracked knowledge file must not silently ride along
`.coding/knowledge/bug/73897135-cff3-4ab4-80d8-8aa20f559a4d.md` is the previous session's resize-seam auto-capture; it references branch `wt/resize-seam-flip-closure` @ d31b606 and is unrelated to this plan. A blanket `git add .`/`git add -A` would carry it into this commit and then into develop/main via merge, desynchronizing it from its own (unmerged) branch. Exclude it from this commit (or, if that branch's work is complete and its file was simply never committed there, commit it deliberately as a separate bookkeeping commit). Incidental: the argLabel test's sample hash "d31b606abc123def456" shares that branch-tip prefix — harmless sample data, no action.

## Summary

The root-cause fix is correct and minimal: `tool_call_id` correlation on memory-entry finalization, with id/index/args stamped at start and index-routed arg accumulation. All downstream paths (foreign results, own results, flag off, auto-recall, eviction, merge logic) behave correctly; the matched-count regex handles the real emitter shapes; the UX chips are tested and render safely mid-stream. The three findings are a comment correction, a plan-file bookkeeping append, and commit-scope hygiene — none block after fixing.
