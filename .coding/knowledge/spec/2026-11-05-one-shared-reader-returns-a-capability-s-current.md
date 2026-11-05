+++
title = "one shared reader returns a capability's current OpenSpec spec"
created = "2026-11-05"
+++

`read_capability_spec(openspec_dir, capability) -> Result<Option<String>, String>` in src/app/openspec/capability_spec.rs is the single reader for `.coding/openspec/specs/<capability>/spec.md`. Callers: PlanBoard::capability_spec (worker plan_read tool) and the Tauri command `read_capability_spec` in src-tauri/src/mnemo2/specs_settings.rs (`invoke<string|null>("read_capability_spec", { capability })`, used by the worker panel Plan tab spec folds). Name must be kebab-case ^[a-z0-9]+(-[a-z0-9]+)*$ else Err("invalid capability name ..."); missing file -> Ok(None); a linked folder or spec.md is refused (symlink_metadata, catches Windows junctions); other io errors name the path.
