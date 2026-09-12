# Review: Unify tab style — right's underline + left's height on both sidebars

**Plan:** afb87d8e "Unify tab style: right's underline + left's height on both sidebars"
**Scope reviewed:** ALL uncommitted changes (`git diff HEAD` + `git status --short`), not just the two source files.

## Verdict

**No findings.** The diff is clean, minimal, and implements exactly what the plan specifies. Safe to commit.

## What was reviewed

| File | Change |
|---|---|
| `frontend/src/components/layout/RightPanel.tsx` (line 58) | TabsTrigger className: dropped `py-2`, added `h-10` |
| `frontend/src/components/layout/Sidebar.tsx` (lines 58–62) | Toggle button: added `border-b-2` base; enabled arm gains `border-cyan-500`; disabled arm gains `border-transparent` |
| `.coding/plans/stack.json` | Bookkeeping (plan-stack state) — expected |
| Untracked: `.coding/plans/afb87d8e-….md`, `.coding/browser/screenshots/browser-1787337990059.png` | Bookkeeping/verification artifacts — expected |

Rust is untouched (confirmed via `git status --short`); no new public functions, so the doc-comment and `#[allow]` rules are not engaged.

## Checks performed

**Correctness**
- `RightPanel.tsx:58` — className is exactly the planned string; `py-2` removed, `h-10` inserted, no other class dropped or reordered, template literal intact, no typos. Active/inactive arms (`border-cyan-500 text-cyan-400` / `border-transparent text-slate-500 hover:text-slate-300`) byte-identical to before.
- `Sidebar.tsx:58–62` — base gained `border-b-2`; enabled arm `border-cyan-500 text-cyan-400 hover:bg-bg-tertiary`; disabled arm `border-transparent text-slate-600 line-through opacity-50 hover:bg-bg-tertiary hover:text-slate-400`. Exactly as planned. `h-10 w-10 rounded-lg` preserved.
- `frontend/src/components/ui/tabs.tsx` — `TabsTrigger`/`TabsList` are pure pass-through wrappers (no merged base classes, no `cn()` conflicts), so `h-10` + `border-b-2` apply cleanly and `flex items-center` keeps icon/label/X vertically centered at 40px.
- No layout shift: both Sidebar state arms now carry a 2px bottom border inside the fixed 40×40 box (`border-transparent` vs `border-cyan-500` — same width); RightPanel triggers already had `border-b-2` on both arms.
- Disabled left toggles still read as disabled: `line-through` + `opacity-50` + `text-slate-600`, and `border-transparent` guarantees no cyan underline when disabled.

**Tab-bar alignment (RightPanel.tsx:43–106)**
- `TabsList` carries only `flex` (no padding), so the trigger's bottom edge is flush with the list's bottom edge; the trigger (now the tallest item at 40px) sets the bar height, and its 2px underline lands at the bottom of the bar, directly on the container's `border-b border-border` line — same geometry as the previous `py-2` version, just taller.
- The bar's `overflow-x-auto` on the Tabs root does not clip the 2px border: the border lies inside the trigger box, which lies entirely inside the scroll container's content box.
- The standalone X close button (line 99–105, ~32px) is vertically centered by the container's `items-center` at the new 40px bar height — no misalignment.

**Consistency**
- Underline semantics now match: cyan underline = right panel's active tab / left sidebar's enabled tool. The enable-vs-select mapping difference was the explicit user request (quoted in the plan), not an oversight.
- Non-tab buttons (About, Folder, Settings, PanelRight — `Sidebar.tsx:38–46, 71–100`) verified untouched in the diff, as intended.

**Constitution (project)**
- *Documentation sync:* searched README.md / PLAN.md and all Markdown for `underline`, `border-b-2`, `TabsTrigger`, `tab style`, `sidebar style` — matches only in `.coding/plans` and `.coding/reviews` (bookkeeping). No doc claims about tab/sidebar styling exist, so no doc updates are required.
- *Multi-platform neutrality:* pure Tailwind className changes in two `.tsx` files; no platform APIs, no `cfg(windows)`, no shell syntax. Neutral by construction.

## Observations (informational only — explicitly NOT findings, no action required)

- With `rounded-lg` retained on the Sidebar toggles, the 2px underline's ends follow the 8px corner radius, so the cyan line curves up slightly at its tips on the 40px-wide button. This was an explicit plan decision (keep `rounded-lg` + hover bg) and reads fine visually.
- The untracked screenshot under `.coding/browser/screenshots/` is a verification artifact from the plan's step 3; harmless if swept into the commit via `git add -A`, but it is not part of the source change.
- Per the task brief, the running app instance serves a pre-rebuild bundle, so visual confirmation happens on restart; the static review above covers the geometry claims (tests: 481 pass, `npm run build` clean per the brief — consistent with a pure-className change).
