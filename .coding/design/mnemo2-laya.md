# Design: attach Laya to Mnemo2's engines

Plan e02de747, step 1 (no-code research). Scope: make the Laya classifier
work inside Mnemo2's headless engines — model routing lanes, memory
auto-typing, failure triage, tool-choice reflex — plus the Mnemo2 settings
surface. Written 2026-11-01 against the tree at the plan's fork point; every
claim carries a `file:line` reference to the code it was read from.

---

## 0. Surveyed ground — what already exists

**The kernel seam is done and waiting.** `wire_brain`
(src/host/brain.rs:342) unconditionally creates the empty classifier slot
(`Arc<RwLock<Option<Arc<dyn Classifier>>>>`, brain.rs:543-561; the type is
`ClassifierSlot`, src/memory/classifier.rs:216) and wires **all five**
consumers into the shared agent factory regardless of config:

| consumer | handle built in | wired into the factory |
| --- | --- | --- |
| memory auto-typing | brain.rs:562-564 (flag mirror) | brain.rs:1200 |
| tool-choice steering (search/search_read) | brain.rs:565-570 | brain.rs:1207 |
| failure triage (+kNN overlay) | brain.rs:608-644 | brain.rs:1218 |
| pre-prompt model routing | brain.rs:646-651 (`RoutingGate`, no flag mirror — reads the live config per turn) | brain.rs:1223 |
| compound reflex call | brain.rs:653-664 | brain.rs:1229 |

The kernel deliberately leaves the slot empty at build time: "the classifier
itself is attached by the HOST after the build (the managed runtime is
host-owned — the app's attach_laya fills the shared slot below before the
first agent spawns)" (brain.rs:550-553), and "Constructing the client does no
I/O: startup is never blocked" (brain.rs:543-546). The slot and the shared
status are public fields on `Brain` (brain.rs:93, :95-101).

**Mnemo1's host fills the slot in three places:**

1. Startup — `attach_laya(&brain, &laya_cfg)` (src-tauri/src/main.rs:78-127):
   builds a `LayaManager` rooted at the global config dir, requires
   `[general.laya] enabled` AND `is_checkpoint_installed(english_checkpoint)`,
   allocates a loopback port, parks the status on `Starting`, fills
   `brain.classifier_slot` with the no-I/O HTTP client
   (`build_managed_classifier`, src/host/laya.rs:516), and returns the pending
   start. The startup hook then spawns `start_managed_server` fire-and-forget
   with that port (main.rs:812-830), and `spawn_startup_finetune` fires the
   startup fine-tune (main.rs:832-845).
2. Settings save — `rewire_vision_embedder_and_classifier`
   (src-tauri/src/ipc/rewire.rs:36, classifier leg :58-126): stop the manager,
   re-read the config, re-run the same attach gate; enabled+installed → port +
   `Starting` + slot fill + spawn; otherwise slot = None + status `Disabled`.
3. Settings → Classifier first install — `Setup::classifier_setup(enable)`
   (src/headless/setup.rs:651): persists + live-applies the flip, then
   `spawn_setup_task` (src/host/laya.rs:811-894) downloads uv → venv →
   `laya[serve]` → the English checkpoint and, engine-backed and still
   enabled at completion, autostarts the sidecar and swaps the slot
   (laya.rs:838-856).

**Mnemo2 attaches none of this.** `LeadRuntime::start_engine`
(src-tauri/src/mnemo2/runtime.rs:889) builds `EngineOptions` and calls
`Engine::builder(options).start()` (runtime.rs:921-922) with no classifier
step; the headless engine itself is deliberately Laya-free — zero
classifier/laya references in src/headless/engine.rs — and the comment at
runtime.rs:481-482 says so ("Left out: the Laya auto-typing gate (the app
wires no classifier into the headless engine either)").

**Mnemo2's topology makes this cheap.** One `LeadRuntime` per process
(`Mnemo2State.runtime: Mutex<Option<Arc<LeadRuntime>>>`, src-tauri/src/mnemo2/ipc.rs:55-56,
doc comment :9). `TeamRuntime` does **not** build worker engines — it holds
the lead's engine (`pub engine: Arc<Engine>`, src-tauri/src/mnemo2/team.rs:420,
:644, doc :9-10) and `spawn_worker_session` spawns every worker as a session
on that same engine (`self.engine.spawn(SpawnOptions …)`, team.rs:2727-2737).
The lead spawns the same way (runtime.rs:980-1016). Every session inherits the
factory's handles, so **one slot fill per engine covers the lead and every
worker** — there is no per-worker seam to touch.

