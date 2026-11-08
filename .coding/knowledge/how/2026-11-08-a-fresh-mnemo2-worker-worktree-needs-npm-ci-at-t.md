+++
title = "a fresh Mnemo2 worker worktree needs `npm ci` at the root before `build:mnemo2`"
created = "2026-11-08"
+++

`mnemo2/node_modules` in a worktree may hold only `.vite`/`.vite-temp` caches (hoisting puts real deps at `<worktree>/node_modules`), so `npm run build:mnemo2` fails inside `npm run check` with "'svelte-check' is not recognized". Fix: `npm ci --no-audit --no-fund` at the worktree root (~30 s, 303 packages), then `npm run build:mnemo2` produces `target/mnemo2-dist` (needed by tauri `generate_context!` for `cargo test --features mnemo2 --lib`). For svelte-check alone before the install, use the main checkout's shim `& C:\Mnemo\mnemo2\node_modules\.bin\svelte-check.cmd` from `<worktree>\mnemo2` and read $LASTEXITCODE. Long cargo runs: launch with `Invoke-CimMethod Win32_Process Create` (not Start-Process), log under `.coding/tmp/`, poll the log for `DONE`.
