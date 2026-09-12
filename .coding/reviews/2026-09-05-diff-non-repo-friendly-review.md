## Verdict: FINDINGS (0 high, 1 low)

Reviewed ALL uncommitted changes on `feat/diff-non-repo-friendly` (`git diff HEAD`):
5 tracked files (`src-tauri/src/ipc/files.rs`, `src-tauri/src/main.rs`,
`frontend/src/lib/tauri.ts`, `frontend/src/components/views/DiffViewer.tsx`,
`frontend/src/components/views/DiffViewer.test.ts`) + 1 untracked plan
(`.coding/plans/08a57024-….md`). The change canonicalizes the Diff tab's
not-a-repo / no-commits git failures into short stable strings and adds an
in-place "Initialize Git Repository" button. It is correct, secure,
well-tested, and constitution-compliant. One low-severity UX finding below.

---

### Point-by-point verification

**1. Error canonicalization correctness — PASS.**
`src-tauri/src/ipc/files.rs:883-907`. The detection runs ONLY in the `Ok(o) =>`
FAILURE arm — i.e. when `o.status.success()` is `false` (the success arm at
`:884-886` returns `cap_diff(&stdout)` and never inspects the text). `git diff
HEAD` exits 0 for both "has changes" and "no changes", so a non-zero exit always
means an error; a real diff's stdout is never scanned. Therefore a diff whose
*content* happens to contain "not a git repository" / "bad revision" cannot be
misclassified — it travels the success arm. The combined `stderr+stdout`
lowercased check (`:890`) is defensive (git writes errors to stderr, but
checking both costs nothing and tolerates odd builds). Confirmed safe.

**2. `git_init_at` hardening parity — PASS.**
`git_init_at` (`files.rs:951-984`) mirrors `git_diff_head_at`
(`files.rs:861-910`) exactly: `current_dir(root)` (`:954`), `env_remove` of
all three `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` (`:955-957`), and
`CREATE_NO_WINDOW` under `#[cfg(windows)]` (`:961-966`). The `git_init` Tauri
command (`files.rs:994-1002`) is structurally identical to `git_diff_head`
(`files.rs:922-941`): clones `root` from the project lock (lock dropped at end
of statement, before spawn), `spawn_blocking`, two-stage `map_err` (JoinError →
String → IpcError, then inner String → IpcError). The only intentional
difference is `git_init` takes no `path` (no sandbox validation needed — it
operates on the project root only). Parity confirmed.

**3. Init button double-fire — PASS.**
`DiffViewer.tsx:416-423`. The button is `disabled={initLoading}` (`:419`) and
`handleInitGit` (`:271-284`) calls `setInitLoading(true)` synchronously as its
first statement. React 18 flushes state updates from discrete events (click)
synchronously before the browser dispatches the next event, so the button is
disabled before a second click can register. The promise chain is correct:
`setInitLoading(true)` → `gitInit()` → `.then` bumps `refreshKey` / `.catch`
sets `initError` / `.finally` clears `initLoading`. No double-fire.

**4. refreshKey re-fetch after init — PASS (with the low finding below).**
`refreshKey` is in the fetch effect's dep array (`DiffViewer.tsx:353`), so the
post-init `setRefreshKey((k) => k + 1)` (`:276`) re-runs the effect. `scopeKey`
(`:323` = `gitScopePath ?? "__tree__"`) is stable across an init (it depends on
`allScope`/`selectedDiffPath`/`fallbackEntry`, none of which change), so the new
"no commits yet" error lands for the right scope and `scopedResult`
(`:325`) surfaces it. The transition not-a-repo → (init) → no-commits is correct
and is pinned end-to-end by the `git_init_at_creates_repository` test
(`files.rs:304-319`).

**5. `classifyGitDiffError` purity + coverage — PASS.**
`DiffViewer.tsx:222-235`. Pure: no side effects, no external state,
deterministic (lowercases input, substring checks). All branches tested in
`DiffViewer.test.ts:101-125`: not-a-repo, no-commits (via "no commits yet",
"bad revision", "unknown revision"), other, and empty string (→ "other").
6 tests, all branches covered.

**6. Doc comments — PASS.** Every new pub item is documented:
`git_init_at` (`files.rs:943-950`), `git_init` command (`files.rs:986-993`),
`gitInit` (`tauri.ts:1281-1286`), `classifyGitDiffError` (`DiffViewer.tsx:218-221`).
The `gitDiffHead` doc comment was also updated to describe the canonical
messages (`tauri.ts:1267-1276`).

