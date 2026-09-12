# Review — `feat/git-graph-base-expand-live` (uncommitted changes)

Scope: ALL uncommitted changes vs HEAD — `frontend/src/components/views/gitLanes.ts`,
`gitLanes.test.ts`, `GitView.tsx`, `src/runtime/agent.rs`, plus `.coding/` bookkeeping.
Backlog ask: base indication, main-spine collapse to ~20 with expand, live refresh on
agent git mutations, solid tip dots. All frontend-only except the agent.rs test fix.

Verified integration assumptions (read from source, not taken on faith):
- Transcript tool entries are `{kind:"tool", name, calls:[{id,index,args:string,result:{success,output}|null}]}`,
  merged per tool card, capped per agent (`agentEventReducer.ts:315-345`, `useAgentStore.ts:178`,
  `types.ts:227`) — matches `countGitMutations`' structural type.
- The `git` tool's real schema is `{"subcommand": ...}` with exactly the subcommands
  supported/filtered here (`src/tool/agent/git.rs:28-176`); the `shell` tool's param is
  `command` (`src/tool/agent/shell.rs:28,112`); `create_plan` takes `branch`.
- `main_spine` = `rev-list --first-parent main -n 400` → newest-first (`src-tauri/src/ipc/files.rs:925-945`);
  JS Set iteration = insertion order, and GitView builds the Set from that array — the
  newest-first ranking premise holds.
- The agent.rs test uses real-time `#[tokio::test]` (not paused), so the `Instant` deadline +
  25ms poll loop terminates; the let-else + refetch+panic path is sound (build green under
  `deny(warnings)`).

## Findings

### 1. CORRECTNESS (medium) — `computeBranchBases` misplaces the base for branches whose tip is on main's spine
`frontend/src/components/views/gitLanes.ts:175` — the BFS skips the tip itself
(`if (sha !== b.tip_sha && mainSpine.has(sha))`). For a normal branch the tip is never on
the spine, so the guard is a no-op — but for a branch whose tip IS a spine commit it
misfires, and that state is common and explicitly in scope: a freshly forked branch before
its first commit (exactly what `create_plan`'s auto-fork produces — the design note says
"the graph should show the new branch immediately"), a stale never-committed branch, or a
branch pinned to an older main commit. In all of these the merge-base/fork point is the tip
itself, but the function returns the tip's nearest spine *ancestor* — one commit too early.
Consequences: the "⟋ branch" pill renders on the wrong row and the branch list shows the
wrong "off <short_sha>".
Fix: drop the `sha !== b.tip_sha` guard (check the tip first). Add a regression test:
branch with `tip_sha` equal to a spine commit → base == tip (fails today, passes after).

### 2. BUGS (low) — `hiddenSpineCount` counts spine commits that were never loaded (independent 400-caps)
`frontend/src/components/views/gitLanes.ts:217-228` — rank/count is computed over the whole
`mainSpine` set, but `main_spine` (`rev-list -n 400`, main only) and `commits`
(`log --branches --topo-order -n 400`, all branches) are independently capped
(`src-tauri/src/ipc/files.rs:925-945`, pre-existing). In a >400-commit repo where side
commits occupy part of the log window, `spineSet` contains shas absent from `commits`;
`hiddenSpineCount` then counts commits that no expansion can ever reveal. Symptoms: the
expand row overpromises ("▸ show N more on main") and, once expanded (`hiddenSpineCount=0`),
older spine commits remain silently absent with no indicator.
Fix: count/rank only spine shas present in `commits` (e.g. filter `commits` by spine
membership first — that also removes the reliance on Set insertion order, see Info B).
Low severity: needs a >400-commit repo with open side branches; cosmetic misinformation,
no crash.

### 3. BUGS (low) — read-only `git branch` (action "list") counts as a mutation
`frontend/src/components/views/gitLanes.ts:266,280` — every successful `git` call with
subcommand `branch` counts. The backend itself classifies `{"subcommand":"branch","action":"list"}`
as read-only (`src/agent/approval.rs:417-422`), contradicting the doc claim that read-only
ops never count. Impact is benign (a spurious debounced refresh), but it contradicts the
stated contract; exclude `action === "list"` when subcommand is `branch`.

### 4. PERFORMANCE (low) — live-refresh selector is O(all transcripts) on EVERY store notify
`frontend/src/components/views/GitView.tsx:247-249` — the zustand selector re-runs on every
store `set()` (including per-chunk streaming batches and unrelated state changes) and
re-derives `countGitMutations` over every agent's full transcript — iterating up to
1000 entries/agent and `JSON.parse`-ing the args of every *historical* git/shell/create_plan
call each time. Bounded, but it is exactly the shape the app's active freeze/"slower over
time" investigation is about, and the cost grows with transcript length. Cheap fix: memoize
per-agent counts keyed by the transcript array reference (transcripts are immutably replaced
per agent, so unchanged agents are O(1) cache hits — see `useAgentStore.ts:938-961`), or
maintain the counter incrementally in the reducer when a `tool_result` lands.

