# Why the project owner stamp diverged from the machine owner id

Research step 1 of plan a73885db. All machine-state evidence was collected read-only on
2026-11-06 between ~04:14 and ~04:30 UTC-local; nothing under `C:\mnemo\.coding\mnemo2`
or either app-data store was modified by this investigation. The only writes were three
probe output files under `C:\Users\hessc\AppData\Local\Temp` (owner-probe*.txt).

## Summary

The machine has **two live views of Mnemo2's app-data directory**, and they hold **two
different owner ids**. Every process in the Claude Desktop plugin chain — `claude.exe`
(the MSIX-packaged Claude Desktop app) → `mnemo2-mcp.exe serve` → `mnemo2.exe --hidden` —
runs inside the Claude Desktop package boundary, where Windows MSIX AppData virtualization
redirects `AppData\Roaming\com.mnemo.mnemo2` to a private per-package copy under
`AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\com.mnemo.mnemo2`. That
private copy was seeded with id `b7bc55c1…` at the first-ever Mnemo2 run (11-05 07:19:42,
inside the boundary). The first Mnemo2 run *outside* the boundary (11-05 ~13:01) saw an
empty real store — the morning run's writes had all been virtualized away — so
`machine_owner_id` created a second, different id `3d688e9f…` there, and that instance
stamped `C:\mnemo\.coding\mnemo2\owner` with it at 13:02:13. The hidden instance started
at 11-06 03:23 runs inside the boundary, read `b7bc…`, and refused the `3d68…` stamp.
**Confidence: high** — the real store's content was read directly from outside the
boundary and matches the original stamp exactly; no other writer or regeneration path
exists in the code.

## Evidence

Each item names the command or file read and quotes what it produced.

### E1. Code: the id and the stamp (src-tauri/src/mnemo2/project.rs)

- `OWNER_FILE = "owner"` (line 226), `OWNER_ID_FILE = "state-owner-id"` (line 232).
- `machine_owner_id` (line 243): reads `<app_data>/state-owner-id`; if the read succeeds
  and the content is 32 hex chars it is returned as-is. Only when the file is **absent**
  does it `create_dir_all` + `OpenOptions::create_new` a fresh `uuid::Uuid::new_v4()`
  (simple, no hyphens). If the file **exists but is invalid**, `create_new` fails with
  `AlreadyExists`, the re-read still fails the 32-hex filter, and the function returns
  `Err("…state-owner-id is unreadable…")` — it can never silently regenerate over an
  existing file.
- `open_state_dir` (line 315): on `create=true` it writes the stamp; at line 344 it
  returns the observed error — `"…was not created by Mnemo2 on this machine (it may have
  come with the repository)…Delete or rename that folder to start a fresh session here."`
  — when the stamp mismatches and the folder holds any file other than `.gitignore`.
- Tests confirm the intended behavior and that tests never touch real app data:
  `shipped_or_foreign_state_is_refused_never_loaded` (line 866, uses `tempdir()`),
  `the_machine_owner_id_is_created_once_and_reused` (line 925, uses `tempdir()`).

### E2. Code: where the app-data dir comes from (src-tauri/src/mnemo2/ipc.rs:272)

`state_owner(app)` resolves `app.path().app_data_dir()` through Tauri and passes it to
`machine_owner_id`. The identifier is `com.mnemo.mnemo2`, fixed in
`src-tauri/mnemo2-app/tauri.conf.json` (`"identifier": "com.mnemo.mnemo2"`) — the same for
dev and release builds. `--hidden`/`--project` parsing is `StartArgs`/`parse_args`
(src-tauri/src/mnemo2/mod.rs:151-175); it only affects window visibility and the startup
project, not path resolution.

### E3. The running hidden instance's process chain is inside the Claude Desktop MSIX package

`Get-CimInstance Win32_Process` ancestry walk from this worker's shell (2026-11-06):

