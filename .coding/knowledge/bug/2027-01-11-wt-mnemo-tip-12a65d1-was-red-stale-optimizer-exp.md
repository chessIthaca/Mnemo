+++
title = "wt/mnemo tip 12a65d1 was red — stale optimizer expectations (DTO assertions + committed fixture)"
created = "2027-01-11"
+++

FOUND 2027-02 at the start of plan 2f74e10a: the wt/mnemo tip (12a65d1 "Cost-saving levers on by default") was RED — `cargo test -p mnemo-app` failed 2 tests, unrelated to Laya. ROOT CAUSE: 12a65d1 turned every `[general.optimizer]` lever ON by default (OptimizerConfig::is_default documents "The default is every lever ON") but left two stale expectations behind: src-tauri/src/ipc/settings.rs::settings_dto_tests::get_settings_response_renders_legacy_shape_with_nulls asserted `optimizer.archive == false` (and delta_reads/quality_score), and frontend/src/lib/ipc-fixtures/dto-get-settings.json still carried `false` for the six levers — it was also missing the Laya `steer_tool_choice` key added by 7b7691b. The CODE is intentional, so the expectations were updated (assertions → true, fixture → true + steer_tool_choice) as collateral of plan 2f74e10a rather than reverting the defaults. Evidence: `cargo test -p mnemo-app` = 328 passed / 0 failed after the fix.
