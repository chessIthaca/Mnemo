## Verdict: FINDINGS (1 high, 3 low)

Re-review of commit 9de7e08 (Laya managed runtime, plan d6fc659a) after the 2026-09-25 review's 3 high + 7 low fixes. **All 10 first-review fixes are verified fixed in the current source** (each re-checked in full, not just diff hunks). One new HIGH — a stale-config-snapshot race in the same lifecycle area the HIGH 2/3 fixes touched — and 3 LOWs remain.

### First-review fix verification (all confirmed)

| # | Fix | Verdict | Anchor |
|---|-----|---------|--------|
| H1 | Settings round-trip `mode`/`checkpoint` | ✅ `LayaWire` carries both, populated from config; JSON test pins them; fixture + Rust initializer updated; FE `laya?.mode` init + save preserve external | settings.rs:476-488, 889-894, 1454-1457; contract_fixtures.rs:242-247; dto-get-settings.json:15-20; ClassifierSection.tsx:113-119, 194-199; tauri.ts:453-458, 560-562 |
| H2 | Generation-scoped child slot | ✅ `AtomicU64` + `Mutex<Option<(Child, u64)>>`; `spawn_server` returns the generation; failed probe calls `stop_if_generation` (`take_if` on match) so a concurrent newer child survives | laya.rs:258-263, 370-384, 401-427, 898-901 |
| H3 | Autostart gated to configured checkpoint; no status clobber | ✅ `autostart = enabled && managed && configured == downloaded`; `Ok(())` branch writes `Disabled` only when Laya is off in config | laya.rs:981-993, 778-796 |
| L4 | Poller aborted before JoinError propagates | ✅ `poll_handle.abort()` precedes `preload_joined.context()??` | laya.rs:717-725 |
| L5 | Absolute probe cap checked unconditionally at loop head | ✅ | laya.rs:839-845 |
| L6 | Scratch extract + rename; sha256 digest verified | ✅ archive → scratch `.extract-uv` → rename; `verify_sha256` handles `sha256:` prefix, case-insensitive | laya.rs:526-565, 572-595 |
| L7 | Dead `port` field deleted | ✅ no `port` state; `alloc_free_port` is the only source | laya.rs:249-266, 386-392 |
| L8 | Archive-boundary tests | ✅ zip (`../evil.bin`), tar.gz (symlink-first + nested `dir/uv`), missing-binary error — one fixture-validity caveat (LOW 3 below) | laya.rs:1165-1250 |
| L9 | PLAN.md documents the managed runtime | ✅ new bullet in "Beyond the original spec"; README feature table + guide updated | PLAN.md:1148-1155; README.md:43, 160 |
| L10 | Per-PID server log | ✅ `server-<pid>.log`, documented in module doc | laya.rs:27-31, 318-324 |

---

### HIGH 1 — `laya_setup` snapshots the config at download START; a mid-setup save makes the completed setup autostart a sidecar the user just disabled (or swap in the stale checkpoint)

- `src-tauri/src/ipc/laya.rs:985-993` — `laya_setup` reads `enabled`/`mode`/`checkpoint` from the config **once**, before the multi-minute setup task is spawned, and passes the derived booleans (`autostart`, `laya_disabled`) into `spawn_setup_task`.
- `src-tauri/src/ipc/laya.rs:755-776` — at completion, `Ok(()) if autostart` unconditionally starts a server and swaps the live classifier slot in — evaluated against the stale snapshot, never the current config.
- `src-tauri/src/ipc/laya.rs:783` — symmetrically, the `Disabled` write is gated on the stale `laya_disabled`.

Scenario A (violates "disabled must change nothing" — a plan-goal contract): managed + enabled + checkpoint `english` not installed → user clicks Download. Mid-download (~5–15 min window; the Enable checkbox, mode radios, and OK are not disabled while `busy` — only the Download button is, ClassifierSection.tsx:105-106, 328), the user unchecks **Enable** and saves. The rewire correctly clears the child/slot/status (rewire.rs:145-154) and the config now says disabled. The setup later completes with the stale `autostart = true` → starts a fresh `laya-serve`, swaps the **live classifier slot in** (laya.rs:773-776), and drives status to Ready. For the rest of the session a live classifier answers decisions while the saved config says Laya is off; only a restart restores the contract.

Scenario B (the silent model-swap HIGH 3 targeted, via a concurrent save): mid-setup of `english`, the user saves checkpoint `multilingual`; the completed setup autostarts `english` (stale snapshot) and swaps the slot — the session serves a different checkpoint than the config until the next save/restart.

