# Review — Git tab: graphical history/branches + merge-to-main (all uncommitted changes)

Scope: `git diff HEAD` (agentState.ts, rightPanelViews.tsx, tauri.ts, vitest.config.ts, files.rs, main.rs) + untracked GitView.tsx, gitLanes.ts, gitLanes.test.ts. `.coding/plans/**` treated as expected bookkeeping noise per instructions. Verification matrix already green (root cargo test 1110, src-tauri cargo build, frontend npm test 294, npm run build).

## Bugs

### B1 (moderate) — `freeLane(from)` can return a lane BELOW `from`, breaking spine-lane-0 reservation — `frontend/src/components/views/gitLanes.ts:72-78`

```ts
const freeLane = (from: number): number => {
  for (let i = from; i < lanes.length; i++) {
    if (lanes[i] === null) return i;
  }
  lanes.push(null);
  return lanes.length - 1;   // ← when lanes.length < from, this is < from
};
```

When `lanes` is empty and `from === 1` (the `spineExists` case, both call sites: `sideClaim` :89 and step-1 :110-113), the loop is skipped, one `null` is pushed, and `lanes.length - 1 === 0` is returned — lane 0, the lane the module documents as "reserved for the spine when one exists" (and which every other code path carefully protects).

Trigger: the FIRST commit in `log --branches --topo-order` is not on main's first-parent spine — i.e. any feature branch with commits newer than main's tip (the common "open branch" state this very repo is usually in; with `--branches` head enumeration, `feat/*` heads are typically listed before `main`, so their exclusive commits lead the list). Trace, spine `{S,I}`, topo `[F2[F1], F1[S], S[I], I[]]` (branch ahead of main):

- F2: non-spine, no expectation → `freeLane(1)` on empty `lanes` → **lane 0** (should be ≥ 1)
- F1: its lane-0 expectation is blocked (spineExists) → `freeLane(1)` → lane 1
- S, I: spine → lane 0.

Result: the feature tip renders ON the spine lane (in the side-palette sky color via the negative-index guard, `laneColor(0,false)`) with its child kinking off to lane 1 — violating the pinned contract ("main's spine PINNED to lane 0") and the plan's core visual promise. No crash, purely visual/contract, but it fires in the feature's flagship scenario. None of the 9 tests catch it because every spine fixture starts its list with a spine commit (test 3's A, test 5's Q).

Fix:
```ts
while (lanes.length <= from) lanes.push(null);
return from;
```
and per the constitution's regression-test rule, add a fixture: spine exists + first commit non-spine (branch ahead of main) → assert that commit's lane ≥ 1, `laneCount === 2`, spine commits still lane 0.

## Minor

1. **main.rs:527** — `    ipc::files::git_history,` is indented 4 spaces; its siblings in the handler list use 12. Cosmetic inconsistency (rustfmt would reformat).
2. **files.rs:921** — the spine read hardcodes `-n "400"` as a literal while the commit log uses `&GIT_HISTORY_COMMIT_CAP.to_string()` (:875). If the cap is ever tuned the two silently diverge. Use the constant for both.
3. **GitView.tsx:131** — the 4s post-merge `window.setTimeout(() => void refresh(), 4000)` is neither tracked in `resultTimer` nor cleared on unmount (only the 6s result banner is, :114). Harmless in React 18 (setState on unmounted is a no-op, and the IPC call is read-only), but tracking it alongside `resultTimer` is cleaner hygiene.
4. **GitView.tsx:294-347 (cosmetic)** — the right-aligned date (`svgW-6`) can overlap dynamically-wide branch pills plus a 48-char subject on the same row, since `LABEL_W` is fixed at 320 while pill width scales with branch-name length. Polish only.
5. **files.rs:868-876 (trivial)** — `--date=unix` is redundant: `%at` always emits unix seconds regardless of `--date`. Harmless.
6. **Test-quality note — files.rs:298-309** (`git_history_tolerates_pipe_in_subject_and_missing_main`): the no-`main` assertions are conditional on `if !has_main`, so on machines with `init.defaultBranch=main` the empty-spine/no-merged-flags path is never exercised there. Not a defect (the first test exercises the has-main path explicitly); consider forcing a non-main default (e.g. `git branch -m trunk`) so the missing-main path always runs.

## Security — no findings

