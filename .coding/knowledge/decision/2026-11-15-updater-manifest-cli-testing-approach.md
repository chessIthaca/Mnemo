+++
title = "updater-manifest CLI testing approach"
created = "2026-11-15"
evidence = "scripts/updater-manifest.test.mjs changes and test results"
+++

For the updater-manifest script, we decided to test the CLI interface by spawning the script as a child process with process.execPath (node) via node:child_process spawnSync. This ensures the invokedDirectly check passes and allows us to cover the command-line path that the release job actually calls, rather than just testing exported helpers.
