+++
title = "link-time levers 1-3 landed + measured (plan 1c712c30)"
created = "2026-09-01"
+++

Link-time levers 1-3 LANDED (plan 1c712c30, 2026-12-19), full baseline in .coding/knowledge/link-time-research.md:
- #1 Cargo.toml: [profile.dev.package."*"] debug=false + [profile.test.package."*"] debug=false (workspace code keeps line-tables-only; deps lose debuginfo → smaller link input + PDBs for every binary).
- #2 Defender exclusions target\ + ~/.cargo + ~/.rustup added via UAC one-shot; re-add after machine wipe (unverifiable non-admin; marker target\defender-exclusions.done — cargo clean wipes it, re-create by hand).
- #3 cargo clean blocked by running app's locked target\release\mnemo-app.exe — equivalent manual wipe of target\ (minus locked exe). From-clean cargo build --workspace = 93.2s. Full cargo test --workspace green, ~27s wall on cached build (lib suite 5.2s).
Final apples-to-apples warm-loop table comes at plan step 8 after levers 4-6.