### 5. TEST QUALITY (info) — the view-level halves of asks #3/#4 are unpinned
The pure halves (bases, collapse ranks, mutation counting) are well tested —
`spineShas.slice(0,20)` visible / `slice(20)` hidden pins the rank boundary exactly (an
off-by-one fails), and the counter table covers in-flight/failed/read/non-git cases. But
there is no component-test infra in the repo (no `*.test.tsx` anywhere; vitest is node-env),
and all of these live only in `GitView.tsx`: the solid-tip dot rendering (`GitView.tsx:439-457`)
— deleting the `isTip` branch would pass the full suite — the expand-row conditions/labels
(`481-504`), the base-pill rendering (`397-421`), and the debounce→refresh wiring
(`251-257`). Consistent with repo convention, so informational; if a follow-up ever extracts
these conditions (e.g. `hasExpandRow`, tip-vs-ring dot props) into `gitLanes.ts`, pin them there.

### Info (no action required)
- **A. Set-order reliance** (`gitLanes.ts:215-219`): ranking depends on the caller's Set
  iteration order being the backend's newest-first spine. Correct today (spec-guaranteed
  insertion order; both call sites build from the array) and documented in a comment, but
  the `ReadonlySet<string>` signature doesn't carry the contract. Deriving rank from the
  `commits` array (the module's stated newest-first topo contract) would be order-safe —
  subsumed by finding 2's fix.
- **B. selected-but-hidden commit**: after collapsing, a previously selected old spine
  commit still renders in the detail row (`GitView.tsx:259-262` searches the full list).
  Defensible (detail survives collapse), no crash; clearing `selected` when it leaves
  `view.visible` would be slightly cleaner UX.
- **C. stub/expand-row overlap**: collapsed-parent clipped stubs run to `svgH` and pass
  under the expand-row text — cosmetic only (connectors render first, text on top).
- **D. shell regex gaps are deliberate and safe**: quoted `-C "path with space"` and
  newline-separated `git` verbs on line 2+ aren't matched; both only delay a refresh to
  the next mutation/manual refresh, per the stated conservative design.

## Reviewed and clean
- **`computeBranchBases` BFS mechanics**: visited-set seeding/updates and the
  `parentsOf.has(p)` window guard are correct (no revisits, no ghosts, no infinite loop);
  branch-off-branch → nearest MAIN ancestor is the intended and tested semantics; beyond-cap
  branches get no entry (correct per design). Only the tip-exclusion (finding 1) is wrong.
- **`visibleCommits` filtering**: preserves input topo order by construction (pure filter);
  hidden parents are always strictly below their children (only oldest spine rows drop), so
  connector direction/stubs stay geometrically consistent; `hasExpandRow`/`svgH` row-budget
  math is consistent in all four expand/collapse × short/long-spine states.
- **`pill()` builder**: closure-mutated `pillX` inside `.map` is evaluated sequentially and
  correctly sizes `subjectX`; keys (`t-`/`b-` prefixes) cannot collide.
- **Lane layout untouched** — still computed over the FULL set (`GitView.tsx:174-177`), so
  lanes are stable across collapse/expand, as designed.
- **Live-refresh wiring**: primitive selector (no re-render loops), ref-seeded baseline
  (no spurious initial refresh), 600ms debounce with pre-clear, unmount cleanup clears the
  timer (`GitView.tsx:142-145`), and the shared `refreshTimer` with `handleMergeConfirm`
  is safe (both paths only schedule refreshes). Cancelled mid-flight calls (result stays
  null) correctly never count.
- **`isGitMutation` never throws from the selector**: all `JSON.parse`+property-access sits
  inside try/catch, so even non-object JSON (`"null"` → TypeError on `.subcommand`) is
  swallowed into the no-match path.
- **`agent.rs` test hardening** (`src/runtime/agent.rs:845-877`): correct bounded poll
  (25ms/5s, real clock), preserves assert-before-Cancel semantics and the diagnostic
  panic message, refetch+panic path typechecks, and the 5s bound only delays a *genuinely
  failing* test — acceptable. Test-only, justified, no product change.
- **Security**: no injection surface — model-controlled strings are only parsed and
  regex-matched, never executed or interpolated into commands; no new IPC, no secrets.
- **Constitution**: doc comments on every new exported symbol/interface field; no
  `#[allow]`; warning-free green `cargo test` (1127/0/1) and frontend 360/360 reported
  pre-review; the only backend-adjacent change is the test fix above.

## Verdict
No blocking defects in the collapse/expand geometry, live-refresh wiring, or the agent.rs
fix. Fix finding 1 (wrong base for tip-on-spine branches — precisely the fresh-fork state
this feature advertises) with its regression test; findings 2-4 are small, well-localized
follow-ups (2 and 3 can be one-line-ish changes in `gitLanes.ts`).