**The one missing piece is a public attach API on the headless host seam.**
`Engine::brain()` is `pub(crate)` (src/headless/engine.rs:214) — a host cannot
reach the slot. The sanctioned host seam is `mnemo::headless::Setup`:
`Setup::open(config_dir)` (setup.rs:420), then `for_engine(&engine)`
(setup.rs:504-529), which re-roots the Setup onto the brain's shared Arcs — a
`LayaManager::with_base(<config_dir>/laya, brain.classifier_status)` at
setup.rs:512-515 and the brain's slot Arc at setup.rs:526. The Setup already
exposes `laya_manager()` (setup.rs:616-618) and `classifier_setup(enable)`
(setup.rs:651), but the **"already installed → attach at engine start"** path
exists only in Mnemo1's GUI (`main.rs attach_laya`); the headless Setup
autostarts only inside the setup pipeline's completion (laya.rs:838-856).

**Config is shared, not duplicated.** Mnemo2 reads the same config dir as
Mnemo1: the engine resolves it through "the ONE resolver in `config/` …
`Setup::open` shares it" (src/headless/engine.rs:747-749), and
`LeadRuntime` carries its own `config_dir` (runtime.rs:250, :396) — so the
engine, the Setup, and the manager always agree on the dir. So
`[general.laya]` (`LayaConfig`, src/config/general.rs:225-338: `enabled`
:226-227, `auto_type_memories` :239-240, `failure_triage` :256-257,
`failure_triage_knn` :271-272, `auto_finetune` :282-283, `steer_tool_choice`
:292-293, `routing` :310-311, `reflex` :336-337 — all off by default,
serde-skipped while false) and `[general.routing]` (`RoutingConfig`,
general.rs:399-431: `cheap` :402, `capable` :406, `lane_medium` :411,
`lane_high` :414, `escalate` :418, `threshold` :423-424 — default 0.80 at
:466-468, `enforce` :429-430) are already live for Mnemo2 once the slot is
filled.

---

## 1. Where Mnemo2 fills the classifier slot

**Decision: fill the slot once per engine, in `LeadRuntime::start_engine`,
through a new public kernel API — `Setup::attach_laya()` on the
engine-backed `Setup`.**

- **Fill site.** In `start_engine` (runtime.rs:889-973), immediately after
  `Engine::builder(options).start()` (runtime.rs:921-922), open
  `Setup::open(Some(config_dir))` and call `.for_engine(&engine)` (the exact
  pattern the settings commands already use, ipc.rs:1121-1137), then
  `attach_laya()`. Because workers share the engine (team.rs:420, :2727-2737),
  this single fill covers the lead session, every worker session, and every
  audit worker — none of the spawn paths change (`SpawnOptions` carries no
  classifier surface because the factory handles are brain-wide).
- **The new API** (`src/headless/setup.rs`, beside `classifier_setup` :651):
  `Setup::attach_laya()` mirrors main.rs:78-127 — read the live config; when
  `[general.laya] enabled` AND
  `self.laya_manager().is_checkpoint_installed(english_checkpoint())`
  (laya.rs:355, :132): allocate a port (`LayaManager::alloc_free_port`), park
  the shared status on `Starting`, write
  `build_managed_classifier(port, status)` (laya.rs:516) into the live slot
  (setup.rs:526 handle), and spawn `start_managed_server` (laya.rs:960)
  fire-and-forget through the Setup's own manager. Otherwise: no-op (the slot
  stays `None`, the status stays `Disabled`). Engine start is never blocked —
  attach only allocates a port and fills an `Arc` (brain.rs:543-546).
- **Rejected alternative — make `Engine::brain()` public** (engine.rs:214):
  would expose the whole `Brain` (factory, stores, model resolver) to every
  host and erode the headless boundary; `Setup::for_engine` already is the
  sanctioned, narrow seam ("the headless twin of Settings → Classifier",
  setup.rs:620-621).
- **Rejected alternative — attach inside `start_inner`** (engine.rs:725): the
  headless engine is deliberately Laya-free (no classifier reference anywhere
  in engine.rs) and the kernel pins the managed runtime as host-owned
  (brain.rs:550-553). Teaching the engine to spawn sidecars would change that
  contract for every embedder.