```
74408    powershell.exe
74552    mnemo2.exe   "C:\mnemo\target\release\mnemo2.exe" --hidden --project C:\mnemo
27780    mnemo2-mcp.exe  "C:\Users\hessc\.mnemo\bin\mnemo2-mcp.exe" serve
19924    mnemo2-mcp.exe  C:/mnemo/mnemo2/plugin/bin/mnemo2-mcp serve
58244    claude.exe
34360    claude.exe   "C:\Program Files\WindowsApps\Claude_4.23118.0.0_x64__pzs8sxrjxfjjc\app\claude.exe"
6064     sihost.exe → svchost.exe → services.exe → wininit.exe
```

The chain passes through the MSIX-packaged Claude Desktop app
(`WindowsApps\Claude_…_pzs8sxrjxfjjc`), so every process below it — including the hidden
`mnemo2.exe` and this investigation's own shell — runs with the Claude package identity,
under which Windows applies MSIX AppData virtualization.

### E4. From inside the boundary, the "real" path IS the private store

`fsutil hardlink list` run from this worker's shell (inside the boundary):

```
=== A: real state-owner-id ===
\Users\hessc\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\com.mnemo.mnemo2\state-owner-id
=== B: private state-owner-id ===
\Users\hessc\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\com.mnemo.mnemo2\state-owner-id
```

Querying `C:\Users\hessc\AppData\Roaming\com.mnemo.mnemo2\state-owner-id` (A) resolves to
the **LocalCache file object** — the real path is redirected for processes with the
package identity. This also explains why the two paths earlier appeared to be
"byte-identical copies with identical timestamps": they are the same file object seen
through two paths. Corroborating detail: the running instance's WebView2 child was
launched with the plain string
`--user-data-dir=C:\Users\hessc\AppData\Local\com.mnemo.mnemo2\EBWebView`, yet its writes
land in the LocalCache — the redirection happens in the OS filesystem layer per process
identity, invisible to Mnemo2's code.

### E5. From outside the boundary, the real store holds the stamp's id

A probe process spawned via WMI `Win32_Process.Create` (hosted by the WmiPrvSE service —
not a descendant of the Claude package, so no virtualization) read the same path:

```
cmd.exe /c type "C:\Users\hessc\AppData\Roaming\com.mnemo.mnemo2\state-owner-id"
→ 3d688e9fa6e04281a772a41376afdbea
cmd.exe /c dir  "C:\Users\hessc\AppData\Roaming\com.mnemo.mnemo2"
→ 11/05/2026  01:01 PM    <DIR>          .
→ 11/05/2026  01:01 PM                32 state-owner-id
```

The **real** `state-owner-id` holds `3d688e9fa6e04281a772a41376afdbea` — exactly the id
the C:\mnemo stamp was created with — and was written at **13:01 PM on 11-05**, one minute
before the stamp. The same probe read the LocalCache copy: `b7bc55c152c542298dba58dc70877540`.

### E6. What each view's store looks like from inside (all one object per view)

`Get-ChildItem`/`Get-Item` from this shell (inside the boundary):

```
C:\Users\hessc\AppData\Roaming\com.mnemo.mnemo2\state-owner-id
    created=11/5/2026 7:19:42 AM  written=11/5/2026 7:19:42 AM  (content b7bc…)
C:\Users\hessc\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\com.mnemo.mnemo2\state-owner-id
    created=11/5/2026 7:19:42 AM  written=11/5/2026 7:19:42 AM  (content b7bc…)
C:\Users\hessc\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Local\com.mnemo.mnemo2
    created=11/5/2026 7:19:41 AM   (WebView2 profile of the inside-boundary instance)
```

The private store was seeded at the first-ever run, 07:19:41-42 on 11-05, and written
exactly once (created == written). The plan context's observation "the machine id file
held b7bc…, last written 07:19:42, six hours before the stamp" was made from inside the
boundary — it describes the **private** copy, not the real file (which was written at
13:01, one minute before the stamp; there was never a six-hour gap).

### E7. Project stamps on disk (read-only)

```
C:\lmv-editor\.coding\mnemo2\owner = b7bc55c152c542298dba58dc70877540
    created=11/5/2026 7:19:42 AM          (opened by the inside-boundary instance)
C:\mnemo\.coding\mnemo2\owner
    created=11/5/2026 1:02:13 PM          (originally 3d688e9f…)
    written=11/6/2026 3:27:00 AM          (the hand-applied workaround → b7bc…)
C:\AgenticCoding, C:\tmp\Wolfgang        (no owner stamp)
```

