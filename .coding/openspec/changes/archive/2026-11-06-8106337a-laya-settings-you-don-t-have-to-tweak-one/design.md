## Decisions

- 2026-11-05: added step 6 (Integration check) because steps 1 and 2 ran in parallel

## Interfaces

- 2026-11-05: `IndexingSnapshot.laya`: Rust `Option<IndexPass>` serialized as `laya`, TS `laya?: IndexPass | null`. Same pass shape and the same `mnemo2://indexing` channel as code and memory. Step 1 owns the Rust side, step 2 the TS type; steps 3 and 4 only read them.
- 2026-11-05: `LeadRuntime::start_laya_check(&self, force: bool) -> Result<(), String>` is the one way to start a check. Step 3 owns it. `force=false` silently skips when routing is off, Laya is down or the fingerprint matches.
- 2026-11-05: Tauri command `laya_check_start` (no arguments) is the Check again button's backend, and `save_routing_settings` and `run_routing_eval` no longer exist. Step 3 owns the commands; step 4 calls `laya_check_start` through `LayaTransport.checkLaya()`.
- 2026-11-05: `save_laya_settings` keeps its signature; step 4 sends one flag at a time with `routing: null`. `routing_settings_status` keeps its shape, and its `eval` record gains an optional `fingerprint`.
