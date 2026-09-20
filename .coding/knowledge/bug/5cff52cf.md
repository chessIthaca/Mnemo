+++
title = "Fix five macOS cargo test --workspace failures (run 35521221353)"
created = "2027-01-11"
+++

Symptom: Five tests fail on the macOS CI leg's `cargo test --workspace` (release run 35521221353, v0.1.1): (1) webview_args::tests::secondary_udf_is_per_pid_under_webview2 — expected path hardcodes `\`, Path::join uses the host separator; (2) codegraph::watcher::tests::watcher_spawns_and_indexes_outside_runtime and (3) watcher_reindexes_on_new_source_file — "watcher must auto-index…" (FSEvents delivers symlink-resolved event paths that never prefix-match a symlinked root, e.g. every macOS tempdir /var → /private/var); (4) browser::tests::profile_dir_is_removed_on_close — profile dir outlives the 15s poll on macOS; (5) agent::factory::tests::tools_array_stays_within_context_budget — deferral-savings assertion c1 >= c0 + 12_000 fails under workspace feature unification (44042 vs 32796 = 11246). · regression test: is_indexable_path_accepts_canonical_event_paths

Full record for plan 5cff52cf (see .coding/plans/5cff52cf.md for the plan file).

regression test: is_indexable_path_accepts_canonical_event_paths · path .coding/plans/5cff52cf.md · branch wt/mnemo @ 98af42a (unmerged — exists only on this branch)
