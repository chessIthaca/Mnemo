+++
title = "mnemo2 store.activity.test.ts is load-flaky under the full parallel vitest run"
created = "2026-11-19"
evidence = "Worktree pool-8: two failing full runs, green single-file run, green full rerun and green full run on the same base tree with the changes stashed."
+++

`npm test --workspace mnemo2` can intermittently fail with exactly 2 failures in `src/lib/store.activity.test.ts`: "updates a row-only worker's phase from streamed events" (expected 'thinking', received 'starting', :164) and "waits on the user when the snapshot says the worker needs an answer" (turnStartedAtMs null, :195). The file passes alone (`npx vitest run src/lib/store.activity.test.ts` in mnemo2/) and the suite passes on rerun; it is a timing flake of the streamed-event harness under the full ~131-worker parallel vitest run, not a diff finding. Re-run (`npm test --workspace mnemo2`) before chasing a working diff that does not touch the store or team activity code.
