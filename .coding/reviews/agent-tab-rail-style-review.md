# Review: Align agent tab bar with tool tab style

**Plan:** 6bec5e06-9f86-4588-b04a-c843063e3859 — restyle MainPanel agent tab bar to RightPanel's flat rail (h-10 triggers, 2px cyan bottom border on active, no raised pill). StatusBar explicitly out of scope.
**Files reviewed:** `frontend/src/components/layout/MainPanel.tsx`, `frontend/src/components/layout/MainPanel.tabStyle.test.ts` (new), `frontend/vitest.config.ts`, `PLAN.md`, `.coding/plans/stack.json` (bookkeeping, ignored). Reference: `frontend/src/components/layout/RightPanel.tsx`, `frontend/src/components/ui/tabs.tsx`, `frontend/src/App.tsx`, `frontend/src/components/chat/InflightBar.test.ts`.

## Verdict: no blocking findings

The diff is minimal (4 lines in MainPanel.tsx: 1 TabsList + 3 TabsTrigger className lines), all semantics are preserved, the test assertions match the edited source exactly, and the change satisfies the constitution checks. Low-severity, non-blocking observations are listed at the end — none require action.

## 1. Correctness / bugs — PASS

- **Semantics preserved** (MainPanel.tsx): status dot / approval ping (116–132), two-line name+model (133–143), approval badge (144–148), subagent close-x with stopPropagation + `cancel()` (152–168), and the non-active approval banner (179–203) are all untouched by the diff. Radix wiring (`value`/`onValueChange`/`activationMode`) unchanged (85–89).
- **No hidden style conflicts:** `ui/tabs.tsx` (22–34) passes `className` straight through with no merged base classes and no `data-[state=active]:` defaults, so the custom classes fully control the look — no shadcn-style pill can leak back in.
- **Two-line content in h-10 — no clipping risk:** name at `text-sm` (14px × leading-tight 1.25 ≈ 17.5px) + model at `text-[0.625em]` (≈8.75px × 1.25 ≈ 11px) ≈ 28.5px total in a 40px trigger (line 105 `h-10`, lines 133–142). ~11px slack; safe.
- **TabsList change (line 90) is intentional parity, not a regression:**
  - Dropped `bg-bg-secondary`: the app root (`App.tsx:541`) and MainPanel root (line 83) are `bg-bg-primary`, so the rail now sits on the same background as the conversation, separated only by the `border-b border-border` hairline — exactly the RightPanel pattern (its rail and content both sit on the panel's `bg-bg-secondary`, `RightPanel.tsx:38,43`). Active state is now conveyed by cyan border/text instead of a bg swap; inactive `text-slate-500` contrast is unchanged. Readability OK.
  - Dropped `gap-1` / `px-2 py-1`: matches RightPanel (`TabsList className="flex"`, line 50; rail padding lives on the triggers' `px-3`). Rail height is now exactly h-10 on both bars → the stated goal (aligned heights) is achieved.
  - `items-center → items-stretch`: equivalent for fixed h-10 triggers; the `tabs.length === 0` placeholder (172–174) is unaffected in practice (no h-10 siblings → stretch is a no-op on its natural height).
- **Border overlap:** trigger `border-b-2` (bottom of 40px trigger) overlays the list's 1px `border-b` — active cyan covers the hairline, inactive transparent lets it show through. Identical mechanism to RightPanel (border on wrapping div, line 43). ✓
- **StatusBar untouched:** confirmed — no StatusBar file appears in `git status` / `git diff`.

## 2. Test quality — PASS

All assertions verified verbatim against the edited source:

| Assertion (test line) | Source match |
|---|---|
| `'? "border-cyan-500 text-cyan-400"'` (31) | MainPanel.tsx:107 exact, incl. `? ` prefix |
| `': "border-transparent text-slate-500 hover:text-slate-300"'` (37) | MainPanel.tsx:108 exact, incl. `: ` prefix |
| `/flex h-10 items-center gap-2 border-b-2/` (44) | MainPanel.tsx:105 |
| `not.toContain("rounded-t-md")` (45) | confirmed absent (only `rounded`, `rounded-full` remain) |
| `"flex h-10 items-center gap-2 border-b-2 px-3 text-sm"` (52) | MainPanel.tsx:105 exact substring |
| `"truncate text-[0.625em] text-slate-500"`, `"agent-running-dot"`, `"Close subagent"` (59–61) | MainPanel.tsx:139, 128, 163 |
| RightPanel guard: `"flex h-10 items-center gap-1.5 border-b-2"`, `'? "border-cyan-500 text-cyan-400"'` (69–74) | RightPanel.tsx:58, 60 exact |

- **Drift guard is sound:** it pins the RightPanel reference rail so a future tool-tab restyle forces a parity decision. Note the `gap-1.5` vs `gap-2` difference is correctly not unified — the guard pins each side's own contract.
- **Regression rule (fails on old code, passes on new):** verified by reasoning from the diff. Old trigger had `? "bg-bg-primary text-slate-200"` (fails test 1), `rounded-t-md` (fails test 4), and no `h-10` (fails test 3 regex). New source passes all. ✓
- **vitest.config.ts:52** include entry matches the file's exact path; follows the established `InflightBar.test.ts` pattern (`?raw` import, `node` environment, vite/client typing precedent already in place).

## 3. Security — PASS

Pure styling + a static source-reading test. No new inputs, IPC, or eval surface.

## 4. Constitution compliance — PASS

- **Documentation sync:** PLAN.md:435 accurately describes the new style ("name + model + status … flat-rail … fixed h-10 triggers, cyan bottom border"). README.md needs nothing (its only "tab" mention, line 61, is about Files/Diff tab file-link routing — unrelated). MainPanel.tsx module doc comment (13–34) describes behavior, not chrome, so it is not stale.
- **Multi-platform neutrality:** Tailwind classes only; no platform-specific code, paths, or `cfg` gates. No Rust changed → no `#[allow(...)]` or doc-comment concerns (and no `#[allow]` added anywhere in the diff).

## 5. Observations (low severity, non-blocking — no action required)

1. **Test over-fitting risk, convention-consistent.** The assertions pin exact class ordering (e.g. `"flex h-10 items-center gap-2 border-b-2 px-3 text-sm"`) and the `? `/`: ` ternary prefixes — a harmless prettier reflow or class reorder would fail the test without a visual change. Also `not.toContain("rounded-t-md")` is file-scoped, so a future legitimate `rounded-t-md` elsewhere in MainPanel.tsx would trip it. This is the accepted trade-off of the project's established static-contract pattern (InflightBar.test.ts behaves identically), the strings match the current source exactly, and the header comments document intent — acceptable as-is.
2. **Approval ring is now square.** Non-active approval tabs keep `ring-1 ring-yellow-500/50` (MainPanel.tsx:110–112); with `rounded-t-md` gone the ring renders square instead of rounded. This is the intended flat-rail look, not a defect.
3. **Lost `bg-bg-secondary` on the rail** (MainPanel.tsx:90) — verified deliberate parity with RightPanel (section 1); flagged here only because the review brief asked. Not a readability regression.

## Conclusion

**No findings requiring fixes.** The restyle is correct, minimal, semantics-preserving, matches the RightPanel reference, the regression test provably fails on the old code and passes on the new, and all constitution checks pass.
