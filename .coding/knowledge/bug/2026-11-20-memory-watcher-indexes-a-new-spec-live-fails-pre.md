+++
title = "memory_watcher_indexes_a_new_spec_live fails pre-existing in this WSL (not a product bug)"
created = "2026-11-20"
evidence = "Plan e90e3047 step 1: j5 (fixed tree) and j6 (git-stashed pristine tree) both fail identically at memory_watcher.rs:550; macOS CI run 44646915241 passed the test."
+++

On this machine's WSL Ubuntu, `host::memory_watcher::tests::memory_watcher_indexes_a_new_spec_live` fails consistently (~10.4s, "a spec written while running must be indexed live", src/host/memory_watcher.rs:550) — even in isolation and even on a PRISTINE tree (proven 2026-11-20 by git-stashing the diff: same panic, same line, on the unmodified lead tip). It is a WSL2 environment quirk, NOT a product regression: the macOS CI leg passes it (run 44646915241 failed only the two shared_service tests). Treat it as expected noise when running the full mnemo lib suite in WSL; do not chase it as a regression and do not "fix" it in product code.
