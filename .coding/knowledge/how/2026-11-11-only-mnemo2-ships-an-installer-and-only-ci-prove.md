+++
title = "only Mnemo2 ships an installer, and only CI proves macOS"
supersedes = "2026-11-05-mnemo-and-mnemo2-ship-as-separate-installers-and"
created = "2026-11-11"
evidence = ".github/workflows/build.yml after commit 674702df; tests build_workflow_bundles_mnemo2, build_workflow_stages_each_macos_dmg and workflows_never_mention_mnemo1_frontend."
+++

Since 2026-11-11 the repo ships one app, Mnemo2.

- Package: `mnemo2-app` in src-tauri/mnemo2-app. Bins: mnemo2 = src/main.rs, mnemo2-mcp = src/mnemo2_mcp.rs. mnemo2_route_eval is an example.
- Installers come from `npx tauri build` run in src-tauri/mnemo2-app, after `npm run build:mnemo2` (frontendDist ../../target/mnemo2-dist).
- build.yml: the windows job uploads target/release/bundle/{msi,nsis}. The macos job builds aarch64-apple-darwin after the gated Apple signing/notarization steps, then stages Mnemo2_*.dmg and mnemo2.app.zip into dist-macos (the staging step creates the directory). The release job attaches both artifacts to a draft release, then publishes it.
- Mnemo1's installer and its frontend steps were removed. tests/integration/ci_workflow.rs guards that no workflow mentions frontend/ and that `npx tauri build` runs only in src-tauri/mnemo2-app.
- macOS is proven only by CI; a local Windows build cannot check it.
