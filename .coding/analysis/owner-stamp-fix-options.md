# Owner-stamp fix options — one machine owner id across app-data views

Step 2 of the owner-stamp divergence research. Step 1's finding
(`.coding/analysis/owner-stamp-divergence.md`, same branch) established the root
cause: MSIX AppData virtualization gives every process under the Claude Desktop
package a private copy of `AppData\Roaming\com.mnemo.mnemo2`, so the
inside-boundary and outside-boundary views of the owner-id file are two different
files holding two different ids, and project stamps written by one view are
refused by the other. This note compares the fix options and recommends one.
Documentation only — no production code was changed by this step.

## The invariant any fix must keep

`open_state_dir` (src-tauri/src/mnemo2/project.rs:315) refuses a `.coding/mnemo2`
folder that holds any file other than `.gitignore` and is not stamped with this
machine's owner id (error at project.rs:343-346; regression test
`shipped_or_foreign_state_is_refused_never_loaded` at project.rs:866). The stamp
exists so a folder that arrived with a cloned repository or archive is never
resumed as the lead's own history — a planted `lead.json` would inject a
conversation, a planted `team.json` would steer worktree cleanup (test comment,
project.rs:861-864). Every option below is evaluated first against: **a folder
shipped in a clone is still refused, and a normal restart never deletes or
rewrites a stamp.**

## Where the id lives today

- `OWNER_ID_FILE = "state-owner-id"` (project.rs:232), created once by
  `machine_owner_id(app_data)` (project.rs:243-270): atomic `create_new`, and an
  existing-but-invalid file is a hard error — it can never silently regenerate.
- The app-data dir comes from Tauri: `state_owner(app)`
  (src-tauri/src/mnemo2/ipc.rs:272-278) resolves `app.path().app_data_dir()` —
  exactly the path MSIX virtualizes (finding E4). Every caller funnels through
  `state_owner` (ipc.rs:422, 503, 566, 1145, 1162; shutdown at
  src-tauri/src/mnemo2/mod.rs:713), so the fix surface is one function plus the
  storage location.
- The config dir `~/.mnemo` (or `$MNEMO_HOME`) resolves from the user-profile
  home, NOT from AppData: `config_dir_named` uses
  `directories::BaseDirs::home_dir()` with `$HOME`/`$USERPROFILE` fallbacks
  (src/config/mod.rs:721-733), reached via `global_config_dir()` (:739) and
  `resolve_config_dir()` (:749, the resolver the engine and mnemo2-mcp share).

**Is `~/.mnemo` outside the virtualization?** Yes, with high confidence, on
three grounds:

1. **Code**: it is derived from the profile root (`home_dir()`), not from
   `AppData\Roaming`. MSIX AppData virtualization redirects only `AppData\Local`
   and `AppData\Roaming` (documented Windows behavior; the finding's E4 shows
   the redirect hitting exactly the Roaming path).
2. **Evidence**: finding E9 — `~/.mnemo/laya/server-57164.log`, created
   11-05 15:04:19 by the *outside*-boundary instance, is readable from *inside*
   the boundary (the investigation's own shell, which runs under the Claude
   package identity, E3). If `~/.mnemo` were virtualized, the inside view would
   be a private copy seeded at the first inside-boundary write (11-05 07:19,
   when the plugin chain first ran) and could not contain a file the outside
   instance created at 15:04.
3. **Architecture**: the plugin chain already depends on `~/.mnemo` being
   shared — `mnemo2-mcp.exe` itself lives at
   `C:\Users\hessc\.mnemo\bin\mnemo2-mcp.exe` and is executed by the
   inside-boundary chain (finding E3), and `projects.toml`, run files and laya
   logs there are demonstrably written by both views (E9).

**Not verified**: a direct outside-boundary probe reading a file written by an
inside-boundary process under `~/.mnemo` was not run (the finding's outside
probes targeted only the app-data store). The code path plus the observed
outside→inside visibility make the shared-location conclusion safe to build on;
the follow-up plan should include that probe as a smoke check.

## Options

### (a) Keep the id in app-data; make the refusal name both ids and the exact file

Change only the error string at project.rs:343-346 to include the folder's stamp
value (when a stamp file exists), this instance's machine id, and both file
paths (`.coding/mnemo2/owner` and the app-data `state-owner-id`).

- **User sees**: the same refusal, but actionable — "expected `3d68…`, found
  `b7bc…`; the stamp is `<path>\owner`, this machine's id file is `<path>`".
- **Cloned repo**: still refused — the logic is unchanged, only the message;
  the existing test's substring assertions keep holding.
- **Divergent-user recovery**: no better than today. The user must still
  hand-edit the stamp (now at least knowing exactly what to write) or rename
  the folder and lose the session history. The flip-flop persists: each view
  alternately refuses the other's stamps on every launch.
- **Inside vs outside the package**: both keep their own ids; nothing
  converges.
- **Risks**: none to security; but it institutionalizes hand-editing as the
  recovery path and leaves the cause untouched.

