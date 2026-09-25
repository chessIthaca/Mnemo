+++
title = "Token Dashboard — savings aggregates + get_savings_stats IPC + Dashboard tab"
created = "2027-01-11"
+++

SPEC (plan 6494b738, backlog 652ae094; committed dd76b9f on wt/mnemo — NOT yet merged to main). Read-only savings dashboard over the `savings_events` ledger that landed in 1a37d2f (schema/writer untouched — `src/memory/schema.rs` + `src/agent/turn.rs` are outside this change).

BACKEND
- `src/memory/types.rs` (after `SavingsEvent`): `SavingsStats` + `SavingsKindBreakdown` / `SavingsDayBreakdown` / `CacheEfficiency`. Token sums are signed i64 (`archive_expand` is negative; re-expansion subtraction must not wrap). `SavingsDayBreakdown.day` is an epoch DAY (not seconds) — deliberately NOT `DayBreakdown`, which is all-u64 prompt/completion with day in seconds.
- `src/memory/mod.rs::savings_stats()` (trait ~:390, impl ~:2200-2299, between `savings_events_rows` and `archive_tool_result`): mirrors `project_stats`'s spawn_blocking + COALESCE-sums pattern; an empty store returns zeros/empty vecs, never an error. SQL: per-kind `GROUP BY kind ORDER BY SUM(tokens_saved) DESC, kind` (deterministic tie-break, negative kind last); per-day `created_at / 86400 … ORDER BY day`; recent `ORDER BY created_at DESC, id DESC LIMIT 50`; cache NULL-safe (`COUNT(cached_tokens)` counts only non-NULL rows, `SUM(COALESCE(cached_tokens,0))`).
- `src-tauri/src/ipc/agent.rs:~960-971` `get_savings_stats` command (mirrors `get_project_stats`), registered in `src-tauri/src/main.rs:~918`. Additive src-tauri delta only.

FRONTEND
- `frontend/src/lib/tauri.ts`: DTOs + `getSavingsStats` binding; `SavingsEvent.detail?: string | null` ↔ serde skip-attr.
- `frontend/src/components/views/DashboardView.tsx`: fetch wrapper + exported `DashboardBody` (the presentational half — `useEffect` never runs under `renderToStaticMarkup`, so the body is what tests render; BacklogItemCard precedent). `fmtDay` multiplies the epoch day by 86_400_000 ms (NOT ×1000). `data-testid` markers in render order: `dashboard-view` :160 → `dashboard-per-kind` :186 → `dashboard-per-day` :208 → `dashboard-recent` :235; the prompt-cache card :260 deliberately carries none (nothing slices it). `DashboardView.test.tsx` (8 tests) registered in `frontend/vitest.config.ts`'s explicit include list (guarded by `vitestInclude.test.ts`).
- Registry: `"dashboard"` immediately before `"memory"` in all three sites — `rightPanelViews.tsx:65` (Gauge icon), the RightPanelTab union (`agentState.ts:333`) and `ALL_RIGHT_PANEL_TABS` (`:347`); `rightPanelViews.test.ts` passes UNMODIFIED.

INVARIANTS
- Metered figures render TOKENS ONLY. Dollar figures live solely in the cache section, always labeled ESTIMATED, never summed into metered totals; no pricing → render "—", never a fabricated "$0.00".
- Every lever behind `[general.optimizer]` (predecessor b1fa7963 invariant, still binding).

TESTS: aggregate test in `src/memory/tests.rs` (~:2090-2166); root `cargo test` 2712 passed / 0 failed / 5 ignored (+19 integration, +1 doc-test); `cargo test -p mnemo-app --no-run` compiles (src-tauri DOES build in this worktree); frontend `tsc --noEmit` clean, vitest 92 files / 1289 tests.

REVIEWS (3 rounds, all uncommitted changesets): r1 FINDINGS 0/4 (all fixed), r2 FINDINGS 0/1 — `cardMarkup`'s per-day slice ran to END OF STRING because `dashboard-per-day` was the last marker, making the epoch-day pin timezone-dependent; fixed by adding the `dashboard-recent` marker (slice now terminates there) + per-day fixture figure 12_000→12_345 (kills the collision with the recent row's `tokens_before`); r3 PASS (`.coding/reviews/2026-09-25-6494b738-token-dashboard-round3-review.md`). Docs (README/FEATURES/CONFIGURATION/PLAN) updated to shipped-feature text — four stale "still-pending" claims corrected.

KNOWN NON-BLOCKING (deliberately left): no live refresh while the tab is open; persisted `disabledTabs` lists predate the tab; `fmtCost(0)` collapses "no pricing" and "priced at $0".
