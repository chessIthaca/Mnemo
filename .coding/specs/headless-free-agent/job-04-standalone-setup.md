# Job 4 — standalone setup + config (U8; G8 memory, G9 config split; D3 the Laya core)

Master-spec anchors: §5.8 :486-589 (the `Setup` API — U8), §5.9.8 :725-731
(G8 — `memory_store` + `SharedResources`), §5.9.9 :733-738 (G9 — the
EngineConfig/UiConfig split), §5.1 :168 (D3 — "the Laya manager's Tauri-free
core moves in job 4" — THIS job), §6 row 4 (:766 — the file map), §7 row 33
(:886-888 — the acceptance), U8 (:47). Master-plan step 10. Branch `wt/mnemo`.

## A. Job map — what lands where

- NEW `src/headless/setup.rs` — `Setup` (§5.8 :525-561), `SetupStatus`
  (:562-571), `SetupStep` (the ordered first-run to-do), `SetupEvent`
  (:588-589) — the whole standalone setup/config API, works without a
  running Engine (first run) and against a live one (live-apply).
- NEW `src/host/rewire.rs` — the live-apply path moved from
  `src-tauri/src/ipc/rewire.rs` (vision/embedder/classifier/resolver
  rebuilds, AppHandle-free — §6 row 4).
- NEW `src/host/embed_download.rs` — the bundled-model download + progress
  sampling moved from `src-tauri/src/ipc/embeddings.rs`.
- NEW `src/host/laya.rs` — the managed Laya core moved from
  `src-tauri/src/ipc/laya.rs` (manager, uv/venv/install pipeline, the
  `laya-serve` child, the readiness probe, markers — D3). The shared
  `ClassifierStatus` Arc stays the status carrier; Tauri emits stay
  app-side.
- `src/config/keys.rs` — `Secret` (zeroize-on-drop, no Debug/Serialize of
  the value) + `KeySource::{File, Env, InMemory}` (§5.8 :577-580).
- `src/config/{mod,general}.rs` — `resolve_config_dir` (explicit →
  `$MNEMO_HOME` → `~/.mnemo`; moved from engine.rs:629-633 so BOTH the
  engine and Setup share one resolver), the G9 EngineConfig/UiConfig split
  (on disk NOTHING moves — same files, same tables; the app owns `[ui]`).
- `src/headless/{error.rs,engine.rs,types.rs}` — `not_configured` (code
  `not_configured`), the `Engine::start` precondition (§5.8 :581-587:
  `ready_to_run == false` → the missing list; a missing embedder model is
  NOT fatal — hash fallback + `semantic: false`), `EngineOptions.
  auto_install_embedder` (default false), the G8 Rust-only builder
  (`memory_store`, `SharedResources`).
- `src/project/scaffold.rs` (kernel) — the scaffold core moved from
  `src/app/scaffold.rs` (2c's kernel-`Workspace` home), so
  `Setup::init_project` needs no `mnemo::app` import.
- `src-tauri/src/ipc/{settings,embeddings,models,mcp,projects,laya}.rs` —
  become thin callers over the moved lib functions (one implementation
  each; the app keeps its Tauri emits/state glue).
- Tests: `tests/integration/headless_setup.rs` (row 33 fixtures + the
  Setup surface contract) + unit tests beside the moved code.
## B. Current state (probe facts, verified)

1. `EngineOptions.config_dir: Option<PathBuf>` already exists
   (src/headless/types.rs:78-79 — "`None` = `MNEMO_HOME` or `~/.mnemo`");
   the resolution lives in the engine (engine.rs:476 comment,
   :629-633 `default_config_dir` reading `std::env::var_os("MNEMO_HOME")`).
   `src/host/brain.rs:1213` already derives the BRAIN's embedder cache from
   the config dir ("~/.mnemo/models or MNEMO_HOME/models") — the IPC side
   does not (fact 9).
2. The G4 close-grep (src/headless/mod.rs:56-80) asserts NO `src/headless`
   file CALLS `global_config_dir(` — `Setup` must take its dir explicitly
   (`open(dir: Option<PathBuf>)`, §5.8 :527) and share the engine's
   resolver. `config/` is allowed to call it (the loader lives there).
3. `KeyStore` (src/config/keys.rs:51-98) is FILE-ONLY today:
   `load_or_default`/`get`/`insert`/`save` (+ `restrict_permissions` —
   user-only ACL, fail-safe). No env/in-memory source, no `Secret`
   newtype — keys move as plain `String`s. `KeySource` is NEW (§5.8
   :578-580; the spec's ASSUMPTION "KeyStore only reads the file today"
   — verified true).
4. `validate_and_apply_settings_patch` (src/config/settings_dto.rs
   :483-1046) is ALREADY lib-side ("pure domain logic, no I/O" — review
   M1b); the app's `save_settings` (ipc/settings.rs:1057-1136) is the
   orchestration: validate → `persist_and_reload` (config_io) →
   `set_safety` → `rewire_vision_embedder_and_classifier` →
   `sync_model_resolver` → factory setters (`set_core_operations`,
   `set_shell_filter_config`, `set_browser_inspection`) → trace setters
   (`set_memory_budget`, `set_request_body_cap`).
5. `rewire_vision_embedder_and_classifier` (src-tauri/src/ipc/rewire.rs
   :27-218, app crate) — callers: `save_settings` (settings.rs:1057),
   `resync_runtime_state` (settings.rs:270); callees include
   `alloc_free_port` + `build_managed_classifier` (ipc/laya.rs:395/:481)
   and 20 more. AppHandle-threaded today (emits as it goes).
6. The Laya core (ipc/laya.rs, 1411 lines): module constants `LAYA_REPO`
   (:61), `UV_RELEASES_API` (:64), `MANAGED_HOST` (:67), `SYSTEMONE_PATH`
   (:70), probe budgets (:74/:78); catalog `LayaCheckpoint` (:86-108,
   one English entry) + `english_checkpoint` (:112). `LayaManager`
   (:236-253) holds NO Tauri handle — fields: `base_dir`, shared
   `Arc<RwLock<ClassifierStatus>>`, `Mutex<Option<(Child, u64)>>`,
   `child_generation: AtomicU64`, `setup_in_flight: AtomicBool` — plus
   path helpers (:272-333: uv/venv/python/laya-serve/hf-cache/
   per-PID-server-log/markers/finetune-marker/installed-check) and the
   setup guard (:336-345). `laya_setup` (:1090-1107) = begin_setup →
   `spawn_setup_task` (:797) + `factory.clone()` (the rewire's rebuild).
7. The Laya pipeline (module doc :12-41): uv single-file binary (GitHub
   releases) → `uv venv --python 3.12` → `uv pip install "laya[serve]"`
   → checkpoint pre-download via the venv's python (`laya.load`,
   `HF_HOME` under the config dir) → runtime spawns `laya-serve` bound to
   `127.0.0.1:<free port>` with `LAYA_MODELS=<checkpoint>` → readiness
   probe on `POST /v1/systemone` (idle budget 120 s, absolute 900 s).
   Opt-in-gated: `[general.laya] enabled = true`, else nothing runs.
   Per-PID logs (`server-<pid>.log`) — concurrent instances sharing a
   config dir never truncate each other.
8. The classifier foundation is already lib-side
   (src/memory/classifier.rs — `Classifier`, `ClassifierStatus`,
   `LayaClassifier`; imported by ipc/laya.rs:55 from `mnemo::memory`).
   Progress rides the shared `ClassifierStatus` Arc today
   (`classifier://status`), the embedder analog `embedder://status`.
9. `embedder_cache_dir` (ipc/embeddings.rs:36-38) = `global_config_dir()
   .join("models")` — APP-side and GLOBAL: it ignores the engine's
   `config_dir` (a custom-config engine would download to `~/.mnemo`).
   The download: `download_bundled_model` → `spawn_download`
   (ipc/embeddings.rs:68+: build a `BundledEmbedder` — fastembed pulls on
   init — progress sampled from the cache-dir size vs `size_mb`). The
   catalog + `is_model_installed` + `BundledModelInfo`/`EmbedderStatus`
   are already lib-side (src/memory/embedder.rs:183/:310).
10. `EngineError` (src/headless/error.rs:15-117) is `{code, message}` —
    the CODE is the stable surface (row 15 pins `session_closed`);
    constructors are pub(crate): no_project/start_failed/spawn_failed/
    session_closed/memory_disabled/session_busy/session_parked/
    stats_failed/unknown_request. NO `not_configured` yet — §5.8's
    `NotConfigured { missing }` maps to code `not_configured` with the
    missing list in the message.11. Model listing (ipc/models.rs:1-308): `resolve_endpoint_credentials`
    (:30-36) — explicit override args first, then the saved endpoint
    (`endpoints.toml` base_url + kind, `keys.toml` key), then the kind-aware
    env fallback, then `"dummy"`; shared by `list_models` +
    `list_vision_models` so the pickers never drift. Domain logic, app
    crate — moves with D2/D6.
12. MCP (ipc/mcp.rs:1-360): `McpServerWire { def: McpServerDef
    (flattened), source: "global"|"project" }` — two scopes union-merged
    (project wins by name), saves SPLIT by source and write both files,
    each validated as its own set. `mnemo::config::mcp` is lib-side
    already; the wire + split logic is app crate.
13. Projects (ipc/projects.rs:1-309): `list_projects`/`get_needs_project`
    read `Config.projects`; switching writes a one-shot marker
    (`write_pending_project`, config/mod.rs) + `tauri::process::restart()`.
    `Project` is `mnemo::app::project::Project` (APP crate — 2b/2c moved
    it); `init_scaffolds_coding_dir` (app/project.rs:278) + the scaffold
    module (src/app/scaffold.rs) are app-owned. `Setup::init_project`
    (kernel) therefore needs the scaffold core MOVED kernel-side (§A).
14. `EngineOptions.memory: MemoryBackend {Sqlite{path}, Disabled}` already
    exists (types.rs:53-70, 3b) — G8's serde half. MISSING: the Rust-only
    `Engine::builder().memory_store(Arc<dyn MemoryStoreTrait>)` +
    `SharedResources` (§5.9.8 :727-731). Row 33: a custom store must
    receive the recall AND consolidation calls.
15. `Config` (config/mod.rs:17-57) = the four-file loader (config/
    endpoints/keys/mcp/projects .toml) with `recover_interrupted_write`
    (:68-70); `GeneralConfig` holds engine AND UI fields together (the
    G9 split target — `Setup`/`SettingsPatch` expose only EngineConfig,
    §5.9.9). `migrate_legacy_config_dir` (:501) + `global_config_dir`
    (:485) are lib-side already.
16. Verify commands (§8 :897-903): `cargo test --workspace` (the battery)
    plus `cargo tree -p mnemo -i tauri` (no tauri in the lib — a moved
    host module must not smuggle one in). `#![deny(warnings)]` at both
    crate roots — zero warnings or no green.

## C. The steps — nine landings, each a battery-green commit

Standing discipline (unchanged from 3a/3b/3c): every step lands with the
full workspace battery green (`cargo test` exit 0, zero warnings), commit
`feat(4.N): ...` on `wt/mnemo`; kernel modules never import `mnemo::app`
(the `kernel_does_not_import_app` boundary test stays green); the app
crate keeps building + behaving identically (the GUI's `[ui]` reads,
event names, and payloads are contract fixtures — event completeness U1);
tests use temp dirs only (real state lives in `~/.mnemo` — never touched).
The G4 close-grep (fact 2) must stay green in every landing.### D1 — the Setup foundation + install isolation + Engine::start preconditions

`src/config/mod.rs` gains `pub fn resolve_config_dir(explicit:
Option<PathBuf>) -> PathBuf` (explicit → `$MNEMO_HOME` → `~/.mnemo` —
engine.rs:629-633's logic MOVED, the engine delegates). NEW
`src/headless/setup.rs`: `Setup { config_dir, engine handles }`,
`open(config_dir: Option<PathBuf>)` (creates the dir + default files +
`migrate_legacy_config_dir`), `for_engine(&Engine)`,
`status(project: Option<&Path>) -> SetupStatus` (§5.8 :562-571:
`config_dir`, `endpoints: Vec<EndpointReadiness {name, has_key,
reachable}>`, `default_model`, `ready_to_run`, `embedder:
EmbedderReadiness`, `classifier: ClassifierStatus`, `project:
Option<ProjectReadiness>`, `missing: Vec<SetupStep>` — the ordered
first-run to-do), `SetupStep` enum, `SetupEvent` (:588-589:
Progress/Done/Failed). `src/headless/error.rs`: `not_configured`
(code `not_configured`, message = the missing list). `src/headless/
engine.rs`: the start precondition (§5.8 :581-587) — `ready_to_run ==
false` → `not_configured`; a missing EMBEDDER model is NOT fatal (hash
fallback + `semantic: false`, row 33's spirit). `src/headless/types.rs`:
`EngineOptions.auto_install_embedder` (serde default FALSE — no surprise
~100 MB downloads in a host process). Fixtures
(`tests/integration/headless_setup.rs`): open() creates dir + files;
status() on a fresh config reports `ready_to_run: false` + the ordered
missing list; Engine::start unready → code `not_configured`. Acceptance:
battery green + the G4 close-grep green (fact 2).

### D2 — keys: Secret + KeySource + the endpoints group

`src/config/keys.rs`: `Secret` — zeroize-on-drop newtype, NO Debug/
Serialize of the value (probe the `zeroize` dep at execution; add it if
absent); `KeySource::{File, Env(String), InMemory(HashMap)}` —
resolution: env/in-memory NEVER write plaintext to disk (§5.8 :577-580).
`Setup`: `endpoints()` (EndpointDto list), `upsert_endpoint` (via
`validate_endpoint`/`apply_endpoints` — config/patch.rs, already lib),
`remove_endpoint`, `set_api_key(Secret)` (write-only), `has_api_key`
(never a value), `list_models(endpoint)` (probe the endpoint — the
credential resolution `resolve_endpoint_credentials` MOVES from
ipc/models.rs), `test_endpoint`. ipc/models.rs + the endpoint commands in
ipc/settings.rs thin over Setup. Fixtures: set→has true, NO getter
exists; a bad upsert is refused and leaves the config unchanged; an
Env-sourced key serves a provider call without touching disk.

### D3 — G9: the EngineConfig/UiConfig split + SettingsPatch

`src/config/general.rs` + mod.rs: the split (§5.9.9 :733-738) —
`EngineConfig` (providers/endpoints, keys source, models incl. `free`,
memory, budget, optimizer, verify, shell filter, context, trace,
safety-mode default) vs `UiConfig` (`[ui]` theme/show_* — app-owned, read
ONLY by src-tauri). ON DISK NOTHING MOVES — same files, same tables;
the app owns `[ui]`. `SettingsPatch` (settings_dto.rs): the lib-side
patch type with NO `[ui]` fields — compile-time absent (row 33 leg 2).
`Setup::settings()`/`patch_settings()` expose ONLY EngineConfig.
src-tauri keeps reading `[ui]` unchanged (its own read path, same TOML).
Fixtures: SettingsPatch has no `[ui]` field to name; the GUI's `[ui]`
reads still work over the same file (the app-side fixture test stays
green unmodified).

### D4 — the rewire move + patch_settings live-apply

NEW `src/host/rewire.rs`: `rewire_vision_embedder_and_classifier` MOVED
from ipc/rewire.rs (:27-218) — AppHandle-FREE signature (the pieces it
rewires — factory/vision slot/embedder/classifier handle/resolver — plus
a progress callback instead of emits; facts 4-5). `Setup::
patch_settings(patch)`: validate → persist → set_safety → the MOVED
rewire → sync_model_resolver → factory setters → trace setters (fact 4's
exact sequence, one implementation). ipc/settings.rs: `save_settings`
thins to state glue + `persist_and_reload` + the moved path;
`resync_runtime_state` delegates too. The Laya handles the rewire needs
(build_managed_classifier) arrive with D5 — D4 threads the moved
non-Laya parts and keeps a seam for them (probe the actual call graph at
execution; the split lands without behavior change). Fixtures: a
patch_settings on a LIVE engine applies on the next request (the same
behavior today's IPC tests pin, driven through Setup); the app's
save_settings tests green unmodified.

### D5 — the Laya core move (D3, the headline) + classifier_setup

NEW `src/host/laya.rs`: the manager + constants + catalog + path
helpers + setup pipeline (uv download → venv → laya[serve] → checkpoint
via HF_HOME) + child spawn + readiness probe + markers (facts 6-7) —
TAURI-FREE: the shared `Arc<RwLock<ClassifierStatus>>` stays the status
carrier; progress surfaces via a callback (the app's emits stay
app-side). `Setup::classifier_setup(enable) -> EventReceiver<
SetupEvent>` (§5.8 :554). ipc/laya.rs: the commands become thin glue
(IpcState holds the lib manager + emits). Fixtures: the moved pipeline's
tests (`with_base` tempdir) stay green; enable=false no-ops; `cargo tree
-p mnemo -i tauri` finds NOTHING (fact 16). Acceptance: the whole app
crate still builds + the classifier flow unchanged (the app-side tests).### D6 — embed download + the embedding/vision surface

NEW `src/host/embed_download.rs`: the download MOVED from
ipc/embeddings.rs (:68+ `spawn_download` — build a `BundledEmbedder`,
fastembed pulls on init, progress sampled from the cache-dir size); the
cache dir DERIVES FROM `config_dir` (fact 9's global coupling dies —
`<config_dir>/models`, matching brain.rs:1213's engine-side derivation).
`Setup`: `embedding_models()` (catalog + installed flag + size),
`install_embedding_model(id) -> EventReceiver<SetupEvent>`,
`install_embedding_model_from(id, dir)` (air-gapped copy — NO network in
tests), `select_embedding_model(id)` (config + live swap + re-embed),
`embedder_status()`, `reembed()`, `vision_model(Option<String>)`.
ipc/embeddings.rs thins. Fixtures: the catalog flags installed against
the config-dir-derived cache; install_from a fixture dir selects without
network; `auto_install_embedder` default FALSE pins no surprise
downloads (D1's flag honored here).

### D7 — G8: memory_store + SharedResources

`src/headless/engine.rs` builder (Rust-only): `memory_store(Arc<dyn
MemoryStoreTrait>)` — overrides the `MemoryBackend` wiring when present
(the enum stays the serde wire, §5.9.8 :726-728); `SharedResources`
(:729-731) — embedder, HTTP client pool, model catalog cache, shared
across engines in ONE process so N tenants don't load N ONNX copies.
Row 33 leg 1 fixture: a custom `MemoryStoreTrait` receives the recall
AND consolidation calls (a scripted engine run over the injected
store). Companion: two engines built over one `SharedResources` reuse
the same embedder (the swap/rebuild path honors the shared slot).

### D8 — the project group + MCP + agent.md + safety rules

`src/project/scaffold.rs` (kernel): the scaffold core MOVES from
`src/app/scaffold.rs` (app keeps re-exports — the 2b pattern), so
`Setup::init_project` needs no `mnemo::app` import. `Setup`:
`init_project(root, InitProjectOptions { register })` (kernel init +
scaffold + the agent.md template), `agent_md(scope: Scope)/
set_agent_md`, `safety_rules/set_safety_rules` (ApproveViaApi's rules,
§5.8 :559-560), `mcp_servers()/save_mcp_servers/test_mcp` (fact 12's
wire + split-by-source moves lib-side; `mcp_oauth_start` — probe
ipc/mcp.rs at execution, move what exists). ipc/{projects,mcp}.rs thin
(fact 13: the marker + restart glue stays app-side — `tauri::process::restart`
is the app's). Fixtures: init_project scaffolds + registers in
`projects.toml` (temp dir); agent_md round-trips Global | Project(path);
MCP saves split by source exactly as today.

### D9 — verify + boundary commit + review rounds

Full battery (`cargo test` exit 0, zero warnings) + `cargo tree -p
mnemo -i tauri` (empty — fact 16); §F landing notes complete; the
boundary commit; reviewer rounds to PASS; `finish` → the master tick
(step 10/11). Acceptance row 33 all three legs: custom store receives
recall+consolidation ✓ (D7), SettingsPatch cannot set `[ui]` (compile-
time absent) ✓ (D3), the GUI still reads `[ui]` unchanged ✓ (D3).

## E. Deviations + notes to watch (authoring-time)

- `EngineError` is `{code, message}` (fact 10) — the spec's
  `EngineError::NotConfigured { missing }` maps to code `not_configured`
  + the missing list in the message. Row 15's pattern (codes stable,
  message detail) wins — NO struct change.
- `Setup` holds NO direct brain handles for a live engine: it threads
  through the engine's OWN config/factory slots (the 3c F1 lesson:
  never write through a SHARED slot — per-Setup handles are clone-local
  or read-only).
- The rewire's Laya dependency (build_managed_classifier, fact 5) is
  split across D4/D5: D4 moves the non-Laya rewire with a seam, D5 fills
  it — both land with the battery green (no dead code: `#![deny(
  warnings)]` forbids ahead-of-use fns).
- `zeroize` may be a NEW dependency for `Secret` (D2) — a PLAIN dep,
  not feature-gated (the config module is kernel-default); check
  Cargo.toml first.
- The G4 close-grep (fact 2) is the hard guard: `setup.rs` takes its
  dir EXPLICITLY and delegates the default resolution to `config/`.

## F. Landing notes (appended per step during execution)

(none yet)### D1 — landed (feat(4.1)): the Setup foundation

- `src/headless/setup.rs` (NEW): `Setup::open`/`status`, `SetupStatus` /
  `SetupStep` / `SetupEvent` / `EndpointReadiness` / `EmbedderReadiness` /
  `ProjectReadiness` — the §5.8 :562-571 readiness report with the
  ordered missing list (the fatal `DefaultModel` leg first, then the
  optional `EmbedderModel` + `Project` legs). `status_from_config` is
  the shared computation (the engine passes its ALREADY-LOADED config —
  no double load).
- `src/config/mod.rs`: `resolve_config_dir` (explicit → `$MNEMO_HOME` →
  `~/.mnemo` — the ONE resolver; engine.rs delegates,
  `default_config_dir` deleted) + `embedder_cache_dir` (in `config/` so
  the G4 close-grep stays green; D6 re-derives it from the engine's
  config dir).
- `src/headless/engine.rs`: the §5.8 :581-587 precondition — WITHOUT an
  injected provider an unready config fails fast with error code
  `not_configured` (the missing list in the message); an INJECTED
  provider (tests, hosts that bring their own transport) bypasses the
  gate — the provider IS the transport, so the config's endpoint/key
  wiring is not load-bearing for it.
- Deviation (documented): `Setup::for_engine` defers to D4 —
  `deny(warnings)` forbids a dead engine field; D1's `Setup` is the
  pure config-dir handle and `for_engine` lands with the first
  live-apply method that consumes it.
- Discovery (fixture-pinned): a FRESH config's
  `bundled_embedding_model` defaults to the lightest bundled model
  (the ~90 MB silent-download the config docs warn about) — `"hash"`
  is the explicit opt-OUT, not the default. A fresh install therefore
  genuinely carries the optional `EmbedderModel` step; the ready-config
  fixtures opt out with `"hash"` to isolate the leg under test.
- The three bare-start fixtures (G3 plain-dir, row-27 state-dir,
  row-15 `start_blocking`) now seed a ready config (endpoint + key +
  default model) — the gate is real for them; the row-9 `no_project`
  legs gate earlier (the workspace gate) and stay untouched; the
  provider-injecting engine tests (rows 17-32) bypass by design.
- Battery: workspace green, exit 0 (60 integration tests incl. the six
  new headless_setup fixtures).### D2 — landed (feat(4.2)): Secret + KeySource + the endpoints group

- `src/config/keys.rs`: `Secret` — the write-only zeroize-on-drop wrapper
  (manual `Debug` printing `[REDACTED]`, NO serde by design — a binding
  receives the plain string on its wire and wraps it here); `KeySource::
  {File, Env, InMemory}` — `File` (the default) is the only persistent
  source; `Env`/`InMemory` are host-managed READ sources (`has_key`
  consults file/env/map and never creates a file; the WRITE paths refuse
  them — inject at construction). New plain dep `zeroize = "1"`.
- `src/provider/models.rs`: `resolve_endpoint_credentials` +
  `fetch_by_kind` MOVED from ipc/models.rs — the GUI pickers and the
  Setup surface share ONE implementation; the kind-aware env fallback
  chain (ANTHROPIC_API_KEY / OPENAI_API_KEY / ANTHROPIC_AUTH_TOKEN →
  `"dummy"`) rides along.
- `src/config/endpoints.rs` (+ the mod.rs re-export): `kind_wire` — the
  `EndpointKind` ↔ wire mapping, one implementation (the app's
  `endpoint_kind_wire` delegates).
- `src/headless/setup.rs` (the D2 group): `endpoints()` — the lean
  `EndpointDto` wire (essential fields + `has_api_key` OUTPUT-only; the
  GUI's card DTO keeps its extra fields and validates through the same
  lib `validate_endpoint`); `upsert_endpoint` — validate →
  replace-or-append by name → the SET re-validation (the default
  provider/model must still resolve) → `apply_endpoints` → `save_all`,
  with `key: None` PRESERVING the endpoint's existing key;
  `remove_endpoint` — the key drops with it (the rebuilt store keeps
  keys for surviving endpoints only), the default provider's endpoint is
  guarded; `set_api_key` — write-only (no getter exists), `File` source
  only; `has_api_key` — never a value; `list_models` / `test_endpoint` —
  the shared resolution + the kind-selected fetcher (`test_endpoint`
  reports bool; `list_models` surfaces the detail). Plus
  `with_key_source` and the `load_config` dedup.
- IPC: ipc/models.rs's two helpers are thin delegates (the `IpcError`
  mapping stays app-side); settings.rs's `endpoint_kind_wire` delegates
  to the lib. The endpoint COMMANDS keep their persist+resync glue —
  D4 lands the moved rewire and the commands thin then.
- Fixtures (headless_setup.rs, +5): the set→has round trip + the
  persisted file; Env/InMemory reads refuse writes and never create
  `keys.toml`; upsert validates/persists/preserves keys and a refused
  upsert changes nothing; remove drops the key + the default-provider
  guard; the credential chain (overrides win, saved-endpoint fallback,
  unknown endpoint errors).
- Battery: workspace green, exit 0 (65 integration tests).### D3 — landed (feat(4.3)): G9 — the EngineConfig/UiConfig split

- The lib patch (`SettingsSaveDto`, settings_dto.rs) LOSES all 14 `[ui]`
  fields (theme, the show_*/chat_*/sound_* toggles, steering_notes) +
  the theme validation + the ~75-line `[ui]` application block —
  compile-time absent (row 33 leg 2: "SettingsPatch cannot set `[ui]`
  fields"). A wire still carrying them deserializes (serde drops
  unknown keys) and the apply leaves `[ui]` untouched — pinned by
  `ui_fields_are_absent_from_the_engine_patch` +
  `ui_table_survives_an_engine_patch` (the G9 on-disk invariant: a
  standalone consumer's engine-side save never nukes the GUI's theme).
- The app owns `[ui]` (ipc/settings.rs): `SettingsSaveWire` = the
  flattened engine patch + `SettingsUiPatch` (the 14 UI fields) — the
  FRONTEND PAYLOAD SHAPE IS UNCHANGED (the split is at the type level
  only); `apply_ui_patch` (moved from the lib) applies the `[ui]` half
  app-side after the engine patch, with the theme validation BEFORE any
  application (no partial writes). The moved per-kind steering-notes
  logic is covered by the app-side test
  `ui_patch_applies_per_kind_and_validates_theme`.
- `Setup::settings()` (§5.8 :539) exposes ONLY `EngineConfig` — the
  `[general]`/`[models]`/`[context]`/`[trace]`/`[memory]`/`[shell_filter]`
  sections + the endpoints DTO; the `[ui]` table has no field to name.
  Fixture: `settings_exposes_engine_config_and_preserves_ui`
  (headless_setup.rs). `endpoint_dto` dedupes the DTO mapping
  (`endpoints()` + `settings()` share it).
- Battery: workspace green, exit 0 (66 integration tests).### D4 — landed (feat(4.4)): the rewire move + patch_settings live-apply

- NEW `src/host/rewire.rs` — the AppHandle-free rewire core MOVED from
  ipc/rewire.rs: `RewireTargets` (factory / memory_store /
  embedder_status / model_resolver — every handle optional, a no-op
  when absent), `rewire_vision_embedder` (the vision rebuild + the
  embedder rebuild + the factory's eight live toggles — the classifier
  FLAGS from `[general.laya]`, the optimizer/verify/budget configs) +
  `sync_model_resolver`. The re-embed future RETURNS to the caller
  (the app spawns on `tauri::async_runtime`, the headless Setup on
  tokio) — a full re-embed never blocks the save call. The embedder
  cache dir derives from `config::embedder_cache_dir()` (D1's helper —
  one derivation).
- ipc/rewire.rs is the thin app wrapper: the lib core + the app-owned
  pieces (the managed-CLASSIFIER leg — the Laya runtime, D5's move
  target — the re-embed spawn policy, the `classifier://status` emit).
  The GUI save path's behavior is byte-identical (the app tests stay
  green).
- `Setup::for_engine` + `Setup::patch_settings` (§5.8 :527/:540):
  `for_engine` Arc-clones the brain's shared handles (factory / store /
  embedder status / resolver / safety mode / trace / config) — the Setup
  never owns the engine; `patch_settings` = validate (the same lib
  rules the GUI's save uses) → persist → the live-apply sequence
  (safety mode → the moved rewire → resolver →
  core-operations/shell-filter/browser-inspection → the trace limits →
  the brain's shared config). Without a live engine only the persist
  happens. `Engine::brain()` (pub(crate)) is the new accessor.
- The managed-CLASSIFIER leg of the live-apply stays app-side until D5
  (documented in `patch_settings`'s doc comment); a standalone engine
  has no managed sidecar yet.
- Fixtures: `patch_settings_live_applies_through_for_engine` (in-crate
  unit — the safety flag + the shared-config swap, no restart) +
  `patch_settings_persists_without_an_engine` (integration — the
  persist + the ready state survives).
- Battery: workspace green, exit 0 (67 integration tests + the new
  unit test).### 4.5 — D5 landed (2026-10-13): the Laya core move (D3) + classifier_setup

The headline move (§5.1 :168 "the Laya manager's Tauri-free core moves
in job 4") — the manager + the pipeline are host-agnostic now; the app
keeps only glue. `cargo tree -p mnemo -i tauri` finds NOTHING (§B fact
16, re-proven after the move) — the lib never names tauri.

- NEW `src/host/laya.rs` (~54 KB incl. the moved tests): the constants
  + catalog (`LayaCheckpoint`, `checkpoint_catalog`, `english_checkpoint`)
  + the pure helpers (`uv_asset_name`/`exe_name`/`venv_bin`/`server_env`/
  `preload_program`/`probe_body`/`probe_url`/`progress_fraction`/`dir_size`/
  `hide_console_window` — now `pub`, finetune.rs imports it) +
  `LayaManager` (the path helpers, the setup guard, `alloc_free_port`,
  `spawn_server` with the generation-tagged child slot, the scoped
  `stop_if_generation`/`owns_generation`) + `kill_child` + `Drop` +
  `fresh_disabled` + `build_managed_classifier` + the pipeline
  (`ensure_uv` with the GitHub digest verify + the atomic scratch
  extract, `run_setup`'s venv/install/checkpoint-preload + the 500 ms
  dir-size progress poller, `is_setup_progress`,
  `setup_autostart_decision`, `spawn_setup_task`, `wait_until_ready`
  with the idle-120 s/absolute-900 s budgets, `start_server_models`,
  `start_managed_server`) + the tests mod moved wholesale (16 tests).
- The Tauri seam: `StatusSink = Arc<dyn Fn(&ClassifierStatus) + Send +
  Sync>` — `push_status`/`emit_download` write the shared status Arc
  AND push; every former `&Option<AppHandle>` param + `app.emit` pair
  is the sink now. The two `tauri::async_runtime::spawn` sites are
  `tokio::spawn` (the app's runtime IS tokio — the wrappers still work).
- The app's `ipc/laya.rs` is the thin layer (5.7 KB): `status_sink`
  (forwards to `classifier://status`) + wrappers keeping the pre-D5
  `Option<AppHandle>` signatures (`start_managed_server`/
  `start_server_models`/`spawn_setup_task`) + re-exports — main.rs,
  console.rs, finetune.rs, rewire.rs, state.rs compile UNCHANGED.
- Deps: + `zip = "8"`, `tar = "0.4"`, `flate2 = "1"` (plain; the uv
  release asset extraction). `anyhow`/`sha2`/`reqwest`/`tempfile` were
  already lib deps.
- The Setup surface (§5.8 :554): `SetupStep::LayaRuntime` (pipeline
  tagging only — never in the missing list); `LiveHandles.classifier_slot`;
  `Setup.laya` (an `Arc<LayaManager>` rooted at `<config_dir>/laya` —
  install isolation) + `Setup.classifier_status` (a fresh `Disabled`
  Arc; `for_engine` re-roots BOTH to the brain's); `laya_manager()` pub;
  `classifier_setup(enable) -> EventReceiver<SetupEvent>` (a broadcast
  channel + the `StatusSink` mapping `ClassifierStatus` → `SetupEvent`).
  The flip persists BEFORE the guard claim (a failed save never wedges
  the guard); disable = stop + `Disabled` + a single terminal `Done`;
  enable = guard (`EngineError::setup_in_flight`, the new stable code)
  + spawn with the LIVE config Arc (a mid-setup save still wins at the
  autostart re-read) + the classifier-slot swap.
- `Setup::status` now surfaces the LIVE classifier status (D1's
  promise for D5): an enabled-but-not-running install reads `Disabled`
  — honest, no call is attempted. `status_from_config` keeps the static
  approximation for the engine's start gate. `patch_settings`'s doc
  now states the split: the classifier flip rides `classifier_setup`
  (the GUI's Download button has the same split from the settings save).
- `LayaManager` gained a manual `Debug` (`Setup` derives `Debug`).
- Fixtures (3): `classifier_setup_disable_persists_and_reports_done`
  (the Done event + the Closed channel + the persisted flip + the live
  status), `classifier_setup_rejects_a_second_enable_with_setup_in_flight`
  (the guard + the idempotent flip), `laya_manager_roots_at_the_config_dir`
  (G4 isolation). The enable pipeline is network-bound — never
  exercised in tests (the app's own suite never tested it either).
- En-route fixes: `mnemo::config::Config` → `crate::config::Config`
  (the in-crate path), `read(&mut buf)`, `&sink` (not `.as_ref()` —
  `start_managed_server` takes `&Option`, the async task owns the
  `Option`), the moved test's `("linux","aarch64")` expectation typo
  (caught by the battery), and the fixture assertions: the round-trip
  goes through `settings().general.laya.enabled` (ONE level —
  `GeneralSection` carries the fields; `Config.general` is the
  `GeneralConfig` wrapper holding `[ui]` beside it) because the
  serializer ELIDES the all-default `[general.laya]` section (absent
  == disabled, the app's opt-in convention).
- Battery: workspace green, exit 0 — the integration harness at 70
  tests (67 → 70), the 16 moved app tests now run as lib unit tests.
### 4.6 — D6 landed (2026-10-13): embed download + the embedding/vision surface

The second core move: the download/load pipeline is host-agnostic now.
Verified BOTH ways: the plain workspace battery (exit 0; integration
70 → 75) AND the feature-enabled run `cargo test --features embeddings`
(exit 0 — the gated unit tests run: the late-arrival guard, the
catalog, the hash-sentinel paths).

- NEW `src/host/embed_download.rs` (~18.2 KB): `EmbedderStatusSink`
  (the Tauri seam — `Arc<dyn Fn(&EmbedderStatus) + Send + Sync>`;
  `push_status` replaces every write + `app.emit` pair) +
  `EmbeddingModelDto` + `embedding_model_catalog(cache_dir)` (the list
  gated, the empty-catalog arm ungated) + `copy_model_from` /
  `copy_dir_recursive` (the AIR-GAPPED install §5.8 :544 — plain fs,
  validates the HF layout `models--<org>--<name>/snapshots/<rev>/
  model_quantized.onnx`, every build) + the gated core
  (`cache_dir_progress`, `DownloadOutcome { StatusOnly,
  SwapIntoStore(store) }`, `install_allowed` + `install_embedder`
  (the late-arrival guard + the fingerprint check + re-embed + swap,
  with the 2026-08-21 review-finding history), `run_startup_load`,
  `spawn_startup_load_task`, `spawn_download` (the 500 ms poller with
  the poller-abort-BEFORE-failure-propagation rule — the laya rule).
  `dir_size` is shared from `host::laya` (pub(crate), one impl).
- The cache-dir re-derivation (§B fact 9): `config::embedder_cache_dir()`
  → `embedder_cache_dir(config_dir: &Path)` — `<config_dir>/models`.
  Callers: the app's ipc/embeddings.rs (the global dir — byte-identical
  GUI behavior), `RewireTargets.config_dir` (NEW optional field; the
  app passes `None` = the global fallback, the Setup passes its own),
  `status_from_config`, and `Brain::spawn_startup_embedder_load` (NEW
  `cache_dir: &Path` param — engine.rs passes its resolved config dir
  via a pre-`wire_brain` clone; the no-feature twin's signature
  matches). `hf_cache_subdir` is un-gated (a pure string map — the
  air-gapped copy needs it in every build).
- The Setup surface (§5.8 :542-549): `embedding_models()` (installed
  flags against THIS Setup's cache — the global coupling died),
  `install_embedding_model(id) -> EventReceiver<SetupEvent>` (the
  network download; the no-feature arm REFUSES with the honest
  message), `install_embedding_model_from(id, dir)` (air-gapped),
  `select_embedding_model(id)` (validates: "hash" or a catalog id;
  rides `patch_settings` — persist + the rewire's rebuild + re-embed),
  `vision_model(Some(dto)/None)` (set/clear via the patch's
  `vision_model`/`clear_vision_model`), `embedder_status()` (LIVE —
  the brain's Arc when engine-backed), `reembed()` (the live-store
  swap re-run without a config change; needs `for_engine`; the
  no-feature arm refuses), `embedder_status_to_event` (gated — its
  callers are). `Setup.embedder_status_arc`: fresh `Checking`,
  `for_engine` re-roots to the brain's. `EmbeddingModelDto` re-exported
  from `headless`.
- The app's `ipc/embeddings.rs` is the thin layer (6.4 KB):
  `status_sink` (`embedder://status`), the wire mapping, the catalog +
  download commands (StatusOnly — the settings-save rewire swaps the
  live embedder, the pre-D5 split unchanged), and the startup twins
  (`spawn_startup_download` SwapIntoStore, `spawn_startup_load`,
  `spawn_startup_load_console` with `None`). main.rs / console.rs
  compile UNCHANGED.
- Fixtures (5): `embedding_catalog_flags_track_the_config_dir_cache`
  (fresh = nothing installed; the doctored HF tree flips the flag),
  `install_embedding_model_from_copies_into_the_config_dir_cache` (the
  copy lands + unknown ids reject),
  `select_embedding_model_persists_and_validates` ("hash" persists;
  unknown rejects clean), `vision_model_set_and_clear_ride_the_settings_patch`
  (set validates the endpoint, None clears),
  `no_surprise_downloads_at_the_setup_surface` (a fresh open + a
  status read stays `Checking` — the behavioral pin for D1's
  `auto_install_embedder: false`; `EngineOptions` has no `Default`,
  so the flag is pinned by construction sites, not a `::default()`
  assert).
- En-route fixes (8 compile errors + fixture shapes): the re-export
  chain (`pub use crate::host::embed_download::EmbeddingModelDto` — a
  private re-import doesn't re-export), the cfg-gated `use`s
  (`EmbedderStatusSink`/`RwLock` unused ungated), the pre-move
  `config_dir.clone()` for the load call (wire_brain consumes it),
  `map(|_| ())`, the `Option::unwrap_or_else` PathBuf mismatch → a
  `match`, the no-feature `let _ = &cache_dir`, the gated mapper, and
  the fixtures (`write_ready_config(dir)` takes the dir and returns
  `()`; one `EmbedderStatus` import was already present from D1).