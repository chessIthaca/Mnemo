+++
title = "release build needs @tauri-apps/cli declared in an npm workspace — PR CI never runs the Tauri CLI"
created = "2026-11-12"
evidence = "build.yml run 43233239658 macOS log; git show v2.2.0:frontend/package.json declares \"@tauri-apps/cli\": \"^2.12.4\"; post-#120 package-lock.json has no node_modules/@tauri-apps/cli."
+++

build.yml builds the installers with `npx tauri build` (working-directory src-tauri/mnemo2-app), which only works when some workspace package.json declares @tauri-apps/cli and package-lock.json locks it (npm ci links node_modules/.bin/tauri). Otherwise npx fails at once with "npm error could not determine executable to run" and the tag build ships nothing. This happened on tag v2.3.0: retiring Mnemo1 deleted frontend/package.json, the only package that declared the CLI. PR CI (ci.yml) and `npm run rebuild:mnemo2` build with cargo (--features custom-protocol) and never run the Tauri CLI, so only a tag build exposes it. The CLI now lives in mnemo2/package.json devDependencies (^2.12.4, same 2.12 line as the tauri crate and @tauri-apps/api); guard test build_workflow_tauri_cli_is_declared_and_locked in tests/integration/ci_workflow.rs. When removing an npm workspace, check which bins only it declared.
