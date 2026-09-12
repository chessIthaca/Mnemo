## Verdict: PASS

**Summary:** The subscription move (App → InflightBar) and the memoization of the three prop-less shell components are React/zustand-correct, behavior-preserving, and pinned by a sound source-contract regression test. No findings (0 high, 0 low).

## Scope reviewed

All uncommitted changes on `wt/agenticcoding` for plan 00d88e13 (mem-perf review HIGH 1):

- `frontend/src/App.tsx` — deleted the whole-agent `activeState` subscription; mount is now `<InflightBar agentId={activeAgent} />`
- `frontend/src/components/chat/InflightBar.tsx` — split into self-subscribing outer gate + unchanged inner `InflightBarPanel`
- `frontend/src/components/layout/{Sidebar,InputBar,StatusBar}.tsx` — wrapped in `memo(...)`
- `frontend/src/App.shellRender.test.ts` — NEW source-contract regression test
- `.coding/backlog.jsonl` — status `pending` → `in_flight` (bookkeeping only)
- `.coding/plans/00d88e13.md` — untracked plan file (bookkeeping)

## 1. React correctness of the InflightBar split — PASS

- **Rules of hooks:** the outer gate's single `useAgentStore` hook runs unconditionally before `if (!state) return null;` (InflightBar.tsx:70-74). The inner panel is byte-for-byte the old body; its hooks all run before its own pre-existing `if (!showBar) return null;` early return (line 216). No conditional-hook risk anywhere.
- **Behavior parity with the old gating:** old `{activeState && <InflightBar state={activeState} agentId={activeAgent} />}` rendered nothing when `activeAgent` was null OR the agent wasn't registered; the new outer gate (`agentId != null ? s.agents[agentId] : undefined` → `if (!state) return null`) produces identical DOM in all three cases.
- **Local-state semantics unchanged:** `expanded`/`height` live in the inner panel. Old code unmounted the whole bar when `activeState` went undefined; new code unmounts just the inner panel — local state is lost in both. Across agent switches the component stays mounted at the same tree position in both old and new code, so panel state persists exactly as before.
- **`agentId` plumbing:** the inner panel still receives `agentId` (used by the Compact button's `compact(agentId)` / `agentId === null` disabled checks) — unchanged.

## 2. Zustand semantics — App no longer re-renders per streaming frame — PASS

- **Premise confirmed:** `appendStreamingText` (useAgentStore.ts:1020-1036) spreads the agent per flush (`[id]: { ...agent, ... }`), so the agent object's identity changes on every rAF flush — subscribing to it re-rendered per frame, exactly as the review found.
- **The new selector is safe:** `(s) => (agentId != null ? s.agents[agentId] : undefined)` returns a stable store object reference or `undefined` — never a fresh object — so zustand v4.5.5's `Object.is` equality has no infinite-loop / spurious-rerender hazard. The per-render closure over `agentId` is standard zustand usage (selector identity changes are handled by `useSyncExternalStoreWithSelector`).
- **App's remaining reactive subscriptions exhaustively verified** (full read of App.tsx, 847 lines): `activeAgent` (number), `rightPanelVisible` (bool), `rightPanelWidth` (number), and five store action functions (stable references) — lines 86-94 only. Everything else is `getState()`/`setState()` (non-reactive) or `useAgentStore.subscribe` for `didMainTurnEnd` (non-reactive callback).
- **Hooks called inside App also checked:** `useAgentEvents()` subscribes only to four stable store action functions (`handleAgentEvent`, `appendStreamingText`, `appendStreamingReasoning`, `applyToolCallArgDeltas`); `useBrowserOverlay(open)` is effect-only with no store subscription. Neither can re-render App per frame. The fix's goal is fully achieved: only InflightBar (and MainPanel, by design — it renders the streaming conversation) re-renders per flush.

## 3. memo() correctness — PASS

- All three components are genuinely prop-less (`memo(function X() {...})` with zero parameters), and App mounts them with no props (`<Sidebar />` App.tsx:674, `<InputBar />` :718, `<StatusBar />` :719). A repo-wide search confirms App.tsx is their **only** importer — nothing anywhere passes props to them, so `memo`'s shallow prop compare always blocks parent-driven re-renders while their own `useAgentStore` slices still drive store-triggered updates.
- Named inner functions preserve DevTools display names; named exports (`export const X = memo(...)`) keep App's named imports valid (tsc exit 0 per the run log).
- MainPanel/RightPanel deliberately NOT memoized — correct: MainPanel must re-render during streaming; RightPanel left conservative (no behavior change).

## 4. Regression test soundness — PASS

`App.shellRender.test.ts` follows the established `?raw` source-contract precedent (InflightBar.test.ts, MainPanel.tabStyle.test.ts, StatusBar.truncate.test.ts — the repo has no React DOM test infra, documented in each):

- Every assertion verified against the actual sources: App.tsx contains no `s.agents[s.activeAgent]` and contains `<InflightBar agentId={activeAgent} />` (line 717, exact); InflightBar.tsx contains `s.agents[agentId]` (line 71) and `if (!state) return null;` (line 72); all three shell sources contain `= memo(function X()`.
- The test genuinely pins the regression: reintroducing the whole-agent selector in App.tsx fails test 1; reverting InflightBar to prop-driven fails tests 3-4; unwrapping any memo fails test 5. Not self-referential (asserts on the component sources, not on itself).
- Pickup + typing: vitest runs with default config (no `test` block in vite.config.ts → default include `**/*.test.ts`, node environment — consistent with every other source-contract test's stated assumptions); `?raw` modules are typed via `vite/client` (referenced in frontend/src/vite-env.d.ts:5), so `tsc --noEmit` covers the file. Naming (`App.shellRender.test.ts`) matches the repo's descriptive-test convention.

## 5. Existing tests still valid — PASS

- `InflightBar.test.ts` asserts only on strings in the inner panel's body (`pb-1 group-hover:block`, `compact(agentId)`, `aria-expanded={expanded}`, `useCountUp(...)`, phase labels, …) — all still present verbatim; nothing asserted the old export signature.
- `InputBar.test.ts` (import lines, JSX markers, `handleSend`→`handleStop` source slice), `StatusBar.truncate.test.ts` (class strings), and `resizeHandleMotif.test.ts` (InflightBar handle classes) — none affected by the memo wrap or the split.

## 6. Constitution checks — PASS

- **Code style / doc comments:** doc comments added on the outer gate, the inner panel, `InflightBarProps`, and all three memo wraps; existing style (copyright headers, comment voice) preserved. No `#[allow(...)]`-style suppressions involved; frontend-only, so the warning-free Rust build is untouched (root + src-tauri `cargo test` green per the run log: 2003 / 191+4 passed; `npm test --workspace frontend` exit 0).
- **Multi-platform neutrality:** pure React/TS change, no platform-specific APIs, paths, or shell syntax. PASS.
- **Documentation sync:** README.md / PLAN.md describe features, config, and architecture — neither documents the shell's internal subscription wiring or InflightBar's prop contract (searched; PLAN.md:819-821's streaming re-render note is a general design statement, not this wiring). An internal perf fix with no user-facing config or behavior change needs no doc update; the contract is documented in the code comments + the regression test. PASS.

## Notes (non-blocking, no action required)

1. **Inherent source-contract brittleness:** `not.toContain("s.agents[s.activeAgent]")` would also fail on a future *comment* in App.tsx quoting the old selector, and the exact-JSX pins (`<InflightBar agentId={activeAgent} />`) fail on reformatting-only changes. This is the accepted trade-off of the repo's established source-contract style (dozens of such tests) and is documented in the test's own header — not a defect in this change.
2. The `agentId != null` (loose) check matches the old App selector's pattern and defensively covers `undefined` as well as `null` — consistent with the codebase.