The original stamp value is preserved at
`…\Temp\claude\…\scratchpad\owner.bak` → read: `3d688e9fa6e04281a772a41376afdbea`.
`C:\mnemo\.coding\mnemo2` file times: `owner` created 13:02:13, `team.json` created
13:02:15, `lead.json` created 15:04:14 / written 18:23:20, `preferences.json` written
11-06 03:07:05.

### E8. No third id or third store exists

- `Get-ChildItem C:\Users\hessc\AppData -Recurse -Filter state-owner-id` (full depth)
  finds exactly the two `com.mnemo.mnemo2` paths above (which from inside are one
  object; from outside they are the two stores).
- `Get-ChildItem C:\Users -Recurse -Depth 6 -Filter state-owner-id`: only
  `C:\Users\hessc\AppData\Roaming\com.mnemo.mnemo2\state-owner-id`. Only user profiles
  `hessc` and `Public` exist; `C:\Windows\System32\config\systemprofile\AppData\Roaming\com.mnemo.mnemo2`
  does not exist.
- No `Roaming\com.mnemo.desktop` or `Roaming\com.mnemo.app` store exists (the older
  identifiers have only WebView2 leftovers under `AppData\Local`), so no other-identifier
  build ever created an owner id.
- Only the Claude package has a `com.mnemo.mnemo2` LocalCache entry among all MSIX
  packages (`Get-ChildItem C:\Users\hessc\AppData\Local\Packages -Recurse -Depth 4 -Filter "com.mnemo*"`).
- The string `3d688e9f…` appears in `C:\mnemo\.coding\mnemo2\team.json` only as part of
  this plan's own text embedded as a worker's `opening_prompt` (line 24760) — a false
  lead, not a usage of the id.

### E9. Instance history that survives

- `C:\Users\hessc\.mnemo\run\mnemo2-74552.json` (started_ms 1793953429195 = 11-06
  03:23:49) is the only run file — earlier ones are removed on exit, so the 07:19 and
  13:01 instances' run files are gone.
- `C:\Users\hessc\.mnemo\laya\server-57164.log` created 11-05 15:04:19 (the engine of
  the instance that opened C:\mnemo at 13:02); `server-74552.log` created 11-06 03:27:02
  (the post-workaround reopen by the hidden instance).
- `C:\Users\hessc\.mnemo\logs\mnemo2-ui.jsonl` (422 bytes): three toasts at 11-06
  03:23:50 — "Mnemo2 has no open lead; call lead_open first" — the visible symptom of
  the refused open; the owner-refusal text itself reached the plugin path.
- From outside the boundary, the real `AppData\Local\com.mnemo.mnemo2\EBWebView` was
  created 11-05 01:01 PM and last written 11-06 03:07 AM — an outside-boundary instance
  was also alive around 03:07 on 11-06 (it wrote `preferences.json` at 03:07:05), while
  the stamp still said `3d68…`, so it accepted the project then.

## Timeline (reconstructed)

1. **11-05 07:19:41-42** — first-ever Mnemo2 run on this machine, inside the Claude
   Desktop package boundary (plugin-started). `machine_owner_id` saw no id in its
   virtualized view → `create_new` → the write was redirected into the package LocalCache
   → private id `b7bc…`. It opened `C:\lmv-editor` and stamped it `b7bc…` (07:19:42).
2. **11-05 ~13:01 PM** — first Mnemo2 run *outside* the boundary (user-started; exact
   launcher not established). Its view of `AppData\Roaming\com.mnemo.mnemo2` was empty —
   the morning run's writes had all been virtualized into the LocalCache — so it created
   the real store and a second id `3d68…`.
3. **11-05 13:02:13** — that instance opened `C:\mnemo` and stamped
   `.coding/mnemo2/owner` = `3d68…`. Its session ran until ~18:23.
4. **11-06 03:23:48** — the Claude Desktop plugin chain started hidden instance 74552
   (inside the boundary). It read the private id `b7bc…` ≠ stamp `3d68…` → refusal; the
   plugin lost its tools.
