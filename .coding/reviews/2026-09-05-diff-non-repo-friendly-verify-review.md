## Verdict: PASS

Verification re-review of commit `7cbb22b` ("feat(diff): friendly non-git error +
'Initialize Git Repository' button") on `feat/diff-non-repo-friendly`. This
follows up the prior review (`.coding/reviews/2026-09-05-diff-non-repo-friendly-review.md`,
FINDINGS 0 high / 1 low) which returned a single low finding (LOW-1: a stale
"not a git repository" panel with a re-enabled init button briefly reappearing
after a successful init). Both recommended fixes were applied and committed in
`7cbb22b`. This review confirms LOW-1 is resolved, introduces no regressions,
and that the prior review's 10 point-by-point checks still hold.

### Scope

`git diff HEAD` is effectively clean for source: the only uncommitted change is a
1-line bookkeeping edit to `.coding/plans/08a57024-….md` (flipping step 3's
checkbox `[ ]`→`[x]`) — no source impact. The feature and both fixes live in
commit `7cbb22b` (HEAD, confirmed via `git log`), so the review surface is
`git show 7cbb22b` (the cumulative diff against its parent), exactly as
instructed. The backend (`src-tauri/src/ipc/files.rs`, `src-tauri/src/main.rs`),
the `frontend/src/lib/tauri.ts` wrapper, and `DiffViewer.test.ts` portions of
that commit are byte-identical to what the prior review verified; only
`DiffViewer.tsx` carries the two fixed lines.

---

### LOW-1 fix verification

**Fix 1 — clear the stale error on init success.** `DiffViewer.tsx:275-281`.
`handleInitGit`'s `.then()` callback now reads:

```ts
.then(() => {
  // Clear the stale not-a-repo error so the re-fetch shows the loading
  // state (not the old error panel with a re-enabled button) during the
  // one IPC round-trip before the "no commits yet" result lands.
  setGitError(null);
  setRefreshKey((k) => k + 1);
})
```

`setGitError(null)` (line 279) precedes `setRefreshKey((k) => k + 1)` (line 280).
`gitError` is `{ scope: string; value: string } | null` (`:260`), so this clears
it outright (not just one scope's entry — there is only ever one live scope at a
time, so this is correct and complete).

End-to-end trace of the post-init transition, confirming the flash is gone:

1. **Pre-click** (not-a-repo panel visible): the prior fetch failed with
   `"not a git repository"`, so `gitError = { scope, "not a git repository" }`,
   `gitResult = null` (cleared by that fetch's `.catch` at `:349`),
   `gitLoading = false` (cleared by its `.finally` at `:352`), `initLoading = false`.
2. **Click** → `setInitLoading(true)`, `setInitError(null)` (`:272-273`) →
   button disabled, label "Initializing…".
3. **`gitInit()` resolves** → `.then` runs `setGitError(null)` + `setRefreshKey(k+1)`;
   `.finally` runs `setInitLoading(false)`. React 18 auto-batches promise/microtask
   state updates into a single render.
4. **At that batched render**: `gitErrorMsg = scopedResult(null, scopeKey) = null`
   (`:329`, `:165-170`) → the `if (gitErrorMsg !== null)` error branch (`:410`) is
   **skipped**. `gitDiff = scopedResult(null, scopeKey) = null` → `:448` skipped.
   `gitDiff === ""` is false → `:450` skipped. Falls through to the `else` arm
   (`:460-463`) → body = **"loading diff…"**, **not** the not-a-repo panel. ✓
5. **Effect re-runs** (`refreshKey` changed, in the dep array at `:357`):
   `setGitLoading(true)` (`:339`) → "loading diff…" persists.
6. **Re-fetch resolves** with `"no commits yet"` (the repo now exists, unborn
   HEAD) → `.catch` sets `gitError = { scope, "no commits yet" }` (`:348`),
   `.finally` `setGitLoading(false)` → `classifyGitDiffError("no commits yet")`
   (`:411`) → `"no-commits"` → the "No commits yet" panel (`:433-442`).

Net transition: **not-a-repo → "Initializing…" (disabled) → "loading diff…" →
"No commits yet"**. The stale not-a-repo panel with a re-enabled button no longer
reappears for even one frame. **LOW-1 resolved.** ✓

**Fix 2 — gate the init button on the git fetch loading too.** `DiffViewer.tsx:423`:

```tsx
disabled={initLoading || gitLoading}
```

Confirmed. The init button renders only inside the `errKind === "not-a-repo"`
branch (`:412-432`), which itself requires `gitErrorMsg !== null` (`:410`).
`gitErrorMsg` is set in the fetch effect's `.catch` (`:348`) and `gitLoading` is
cleared in the same fetch's `.finally` (`:352`) — so whenever the not-a-repo
panel is visible, `gitLoading` is already `false`. Therefore on the panel's
first render the button is enabled (`initLoading=false && gitLoading=false`) and
the user can click it. The `|| gitLoading` term only ever *widens* the disabled
condition (it can never re-enable a button that was disabled); it is a
belt-and-suspenders guard for the post-init re-fetch window — where fix 1 has
already cleared `gitError` to `null`, so the not-a-repo panel is not rendered at
all and the term is moot. No regression to the normal Refresh flow (a separate
button at `:399-407`, independently gated on `gitLoading`) or to the not-a-repo
panel's initial render. ✓

---

### No-regression check

- **Init still disabled during init** (`initLoading`): unchanged —
  `setInitLoading(true)` is still the first statement of `handleInitGit`
  (`:272`), flushed synchronously by React 18 before the next discrete event, so
  the button disables before a second click can register. The added `|| gitLoading`
  only makes this stricter. No double-fire. ✓
- **Refresh flow intact**: the Refresh button (`:399-407`) is a separate element
  with its own `disabled={gitLoading}`; the init button's gate is independent and
  does not touch it. ✓
- **Not-a-repo initial render**: on the first fetch's error, `gitLoading` is
  `false` (the fetch completed), so `initLoading || gitLoading` is `false` →
  button enabled. The user can init. ✓
- **No-commits / other branches**: untouched by the fix; `classifyGitDiffError`
  routing (`:411-447`) is unchanged. ✓

---

### Re-confirmation of the prior review's 10 point-by-point checks

The backend, `tauri.ts`, and test portions of `git show 7cbb22b` are identical to
what the prior review verified (the user confirms no other files changed since;
the commit diff confirms the code matches the prior review's line citations).
Checks 1, 2, 5, 6, 7, 8, 9, 10 are carried forward unchanged — all still PASS.
Checks 3 and 4 touch the handler/button, which changed by exactly the two fixed
lines above; both remain valid:

1. **Error canonicalization correctness — PASS (carried forward).** Detection
   runs only in the `Ok(o) =>` failure arm of `git_diff_head_at` (success arm
   returns `cap_diff(&stdout)` before any text inspection), so a real diff whose
   *content* contains "not a git repository"/"bad revision" travels the success
   arm and is never misclassified. Combined lowercased `stderr+stdout` check is
   defensive. Unchanged.
2. **`git_init_at` hardening parity — PASS (carried forward).** Mirrors
   `git_diff_head_at` exactly: `current_dir(root)`, `env_remove` of
   `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE`, `#[cfg(windows)]`
   `CREATE_NO_WINDOW`; `git_init` command clones root (lock dropped pre-spawn),
   `spawn_blocking`, two-stage `map_err`. Unchanged.
3. **Init button double-fire — PASS (re-verified).** `setInitLoading(true)` is
   still the synchronous first statement (`:272`); React 18 flushes discrete-event
   updates before the next event. `disabled={initLoading || gitLoading}` (`:423`)
   only widens the disabled condition. No double-fire.
4. **refreshKey re-fetch after init — PASS (re-verified, low finding now resolved).**
   `refreshKey` still in the dep array (`:357`); `setRefreshKey` still bumped in
   `.then` (`:280`); `scopeKey` (`:327`) stable across an init; transition
   not-a-repo → no-commits pinned end-to-end by `git_init_at_creates_repository`.
   Fix 1 makes the transition clean (no stale-error flash).
5. **`classifyGitDiffError` purity + coverage — PASS (carried forward).** Pure,
   deterministic; all branches tested (6 tests). Unchanged.
6. **Doc comments — PASS (carried forward).** `git_init_at`, `git_init`,
   `gitInit`, `classifyGitDiffError` documented; `gitDiffHead` doc updated.
   Unchanged.
7. **No `#[allow(...)`; warning-free — PASS (carried forward).** No `#[allow]`;
   `git_init_at` exercised by tests, `git_init` registered in `main.rs`.
   Unchanged.
8. **Multi-platform neutrality — PASS (carried forward).** Only platform-specific
   code is the `#[cfg(windows)]` `CREATE_NO_WINDOW` block mirroring the existing
   pattern; frontend is pure TS/CSS. Unchanged.
9. **Documentation sync — PASS (carried forward).** UX refinement of an existing
   feature; no README/PLAN.md edit required; new pub items documented. Unchanged.
10. **Security — PASS (carried forward).** `git init` at the sandbox-validated
    project root; no user-supplied args (`cmd.arg("init")` only); `env_remove`
    prevents inherited-env redirect; idempotent. Unchanged.

---

### Summary

LOW-1 is fully resolved by the two applied fixes, verified by an end-to-end
state trace: after a successful init the stale `gitError` is cleared to `null`
before the refresh-key bump, so the render falls through to the "loading diff…"
state instead of re-rendering the not-a-repo panel, and the init button is
additionally gated on `gitLoading` so it cannot be clicked during any in-flight
diff. No regressions: the button remains disabled during init, the Refresh flow
and the not-a-repo panel's initial render are unaffected, and the no-commits /
other error branches are untouched. All 10 prior point-by-point checks still
hold (8 carried forward unchanged; 2 re-verified against the fixed lines). No
new findings.
