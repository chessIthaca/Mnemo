## Verdict: FINDINGS (1 high, 4 low)

Review of all uncommitted changes on `wt/agenticcoder` (3 files, +408/−88) for plan 195b2eb1
"Trace stats: legend chips as per-chart series toggles".

**Summary:** The legend-toggle UI, persistence layer, lib helpers, and the phase-chart
rescale math are correct and well tested. But the token chart's height/max driver
double-counts `cached` — `heightVisibility` forces `cached: true` into `stackTotal`
(which sums it) instead of excluding it — inflating Relative-mode heights by each
row's cache-hit share and breaking the all-hidden empty state. The lib tests pass
because the bug lives in the untested component wiring.

<!-- findings appended below -->

---

### High 1 — Token-chart heights & maxima double-count `cached` (stack-math regression vs main)

`frontend/src/components/views/TraceStats.tsx:344-349`:

```ts
// `cached` is a subset of prompt (an overlay) — it never contributes to
// the height, so the height math always treats it as "visible".
const heightVisibility = { ...visibility, cached: true };
const hasData  = rows.some((r) => stackTotal(tokenMsByKey(r), heightVisibility) > 0);
const maxStack = Math.max(0, ...rows.map((r) => stackTotal(tokenMsByKey(r), heightVisibility)));
```

`stackTotal` sums **every** key of `msByKey` whose visibility is not `false`, and
`tokenMsByKey(r)` returns `{ prompt, cached, completion, reasoning }`. Forcing
`cached: true` therefore makes the height driver sum `prompt + cached +
completion + reasoning` — the cached subset of prompt is counted twice (once
inside `prompt`, once standalone). This contradicts:

- the old driver on main, `tokenStackTotal` (prompt + completion + reasoning,
  doc: "cached is a SUBSET of prompt … not additive");
- `stackTotal`'s own doc comment in `traceStats.ts:295-299`: *"Pass a map
  WITHOUT non-additive keys (e.g. the token chart's `cached` …)"*;
- the module doc's invariant ("the prompt bar always counts toward the height",
  cached never additive) and the plan goal.

The comment inverts the intent: for `cached` to *never contribute* to the sum it
must be **excluded** (`cached: false`), not forced visible.

Concrete impact (Relative mode — the **default**): prompt=1000, cached=800,
completion=50, reasoning=20 → old column total **1070**, new **1870** (+75%).
A heavily-cached request renders far taller than an identical uncached request
even though the visible bar content is identical, and each bar's pixel height no
longer matches the sum of its rendered segments' `flexGrow` values
(prompt+completion+reasoning). Second symptom: hiding **all four** token chips
leaves `total = cached > 0` for any row with cached tokens, so `hasData` stays
true — the chart renders empty-height columns (the `stacked` palette excludes
`cached`, so no segment renders) and the "All series hidden" hint never appears.
Also, hiding only `prompt` still counts `cached` (a subset of the hidden
segment): total = cached + completion + reasoning.

**Fix:** `const heightVisibility = { ...visibility, cached: false };` and reword
the comment to "excluded from the height sum (non-additive overlay)". With that,
toggling `cached` stays height-neutral, hiding `prompt` yields
completion+reasoning, all-hidden → `hasData` false → the hint shows, and the
math matches `tokenStackTotal` semantics. The lib tests pass today only because
this wiring is component-internal — see Low 4 for making it unit-testable.

### Low 1 — Side effect inside the `setState` updater (updater purity)

`TraceStats.tsx:448-457`: `toggleSegment` calls `storeHiddenSegments(next)`
**inside** the `setHiddenLists((prev) => …)` updater. React updaters must be
pure; under StrictMode they are double-invoked (two identical idempotent writes
today — benign, but a latent hazard), and React reserves the right to re-invoke
or discard updaters. Compute `next` from the render-fresh `hiddenLists` outside
the updater and then call both `setHiddenLists(next)` and
`storeHiddenSegments(next)` (the component re-renders on every poll anyway, so
the closure is never stale for a click handler).

### Low 2 — "All series hidden" hint fires when merely ANY series is hidden

