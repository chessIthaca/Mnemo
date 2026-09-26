## Verdict: FINDINGS (3 high, 7 low)

Review of all uncommitted changes on `wt/mnemo` for plan d6fc659a (Laya managed runtime). The architecture is sound and the opt-in contract holds, but the settings **read** path was never extended for `mode`/`checkpoint`, which silently converts external configs to managed on save, and the sidecar lifecycle has two reachable races.

---

### HIGH 1 — Settings read path omits `mode`/`checkpoint`: external configs silently flip to managed and the endpoint is erased

- `src-tauri/src/ipc/settings.rs:475-481` — `LayaWire` still has only `enabled` + `endpoint`.
- `src-tauri/src/ipc/settings.rs:882-885` — construction copies only those two fields.
- `frontend/src/components/settings/sections/ClassifierSection.tsx:113-119` — `load()` reads `laya?.mode ?? "managed"`, `laya?.checkpoint ?? "english"`.
- `frontend/src/components/settings/sections/ClassifierSection.tsx:193-199` — save sends `laya_mode: mode` and `laya_endpoint: mode === "managed" ? "" : endpoint`.
- `frontend/src/lib/tauri.ts:449-456` — the TS type declares `mode`/`checkpoint` as required members, hiding the gap from `tsc`.

Because the backend never sends `mode`, the UI **always** initializes the mode radio to Managed — including for every pre-existing external config (`external` is the serde default). The dirty snapshot is built from the same defaulted draft, so the section looks clean while misrepresenting the mode. One OK (save) then persists `laya_mode: "managed"` + `laya_endpoint: ""`, flipping the config to managed and **erasing the user's external endpoint**. This directly violates the plan goal "External mode is preserved".

Fix: add `mode: LayaMode` (lowercase serde already derives) and `checkpoint: Option<String>` to `LayaWire`, populate at settings.rs:882, and extend the JSON test at settings.rs:1439-1440 — it currently pins only `enabled`/`endpoint`, which is exactly why this slipped through.

### HIGH 2 — Child-slot races: unscoped `stop()` + concurrent start tasks kill each other's sidecar

- `src-tauri/src/ipc/laya.rs:343-358` — `stop()` kills whatever child is in the slot, with no notion of ownership.
- `src-tauri/src/ipc/laya.rs:798-810` — a failed readiness probe calls `manager.stop()`, killing whichever child currently exists — not the one that probe spawned.
- `src-tauri/src/ipc/laya.rs:372-373` — every `spawn_server` stops the current child first.
- Producers of concurrent starts: the startup hook (`src-tauri/src/main.rs:619-636`, probe may run for minutes on a cold model load), the rewire (`src-tauri/src/ipc/rewire.rs` managed branch: `laya.stop()` → alloc new port → background start), and the setup autostart (`laya.rs:675-688`, its own fresh port).

Scenario A: cold start with managed enabled → startup hook probes; user saves any setting within that window → rewire stops the probing child and starts a new one on port B; the orphaned startup probe for port A then hits its idle budget and its failure path `stop()` **kills child B** → status Failed, classifier dead until the next save.

Scenario B: a save during a setup's autostart produces two live `start_managed_server` tasks; each `spawn_server` kills the other's child, and the loser's failure-path `stop()` kills the winner's; the classifier slot can be left pointing at a dead port while the status reads Ready or Failed incorrectly.

Fix sketch: a generation token captured at spawn (`stop_if_generation(gen)`), failed probes only stopping the child they spawned; and/or have the rewire skip/delegate the start when `setup_in_flight` is set.

### HIGH 3 — Downloading a non-configured checkpoint silently swaps the live model

- `src-tauri/src/ipc/laya.rs:879-883` — autostart is decided by `enabled && mode == Managed` only, without comparing the **downloaded** checkpoint to the **configured** one.
- `src-tauri/src/ipc/laya.rs:689-695` — on success the live classifier slot is swapped to the fresh sidecar serving the downloaded checkpoint.