**Rejected as the fix** — it documents the symptom. Worth folding into whichever
fix is chosen, because a refusal that names both ids and files is strictly
better than today's misleading "it may have come with the repository".

### (b) An explicit user-confirmed "adopt this folder" action

Offer a dialog when the folder holds Mnemo2's own state files; on confirm,
rewrite the stamp to the current machine id.

- **User sees**: "This folder holds Mnemo2 state stamped by another machine id
  (`b7bc…`). Adopt it?" → one click, the folder is trusted.
- **Cloned repo**: **not reliably refused anymore.** A clone ships exactly
  "Mnemo2's own state files" — that is the threat model (project.rs:861-864).
  The gate "offered only when the folder holds Mnemo2's own state files"
  cannot distinguish my-other-view state from a clone's shipped state, and
  neither can the user — the incident message itself ("it may have come with
  the repository") shows the tool cannot tell. One click resumes untrusted
  transcript and worker history, the exact injection the stamp exists to
  prevent.
- **Divergent-user recovery**: yes, no hand-editing — but the user must re-adopt
  on every view switch, because each adoption re-stamps to the current view's
  id and the other view then refuses again (ping-pong).
- **Inside vs outside the package**: works in both, and ping-pongs in both.
- **Risks**: converts a security refusal into a confirmation dialog; a clone's
  payload becomes one click away.

**Rejected** — it weakens the invariant to provide a recovery convenience that
option (c) makes unnecessary. (If ever wanted as a last-resort escape hatch it
needs a stronger signal than "holds state files"; out of scope here.)

### (c) Store the machine owner id where both views share it (`~/.mnemo`), with a one-time migration

Move the id file from the Tauri app-data dir to the config dir
(`resolve_config_dir(None)` → `$MNEMO_HOME` or `~/.mnemo`), migrating any
existing app-data id once, first-wins.

- **User sees**: nothing new in the normal flow. Both views resolve the same
  config dir — which MSIX does not virtualize (above) — so both compute the
  same owner id and stamps can never diverge again.
- **Cloned repo**: still refused. The stamp check is unchanged, and a clone
  cannot ship `~/.mnemo/state-owner-id`: it is per-user-profile, per-machine,
  outside the repository. `shipped_or_foreign_state_is_refused_never_loaded`
  passes unchanged.
- **Divergent-user recovery**: the migration converges the machine id on first
  run (first-wins). Projects stamped with the id that loses are still refused
  exactly once more; for those, the supported no-hand-edit recovery is the one
  the error already names — rename `.coding/mnemo2` aside (history preserved
  on disk under the renamed name, session starts fresh). Folding (a)'s message
  improvement in makes that one-time nature clear. On the incident machine
  this is moot: both stamps and both app-data views were already hand-converged
  to the same id, so the migration picks it up and everything simply works.
- **Inside vs outside the package**: identical — both resolve the same
  `~/.mnemo`; no per-view behavior remains.
- **Risks**: (i) a user who launches with different `MNEMO_HOME` values
  reintroduces divergence by explicit choice — acceptable, same as any config;
  document it. (ii) The untested direction of the non-virtualization claim —
  mitigated by the smoke probe named below. (iii) The migration must never
  overwrite an existing config-dir id (first-wins, `create_new` semantics, same
  discipline as `machine_owner_id` today, project.rs:257-267). (iv) The app
  resolves config via `global_config_dir()` in places (e.g. src/main.rs:43),
  which ignores `MNEMO_HOME`; the fix must use `resolve_config_dir(None)` so
  the app, the engine and mnemo2-mcp agree on one directory.

### (d) A second stamp recording a stable project id

`.coding/mnemo2` would also record a project-unique id; the machine keeps a
registry of ids it created; trust = stamp match OR registry hit, so a missing
or regenerated machine id does not orphan the folder.

- **Cloned repo**: still refused — the clone's project id is not in the machine
  registry. The invariant holds.
- **But**: the registry must live somewhere. In app-data it has exactly the
  same two-view divergence (the inside registry lacks the outside view's
  projects → the same refusal). In `~/.mnemo` it works — but then it is option
  (c) plus a second id, a registry file, and a two-branch trust rule. It adds a
  real new corruption/sync surface to defend against a problem today's code
  already refuses to let happen silently (project.rs:243-270: an
  existing-but-invalid id file is an error, never regenerated).
- **Divergent-user recovery**: same as (c) if the registry is in `~/.mnemo`;
  no better.

**Rejected** — strictly more machinery than (c) for no additional protection
against the actual cause.

### (e) Other options considered and rejected

- **(e1) Detect the MSIX packaged context** (e.g. `GetCurrentPackageFullName`)
  and only then use a non-virtualized location: works, but gates on detecting a
  host we cannot enumerate — Claude Desktop today, any other MSIX host
  (Windows Terminal, …) tomorrow. (c) is unconditional and simpler.
- **(e2) Probe for the package LocalCache shadow and merge ids**: hardcodes a
  package-family path (`Claude_pzs8sxrjxfjjc`), fragile and undiscoverable in
  general.
- **(e3) Do nothing / document the workaround**: leaves every plugin user one
  outside-boundary launch away from losing tool access, with a message that
  actively misleads ("it may have come with the repository" — it did not).

## Recommendation

**Option (c)** — move the machine owner id to the config dir (`~/.mnemo` /
`$MNEMO_HOME`) with a one-time, first-wins migration from the app-data file —
**with (a)'s message improvement folded in as an adjunct.**

(c) removes the cause: one id file in one location both views genuinely share,
proven by the existing architecture — the MCP binary, `projects.toml`, run
files and laya logs already live in `~/.mnemo` and are demonstrably written and
read by both views (finding E3, E9). (a) alone leaves the flip-flop and keeps
hand-editing as the only recovery. (b) breaks the cloned-repository invariant
for a convenience (c) makes unnecessary. (d) reduces to (c) plus complexity.
(e1)/(e2) are detection heuristics for what (c) fixes unconditionally.

## Recommended change (for the follow-up bug_fixing plan)

1. `src-tauri/src/mnemo2/project.rs`
   - Keep `machine_owner_id(dir: &Path) -> Result<String, String>` unchanged —
     it is location-agnostic.
   - Update `OWNER_ID_FILE`'s doc comment (project.rs:228-232) to name the
     config dir as its home and the app-data file as the legacy location.
   - Add
     `pub fn machine_owner_id_shared(config_dir: &Path, legacy_app_data: &Path) -> Result<String, String>`:
     if `config_dir/state-owner-id` is absent and
     `legacy_app_data/state-owner-id` is a valid 32-hex id, copy it in with
     `create_new` (first-wins, never overwrite); then `machine_owner_id(config_dir)`.
2. `src-tauri/src/mnemo2/ipc.rs::state_owner` (line 272): resolve
   `mnemo::config::resolve_config_dir(None)` (honors `MNEMO_HOME`; agrees with
   the engine and mnemo2-mcp) and the legacy Tauri `app_data_dir`, and call
   `machine_owner_id_shared`. All other callers go through `state_owner`, so
   nothing else changes (including the shutdown path, mod.rs:713).
3. Refusal message (project.rs:343-346): include the folder's stamp value (when
   a stamp file exists), this machine's id, and both file paths — keeping the
   leading sentence and the "Delete or rename" guidance verbatim so the existing
   test's substring assertions (project.rs:878, 882) hold.

