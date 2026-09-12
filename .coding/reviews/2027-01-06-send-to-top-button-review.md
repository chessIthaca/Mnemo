## Verdict: PASS

Review of ALL uncommitted changes on `wt/agenticcoding` (`git diff HEAD` + untracked) for plan **0fb62d71 — "Backlog items: send-to-top button"**. The change is a clean, minimal, well-tested pure-frontend feature that reuses the existing reorder path exactly as planned. Zero findings.

**Scope reviewed** (git status):
- `frontend/src/components/views/BacklogView.tsx` (+35) — the feature
- `frontend/src/components/views/BacklogView.test.tsx` (+38/−4) — its tests
- `.coding/plans/037f2bdc.md` (1 checkbox tick) — unrelated-but-accurate bookkeeping, see Observations
- `.coding/plans/0fb62d71.md` (untracked) — this plan's own file (`.coding/` travels with git by convention)

No Rust changes, as the plan promised.

### 1. Correctness

**`moveIdToFront` (BacklogView.tsx:257-261)** — verified all edges:
- Middle id: `[id, ...slice(0, at), ...slice(at+1)]` — new array, id first, rest in relative order; the two slices cover every position except `at`, so **no id can be dropped**.
- Already-first (`at === 0`) and absent (`at === -1`, incl. empty input) return the input order unchanged — a stale-render click degrades to a harmless no-op reorder. The `at <= 0` early-return hands back the *input* array rather than a copy; safe because the sole caller (`handleSendToTop`) only forwards it to `backlogReorder` and never mutates it (plan decision #3, and the doc comment honestly says "an equal order" rather than claiming a fresh array).
- Exported with a doc comment (constitution requirement for public functions — met).

**`handleSendToTop` (BacklogView.tsx:509-519)** — mirrors `handleMove`'s shape exactly (build ids from the store backlog → mutate order → `await backlogReorder(ids)` → `console.error` on failure). It resolves the target by `item.id` via `indexOf` rather than by the render-time `index` prop used by `handleMove`'s swap — strictly *more* robust against a stale index. No guard needed for `index === 0`: the button is disabled there, and even a stale fire would be the no-op path above.

**Button (BacklogView.tsx:592-599)** — placed immediately before the ChevronUp move-up button; identical class string to the move buttons (`rounded p-1 text-slate-400 transition-colors hover:bg-bg-primary hover:text-slate-200 disabled:opacity-30`); `disabled={index === 0}` matches move-up's gate; `title="Send to top"`; `ArrowUpToLine` at `h-3.5 w-3.5` matching the row's icon sizing. `ArrowUpToLine` exists in the pinned `lucide-react@^0.460.0` (added upstream ~0.263), and the reported clean `tsc --noEmit` confirms the import resolves.

### 2. Bugs — none found

- **Id preservation**: `moveIdToFront` cannot drop or duplicate ids (slice coverage above); the backend `BacklogStore::reorder` (src/backlog.rs:314-332) additionally dedupes and skips unknown ids, so even a malformed push is safe.
- **Stale-render safety**: item removed between render and click → absent id → unchanged order pushed → no-op. Items added after render → the stale full list is pushed and the new ids keep relative order, going last — identical to the pre-existing `handleMove` behavior (same render-time `backlog` snapshot), so no regression.
- **Interaction with move buttons**: independent handlers, independent disabled gates; the send-to-top result (id at index 0) is exactly what N move-ups would produce, so the two controls can't fight.
- **No status gating** (plan decision #4): moving a done/in_flight item to the top is a display reorder only — `next_pending` (src/backlog.rs:522-527) skips non-pending items, so dispatch order of pending work is unaffected. Consistent with the existing move up/down buttons.

### 3. Backend contract — verified at source (no changes needed or made)

- `backlog_reorder` (src-tauri/src/ipc/backlog_cmds.rs:158-166) → `BacklogStore::reorder` (src/backlog.rs:314-332): listed ids placed first in that order, unlisted keep relative order and go last — exactly the semantics the full-order push relies on.
- `next_pending` (src/backlog.rs:522-527): first *live* pending item in display order → **index 0 of the rendered list = next dispatched**, confirming the feature's core claim ("making it the next dispatched pending item").
- Frontend wrapper `backlogReorder` exists at frontend/src/lib/tauri.ts:1547.

### 4. Tests

- `renderCard(text, index = 0, total = 1)` — defaults keep all four pre-existing markup tests byte-identical; only the new test passes non-defaults.
- `buttonMarkup(html, title)` — correct extraction for this markup: `lastIndexOf("<button", at)` finds the opening tag of the button owning the title; `indexOf("</button>", at)` finds its own close (children are a single SVG, no nested buttons); the `expect(at).toBeGreaterThanOrEqual(0)` guard fails clearly if the title is absent. No other title in the card contains `title="Send to top"` as a substring, so the match is unambiguous.
- The markup test asserts on the rendered **`disabled=""` attribute**, not the `disabled:opacity-30` Tailwind class (present on the button in both states) — the assertion is precise, and because `buttonMarkup` isolates one button, the `not.toContain('disabled=""')` mid-queue check can't be polluted by other buttons.
- `moveIdToFront` unit tests cover middle→front (order preserved), already-first, and absent id — the three behavioral edges.
- File already registered in `frontend/vitest.config.ts` include list (line 31) — no registration needed, no new test files. File now holds 5 + 4 + 3 = 12 tests, matching the reported run.
- Pre-review results (tsc clean, vitest 64 files, cargo test 2003 + 16, zero warnings) are consistent with everything inspected; no Rust code was touched, so the cargo run is a blanket-constitution confirmation as planned.

### 5. Constitution / project-specific checks

- **Doc comments**: new exported helper and the new handler both carry doc comments; style matches the file's existing comment voice (rationale + user-request provenance).
- **Code style**: identical button markup/handler patterns to the neighboring controls; no deviations.
- **Documentation sync**: README's Backlog-tab section (README.md:76) documents feature-level behavior and does not enumerate per-card controls — the existing move up/down, edit, copy, dispatch, retry, and remove buttons are not listed there either — so a per-card micro-control requires no README update. Module-level doc comments are in place.
- **Multi-platform neutrality**: pure React UI + lucide icon; no platform-specific code, paths, or shell syntax. Nothing to flag.

### Observations (non-findings)

1. `.coding/plans/037f2bdc.md`'s single-line diff ticks step 3 of the *prior* full-review-round plan (queue findings + summarize). That step was in fact completed (the 2027-01-06 review round's 18 queued findings are recorded in memory), so the tick is accurate bookkeeping that legitimately rides along in this commit — `.coding/` plan files travel with git by convention. No action needed.
2. The controls row now holds up to 8 icon buttons on fully-populated cards (retry/edit/copy/dispatch/send-to-top/up/down/remove). All share the compact `p-1` icon styling; visual density is a deliberate, consistent trade-off already present before this change.

**Conclusion**: the implementation matches the plan's four design decisions exactly, the tests pin the behavior at the right precision, and no correctness, bug, security, constitution, documentation, or platform finding exists. Ship it.
