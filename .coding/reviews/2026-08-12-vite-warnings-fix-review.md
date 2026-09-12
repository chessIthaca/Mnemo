# Vite Build Warnings Fix — Review

**Date:** 2026-08-12
**Reviewer:** read-only subagent (spawned review)
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD`), focused on the "Fix Vite build warnings" plan.
**Branch:** (current feature branch)

## Changed files reviewed

Plan scope:
- `frontend/package.json` — `@vitejs/plugin-react` devDep `^4.3.3` → `^6.0.5`
- `frontend/vite.config.ts` — unchanged from HEAD (net zero after oxc→plugin-react swap)
- `frontend/src/App.tsx` — `getSettings` moved to static import; dynamic `import("./lib/tauri")` removed
- `frontend/src/components/layout/StatusBar.tsx` — `getGitBranch` moved to static import; dynamic `import("../../lib/tauri")` removed
- `package-lock.json` — regenerated (Babel deps removed; plugin-react 6.0.5 added)

Other uncommitted changes (not from this plan):
- `.coding/safety.toml` — rewritten by a prior plan (command-class rules); two new chained `Get-ChildItem` rules appended
- `.coding/plans/2bdd766a-*.md`, `.coding/plans/stack.json`, `.coding/plans/0beb83ef-*.md` (untracked) — plan/bookkeeping state

## Verification performed

- `npm run build` in `frontend/` → **exit 0**. Both target warnings are gone:
  - `optimizeDeps.rollupOptions` deprecation — no longer emitted (plugin-react 6.0.5 uses Rolldown-native options).
  - `INEFFECTIVE_DYNAMIC_IMPORT` — no longer emitted (no dynamic `import("./lib/tauri")` remains in source).
  - Only the pre-existing chunk-size >500 kB warning remains (unrelated).
- `npx tsc --noEmit` → **exit 0**, no errors (no unused imports, no type errors).
- `cargo test --lib safety_rules` → **63 passed, 0 failed**.
- Confirmed `getSettings` (`frontend/src/lib/tauri.ts:297`) and `getGitBranch` (`frontend/src/lib/tauri.ts:522`) exist as `export async function`.
- Confirmed both call sites remain inside `try/catch` (App.tsx:204, StatusBar.tsx:78) — failures stay non-fatal.
- Confirmed no remaining dynamic `import("./lib/tauri")` / `import("../../lib/tauri")` in `frontend/src` (only in plan markdown).
- Confirmed installed `@vitejs/plugin-react@6.0.5` peer dep is `vite:^8.0.0`; installed Vite is `8.2.1` — compatible.
- Confirmed `vite.config.ts:2` imports `@vitejs/plugin-react`, which is installed.

## Findings

### Correctness

**no findings.**

- Static imports are correct: `getSettings` and `getGitBranch` are real exports of `frontend/src/lib/tauri.ts`.
- The plugin-react 6.0.5 upgrade is valid for Vite 8.2.1 (peer dep `vite:^8.0.0` satisfied; `engines` `^20.19.0 || >=22.12.0` satisfied by Node 24.7.0).
- Removing the dynamic imports does **not** change runtime behavior in any harmful way:
  - The module was already statically imported in both files, so it was already in the initial bundle — the dynamic import was purely redundant (the source of the `INEFFECTIVE_DYNAMIC_IMPORT` warning). No lazy-loading or code-splitting was lost.
  - Error isolation is preserved: both call sites are still inside `try/catch` blocks, so a failure of `getSettings()` / `getGitBranch()` remains non-fatal (App.tsx:204 catches and logs; StatusBar.tsx:78 catches with `/* non-fatal */`).
- `vite.config.ts` is byte-identical to HEAD (the oxc swap was reverted) — no stale import of an uninstalled package.

### Bugs

**no findings.**

- No unused imports left behind (tsc --noEmit clean).
- No remaining dynamic `import("./lib/tauri")` calls in source.
- `vite.config.ts` still imports from `@vitejs/plugin-react`, which is installed (6.0.5).

### Security

**no findings.**

- The change replaces a redundant dynamic import with a static import of an already-bundled module — no new data path, no new exposure. The `getSettings`/`getGitBranch` IPC calls are unchanged.

### Constitution compliance

**no findings** (one minor, non-blocking observation below).

- **Windows 11 / PowerShell**: shell commands used Windows paths and PowerShell syntax. ✓
- **Public function doc comments**: `getSettings` has a doc comment (`tauri.ts:296`). `getGitBranch` (`tauri.ts:522`) does **not** have a doc comment — but this is a **pre-existing** condition (the function was not touched by this plan; it was already exported without a doc comment before this change). Not introduced by this diff, so not a finding against this plan. (Optional follow-up: add a one-line doc comment to `getGitBranch` for consistency with the rest of the file.)
- **No commits to main**: no commits were made by this plan; changes are uncommitted on the feature branch. ✓
- **Line-ending style**: the file tools normalized to the files' existing style; git warns about LF→CRLF normalization on `safety.toml` and a plan `.md`, which is expected Windows behavior and not a mixed-ending issue.

## Minor observation (non-blocking, NOT a finding against this plan)

`.coding/safety.toml` (changed by a prior plan, not this one) appends two chained-class rules at lines 115-123:
- `Get-ChildItem;Get-ChildItem;Get-ChildItem;Get-ChildItem`
- `Get-ChildItem;Get-ChildItem`

These are **not redundant** with the single `Get-ChildItem` rule (line 72): the classifier joins primaries with `;`, so `Get-ChildItem;Get-ChildItem` is a distinct class from `Get-ChildItem`. They appear to have been auto-saved by the agent's own "Mark Safe (same operation)" clicks during prior sessions. They are harmless (each only auto-approves the exact chained read-only listing it names) but are slightly noisy. Cleaning them up is optional and out of scope for this plan.

## Overall verdict

**ship.**

Both Vite v8.2.1 build warnings are resolved, the build and type-check are clean, the Rust safety tests pass, the static imports are correct and preserve the try/catch error isolation, and the plugin-react 6.0.5 upgrade is a valid peer-dep match for Vite 8.2.1. No correctness, bug, security, or constitution findings introduced by this plan.