- **External-lead caveat.** An external lead over MCP gets the engine's
  *store* handles (runtime.rs:260-262) but runs its own agent in its own host
  — its auto-typing depends on that host's attach. Out of scope here.

## 2. Lifecycle — who starts the sidecar, sharing, stopping

- **Reuse Mnemo1's managed runtime wholesale.** The install lives at
  `<config_dir>/laya` (the engine-backed Setup's manager roots there,
  setup.rs:512-515) — the *same* uv binary, venv, checkpoint and HF cache
  (`hf_cache_dir`, laya.rs:325-327) Mnemo1 manages. If Mnemo1 already
  installed it, Mnemo2 attaches with zero downloads. No per-project state
  exists or is needed; Laya config is global, not per-project.
- **Starter: the Mnemo2 process, at lead open.** `start_engine` runs
  `attach_laya()` (gate: enabled + installed). First-time install stays a
  settings action (`classifier_setup(true)`, setup.rs:651) — its completing
  pipeline autostarts and swaps the slot without a restart (laya.rs:838-856).
- **Holder: `LeadRuntime` — not the per-call `Setup`.** The running
  `laya-serve` child lives *inside* `LayaManager` (laya.rs:263) and
  `impl Drop for LayaManager` kills it (laya.rs:486-490; "One instance per
  app", laya.rs:250-253). The Setup built by a settings command is dropped at
  the end of that command (ipc.rs:1121-1137), so it must not own the child:
  `LeadRuntime` gains `laya: std::sync::Mutex<Option<Arc<LayaManager>>>`
  (beside `engine`, runtime.rs:244-265), filled by `start_engine` and held
  for the engine's lifetime. The manager Arc also serves the settings
  commands later (`setup.laya_manager().clone()` — same instance is
  re-derivable via `for_engine`, which re-roots the status Arc onto the
  brain's, setup.rs:512-515; the *child* stays owned by the LeadRuntime's
  manager Arc — attach/rewire paths must reuse that instance).
- **One sidecar per process, not per project.** One `LeadRuntime` per process
  (ipc.rs:55-56) ⇒ one engine, one brain, one slot, one sidecar — regardless
  of how many projects are opened. On project switch, `lead_open` tears the
  old runtime down first (ipc.rs:515-523), so the sidecar stops and the next
  engine start re-attaches (new port, same install). Two Mnemo2 *windows* are
  two processes ⇒ two sidecars.
- **Mnemo1 and Mnemo2 at the same time:** separate binaries (`mnemo-app`,
  src-tauri/Cargo.toml:2; `mnemo2`, Cargo.toml:50-52) ⇒ two processes, each
  with its own manager and its own sidecar on its own allocated port, against
  one shared install + HF cache. The concurrent-instance case is already
  designed for at the install level: server logs are per-PID
  (laya.rs:329-330) and `wait_until_ready` tolerates a second instance
  sharing the HF cache (laya.rs:929 area). Cost: the checkpoint sits in RAM
  twice. Accepted; a cross-process shared sidecar (port-file handshake) is
  deliberately *not* in scope — it adds IPC fragility for a RAM saving, and
  the two apps are usually not open together.
- **Stopping:** `LeadRuntime::shutdown` (runtime.rs:874) stops the manager
  explicitly (`laya.stop()`, laya.rs:373-386) **before** the engine drops;
  the manager's `Drop` (laya.rs:486-490) is the safety net for every other
  teardown path. Every exit routes through `shutdown` (project close/
  re-open, ipc.rs:515-523), so no orphaned `laya-serve` can outlive the app
  (the stated contract, laya.rs:482-485).
- **Startup fine-tune: reuse `ipc::finetune` as-is.** `spawn_startup_finetune`
  (src-tauri/src/ipc/finetune.rs:585-599) already takes `Option<AppHandle>`
  + manager + slot + status + live config Arc and emits `classifier://status`
  through the app handle; its gate `startup_finetune_checkpoint`
  (finetune.rs:212-221) requires `enabled && auto_finetune && installed`.
  Mnemo2 spawns it right after a successful attach, passing its own
  `AppHandle`. That needs two more Setup accessors (below, Q4): the slot Arc
  and the live config Arc.

## 3. Behaviour when Laya is disabled, missing, or down

- **Disabled (`enabled = false` or absent):** `attach_laya()` is a no-op —
  slot `None`, status `Disabled` (its creation state, brain.rs:547-549). Every
  consumer reads the slot at call time, finds `None`, and short-circuits: the
  app "behaves exactly as before" with "no HTTP client at all"
  (brain.rs:543-546). This is the shipped default (all `LayaConfig` opt-ins
  are off and serde-skipped, general.rs:225-338). **Byte-identical to today —
  the acceptance scenario's "nothing changes" half.**
- **Enabled but not installed:** attach no-ops the same way (the gate requires
  `is_checkpoint_installed`, mirroring main.rs:78-127 and rewire.rs:113-126's
  "managed but not ready → Disabled + slot None" parking). The settings page
  shows the Install button; `classifier_setup(true)` then installs and
  autostarts (laya.rs:838-856) with no restart.
- **Down mid-run:** the HTTP client gets no answer and every consumer falls
  back by design — routing keeps today's model and logs nothing
  (src/agent/model_routing.rs:24-43: a missing/below-threshold answer keeps
  the configured model; the log write is best-effort), auto-typing keeps the
  writer's prefix (general.rs:230-232), failure triage keeps the existing
  rules (general.rs:248-250), tool-choice keeps the regex heuristics
  byte-identically (general.rs:284-288), reflex keeps the step's configured
  model (general.rs:324-327). No turn ever fails on a classifier error; the
  status surface self-heals to `Failed` and the settings page shows the hint
  + the per-PID server log path (statusHint copy,
  frontend/src/components/settings/sections/ClassifierSection.tsx:47-68).
- **Flag flips mid-run need no re-attach:** `auto_type_memories`,
  `steer_tool_choice`, `failure_triage` (+`_knn`), `reflex` flip their
  AtomicBool mirrors live (brain.rs:555-570, :608-612, :653-658);
  `routing`, the lane targets, `threshold`, `enforce` are read from the live
  config per turn (brain.rs:646-650). Only the `enabled` flip needs the
  attach/detach step — and the existing `classifier_setup(false)` already
  covers detach (stop + clear slot + park `Disabled`, setup.rs:638-643).

## 4. Reuse vs. add — commands and functions

**Reuse unchanged (kernel, `mnemo` crate):** `LayaManager` (laya.rs:254-271),
`spawn_setup_task` (:811-894), `start_managed_server` (:960),
`build_managed_classifier` (:516), `english_checkpoint` (:132),
`ClassifierSlot` (src/memory/classifier.rs:216), `Setup::for_engine`
(setup.rs:504), `Setup::laya_manager` (:616), `Setup::classifier_setup`
(:651). The module is already re-exported as `mnemo::laya` (src/lib.rs:74)
and is Tauri-free + host-agnostic (src/host/laya.rs:5-22).

**Reuse unchanged (src-tauri crate — same crate tree as the `mnemo2`
binary):** `ipc::finetune::{spawn_startup_finetune (finetune.rs:585),
startup_finetune_checkpoint (:212), write_finetune_marker (:153)}` — the
fine-tune gate + hot-swap discipline; the `LayaWire` DTO shape
(src-tauri/src/ipc/settings.rs:461+) as the wire model for Mnemo2's own
save command. Mnemo1's `attach_laya` (main.rs:78) is *not* reusable directly —
it takes `&Brain`, which hosts cannot obtain (engine.rs:214) — hence the new
kernel API.

**Add (kernel — `src/headless/setup.rs`):**
1. `Setup::attach_laya()` — the already-installed autostart twin of
   main.rs:78-127 (§1). Extract the gate as a pure
   `attach_decision(laya_cfg, installed) -> Option<u16>`-style function so the
   spawn leg (a real child) stays out of tests, mirroring the existing
   discipline ("the enable pipeline is network-bound — never exercised in
   tests", setup.rs:648-650).
2. `Setup::classifier_slot()` — public accessor for the brain's slot Arc
   (needed by `spawn_startup_finetune`, finetune.rs:588; engine-backed only,
   `None` otherwise like the other live handles).
3. `Setup::live_config()` — the brain's live config Arc (same constraint),
   the last argument `spawn_startup_finetune` needs (finetune.rs:590;
   main.rs:844 passes `brain.config` today).

**Add (Mnemo2 — `src-tauri/src/mnemo2/ipc.rs` + `mod.rs`):**
- `mnemo2_classifier_status` — the twin of `get_classifier_status`
  (settings.rs:43-46): status + `enabled` + `installed` flags so the page can
  show "install needed" vs "off".
- `mnemo2_laya_setup` — enable + install: `classifier_setup(true)` through a
  `for_engine` Setup when a lead is open (else standalone `Setup::open(None)`
  — install without an engine, the next engine start attaches,
  setup.rs:631-634); stream the returned `EventReceiver<SetupEvent>`
  (setup.rs:651-654) to the frontend.
- `mnemo2_laya_disable` — `classifier_setup(false)`: stop + clear + park
  `Disabled` (setup.rs:638-643).
- `mnemo2_save_laya_settings` — persist the seven opt-in flags +
  `[general.routing]` and live-apply. **`enabled` must NOT ride this patch**:
  the patch validator rejects `laya_enabled` whole
  (`unsupported_patch`, setup.rs:541-545) precisely because a patch would
  half-apply it — the enable flip owns the sidecar lifecycle and goes through
  `mnemo2_laya_setup`/`mnemo2_laya_disable` only.
- All four registered in the `invoke_handler` (src-tauri/src/mnemo2/mod.rs:390-420).

**Deliberately NOT reused:** `laya_catalog` (src-tauri/src/ipc/laya.rs:112) —
the managed runtime serves the English checkpoint only, so a catalog browser
is noise for Mnemo2; `rewire_vision_embedder_and_classifier` (rewire.rs:36) —
its vision/embedder legs are GUI machinery; Mnemo2's save path uses the Setup
patch + attach APIs instead.

## 5. The Mnemo2 settings page

A third rail page — **"Laya"** — in `SettingsDialog.svelte`
(mnemo2/src/components/SettingsDialog.svelte:10-14 rail, pages beside
Endpoints/Tiers), a new `LayaPage.svelte`, and transport additions in
`mnemo2/src/lib/settingsEngine.ts` (the `SettingsTransport` invoke pattern,
settingsEngine.ts:83-127).

**Show:**
- Enable toggle + status chip + hint — reuse the `statusHint` copy
  (ClassifierSection.tsx:47-68) and the near-chance-until-fine-tuned warning
  (:795-800).
- Install/runtime status with progress (the `SetupEvent` fractions,
  setup.rs:635-637) and the Install button when not installed.
- The seven opt-in flags, each with its own warning that only
  `auto_finetune` is meaningful against a base checkpoint
  (general.rs:233-238, :251-254, :288-290, :305-308, :332-334): routing,
  reflex, auto_type_memories, steer_tool_choice, failure_triage,
  failure_triage_knn, auto_finetune.
- Routing block: `cheap` / `capable` / `lane_medium` / `lane_high` /
  `escalate` model pickers (endpoint+model, reusing the endpoints/tiers data
  the dialog already loads) + `threshold` (default 0.80, general.rs:466-468) +
  `enforce` with the shadow-first copy ("while enforce is off every turn is
  still classified and its decision + outcome logged … model choice is
  byte-identical to today", general.rs:300-302).
- Auto-finetune status line (last-finetune marker / row counts) — read-only.

**Drop from Mnemo1's ClassifierSection (noise for Mnemo2):** the budget caps
block (`BudgetNumberRow`, ClassifierSection.tsx:76-104, :157-164, :690-731 —
`[general.budget]` belongs to Mnemo1's deterministic budget layer, which
Mnemo2 does not have); the checkpoint catalog list; manual fine-tune/export
controls (`auto_finetune` + the status line cover it).

**With no lead open**, the page still works: `settings_setup`'s standalone
leg (ipc.rs:1132-1135) gives a `Setup::open(None)` — install runs, parks
`Disabled`, and the next engine start attaches (setup.rs:631-634).

## 6. Implementation steps (ownership, tests, risks)

Ordered; each lands green on its own. Verification batteries per the plan:
`cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::`
for Mnemo2 code, plain `cargo test --workspace` for the kernel (both features
compile the touched paths via the workspace run), `cargo clippy` for both
crates clean, and in `mnemo2/`: `npm run check` + `npx vitest run`.

**Step 1 — kernel attach seam** (owns: `src/headless/setup.rs`; tests in the
same file's test module or `src/agent/tests.rs`).
- Add `attach_laya()`, `classifier_slot()`, `live_config()` per §4; the pure
  attach-decision helper.
- Tests (red first): engine-backed Setup with `enabled + installed` leaves
  the slot `None` today (red — no attach path exists; the Setup only
  autostarts inside the setup pipeline, laya.rs:838-856); after the change it
  fills the slot and spawns; disabled → no-op; not-installed → no-op; the
  decision helper unit-tested without spawning children.
- Risks: LOW — additive, no existing caller changes; keep the spawn leg out
  of tests (the network-bound discipline, setup.rs:648-650).

**Step 2 — Mnemo2 runtime attach** (owns: `src-tauri/src/mnemo2/runtime.rs`).
- `laya: Mutex<Option<Arc<LayaManager>>>` field (runtime.rs:244-265), the
  `attach_laya()` call at the end of `start_engine` (runtime.rs:889-973),
  `laya.stop()` in `shutdown` (runtime.rs:874) before the engine drops, and
  the `spawn_startup_finetune` spawn (finetune.rs:585) after a successful
  attach with Mnemo2's `AppHandle`; update the stale comment
  (runtime.rs:481-482).
- Tests (`--features mnemo2 --lib mnemo2::`): engine start with laya disabled
  → slot stays `None` (the observable for "disabled changes nothing" — read
  via the new `Setup::classifier_slot()`); enabled + a doctored tempdir
  install (gitignored red-check pattern, `.coding/tmp/`) → slot `Some`,
  manager held, `shutdown()` stops it (the child is gone). Never block on a
  real sidecar in tests — assert the spawn request, not the server.
- Risks: MEDIUM — (a) dropping the manager early kills the child
  (laya.rs:486-490): the LeadRuntime holds it, and rewire-style commands must
  reuse that Arc, not a fresh `for_engine` manager (setup.rs:512-515 creates a
  *new* manager per call — only the attach call may spawn the child, through
  the held instance); (b) attach must never block engine start (fill the slot,
  spawn the start task, return — main.rs:816-830 discipline); (c) project
  switch stop/start ordering (ipc.rs:515-523).

**Step 3 — Mnemo2 commands + settings save** (owns:
`src-tauri/src/mnemo2/ipc.rs`, the `invoke_handler` list in
`src-tauri/src/mnemo2/mod.rs:390-420`).
- The four commands of §4; the general patch carries the seven flags +
  `[general.routing]` but never `enabled` (setup.rs:541-545).
- Tests: command round-trips against a tempdir config; the save rejects an
  `enabled`-carrying patch; setup/disable route through the real
  `classifier_setup` legs; the status command reports installed/enabled
  honestly for each park state.
- Risks: LOW — mirrors existing command shapes (ipc.rs:1147+); the
  `SetupEvent` → frontend channel contract is the only new surface.

**Step 4 — settings page** (owns: `mnemo2/src/components/LayaPage.svelte`,
`SettingsDialog.svelte`, `mnemo2/src/lib/settingsEngine.ts`).
- The §5 page: toggle + status + progress + flags + routing block; transport
  types for the four commands + the status/event payloads.
- Tests (`npx vitest run`): component renders each status honestly
  (Disabled/Installing/Starting/Ready/Failed); flag toggles invoke the save
  with the right payload; the enable button routes to setup, not the patch;
  `npm run check` clean.
- Risks: LOW — pure UI over the step-3 commands; reuse the dialog's
  endpoints/tiers data for the pickers.

**Step 5 — closing verification** (the plan's acceptance scenarios, on the
integrated tree):
- Laya enabled + installed + `routing` on (shadow, `enforce` off): a Mnemo2
  lead turn appends a `RouteDecisionRow`
  (src/agent/model_routing.rs:201) to the routing log
  `<config_dir>/laya/training/routing.jsonl` (model_routing.rs:282-286) —
  observable without trusting internals.
- `auto_type_memories` on + a fine-tuned checkpoint: a `memory_write` with a
  deliberately wrong typed prefix comes back corrected in the knowledge store
  (the auto-typing handle, brain.rs:1200).
- `steer_tool_choice` / `failure_triage` / `reflex` on: the respective logs
  (`routing.jsonl` siblings under `<config_dir>/laya/training/`) gain rows;
  a transient tool failure is re-run harness-side (general.rs:244-247).
- Laya disabled: slot `None` (assert via `Setup::classifier_slot()`), zero
  sidecar processes, zero log rows — byte-identical behaviour.
- Both crates clippy-clean; the standard batteries green.

**Deferred explicitly (not in this plan):** a cross-process shared sidecar
(§2); per-project Laya config (global today by design); the external lead's
own engine (§1 caveat); a Mnemo2-native fine-tune training surface beyond the
reused startup gate.