Scenario: managed+enabled with `english` installed and serving; the user clicks Download on the `multilingual` card (no save needed) → setup completes → autostart starts multilingual on a new port, `spawn_server` stops the english child, and the slot is swapped. The saved config still says english; a restart serves english again. The live classifier changes model identity with no config change.

Fix: autostart only when the downloaded id matches the configured checkpoint (the config is already read at :881-883). Relatedly, the `Ok(()) => Disabled` branch at :697-703 unconditionally overwrites the shared status — it must not clobber a live external-mode classifier's Ready status (reachable once HIGH 1 misdirects a user into the managed UI).

---

### LOW 4 — Progress poller leaks on a JoinError, permanently corrupting the status

`src-tauri/src/ipc/laya.rs:645-649`: the `?` on the join result returns **before** `poll_handle.abort()` runs. A panicked/cancelled preload task leaks the 500 ms poller forever; it keeps rewriting the shared status to `Downloading` every tick, so any later Failed/Ready is clobbered twice a second and the UI shows a phantom progress bar for the rest of the session. Abort before propagating, or use a drop guard.

### LOW 5 — Readiness probe's absolute cap is bypassed by cache growth

`src-tauri/src/ipc/laya.rs:755-766`: `elapsed >= PROBE_ABSOLUTE_BUDGET` is only checked in the no-growth `else` branch; any writer that keeps growing the HF dir resets `idle` indefinitely (a wedged server retrying downloads, or a second app instance sharing the same global `laya/hf`). This contradicts the documented 900 s absolute cap (:75-77) and leaves status Starting forever. Check the absolute budget unconditionally at the loop head.

### LOW 6 — uv install is non-atomic and unverified

`src-tauri/src/ipc/laya.rs:504-521, 537-539`: the archive downloads to a scratch name (good), but extraction writes `bin/uv` in place — an app kill mid-extract leaves a partial binary that the `is_file()` fast-path at :458-460 accepts, so every later setup fails cryptically until the user manually deletes it from the app config dir. Extract to a temp name + rename. Also: the GitHub release API exposes an asset `digest` (sha256) that is not verified for a binary the app will later execute — cheap hardening worth adding.

### LOW 7 — Dead `port` state

`src-tauri/src/ipc/laya.rs:258, 349, 396`: `port: RwLock<Option<u16>>` is written, never read (the removed `port()`/`endpoint()` accessors left no callers — verified by search; no regression, just leftovers). Delete the field and its writes, or restore a reader. It only escapes the dead_code lint because method-call writes count as uses.

### LOW 8 — `extract_uv_asset` (the archive-extraction boundary) has zero tests

`src-tauri/src/ipc/laya.rs:528-563`. The implementation is safe today — dest is a fixed path (no entry-name-derived paths → no traversal), the tar branch requires `entry_type().is_file()` (symlinks/hardlinks skipped), the zip branch matches only the file-name component — but nothing pins that. Add fixture tests (zip + tar.gz) covering a `../evil`-named entry, a symlink entry named `uv`, a nested `dir/uv`, and the missing-binary error, so a future refactor cannot silently regress the boundary.

### LOW 9 — PLAN.md is stale for the managed runtime

`PLAN.md:1138-1147` still describes the foundation-era section ("carries the toggle, endpoint URL, install hints, and live status"). README and the module docs are updated; per the documentation-sync check, the shipped-features list should gain one sentence for the managed runtime (download-in-Settings sidecar, uv + venv + checkpoint, auto start/stop).

### LOW 10 — Global base dir shared by concurrent app instances

`src-tauri/src/ipc/laya.rs:266, 382`: `<global_config_dir>/laya` is process-global; two live instances each spawn their own sidecar but share `server.log` (each spawn truncates it via `File::create`) and race the same HF cache during preloads. Mostly benign — consider a per-PID log name or documenting the single-instance expectation.
