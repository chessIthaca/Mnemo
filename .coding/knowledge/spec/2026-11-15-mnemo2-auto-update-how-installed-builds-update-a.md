+++
title = "Mnemo2 auto-update — how installed builds update and what a release needs"
created = "2026-11-15"
evidence = "updates.rs tests; ci_workflow guard build_workflow_publishes_the_auto_update_manifest; user created the key 2026-11-15"
+++

Installed Mnemo2 checks the GitHub releases/latest/download/latest.json 30 s after start and every 6 h (src-tauri/src/mnemo2/updates.rs), downloads quietly, shows the Restart-now banner (mnemo2/src/components/UpdateBanner.svelte); install runs only in the orderly-shutdown exit closure after Restart now. Only builds with Cargo feature `auto-update` update; source builds report disabled/source_build.
Release needs GitHub secrets TAURI_SIGNING_PRIVATE_KEY + TAURI_SIGNING_PRIVATE_KEY_PASSWORD (a v* tag fails without them); build.yml adds --features auto-update --config tauri.updater.conf.json, uploads .sig and Mnemo2.app.tar.gz; scripts/updater-manifest.mjs writes latest.json into the draft release. Public key: tauri.conf.json plugins.updater.pubkey. Private key only on the user's machine and in secrets; never read it; losing it strands installed copies.
