+++
title = "testing updater-manifest CLI with spawnSync"
created = "2026-11-15"
evidence = "scripts/updater-manifest.test.mjs changes and test results"
+++

To test the CLI interface of scripts/updater-manifest.mjs, we use spawnSync to invoke the script as a child process with process.execPath (node). This approach bypasses the invokedDirectly check that would otherwise prevent direct execution, allowing us to test the actual command-line path used by the release job. The tests verify three scenarios: successful manifest generation, invalid tag rejection, and missing flag detection.
