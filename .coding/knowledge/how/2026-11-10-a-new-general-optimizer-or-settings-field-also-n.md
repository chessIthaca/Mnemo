+++
title = "a new [general.optimizer] or settings field also needs the Mnemo1 dto-get-settings contract fixture"
created = "2026-11-10"
evidence = "src-tauri/src/ipc/contract_fixtures.rs assert_fixture; cargo test --workspace --no-fail-fast before the v2.2.0 tag failed only on dto_fixtures_match_serde (missing optimizer.idle_compaction)"
+++

Any new field on the settings DTO (e.g. an OptimizerConfig lever) changes the serde output that ipc::contract_fixtures::dto_fixtures_match_serde compares against frontend/src/lib/ipc-fixtures/dto-get-settings.json. That test lives in the Mnemo1 app binary (cargo test --manifest-path src-tauri/Cargo.toml -p mnemo-app --bin mnemo-app), so filtered runs ("settings", --lib, --features mnemo2) and the Mnemo2-only checks never see it, but CI's workspace test does and the Windows job stops there. Add the key to the fixture by hand (it is pretty JSON; the test only writes a fixture when the file is missing), then run the full --bin mnemo-app suite and frontend/src/lib/ipc-contract.test.ts. Found when idle_compaction shipped without it and only the pre-release workspace run caught it.