**Migration rule**: one-time, lazy, first-wins. The first post-fix instance
copies its view's valid legacy id into the config dir; every later instance
(any view) finds the config-dir id present and uses it. Never writes over an
existing config-dir id; never touches project stamps; a restart therefore
never deletes or rewrites a stamp.

## Regression tests (red today, green after)

- **`two_app_data_views_converge_on_one_shared_owner_id`** — file
  `src-tauri/src/mnemo2/project.rs` (tests module, beside
  `the_machine_owner_id_is_created_once_and_reused`, :925). Setup, all temp
  dirs: `config` (empty), `view_a` and `view_b` app-data dirs, project root
  `P`. Instance A: `machine_owner_id_shared(config, view_a)` → id X;
  `open_state_dir(P, X, true)` stamps P. Instance B (the other view):
  `machine_owner_id_shared(config, view_b)` must return X (config-dir id
  present; view_b's own legacy id ignored), and `open_state_dir(P, X, false)`
  must return `Some(dir)`. **Red today**: with the current code the second
  instance resolves its owner from `view_b` alone (`machine_owner_id(view_b)`
  → Y ≠ X), so `open_state_dir(P, Y, false)` errors "was not created by
  Mnemo2 on this machine" — the exact incident reproduced with temp dirs, no
  MSIX needed. Green after: both resolve through `config` and B trusts A's
  stamp.
- **`the_legacy_app_data_owner_id_migrates_once_into_the_config_dir`** — same
  file: config empty, legacy holds a valid id → first call returns it and
  writes `config/state-owner-id`; a second call with a *different* legacy id
  returns the first (no flip-flop); a config dir that already holds an id is
  never overwritten.
- **Message test** — extend the assertions in/near
  `shipped_or_foreign_state_is_refused_never_loaded` (project.rs:866) that the
  refusal names both ids and both file paths when a foreign stamp exists.
- **Smoke probe (manual, once)** — from outside any MSIX package, read a file
  the inside-boundary instance wrote under `~/.mnemo` (e.g. the current run
  file), closing the untested direction above.

## What was not verified

- The outside→inside direction of `~/.mnemo` sharing (probe listed above).
- Whether any MSIX host other than Claude Desktop packages Mnemo2 today (none
  found on this machine, finding E8).
- How the frontend renders the refusal string (the message is built in
  `open_state_dir`; the UI path was not audited — the incident shows it reaches
  the plugin verbatim, finding E9).
