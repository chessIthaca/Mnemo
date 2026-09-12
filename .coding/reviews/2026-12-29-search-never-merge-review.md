## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes on `wt/agenticcoding` for plan dc534943 — "Exempt search/search_read tool calls from transcript card merging" (backlog 24ddb845). Scope (`git diff HEAD` + status): 4 modified files — `.coding/backlog.jsonl`, `.coding/knowledge/spec/2026-12-23-shell-browser-tool-calls-always-render-on-their.md`, `frontend/src/hooks/agentEventReducer.ts`, `frontend/src/hooks/useAgentStore.test.ts` — plus 2 untracked `.coding/` files (finding L1). Static review (read-only reviewer); the stated verification (vitest 744/744 across 51 files, `tsc && vite build` green) is consistent with everything below.

### Verified correct

**(a) Correctness — neverGroups extension; merge machinery intact**
- `agentEventReducer.ts:545-549`: `neverGroups` now matches `shell`, `search`, `search_read`, and `isBrowserToolName(event.name)` (the `browser_*`/`offscreen_browser_*` prefix predicate, `toolCardPaths.ts:463-468`). `canMerge` (:550-556) requires `!neverGroups && last.kind === "tool" && last.name === event.name && !lastCallFailed && last.calls.length < MAX_CALLS_PER_TOOL_CARD` — different-name calls never merge by construction, and search/search_read are excluded in every position/state (parallel batch, after a result, after a failure).
- Other tools keep the compact behavior: the merge decision lives solely in `reduceToolCallStart`/`canMerge` — no duplicated policy elsewhere. The four tests switched to `file_edit` still exercise the real merge path: `file_edit` matches no exemption (not shell/search/search_read; `isBrowserToolName("file_edit")` is false by the prefix predicate, pinned at `toolCardPaths.test.ts:513-516`). The MAX_CALLS_PER_TOOL_CARD split regression (`useAgentStore.test.ts:489-524`) is arguably stronger on `file_edit`: 60 starts → exactly 2 cards (50+10) with second-card result resolution by call id.
- Doc comments updated in step: the reducer doc comment (:478-484) and the inline comment (:539-542) both name shell, browser, and search/search_read with the backlog citation.

**(b) No other code path or test relied on search merging**
- Exhaustive sweep of `name: "search"` in frontend tests: 5 occurrences, all accounted for — the three new never-merge tests (:429/:433, :469/:481) and the foreign-result correlation regression (:538), which dispatches `git_read` + `memory_search` + `search` (three DIFFERENT names — it never depended on merging; `canMerge` requires same-name). Leaving it on `search` is correct: it tests tool_result correlation, not merging.
- `useAgentStore.preview.test.ts` uses `file_edit` only. `toolCardPaths.test.ts` asserts `isBrowserToolName("search") === false` (:514) — the browser predicate was not silently widened; its `searchResultInfo` tests are result-parsing contracts, unrelated to merging.
- `Message.tsx` search rendering (argLabel `pattern` branch :576-580; `searchResultInfo` chips :687-705) is per-call inside the calls loop — identical behavior for single-call cards; no grouped-card assumption breaks. `toolCardPaths.ts:71` (`search_read` in the argPaths label-tool exclusion) is a header-chip concern, not merging.

**(c) Security** — pure frontend display logic; string-equality comparisons only; no new IPC, no untrusted-input handling change, no injection surface.

**(d) Constitution** — doc comments updated; 3 regression tests added mirroring the shell/browser pattern with backlog citations (:423, :441, :464); no warning suppressions; stated build/test verification green.

**(e) Documentation sync** — the SPEC record update is accurate and complete: title now "shell/browser/search", body records the extended set with backlog 24ddb845 / plan dc534943 pointers, correctly drops the stale "generic-merge test uses search" note (the generic-merge tests now use `file_edit`), and its regression-test list (shell, browser_navigate, offscreen_browser_navigate, search, search_read) matches the actual suite (`useAgentStore.test.ts:311/344/361/392/423/441/464`). `README.md`: no tool-card grouping/merging documentation exists (transcript mentions cover shell-output filtering, compaction, image parsing, context menu only) — no change needed. `PLAN.md`: no tool-card grouping docs (branch topology + a component listing only) — no change needed.

**(f) Multi-platform neutrality** — pure TS logic in a frontend hook; no platform-specific code, paths, or APIs.

### Findings

**L1 (low) — two untracked `.coding/` side-car files must ride in the commit.** `git status --short` shows `.coding/plans/dc534943.md` (this plan) and `.coding/knowledge/bug/2026-12-29-reasoning-effort-off-deepseek-instant-400-fixed.md` (the prior bug plan's knowledge record — the fix itself was merged into main at 07365fc, but this record was left untracked when the old `wt/agenticcoding` was deleted). Per the side-car policy (`.coding/` knowledge + plans travel with git), both belong in this plan's commit alongside the review report — otherwise the DeepSeek bug record is orphaned in the worktree and lost on branch deletion. Fix: include both in the commit (no code change).

### Clean areas
- `backlog.jsonl`: 4 appended lines, valid JSONL bookkeeping (3 new pending items + 24ddb845 in_flight, to be closed at finish).
- The never-merge test trio covers the three shapes that matter: same-batch parallel starts, the `search_read` variant, and the sequential call→result→call run.
