+++
title = "Make active on Settings > Specs wipes governed_files from .coding/openspec/config.yaml"
created = "2026-11-08"
+++

Symptom: clicking Make active on Settings > Specs rewrote .coding/openspec/config.yaml from 6,264 bytes to `schema: <name>`, so governed_files and its comments were gone, and plan_create's spec-impact gate silently stopped applying. Root cause: ConfigFile in src-tauri/src/mnemo2/specs_settings.rs (the write shape used by write_config) carries schema, context, rules and extra but not governed_files, which became a known key after the Specs page was built, so it is no longer in `extra`. store_config uses the same writer, so saving context or rules should lose it too (that path read in code, not run). Fixed 2026-11-08: write_config edits config.yaml in place, one top-level key at a time, and refuses an edit that would not read back as the intended config. Regression test: make_active_keeps_governed_files_and_comments in src-tauri/src/mnemo2/specs_settings.rs. Confirmed on the real file: Make active added one line and changed nothing else. Recovery: `git checkout -- .coding/openspec/config.yaml`.
