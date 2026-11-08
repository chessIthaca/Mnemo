+++
title = "Mnemo2 frontend invoke strings are cross-checked against the Tauri command list in Rust"
created = "2026-11-07"
+++

`src-tauri/src/mnemo2/mod.rs`, in its `#[cfg(test)] mod tests`, cross-checks the Tauri command list against the frontend: `registered_commands(include_str!("mod.rs"))` parses the real `generate_handler![ ... ]` list (first `![` to first `]`, comma-split, `//` comments stripped, last `::` segment kept), `invoked_commands` scans every non-test `.ts`/`.svelte` under `mnemo2/src` (skips `node_modules`, `*.test.ts`, `*.d.ts`) for literal `invoke[<T>]( "<name>" ...)` only — plugin calls (`"plugin:dialog|open"`), dynamic args and `reinvoke(...)` are skipped, and a `>` directly after `=` does not close the generic (so `invoke<() => void>` works). Test `every_frontend_invoke_string_is_a_registered_command` requires both sides >= 100 entries (a moved list or changed call shape fails loudly, never passes vacuously) and names `file: name` for unregistered strings.
Practical consequences: renaming a Tauri command means updating the `generate_handler` entry AND the frontend call string; new fixture literals in that test module must never spell `generate_handler![` contiguously (build with `concat!`) because the parser takes the FIRST occurrence in the file. Tree reference points: 139 registered commands, 137 invoked; `routing_log_summary` and `team_audit_dispatch` are registered but have no UI caller.
Run: `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::tests`.
