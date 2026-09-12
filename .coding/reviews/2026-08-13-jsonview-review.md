# Code Review — Collapsible JSON viewer for the Trace panel

**Reviewer:** read-only reviewer
**Date:** 2026-08-13
**Scope:** all uncommitted changes (`git diff HEAD` + untracked) for plan
"Collapsible syntax-highlighted JSON viewer for the Trace panel":
`frontend/src/components/common/JsonView.tsx` (new),
`frontend/src/components/common/JsonView.test.ts` (new),
`frontend/src/components/views/LlmTraceView.tsx`,
`frontend/vitest.config.ts`,
`.coding/plans/e3028f08-*.md`, `.coding/plans/stack.json`.

## Verdict

**No findings.** The diff is clean: the recursion is sound, depth semantics are
correct and tested, no security issues, no constitution violations, and the
styling matches project conventions. Two low-severity observations below are
explicitly judged acceptable — neither requires a fix.

## Correctness — no findings

- **Recursion** (`JsonView.tsx:64-138`): `Node` is defined at module top level
  (stable component identity), recurses with `depth + 1`, and keys children by
  object key / array index string (`key={k}`, line 126). Sibling keys are
  unique in both cases (object keys unique; array indices unique) — no key
  collision possible.
- **`defaultDepth` semantics** (`shouldAutoExpand`, lines 31-34): `depth <
  defaultDepth` with depth 0 = root. Verified against the test matrix
  (`JsonView.test.ts:31-43`): `defaultDepth 1` opens only the root,
  `defaultDepth 2` opens root + one level, `defaultDepth 0` collapses even the
  root. Matches the doc comment and both call sites (`defaultDepth={2}` for the
  full request body, `defaultDepth={1}` for per-message rows).
- **`summarize` counts** (lines 19-29): arrays → `[N item(s)]`, objects →
  `{N key(s)}`, leaves → `""`. Singular at exactly `n === 1`. Test coverage
  (`JsonView.test.ts:4-29`) includes 0/1/N for both containers, all leaf types
  (null, undefined, string, number, boolean), and nested containers counted by
  their own children. All expectations match the implementation.
- **Leaf rendering** (lines 37-61): `isLeaf` treats `null` and every
  non-object as a leaf (so `undefined` renders as the string `"undefined"` in
  slate — can't occur in real JSON, harmless for absent keys since
  `Object.entries` skips them). `leafText` puts strings through
  `JSON.stringify` (correct escaping + quotes) and uses `String()` for other
  primitives — `NaN`/`Infinity` cannot appear in parsed JSON.
- **Empty containers** (lines 118-122): expanded empty object/array renders
  the closing bracket plus an `(empty)` marker instead of children; collapsed
  it shows `{0 keys}` / `[0 items]`. Both paths correct.
- **Stale state**: `open` is initialised from `useState(shouldAutoExpand(...))`
  at mount. `JsonView` is conditionally rendered inside `MessageRow`
  (`LlmTraceView.tsx:244-248`), so collapsing a row unmounts it and re-expanding
  remounts fresh — no stale open-state within a row. If a `Node` stays mounted
  while its `data` prop changes (live poll refresh), the user's manual
  expand/collapse choices survive — desirable, not a bug.
- **No dead code left behind**: the removed `<pre>` blocks used inline
  `JSON.stringify(msg, null, 2)` only; all remaining helpers in LlmTraceView
  (`firstLine`, `contentString`, `fmtInt`, `cachePct`) are still referenced
  (lines 52, 275, 427). Plan step 2's "remove dead helpers" clause is
  satisfied vacuously.
- **Response section**: intentionally untouched — the plan states the RESPONSE
  section stays as raw SSE text (not JSON), and its `<pre>`
  (`LlmTraceView.tsx:424`) remains. In scope and correct.

## Bugs — no findings (two acceptable observations)

1. **Array children render without index labels** (`JsonView.tsx:127`,
   `name={Array.isArray(data) ? undefined : k}`). Arrays in traces are
   `messages` / `tool_calls`, where the old flat JSON showed numeric indices.
   Omitting the index loses explicit positional info (e.g. "message 3 of 40"),
   but order is visually implicit and the rows stay clean — this is a
   deliberate design call, consistent with the plan's goal of readability.
   Acceptable as-is; if positional labels are ever wanted, passing
   `name={String(i)}` for arrays is a one-line change.
2. **`RequestSection` is not keyed by request id** (`LlmTraceView.tsx:759`), so
   `expandedMsgs` (index-keyed) and any mounted JsonView `open` states persist
   when selecting a different request. This is **pre-existing** behaviour
   (`expandedMsgs` predates this diff); JsonView merely inherits the same
   pattern. Cosmetic at worst, and out of scope for this plan.

## Security — no findings

No `dangerouslySetInnerHTML` anywhere in the diff (verified by repo-wide
search; the only match is the doc comment in `JsonView.tsx:15` saying it is
*not* used). All keys and values render as React text nodes
(`{leafText(data)}`, `{name}:`), which React auto-escapes.

## Constitution compliance — no findings

- **Doc comments**: all exported items documented — `JsonView` (line 140),
  `summarize` (line 18), `shouldAutoExpand` (line 31) — plus every internal
  helper (`isLeaf`, `leafColor`, `leafText`, `Node`). Matches the doc-comment
  header style used in `LlmTraceView.tsx` / `StatsView.tsx`.
- **No suppressions**: no `#[allow(...)]`, no `@ts-ignore` /
  `@ts-expect-error`, no eslint-disable comments added (verified by search
  across `frontend/`). The Rust side is untouched by this diff, so
  `#![deny(warnings)]` status is unchanged.
- **Tests registered**: `src/components/common/JsonView.test.ts` added to the
  `include` array in `frontend/vitest.config.ts:15`. The test imports only
  pure helpers from the `.tsx` file — the same established pattern as
  `DiffView.test.ts` → `DiffView.tsx` under the plugin-free
  `environment: "node"` vitest config (esbuild transforms JSX; nothing is
  rendered).

## Style — no findings

JsonView follows project tailwind conventions: `hover:bg-bg-tertiary` (the
established token, 65 uses across the codebase), muted slate palette
(`text-slate-400/500/600` for keys/chevrons/brackets), cyan/emerald/amber
accents for leaf values matching the Trace panel's existing accent set,
`font-mono text-[0.68em]` sizing identical to the `<pre>` it replaces,
`lucide-react` chevrons (`ChevronDown`/`ChevronRight`) as elsewhere, and a
1px `border-l border-border` indent guide consistent with the panel's borders.
The `max-h-80 overflow-auto p-2` wrapper preserves the old Raw-JSON pre's
scroll behaviour.

## Bookkeeping

`.coding/plans/stack.json` and the new plan file are `.coding/` state changes
expected from the plan workflow; nothing to flag.
