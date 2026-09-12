# Review — RightPanel stray left-border removal

**Plan goal:** Remove the redundant full-height vertical border on the left edge of the tools (right) panel in `frontend/src/components/layout/RightPanel.tsx`. The resize handle in `App.tsx` (spacer + grip pill) already provides the visual divider, so the panel's own `border-l border-border` was a stray "white line".

## Verdict

**No findings.** The diff is clean and matches the plan exactly.

## Scope of review

Reviewed ALL uncommitted changes via `git status` / `git diff HEAD`:

| File | In plan? | Assessment |
|---|---|---|
| `frontend/src/components/layout/RightPanel.tsx` | YES (the change) | Clean — see below |
| `.coding/backlog.json` | no (bookkeeping) | Unrelated: one backlog item `status: pending → done`. Not a finding. |
| `.coding/plans/506f75c5-….md` | no (bookkeeping) | Unrelated: step 7 checkbox `[ ] → [x]`. Not a finding. |
| `.coding/plans/stack.json` | no (bookkeeping) | Unrelated: plan-stack pointer update. Not a finding. |
| `src/codegraph/watcher.rs` | no (pre-existing) | Unrelated: Tokio-runtime fallback for `GraphWatcher::spawn` (spawn on current runtime if present, else dedicated thread + own current-thread runtime) plus a regression test. From a prior plan; reviewed there. |
| `src-tauri/Cargo.toml` | no (pre-existing) | Listed modified in `git status` but absent from `git diff HEAD` — line-ending-only touch (CRLF warning in stderr). No content change. Not a finding. |
| `frontend/src/App.tsx` | claimed reverted | **Verified: NO net change** — file does not appear in `git status` at all. |

## Detailed checks

### 1. Correctness — the intended change is exactly right

`RightPanel.tsx` line 34, the only hunk in the file:

```diff
-      className="flex min-w-[300px] flex-col border-l border-border bg-bg-secondary"
+      className="flex min-w-[300px] flex-col bg-bg-secondary"
```

Both classes (`border-l` and `border-border`) fully removed; `flex`, `min-w-[300px]`, `flex-col`, `bg-bg-secondary` preserved in original order; the inline `style` width-clamp on line 35 untouched.

### 2. No collateral damage inside the file

Read lines 28–47 of the current file: the tab bar's own horizontal separator on line 39 (`<div className="flex items-center border-b border-border">`) is intact, as required. No other lines in the file were touched (single-hunk diff).

### 3. App.tsx — no net change, grip pill intact

`App.tsx` does not appear in `git status --short`, confirming the mid-session edit was fully reverted. Read the full file (676 lines) to confirm the `ResizeHandle` component (lines 589–676) still renders both required elements:

- Line 669 — invisible wider hit area: `<div className="absolute inset-y-0 -left-1 -right-1" />`
- Line 673 — centered grip pill: `<div className="pointer-events-none h-8 w-0.5 rounded-full bg-border" />`
- Line 666 — the w-1 spacer with hover tint: `className="group relative flex w-1 shrink-0 cursor-col-resize … hover:bg-cyan-500/20"`

The `ResizeHandle` is rendered immediately before `<RightPanel />` (lines 568–573), so removing the panel's own border leaves exactly one visual divider — the handle's grip pill — which is the plan's stated goal.

### 4. No hidden dependencies on the removed classes

Searched all `.tsx` files for `RightPanel` usages (31 matches, 7 files). Every consumer references the component symbol or the store's width/tab state — none reference or depend on the panel's left border. `bg-border` remains defined and used (grip pill, line 673; tab-bar `border-b`, line 39), so no orphaned theme token.

### 5. JSX/TS breakage

None possible — a class-string-only edit inside an existing `className` attribute; no identifiers, imports, or structure changed. (`npx tsc --noEmit` / vitest were not re-run by this read-only reviewer; the change cannot affect types, and the main agent's closing sequence runs the suites.)

### 6. Constitution compliance

- No `#[allow(...)]` added anywhere in the diff. ✅
- No Rust source touched by this plan (the `watcher.rs` change belongs to a prior plan and carries its own regression test). ✅
- No protected-file writes; only `.coding/*` bookkeeping alongside the one frontend file. ✅
- Line endings: the RightPanel.tsx hunk introduces no LF/CRLF churn (no warning for that file in stderr). ✅

## Findings by severity

- **Correctness:** none.
- **Bugs:** none.
- **Security:** none (pure CSS class removal; no new surface).
- **Constitution compliance:** none.

**NO FINDINGS** — safe to commit.
