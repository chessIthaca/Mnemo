Mnemo2 gains the six features only Mnemo1 had: memory cleanup and rebuilds in the Memory tab, a freeze report, a remembered window size and position, images from tool calls in the chat, steering stats in the Trace tab, and an About page with the version.

WHY: a feature comparison of Mnemo1 (React app in frontend/, Tauri binary src-tauri/src/main.rs + src-tauri/src/ipc/**) and Mnemo2 (Svelte app in mnemo2/, Tauri host src-tauri/src/mnemo2/**, binary src-tauri/mnemo2-app) found six user-facing features that exist only in Mnemo1. The user asked for all six to be ported. Port the BEHAVIOUR; do not port React code. Each step names the Mnemo1 file to read for reference.

The six features and their Mnemo1 sources:
1. Memory upkeep — Mnemo1 Settings > Memory & Search has Clean up memory / Rebuild semantic search / Rebuild derived index (src-tauri/src/ipc/memory_maintenance.rs, frontend/src/components/settings/sections/MemorySection.tsx:340, maintenanceUi.ts). The engine already exposes all three: mnemo::headless::MemoryApi::{cleanup, rebuild_search, rebuild_index} (src/headless/memory_api.rs:339-387).
2. Freeze report — src-tauri/src/watchdog.rs writes hang-<ms>.txt when the main thread stalls >10 s. It is declared only in the Mnemo1 binary (src-tauri/src/main.rs:25 `mod watchdog;`), so Mnemo2 has none.
3. Window size and position — Mnemo1 restores them with a clamp (frontend/src/lib/windowRestore.ts: min 800x560 logical, fully on a monitor's work area, centred when less than 80 px overlaps). Mnemo2 always opens 1440x900 centred (src-tauri/src/mnemo2/mod.rs install_shell).
4. Images from tool calls — Mnemo1 shows screenshot and image-tool images inside the tool card with a full-size viewer (frontend/src/components/chat/ToolImage.tsx). Mnemo2 only lists worker screenshots as links (mnemo2/src/components/WorkerThread.svelte:151).
5. Steering stats — Mnemo1's Trace view shows how often each steering note fired and was followed, and file edits by vehicle (src-tauri/src/ipc/trace.rs:131 get_steering_stats, frontend/src/components/views/LlmTraceView.tsx:1125). The counters live on the engine's loop factory: engine.host().factory.steering().snapshot() (src/agent/steering_stats.rs SteeringStatsSnapshot, re-exported as mnemo::SteeringStatsSnapshot).
6. Version / About — Mnemo1 has an About dialog (frontend/src/components/about/AboutDialog.tsx + dependencies.ts). Mnemo2 shows its version nowhere.

CONSTRAINTS AND DECISIONS (settled, do not revisit):
- No new settings switches. Mnemo1's on/off switches for these features (for example "Show images from image tools in chat") are NOT ported; the features are simply on.
- Mnemo2 UI text is short and plain: no jargon, no internal ids. Colours are CSS tokens from mnemo2/src/app.css (var(--line), var(--muted), ...); never hard-code colours.
- Mnemo1 behaviour must stay the same. The only Mnemo1-side change is that src-tauri/src/watchdog.rs moves from the binary into the src-tauri lib crate (mnemo_app) so both apps can use it.
- Each new frontend transport goes in its OWN new file under mnemo2/src/lib/ (pattern: createXTransport() returns null outside the desktop app, checked with isTauriRuntime() from mnemo2/src/lib/runtime.ts). Do not add methods to existing transport interfaces (MemoryTransport, TracePageTransport), because existing tests and previews build fakes of them.
- Every new Tauri command must be registered in the generate_handler! list in src-tauri/src/mnemo2/mod.rs; the test every_frontend_invoke_string_is_a_registered_command fails otherwise. Call commands with a literal string: bindings.invoke<T>("command_name", {...}) so that guard sees them. Tauri maps camelCase argument keys (workerId) to snake_case parameters (worker_id).
- Host events go out with tauri::Emitter (app.emit(CHANNEL, payload)); the frontend subscribes with listen from @tauri-apps/api/event (pattern: mnemo2/src/lib/backlog.ts:86).
- src-tauri/src/lib.rs has #![deny(warnings)].
- The lead and every worker are spawned from ONE engine (src-tauri/src/mnemo2/runtime.rs:976 and team/spawn.rs:387), so engine.host().agent_loops and engine.host().factory cover the whole team.

VERIFICATION COMMANDS (run the ones your step touches; all must pass):
- Rust core: cargo test -p mnemo --lib agent::factory
- Mnemo2 host (needs the feature, plain runs compile the module out): cargo test -p mnemo-app --features mnemo2 --lib mnemo2::<module>
- Watchdog (unconditional lib module): cargo test -p mnemo-app --lib watchdog
- Command registry guard: cargo test -p mnemo-app --features mnemo2 --lib mnemo2::tests
- Builds: cargo build -p mnemo-app (Mnemo1 binary) and cargo build -p mnemo2-app
- Lint, with AND without the feature: cargo clippy -p mnemo-app --all-targets -- -D warnings ; cargo clippy -p mnemo-app --features mnemo2 --all-targets -- -D warnings ; cargo clippy -p mnemo --all-targets -- -D warnings when src/ changed
- Frontend, from mnemo2/: npx vitest run <your test files> ; npm run check (svelte-check) ; npm test (whole suite once at the end)
