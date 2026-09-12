# Review: agent tab rail background restore (fix/agent-tab-rail-background)

**Date:** 2026-08-22
**Branch:** fix/agent-tab-rail-background
**Scope:** all uncommitted changes (`git diff HEAD` + `git status`)

## Changes reviewed

| File | Change |
|---|---|
| `frontend/src/components/layout/MainPanel.tsx` | Line 90: TabsList className gains `bg-bg-secondary` (`flex items-stretch overflow-x-auto border-b border-border` → same + ` bg-bg-secondary`) |
| `frontend/src/components/layout/MainPanel.tabStyle.test.ts` | Lines 56–64: new test "keeps the rail on its own bg-bg-secondary chrome band" asserting the edited class string |
| `.coding/plans/stack.json` | Plan-stack bookkeeping (not source) |
| Untracked: `.coding/plans/913494ac-2605-488e-a7bb-394a60127465.md`, `.coding/browser/screenshots/browser-1787350784740.png` | `.coding/` bookkeeping/artifacts (not source) |

## Verdict: no findings

### 1. Correctness / bugs — clean

- The MainPanel edit does exactly what it claims: one class added to the TabsList at line 90. Trigger styling (h-10, cyan `border-b-2` active motif, model second line, close affordance) is byte-for-byte untouched — verified by reading lines 82–139.
- The new test asserts the exact literal `flex items-stretch overflow-x-auto border-b border-border bg-bg-secondary`, which matches MainPanel.tsx:90 verbatim. The assertion is order-sensitive, but that is the established pattern of every other test in this file (the active-branch and h-10 assertions are likewise exact-string), so it is consistent, not a defect.
- Regression-guard check: against the pre-edit source (`...border-b border-border"`) the asserted string is absent, so the test fails without the fix and passes with it. It is a real guard against silently dropping the background again.
- No risk to RightPanel or other components: RightPanel is untouched, and its tab bar already inherits `bg-bg-secondary` from the panel container (RightPanel.tsx:38), so the visual-parity rationale holds. Repo-wide grep for `bg-bg-secondary` confirms Sidebar (Sidebar.tsx:37), InputBar (InputBar.tsx:605), StatusBar (StatusBar.tsx:540), and InflightBar (InflightBar.tsx:195) all sit on the same token — the change restores consistency rather than introducing a one-off.

### 2. Security — clean

Pure styling plus a static source-contract test. No new dependencies, no network/file/system access, no user-input handling, nothing executable added.

### 3. Constitution compliance — clean

- **Documentation sync:** no updates required. PLAN.md:435 describes the tab bar's *trigger* style ("same flat-rail style as the right-panel tools (fixed h-10 triggers, cyan bottom border on the active tab)") and makes no claim about the rail's background, so restoring `bg-bg-secondary` makes nothing stale. README.md documents no tab-bar styling at all. Module/test doc comments remain accurate (the file header's contract description still describes the rail parity correctly).
- **Multi-platform neutrality:** a single Tailwind utility class. No `cfg(windows)`, no Windows-only APIs, no platform paths, no shell syntax. Behaves identically on macOS and Windows.
- **Warning-free build:** the change is frontend TypeScript; no `#[allow(...)]` was added anywhere, no dead code, no unused imports (the test reuses the already-imported `describe`/`expect`/`it`/`mainPanelSource`). Nothing here can trip `#![deny(warnings)]` at either Rust crate root.
- Project rule "regression test for every defect": satisfied — the defect was the dropped background, and the new test reproduces it (fails on the old source) and asserts the fix.

### 4. Test quality — good

Exact, minimal, and fails on the old source. The comment block above the assertion documents *why* the band matters (chrome-band parity with the other five regions), which is exactly the context a future restyler needs.

## Informational notes (not findings)

- Commit hygiene: the untracked `.coding/browser/screenshots/browser-1787350784740.png` is a browser-tool screenshot artifact. Harmless either way, but consider whether it belongs in the commit or should be left untracked.
- Test-string ordering: if a future edit reorders the TabsList classes (e.g. `bg-bg-secondary` before `border-b`) while keeping the background, this test fails despite correct styling. Accepted trade-off, consistent with the file's existing exact-string style; flagged only for awareness.