- `git_history` is strictly read-only (`for-each-ref` / `log` / `rev-list` / `branch --merged`); all argv entries are fixed strings — no path or user input reaches any git argument.
- Branch names flow only into the LLM prompt (`mergePrompt`), the same trust domain as the agent's own git reads; git refname rules forbid spaces/control chars, limiting prompt-shaping surface.
- `git_stdout` hardening exactly mirrors `read_git_branch` (:549) / `git_diff_head_at` (:647): `current_dir(root)`, `env_remove` of GIT_DIR/GIT_WORK_TREE/GIT_INDEX_FILE, `CREATE_NO_WINDOW` on Windows, stderr-first error reporting — plus it correctly avoids the pipe-exit-code trap by reading `status.success()` directly.
- The frontend merge gate (`activeAgent !== null && workflowState === "complete" || "planning"`, GitView.tsx:95-96) matches the StatusBar precedent (StatusBar.tsx:673; `workflowState` is null when there is no active agent, so the two are equivalent) — and the backend independently enforces skill availability in the current workflow state (agent.rs:621-626). Defense in depth confirmed.

## Constitution compliance — no findings

- Doc comments present on every new public Rust item (structs + all fields, `git_history`, const) and every TS export (`gitHistory`, the three interfaces, `computeLanes`/`laneColor`/`shortDate`, `mergePrompt`, `GitView`); the private `git_stdout`/`git_history_at` are documented too.
- No `#[allow(...)]` anywhere in the diff; no dead code (every new export is consumed — `mergePrompt` is used by `handleMergeConfirm`, not just exported).
- `// eslint-disable-next-line react-hooks/exhaustive-deps -- load once on mount` matches the house style (justified suffix as in EndpointCard.tsx:133, PlanProgress.tsx:64/79/88, StatsView.tsx:436).
- `gitLanes.test.ts` correctly registered in vitest.config.ts (colocated tests don't run otherwise).
- No unrelated changes; only the declared plan bookkeeping files besides the feature.

## Verified correct (traced, no action needed)

- **%x1f parsing robustness** — subjects containing `|`, `, `, `->` survive verbatim (pinned by test). Multi-line subjects cannot split a record: git's pretty formatter folds the subject paragraph onto one line for `%s` (joining with a space), so `.lines()` yields exactly one line per commit. `%D` decorations are `", "`-separated and refnames cannot contain spaces, so `split(", ")` cannot mis-split them; empty `%P` → empty vec via `split_whitespace`; empty `%D` → filtered empty `refs`; `%at` parse failure degrades to 0.
- **Missing main** — `has_main` gates both the spine read and the merged-flags read; spine stays empty, `merged_into_main` stays `false`; no error. Frontend handles the empty-spine case (lane 0 unreserved — pinned by the no-main test).
- **Async shape** — project root cloned (lock dropped) before `spawn_blocking`, matching `get_git_branch`'s F1 freeze-lesson pattern; `JoinError` mapped to `IpcError`.
- **Sort order** — main-first → current → name, correct `bool` `Ord` usage (`false < true` with reversed comparands).
- **Lane engine traces** — lane reuse after merge ✓ (H reuses F's freed lane), convergence duplicate-clearing ✓, `spineClaim`'s unconditional `lanes[0] = sha` overwrite is benign (spine commits never consult expectations and each re-claims its own spine parent when rendered), first-parent lane-keep correctly restricted to non-spine commits ✓ (test 3), unknown parents extend nothing but are preserved for stub connectors ✓ (test 5), `laneCount` min 1 ✓. The only hole found is B1.
- **`enter_skill` prompt plumbing** — agent.rs:627 `prompt.unwrap_or_else(|| spec.prompt.clone())` confirms the branch-specific prompt is honored; the wrapper's `prompt ?? null` (tauri.ts:1178) matches the backend's `Option<String>`.
- **`mergePrompt` fidelity** — every hard rule covered: checkout target branch first if needed (step 2), `--no-ff` (3), union conflict resolution / never `-X ours`/`-X theirs` (4), BOTH `cd frontend; npm run build` AND `cd src-tauri; cargo build` plus root `cargo test` + frontend `npm test` unpiped before cleanup (5), `git branch -d` only after green / never `-D` (6), no push + `skill_end` (7).
- **`MergeToMainDialog` reuse** — props (`open`/`sourceBranch`/`merging`/`onConfirm`/`onCancel`) match the component's interface exactly; transient-result + timer pattern mirrors StatusBar's.
- **Registry wiring** — union, `ALL_RIGHT_PANEL_TABS`, and `RIGHT_PANEL_VIEWS` updated together; rightPanelViews.test.ts enforces the sync (coverage, no extras, equal counts) and ran green; tsc build green confirms no missed exhaustive switches.
- **IPC field naming** — TS interfaces mirror the Rust serde field names verbatim (snake_case), which is what serde serializes over the bridge.

## Verdict

One moderate bug (B1 — fix `freeLane` + regression test) and a handful of minor/cosmetic items before commit; everything else is correct, consistent with existing patterns, and constitution-clean.