5. **11-06 ~03:27** — workaround: the stamp was hand-edited to `b7bc…`; 74552 reopened
   successfully (laya log 03:27:02) and the plugin reconnected.

## Hypotheses

1. **"The user-started and hidden-started instances resolve different app-data dirs
   (environment, sandbox redirection, dev vs release identifier)" — CONFIRMED**, in the
   specific form of MSIX AppData virtualization under the Claude Desktop package: same
   path string, two backing stores, selected by process package identity (E3, E4, E5).
   It is not an environment variable and not an identifier difference — the identifier
   is `com.mnemo.mnemo2` for both, and the running inside-boundary instance's WebView2
   child was launched with the plain real path string (E4).
2. **"The id file was deleted and recreated by something and its mtime was not
   updated" — REJECTED.** The real file was created 11-05 13:01 and never rewritten
   (mtime = creation minute; content matches the 13:02:13 stamp, E5); the private file
   was created 07:19:42 and written exactly once (E6); no third id exists anywhere
   (E8). Nothing deleted or recreated either file.
3. **"machine_owner_id's 'not 32 hex chars' branch regenerated a valid id after a read
   failure" — REJECTED by code** (E1): an existing-but-invalid file makes `create_new`
   fail with `AlreadyExists` and the function return `Err`, never a fresh id; a
   regeneration only happens when the file is absent, and it writes a new file (which
   would set a fresh creation time). Both stores' files are valid 32-hex and were
   written exactly once.
4. **"The stamp was written by a different build or a test run" — REJECTED.** Tests use
   `tempdir()` (E1); no other identifier's store exists (E8); dev and release share the
   identifier (E2). The stamp was written by the same release binary
   (`C:\mnemo\target\release\mnemo2.exe`) running outside the package boundary.
5. **(Added) MSIX AppData virtualization created two divergent owner-id stores —
   CONFIRMED; this is the root cause** (E3-E7).

## What could not be established

- **How the 13:01 outside-boundary instance was launched** (Start Menu, double-click, or
  a plain-terminal Claude Code CLI): run files are removed on exit and only
  `mnemo2-74552.json` survives (E9). It was outside the boundary because it read the
  real store's `3d68…` — that much is certain.
- **Whether the 07:19:41 inside-boundary instance was auto-started by the Claude
  Desktop plugin or triggered by the user from within Claude Desktop**: either way it
  was inside the boundary — all of its writes (WebView2 profile, state-owner-id,
  lmv-editor stamp) landed in the LocalCache (E6, E7).
- **The exact MSIX filter mechanics** that seeded the private copy. An independent
  redirected `create_new` is the only consistent explanation — the private file's
  content differs from the real file's, so it is not a copy-on-write of it — but the
  filter's internal ordering was not observable from user mode.
- **What the outside-boundary instance alive at 03:07 on 11-06 did** (E9): it accepted
  the project (the stamp still said `3d68…`); whether the user was already
  investigating is not recoverable from disk.

## Open questions for the owner

- **The workaround fixed only one side.** The real store still holds `3d68…` (E5), so
  the next *outside-boundary* start (double-click, Start Menu, plain terminal) will
  refuse `C:\mnemo` again — now because the stamp says `b7bc…` — and will also refuse
  `C:\lmv-editor` (stamped `b7bc…`). Any fix or recovery procedure must converge or
  reconcile the two views, not just the stamp.
- **Should Mnemo2 detect the virtualized double-view?** Options for the fix step
  (step 2): probe for the package-LocalCache shadow when reading/writing the owner id;
  or surface a refusal message that names the two-view situation and offers a recovery
  that does not hand-edit files and does not delete session history. The
  cloned-repository protection (refuse unstamped/foreign-stamped folders, E1 test at
  line 866) must be preserved either way.
- **Is running Mnemo2 under the Claude Desktop plugin a supported topology on this
  machine?** If yes, the two-view hazard applies to every MSIX-packaged host (Claude
  Desktop today; Windows Terminal would create its own package store if a CLI chain ran
  under it), and the fix should treat "owner id" as per-view rather than per-machine.