Fix: re-evaluate the decision from the **live** config at completion time — re-read the config (or a shared handle) inside `spawn_setup_task`'s completion arm before acting on `autostart`/`laya_disabled` (and/or have the rewire consult `setup_in_flight`, as the first review's HIGH 2 sketch suggested). Extracting the decision into a pure helper (`fn setup_autostart_decision(laya_cfg, downloaded_id) -> (autostart, laya_disabled)`) would also make the gating — currently untested, since it lives inline in the Tauri command — unit-testable.

### LOW 2 — An orphaned probe's failure path still writes `Failed` over a live newer child's status

- `src-tauri/src/ipc/laya.rs:897-912` — when `stop_if_generation(generation)` does **not** match (a newer concurrent start owns the slot), the failure path still writes `ClassifierStatus::Failed` and emits. If that newer child already reached `Ready`, the UI shows Failed while the sidecar and classifier slot are healthy. Self-heals on the next successful classifier call (LayaClassifier flips Ready) or the next save, so status-only — but it is the residual half of first-review Scenario A. Fix: only report Failed when the generation still matched (use `stop_if_generation`'s bool) or the slot is still empty.

### LOW 3 — The tar fixture's "decoy symlink named `uv`" has an empty entry name, so the name-collision aspect isn't actually exercised

- `src-tauri/src/ipc/laya.rs:1186-1193` — the symlink header is appended via raw `tar.append(&mut link, …)` with `Header::new_gnu()` and **no `set_path`** (only `set_link_name`, which sets the link *target*), so the entry's path is empty — not `"uv"`. The test still pins the important properties (a symlink entry is skipped by type, the nested real binary wins), but a future refactor that drops the `is_file()` check and matches purely on name would no longer be caught by a genuine name collision. Fix: `link.set_path("uv")` (or `append_data`) before `set_cksum()` so the decoy really is named `uv`.

### LOW 4 — Startup falls back to the external endpoint in managed mode, contradicting the documented "ignored in managed mode" and diverging from the rewire

- `src-tauri/src/main.rs:1434` — `managed_classifier.or(classifier)`: managed + enabled + checkpoint-not-installed silently builds the external-endpoint classifier if a stale `endpoint` lingers in the config.
- `src/config/general.rs:219-225` — the `endpoint` doc says "Ignored in managed mode".
- `src-tauri/src/ipc/rewire.rs:99-154` — the rewire's managed branch never falls back to the endpoint, so a hand-edited managed+endpoint config gets a classifier at startup that any later save then drops. UI-unreachable (saving in managed mode blanks the endpoint), so low; align the two paths or fix the doc.

---

### Constitution checks

- **Multi-platform neutrality: PASS.** Per-platform branches, not platform assumptions: `hide_console_window` is `cfg(windows)` with a `cfg(not(windows))` no-op (laya.rs:230-239); the uv exec-bit is `cfg(unix)` (laya.rs:626-630); `uv_asset_name`/`exe_name`/`venv_bin` cover Windows/macOS/Linux layouts and are table-tested (laya.rs:1014-1060); the zip (Windows asset) and tar.gz (unix assets) extract paths are both fixture-tested. No Windows-only API outside the sanctioned gates.
- **Warning-free build: PASS (by evidence).** No `#[allow]` anywhere in the new code; both crate roots carry `#![deny(warnings)]`, so the reported green `cargo test` / `cargo check -p mnemo-app` proves zero warnings.
- **Documentation sync: PASS.** PLAN.md:1148-1155 (new managed-runtime bullet), README.md:43 + 160, extensive module docs in laya.rs:5-41, config docs updated for `mode`/`checkpoint` semantics.
- **File-tools-first: PASS.** All 27 files are source/doc changes via the file tools; no shell-based mutation anywhere.
- **Test quality: PASS with LOW 3.** Rust side covers config round-trips (general.rs:1214-1273), patch semantics (settings_dto.rs:898-926), wire shape (settings.rs:1454-1457) + golden fixture equality, marker gating, extract boundary, sha256 vectors (the `sha256("abc")` constant is correct). FE tests follow the repo's established source-assertion convention. Gap: the autostart gating decision itself is untested (see HIGH 1 fix suggestion).
- **Regressions from the fixes themselves:** none found beyond the items above — the generation mechanism, poller abort ordering, scratch-rename install, and per-PID log are all correct as implemented; `ClassifierStatus` serialization (`{"downloading": {…}}`, lowercase unit strings) matches the TS `ClassifierStatusWire` union on both the event and startup-snapshot paths, and `laya_setup` correctly drops the config lock before spawning the task.