`TraceStats.tsx:272` and `:347`: `anyHidden = …some((s) => hidden.has(s.key))`
selects the message "All series hidden — click a legend chip to show one."
whenever `hasData` is false. With genuinely **no recorded data** plus one hidden
segment (a plausible fresh-session state, since the hidden lists persist), the
chart falsely claims all series are hidden. Use `every` for the all-hidden
message (or keep the "No … yet" text unless every palette key is hidden). Same
logic in TokenChart. (The token all-hidden symptom from High 1 disappears with
the fix, but the any-vs-all condition is wrong in both charts regardless.)

### Low 3 — Documentation sync: README + .coding/llm-trace.md not updated

`README.md:61` (Trace-tab bullet) and `.coding/llm-trace.md:10-17` (stats-strip
paragraph) describe these exact charts in detail — stacked phase segments, token
stacks with dithered cached overlay, Fill/Relative toggle, tooltips — but
neither mentions the new legend-chip series toggles, visible-only max
recomputation, or the `tracestats.hiddenSegments` persistence. Per the
constitution's documentation-sync expectation, add a short clause to both.

### Low 4 — Divergent "total" implementations + untested component wiring (the gap that let High 1 through)

- `tokenStackTotal` (`traceStats.ts:107`) is now production-dead — its only
  references are its own tests (the `TraceStats.tsx` import was removed). Two
  "stack total" implementations with subtly different semantics is exactly what
  allowed High 1; delete `tokenStackTotal` (port its tests to
  `stackTotal(tokenMsByKey(r), { cached: false })`) or keep it as the height
  driver's reference.
- `tokenTipRows` (`traceStats.ts:335-355`) still hardcodes the four `bg-*`
  classes that `TOKEN_SEGMENTS` now centralizes — a future palette edit would
  change chart + legend but not the tooltip dots.
- The new lib helpers are well covered, but nothing tests the component wiring
  (no component-test infra exists: no @testing-library, no `*.test.tsx), which
  is where High 1 lives. Extracting the rule into `traceStats.ts` — e.g.
  `chartVisibility(segments, hidden)` plus a token-specific height-visibility
  that excludes non-additive keys — would pin the cached-excluded invariant in
  a unit test.

---

## Verified correct (checked, no action needed)

- **Phase-chart math:** hidden phases excluded from both per-row heights and
  `maxTotal`; with nothing hidden, `stackTotal(phaseMsByKey(r), …)` ≡ the old
  `r.totalMs` (prep+compact+send+wait+reason+generate+tools = `totalMs` by
  construction) — a behavior-preserving refactor. Sliver guard
  `Math.max(2, …)` preserved in both charts, gated on `total > 0`.
- **Stack order:** phase segments render in palette order (prep bottom → tools
  top via `flex-col-reverse`); token columns stack prompt → completion →
  reasoning with `cached` overlay-only (`stacked` filters it out; overlay gated
  on `!hidden.has("cached") && r.cached > 0`) — exactly the plan's cached
  overlay-only semantics for the chip itself.
- **`phaseTipRows` refactor** to `phaseMsByKey` is behavior-identical (same
  key→field mapping); existing tests still assert the values.
- **Persistence:** sparse hidden-key lists; corrupt/missing JSON tolerated
  (try/catch, `?? {}`, arrays filtered to strings); write best-effort
  (quota/private-mode); stale persisted keys harmless (`stackSegments` iterates
  the palette, `stackTotal` iterates `msByKey` — an unknown key is never
  consulted); duplicate entries harmless (Set for rendering; `filter` removes
  all copies on untoggle). New segments default visible.
- **Security:** `JSON.parse` output used only for set membership — never
  rendered as HTML; no XSS surface. Keys/labels come from code constants.
- **Multi-platform neutrality:** browser APIs (`localStorage`, portal) stay in
  the React component; the lib stays pure; no platform-specific code anywhere.
- **A11y:** toggle chips are `type="button"` with `aria-pressed`,
  `aria-label`, and `title`; off-state = faded swatch + line-through; hidden
  series keep their legend slot (no layout jump). The `Legend` non-toggle
  branch is currently unreachable (both call sites pass `onToggle`) — kept as a
  guarded API, informational only.
- **Tests/build:** `npm test` and `npm run build` reported green by the main
  agent; the new helper tests are meaningful (key alignment with palettes,
  zero-drop + hidden exclusion + order, visible-only sums, rescale driver).