**7. No `#[allow(...)`; warning-free — PASS.** No `#[allow(...)]` anywhere in
the diff. `git_init_at` is exercised by tests (imported at `files.rs:160`) and
`git_init` is registered in `main.rs:714` — no dead code. `#![deny(warnings)]`
is in effect at both crate roots; the task confirms `cargo test` passed green
in both the root lib and src-tauri.

**8. Multi-platform neutrality — PASS.** The only platform-specific code is the
`#[cfg(windows)]` `CREATE_NO_WINDOW` block (`files.rs:961-966`), which exactly
mirrors the pre-existing pattern in `git_diff_head_at` / `read_git_branch` /
`git_stdout`. It is a cross-platform feature with a Windows-only implementation
detail, gated by `cfg(windows)` and absent (no-op) on macOS — not a
Windows-only addition. The frontend changes are pure TS/CSS with no platform
assumptions.

**9. Documentation sync — PASS (no update required).** This is a UX refinement
of an existing feature (the Diff tab's vs-Git mode), not a new feature.
README.md mentions the Diff viewer / `git` tool only generically (lines 66,
69-72, 528); PLAN.md likewise (line 528). Neither documents the vs-Git error
UX, so no README/PLAN.md edit is needed. The new functions carry thorough doc
comments (point 6).

**10. Security — PASS.** `git_init` runs `git init` at the project root — the
already-open, sandbox-validated project (`files.rs:996`). No user-supplied args
reach the subprocess (`cmd.arg("init")` only, `files.rs:953`), so there is no
path/flag injection surface. `env_remove` of `GIT_DIR`/`GIT_WORK_TREE`/
`GIT_INDEX_FILE` (`:955-957`) prevents an inherited env override from
redirecting the init at a different directory. `git init` is idempotent (a
re-run on an existing repo is a no-op), matching the doc comment. No escalation.

---

### Findings

**LOW-1 — Stale "not a git repository" panel (with re-enabled init button) briefly reappears after a successful init.**
`frontend/src/components/views/DiffViewer.tsx:271-284` (handler) and
`:406-423` (render).

After `gitInit()` resolves, `handleInitGit` bumps `refreshKey` (`:276`), which
re-runs the fetch effect (`:332-353`). The effect sets `gitLoading(true)` and
calls `gitDiffHead`, but it does NOT clear the stale `gitError` first — and the
render branch checks `gitErrorMsg !== null` (`:406`) before considering
`gitLoading`. So for the duration of one IPC round-trip (the re-fetch), the
body re-renders the **not-a-repo** panel with the init button **re-enabled**
(`initLoading` is already `false` from the `.finally` at `:281-283`; the button
is gated only on `initLoading`, `:419`, not `gitLoading`). Only when the
re-fetch resolves with `"no commits yet"` does the panel switch to the
no-commits view.

Impact: a user who just clicked "Initialize Git Repository" sees the same
"not a git repository" panel flash back (with a clickable button) for a
moment before the "No commits yet" panel appears — momentarily confusing, and
a click during the flash triggers a redundant idempotent `git init` + a second
`git diff HEAD` round-trip. Harmless (init is idempotent) and pre-existing in
shape (the effect never cleared stale errors before this change either), but
this change makes it more prominent (a friendly panel with a button vs. the
old red text line).

Recommended fix (either or both):
- Clear the stale error on init success, right before bumping the refresh key,
  so the re-fetch shows the loading state instead of the old error:
  ```ts
  .then(() => {
    setGitError(null);
    setRefreshKey((k) => k + 1);
  })
  ```
- Gate the init button on the git fetch loading too, so it can't be clicked
  during any in-flight diff: `disabled={initLoading || gitLoading}` (`:419`).

Neither is blocking — the behavior is correct and harmless — but either makes
the post-init transition clean.

---

### Summary
The implementation is sound: canonicalization is correctly scoped to the
failure branch (no real-diff misclassification), `git_init_at`/`git_init`
achieve exact hardening parity with the diff path, the init button can't
double-fire, the post-init re-fetch lands the no-commits panel for the right
scope, `classifyGitDiffError` is pure and fully tested, all new pub items are
documented, the build is warning-free, the change is multi-platform neutral,
no docs need updating, and there is no security surface. The single low
finding is a cosmetic stale-error flash during the post-init re-fetch with a
trivial one-line fix.
