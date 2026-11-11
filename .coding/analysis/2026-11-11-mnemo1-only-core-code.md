# Mnemo1-only code and settings left in the core

Date: 2026-11-11  
Base: `7122e1965d143857fae4d0f2c7c908a646893801` (`wt/agent-coreInve`)  
Deleted-shell evidence: `674702df^` (the parent of the Mnemo1 deletion; merge `6748f8c4`, first parent `7c38fafc`).  
Scope: inventory only. No source was changed and no experimental edit was needed.

**Reading the verdicts.** REMOVE means the only non-test producer/consumer was the deleted Mnemo1 shell (or the code exists solely to support another REMOVE row). RETIRE-SETTING means the key/wire value is persisted, and in some cases still shown by Mnemo2, but no live behavior consumes it. KEEP includes public `app`/`headless` embedding surface even where Mnemo2 deliberately disables a feature. Line counts are approximate deletion counts including directly associated tests, not whole-file sizes unless stated.

## Summary

| Candidate | Defined at (file:line) | Verdict | Evidence (one line) | ~Lines |
|---|---|---|---|---:|
| `build_brain`, `build_brain_inner`, `BrainOutcome`, `project_flag_from_args` | `src/host/brain.rs:131,164,183`; `src/host/startup.rs:13` | REMOVE | Only deleted `src-tauri/src/main.rs` called the builder/CLI discovery; headless calls `wire_brain` directly. | 170 |
| `should_reembed_at_startup` | `src/host/startup.rs:63` | KEEP | Live `wire_brain` calls it at `src/host/brain.rs:931` during memory startup. | 0 |
| `Brain`, `SharedResources`, `wire_brain`, `BrainMemoryOptions` | `src/host/brain.rs:44,326,353` | KEEP | `src/headless/engine.rs:31,875-891` builds every live engine through them. | 0 |
| `resolve_startup_provider` | `src/host/startup.rs:33` | KEEP | Live `wire_brain` calls it at `src/host/brain.rs:418`. | 0 |
| Embedder download/install | `src/host/embed_download.rs:46` | KEEP | Public headless setup uses/re-exports it at `src/headless/setup.rs:111,902-937`; Mnemo2 uses that API. | 0 |
| Index-progress snapshots | `src/host/index_progress.rs:138` | KEEP | `src/headless/codegraph_api.rs:112` reads the snapshot; live `wire_brain` records progress. | 0 |
| Laya host manager | `src/host/laya.rs:255` | KEEP | Headless setup and Mnemo2 `laya_settings.rs:74`/`laya_check.rs:35` use it. | 0 |
| Maintenance gate | `src/host/maintenance.rs:88` | KEEP | Live `wire_brain` claims it at `src/host/brain.rs:711,738`; Mnemo2 memory maintenance uses it too. | 0 |
| Startup notifier | `src/host/notifier.rs:19,39` | KEEP | `src/headless/engine.rs:32,45,842` implements and passes it to `wire_brain`. | 0 |
| Live settings rewire | `src/host/rewire.rs:35,73` | KEEP | `src/headless/setup.rs:577-590` calls it; Mnemo2 reaches it through `Setup::patch_settings`. | 0 |
| Host runtime | `src/host/runtime.rs:1` | KEEP | Re-exported by `src/headless/mod.rs:54`; used by headless sessions and Mnemo2 runtime. | 0 |
| Run-All item worktrees and landing | `src/app/worktrees.rs:93,100,204,232,359,543,590` | REMOVE | Current Mnemo2 uses only `agent_*`; the sole current item-worktree caller is one compatibility test. | 950 |
| Seeded code-graph copies | `src/codegraph/mod.rs:312,502` | REMOVE | `snapshot_db`/`index_seeded` have only their tests; deleted `ipc/spawn.rs:162,181` was the caller. | 180 |
| Backlog dispatch selectors | `src/app/backlog.rs:809,817,832,848` | REMOVE | `pending_item`, `next_pending`, and eligible variants have no live caller outside their tests. | 110 |
| Backlog deferral and backlog tools | `src/app/backlog.rs:259,918`; `src/app/tool/workflow/backlog.rs:255` | KEEP | Mnemo2 Plans uses `set_deferred`; live agents use `backlog_status` auto-defer. | 0 |
| Run-All mentions in general workflow/root/context code | `src/app/project.rs:356`; `src/agent/factory.rs:95`; `src/agent/context.rs:123` | KEEP | The remaining code is generic merge/root/compaction/plan behavior; only wording is Run-All-specific. | 0 |
| `.worktrees` graph exclusion | `src/codegraph/walk.rs:254`; `src/codegraph/watcher.rs:295` | KEEP | It also excludes Mnemo2 `.worktrees/agent-*`; `runall-*` is test data, not a special code path. | 0 |
| `[general.budget]` and `BudgetConfigDto` | `src/config/general.rs:593`; `src/config/settings_dto.rs:117` | KEEP | `src/agent/turn.rs:596-607,1794-1813` enforces caps; Mnemo2 Models edits them. | 0 |
| `auto_compact_on_plan_complete` | `src/config/general.rs:175`; `src/config/settings_dto.rs:428` | RETIRE-SETTING | After deletion there is no behavioral reader; only serde, patch application, and tests remain. | 60 |
| `enable_browser_inspection` | `src/config/general.rs:184`; `src/config/settings_dto.rs:424` | RETIRE-SETTING | Mnemo2/headless force inspection off; retire with the Mnemo1-only inspection implementation. | 35 |
| Mnemo1 WebView/CDP inspection | `src/browser/mod.rs:72,346,1135`; `src/tool/browser/mod.rs:1` | REMOVE | Only deleted `ipc/browser*.rs` consumed it and only deleted `main.rs` enabled it. | 1,550 |
| `webview_args` | `src/webview_args.rs:20` | REMOVE | The only caller was deleted `mnemo-app` startup; no headless/example/Mnemo2 reference remains. | 300 |
| `[ui]` chat/steering display keys | `src/config/general.rs:1005,1107-1167,1234` | RETIRE-SETTING | No reader exists outside `general.rs` and two DTO preservation tests. | 330 |
| `SteeringNotesDto` | `src/config/settings_dto.rs:227` | REMOVE | Only deleted `ipc/settings.rs` imported it; it is not a `SettingsSaveDto` field. | 20 |
| `[general.laya].auto_finetune` | `src/config/general.rs:283`; `src-tauri/src/mnemo2/laya_settings.rs:131` | RETIRE-SETTING | It is persisted and exposed on Tiers, but the sole fine-tune consumer was deleted `ipc/finetune.rs`. | 190 |
| Remaining settings DTO surface | `src/config/settings_dto.rs:27-447` | KEEP | Mnemo2 settings and public `Setup::patch_settings` (`src/headless/setup.rs:551`) share it. | 0 |
| Per-task model slots and per-turn routing | `src/config/general.rs:398,859-881` | KEEP | `src/model_resolver.rs` resolves them and `src/agent/loop_impl.rs` routes each turn; Mnemo2 merely sets `skip_turn_routing`. | 0 |
| External/off-screen browser | `src/browser/mod.rs:273,1903` | KEEP | Mnemo2 browser uses `normalize_url`; core off-screen browser is an embedder/agent feature. | 0 |
| `SerializableAgentEvent` core contract | `src/runtime/channels.rs:723` | KEEP | Headless sessions, Mnemo2 runtime/team pumps, and `examples/second_consumer.rs:47` use it. | 0 |
| `PromptDispatched`, `SkillStarted`, `ChildFinished` event variants | `src/runtime/channels.rs:385,516,645,776,807,833` | REMOVE | No live producer/consumer; deleted `ipc/agent.rs`, `events.rs`, and `spawn.rs` created them. | 105 |
| `RoutingNotice` | `src/runtime/channels.rs:441,795` | KEEP | Core still produces it at `src/agent/loop_impl.rs:1697`; Mnemo2 intentionally filters it. | 0 |
| `UNATTENDED_PROMPT_PREFIX` / automatic unattended policy | `src/runtime/channels.rs:167`; `src/app/workflow_policy.rs:356` | UNSURE | Its only identified producer was Run-All, but it is also a public prompt/policy convention an embedder could use. | 120 |

## Candidates

### `build_brain` / `build_brain_inner` / `BrainOutcome` / `project_flag_from_args` — REMOVE

The synchronous timing wrapper at `src/host/brain.rs:164-177` and project-discovery body at `:183-274` constructed the Mnemo1 process state and its project-picker outcome. `BrainOutcome` (`:131-139`) exists only for that builder. `project_flag_from_args` (`src/host/startup.rs:13-22`) is called only by this discovery body at `brain.rs:224`; the `--project`/`--console` hosts it describes were deleted.

- **Live references outside the module:** none to these four symbols. Searches covered `src-tauri/**`, `src/headless/**`, `examples/**`, and `tests/**`. The large shared body beginning at `brain.rs:353` is `wire_brain`, **not** `build_brain_inner`, and must not be deleted.
- **Dead references:** `674702df^:src-tauri/src/main.rs` called `mnemo::build_brain`; `console.rs` only cited it in comments. `src/codegraph/watcher.rs:155` and `src/config/mod.rs:807,866,1424` are comments, not callers. `src/codegraph/query.rs:517,524` uses the string as unrelated parser fixture data.
- **Removal touches:** `src/host/brain.rs` (the two builder functions, `BrainOutcome`, and now-unused imports); `src/host/startup.rs` (`project_flag_from_args` only); comments in `src/codegraph/watcher.rs` and `src/config/mod.rs` belong to documentation cleanup. Keep `Brain`, `wire_brain`, `SharedResources`, `resolve_startup_provider`, and `should_reembed_at_startup`.

### `should_reembed_at_startup` — KEEP

`src/host/startup.rs:63-76` decides whether startup should re-embed based on the configured model, pending download/load, and ready status. Its caller at `src/host/brain.rs:931` is inside the live `wire_brain` memory-startup path, which the headless engine reaches. It is a general embedding/maintenance safeguard, not Mnemo1-only. No references need removal.

### `Brain` / `SharedResources` / `wire_brain` / `BrainMemoryOptions` — KEEP

These are the actual live construction path. `src/headless/engine.rs:31` imports them, `:875-891` invokes `wire_brain`, and `:1093-1101` repeats it in the replacement-engine path. `src/headless/mod.rs:49` publicly re-exports `Brain` and `SharedResources`; `engine.rs:98,143,781` supports shared resources. Mnemo2 reaches them through its headless `Engine`. The field `BrainMemoryOptions.on_screen_browser` is not kept by this row: it is part of the WebView removal below. No dead-only reference makes the surrounding types removable.

### `resolve_startup_provider` — KEEP

`resolve_startup_provider` (`src/host/startup.rs:33`) is called by live `wire_brain` at `src/host/brain.rs:418`. `maybe_migrate_default_provider` in `config/mod.rs` is likewise called by `wire_brain`. Only `project_flag_from_args` becomes orphaned with the Mnemo1 builder, as described above.

### Embedder download/install — KEEP

`src/host/embed_download.rs` is not Mnemo1-only. `src/headless/setup.rs:111-113` publicly re-exports its DTO/status sink, `:902-937` uses its catalog/download functions, and `:1039` installs an embedder. Mnemo2 reaches this functionality through the headless setup transport rather than importing the host module directly. Old IPC references were additional callers, not the reason this module exists.

### Index-progress snapshots — KEEP

`startup_index_snapshot` is called by `src/headless/codegraph_api.rs:112`; live `wire_brain` imports the recording/visibility functions at `src/host/brain.rs:16` and emits them through the host notifier. Mnemo2 consumes the resulting headless `HostEvent` stream in `src-tauri/src/mnemo2/indexing.rs:19`. The old `ipc/codegraph.rs` references were additional consumers of a still-live kernel facility.

### Laya host manager — KEEP

`src/headless/setup.rs:34,114,300-321` drives `LayaManager`; `src-tauri/src/mnemo2/laya_settings.rs:74` and `laya_check.rs:35` are live Mnemo2 callers. `src/host/brain.rs` uses the manager in `wire_brain`. The deleted `ipc/finetune.rs` consumer matters only to `auto_finetune`, not to checkpoint/classifier setup, status, or checking.

### Maintenance gate — KEEP

`src/host/maintenance.rs` provides the cross-operation busy guard. Live `wire_brain` claims it at `src/host/brain.rs:711,738`; `src-tauri/src/mnemo2/memory_maintenance.rs:134-136` also claims it and `:252-255,323-326` uses its progress throttle. `src/headless/engine.rs:979` calls `src/memory/maintenance.rs` archive pruning instead, a separate live subsystem that must not be confused with this host gate.

### Startup notifier — KEEP

`src/headless/engine.rs:32,45-58` defines `BusNotifier`, and `:842-850` passes it into `wire_brain`; `src/host/brain.rs:21` consumes `HostEvent`/`SharedNotifier` in both construction paths. After `build_brain` is removed, retain the trait, event type, alias, and the `wire_brain` notifications; remove only imports left orphaned by the builder.

### Live settings rewire — KEEP

`src/headless/setup.rs:577-590` builds `RewireTargets`, invokes `rewire_vision_embedder`, and syncs the model resolver after a patch. Mnemo2 reaches the same implementation indirectly because its settings commands call `Setup::patch_settings` (`src-tauri/src/mnemo2/model_settings.rs:18-23` describes that pipeline). The separate `set_browser_inspection(false)` at `setup.rs:598` disappears with the dead feature; vision/embedder/factory rewiring remains live.

### Host runtime — KEEP

`src/headless/mod.rs:54-57` re-exports the runtime API; `src/headless/session.rs:24,1133` and `engine.rs:33-39` use it. Mnemo2 runtime is built on those headless types. `examples/second_consumer.rs` uses the same event/runtime contract. Nothing in this module is made dead merely because old `ipc/events.rs` also forwarded it.

### Run-All item worktrees and landing — REMOVE

The item side of `src/app/worktrees.rs` is separate from the live agent side. Remove:

- `item_short_id` (`:93`) and `item_branch` (`:100`);
- `provision_item_worktree` and its impl (`:204,214`);
- `remove_item_worktree` (`:232`);
- `sweep_merged_runall_branches` and its impl (`:359,366`);
- `LandError`/`Landed` (`:543,556`), `land_item_branch` and its impl (`:590,600`), and `land_via_pull_request` (`:705`);
- the landing-only helpers `gh_raw` (`:59`), `parse_github_remote_url` (`:414`), `remote_github_slug` (`:459`), `rulesets_json_blocks_direct_push` (`:474`), and `main_is_protected` (`:512`);
- their item-dispatch, branch-sweep, GitHub-ruleset, direct-merge, PR, and conflict tests in the same file.

**Live references outside the module:** `src-tauri/src/mnemo2/team/tests.rs:2852,2857` calls `provision_item_worktree` only to create a legacy `runall-*` fixture for `cleanup_never_touches_mnemo1_runall_worktrees` (`:2841`). Rewrite that test to create/register the legacy worktree directly with git, or retain it as a manually constructed fixture; do not delete the safety assertion. There are no headless/example callers.

**Dead references:** `674702df^:src-tauri/src/ipc/{run_all.rs,backlog_cmds.rs,spawn.rs}` contained the dispatch/landing callers. Keep `current_branch`, `agent_short_id`, `agent_branch`, `provision_agent_worktree`, `remove_worktree_only`, `remove_agent_worktree`, `rollback_agent_worktree`, and their shared private helpers: Mnemo2 team audit/control/deletion/merge/spawn/workbench uses them (`team/audit.rs:12`, `deletion.rs:12`, `merge.rs:14`, `spawn.rs:13`, `workbench.rs:12`). `provision_worktree_impl`, `remove_worktree_only_impl`, `git_raw`, retry/wait helpers, and `worktrees_dir` are shared, not part of this deletion.

### Seeded code-graph copies — REMOVE

`CodeGraph::snapshot_db` (`src/codegraph/mod.rs:312`) and `CodeGraph::index_seeded` (`:502`) were the Run-All worktree fast path. All current non-test references are documentation between those two functions and their tests; neither `src-tauri/**`, `src/headless/**`, nor `examples/**` calls them. Deleted `674702df^:src-tauri/src/ipc/spawn.rs:162,181` was the real caller. Remove both functions, imports used only by them, the seeded-index branch/helpers that become unreachable, and the tests at `mod.rs:1167` and `:2065` onward that exist specifically for snapshot/seeded behavior. Keep ordinary `CodeGraph::index` and watcher behavior.

### Backlog dispatch selectors — REMOVE

The shell-only selectors are `pending_item` (`src/app/backlog.rs:809`), `next_pending` (`:817`), `next_pending_eligible` (`:832`), and `next_pending_eligible_excluding` (`:848`). Current searches found no non-test caller outside `src/app/backlog.rs`; `src-tauri/src/mnemo2/backlog.rs` lists/edits items and never dispatches through them. Deleted `ipc/backlog_cmds.rs:438` used `next_pending`, and deleted `ipc/run_all.rs` used the eligible variants.

Removal edits only `src/app/backlog.rs`: delete these methods and the tests whose sole purpose is selection/dispatch order. Rewrite deferral/requeue tests that use `pending_item` merely as a convenient lookup to use `items().iter().find(...)` instead; those tests protect live behavior and must not be deleted. Do **not** remove `deferred`, `set_deferred`, `set_auto_defer_reason`, `transition`, or `set_status`.

### Backlog deferral and backlog tools — KEEP

Mnemo2's own backlog is a live caller: `src-tauri/src/mnemo2/backlog.rs:37` imports the core store, `:149-154` calls `set_deferred`, `:170` calls `requeue`, and `:188` calls `clear_finished`. The core agent tool is also live: `src/app/tool/workflow/backlog.rs:272-312` changes status/deferral and `:589-610` persists auto-defer reasons. `src/app/plan_board.rs:25` and `src/app/policy_factory.rs:19` are additional core consumers. Thus the Run-All wording in field/tool descriptions is stale, but the persisted shape and behavior are not Mnemo1-only.

### Run-All mentions in general workflow/root/context code — KEEP

The remaining hits are not a removable Run-All subsystem:

- `src/app/project.rs:356` documents the `.coding/backlog.jsonl` union merge driver. That driver also protects Mnemo2 worker checkouts and the current backlog editor.
- `src/app/tool/workflow/plan.rs:737,848,4093` concerns generic plan questions, automatic plan forking, and stack persistence. These paths are reachable from ordinary `update_plan` use, not only Run-All.
- `src/agent/factory.rs:95` (`AgentRootSpec`) and `build_with_root_spec` (`:1103`) are used by the live headless spawner at `src/headless/host_tools.rs:300-311`; `src/host/runtime.rs:750` propagates the root. `src/agent/loop_impl.rs:611,1401` is the corresponding general root override.
- `src/agent/context.rs:123,230,419` and `loop_impl.rs:1873` describe shared compaction thresholds/headroom. The compaction functions remain used independently of the retired flag.

Only comments that present Run-All as a current host feature need editing. Do not remove these code paths.

### `.worktrees` graph exclusion — KEEP

`src/codegraph/walk.rs:254` and `watcher.rs:295` exclude the whole `.worktrees` directory, not just `runall-*`. Mnemo2 stores `agent-*` worktrees there, so the exclusion is still required. Tests at `walk.rs:257-263` and `watcher.rs:301-309` may rename their fixture from `runall-abcd12` to `agent-abcd12` for clarity, but the assertion must stay. `src/codegraph/mod.rs:291,495` comments about seeding become unnecessary only when the seeded-copy functions in the earlier row are removed; the `run_all_dispatch_next` strings at `:1368,1374` are arbitrary search fixtures, not dead code.

### `[general.budget]` and `BudgetConfigDto` — KEEP

This is enforced, not a dead display field. `src/host/brain.rs:1055-1059,1259` shares the loaded config with every factory-built loop, and `src/host/rewire.rs:114` updates it after a live settings patch. `src/agent/turn.rs:596-613` runs the gate on every iteration and `:1796-1813` reads the live caps before pausing a plan; `src/agent/budget.rs:127-183` evaluates per-plan tokens, per-plan escalations, and per-step failed-cycle retries. The feature is independently reachable by headless embedders and does not depend on per-turn model routing.

Mnemo2 reads it at `src-tauri/src/mnemo2/model_settings.rs:75,161-175,208-213` and writes it through the shared patch. `mnemo2/src/lib/modelSettings.ts:71-85,181-195,253-270` converts the wire/draft and `ModelsPage.svelte:365-390` shows it. Keep the config, DTO, backend, UI, and tests. This evidence supersedes the earlier assumption in the 2026-11-11 settings decision that the whole section could retire.

### `auto_compact_on_plan_complete` — RETIRE-SETTING

Defined at `src/config/general.rs:166-175`, defaulted at `:923`, and patched at `src/config/settings_dto.rs:425-428,781-783`. The former runtime reader (`read_run_all_auto_compact` in deleted-shell history) is gone; current searches find no read in `src/runtime/**`, `src/headless/**`, `src-tauri/**`, or examples. Mnemo2 does not show it.

Removal touches `src/config/general.rs` (field/default, `auto_compact_on_plan_complete_defaults_false` and `..._round_trips` at `:2271-2295`, and the stale removed-key comment at `:2226-2230` if still accurate) and `src/config/settings_dto.rs` (wire field, apply block, `auto_compact_on_plan_complete_patch_applies` at `:954-975`). Existing TOML keys remain harmless because unknown fields are accepted.

### `enable_browser_inspection` — RETIRE-SETTING

The key is defined at `src/config/general.rs:184` and patched at `src/config/settings_dto.rs:424,778-780`. Its only behavior is the Mnemo1 WebView/CDP tool path: `src/host/brain.rs:840` enabled it, while live headless construction explicitly forces it off (`src/headless/setup.rs:598`, `src/headless/engine.rs:884-887,1093-1098`). Mnemo2 has no settings control for it.

Retire the field/default and patch field/apply/test together with the inspection implementation below. Removal also touches the config fixture at `src/config/general.rs:2223-2235`; there is no dedicated settings-DTO browser-inspection test. Until the implementation step lands, do not remove the key by itself: `AgentLoopFactory::with_config` still reads it at `src/agent/factory.rs:988-1003`.

### Mnemo1 WebView/CDP inspection — REMOVE

This is the on-screen browser attached to the deleted Mnemo1 WebView2, not the external/off-screen browser. Only deleted `674702df^:src-tauri/src/ipc/browser.rs`, `browser_webview.rs`, and `main.rs` called its host API. There are no current Mnemo2/headless/example producers of a WebView CDP endpoint.

Removal must cover all of the following:

- `src/tool/browser/mod.rs`: `BrowserScreenshotTool` (`:15`), `BrowserEvalTool` (`:43`), `BrowserSnapshotTool` (`:74`), `BrowserNavigateTool` (`:113`), `BrowserClickTool` (`:144`), and `BrowserTypeTool` (`:178`), their helpers and tests. This module also contains external-browser tools; retain those and their `pub use` entries.
- `src/agent/factory.rs`: the inspection flag (`:423,584`), `set_browser_inspection`/`browser_inspection_enabled` (`:992-1003`), registration of the six tools (`:1773-1778`), gating (`:3182-3197`), and the associated tests around `:2599-2620,3236`. Remove `on_screen_browser` handling from `BrainMemoryOptions` and the corresponding factory setup in `src/host/brain.rs`; change the two headless initializers at `src/headless/engine.rs:878-887,1093-1098` and remove/replace the tests at `engine.rs:1229` and `src/headless/setup.rs:1859,1892`.
- `src/browser/mod.rs`: `PageInfo`, `ConsoleEntry`, `ConsoleEvent` (`:72-105`), WebView state and `set_webview_enabled`/`subscribe_console` (`:287-352`), WebView launch/navigation/close/CDP/page/console/evaluate/screenshot APIs (approximately `:346-385` and `:1135-1530`), child-ensurer wiring used only by old startup, and their tests. Preserve `BrowserManager` itself and the external/off-screen operations.

The approximate count is intentionally conservative; the implementer should use the symbol list, not delete whole large modules.

### `webview_args` — REMOVE

`src/webview_args.rs:20-291` builds `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` and chooses a CDP port for `mnemo-app`. The only real caller was deleted `674702df^:src-tauri/src/main.rs`. Current searches find no caller in Mnemo2, headless, examples, or tests outside this module. Delete the file, `pub mod webview_args;` at `src/lib.rs:43`, and the module's tests. This is independent of the external browser feature.

### `[ui]` chat/steering display keys — RETIRE-SETTING

The dead keys are `show_token_usage`, `show_tool_images`, `show_tool_activity`, `show_knowledge_activity`, `show_delegation_notes`, `chat_thread_line`, `chat_prose_cap`, `chat_turn_tint`, and `chat_hover_timestamps` (`src/config/general.rs:1107-1167`), plus `steering_notes` (`:1143-1149`) and all `SteeringNotesCfg` fields (`:1005-1099`). Searches found no reader in `src-tauri/**`, `src/headless/**`, `mnemo2/**`, examples, or elsewhere in `src/**` outside config code/tests. They do not suppress steering notes in model output; they were display-only.

Delete `SteeringNotesCfg`, `UiConfig::STEERING_NOTE_KEYS`, `effective_steering_note_visible`, the nine fields, their defaults, and their dedicated tests at `general.rs:2322-2328,2358-2515` plus related preservation assertions at `:1575-1595,2678-2679`. Keep `UiConfig` itself and its `theme`, `sound_complete`, `sound_input_needed`, and `sound_stopped_errors`: Mnemo2 reads/writes those at `src-tauri/src/mnemo2/appearance_settings.rs:53-70,116-139`.

In `src/config/settings_dto.rs`, rewrite `ui_fields_are_absent_from_the_engine_patch` (`:905-934`) and `ui_table_survives_an_engine_patch` (`:937-951`) to use live `theme`/sound fields rather than deleted ones. No Mnemo2 page or test change is needed for this row because none references these keys.

### `SteeringNotesDto` — REMOVE

`src/config/settings_dto.rs:219-250` defines a standalone DTO that deleted `674702df^:src-tauri/src/ipc/settings.rs:17,1042,1603` used in the app-owned half of the old wire. It is not a field of `SettingsSaveDto` and has no current importer. Delete the struct; update the UI-preservation tests as described above. Do not remove the whole settings DTO module.

### `[general.laya].auto_finetune` — RETIRE-SETTING

The core field is `src/config/general.rs:283`; it participates only in `LayaConfig::is_default` at `:360` and serde tests. `src/memory/classifier.rs:51` is explanatory text, not a consumer. Deleted `674702df^:src-tauri/src/ipc/finetune.rs` was the fine-tune implementation/reader. No current code starts training or changes behavior from this flag.

Mnemo2 nevertheless exposes it: `src-tauri/src/mnemo2/laya_settings.rs:131,193,231,359`; `mnemo2/src/lib/layaSettings.ts:24,34,117-127`; the generic Tiers metadata renderer at `TiersPage.svelte:328-336`. Removal touches:

- `src/config/general.rs`: field, `is_default`, and assertions at `:1742,2118`;
- `src/config/settings_dto.rs`: `laya_auto_finetune` at `:322-325`, apply at `:686-688`, and `laya_auto_finetune_patch_applies` at `:1030-1054`;
- `src-tauri/src/mnemo2/laya_settings.rs`: status/patch fields, read/write mapping, and tests at `:1049,1210-1212`; update the stale deleted-finetune comment at `src-tauri/src/mnemo2/runtime.rs:1137` in the documentation step;
- `mnemo2/src/lib/layaSettings.ts`: flag union/type/metadata; `mnemo2/src/lib/layaSettings.test.ts:70`; fixtures in `mnemo2/src/components/SettingsDialog.test.ts:173`, `TiersPage.test.ts:75`, and `mnemo2/src/test/previewLaya.ts:29`.

`TiersPage.svelte` needs no direct edit: it renders the shared metadata array, so removing that entry removes the checkbox. Keep all other Laya flags and `laya_a`/`laya_b` routing.

### Remaining settings DTO surface — KEEP

`SettingsSaveDto` and its validation/assembly are used by Mnemo2 (`model_settings.rs:21,199-220`, `laya_settings.rs:71,354-363`, `safety_settings.rs:143-157`) and by the public embedding API `Setup::patch_settings` (`src/headless/setup.rs:551-562`). `VisionModelDto`, `EmbeddingModelDto`, `ModelRefDto`, `ModelsConfigDto`, `RoutingConfigDto`, `BudgetConfigDto`, `MemorySearchDto`, the Laya patch fields, and the validation functions remain reachable there. `PricingDto`/`SettingsSaveDto.pricing` (`settings_dto.rs:178,418`) have no current in-repo constructor, but they are validated and assembled into endpoint pricing at `:525-536,860-870` through the public headless patch. Mnemo2 edits pricing through its separate `src-tauri/src/mnemo2/pricing.rs` command surface; that choice is not removal evidence for the embedder patch.

Only the specifically identified dead fields (`auto_compact_on_plan_complete`, `enable_browser_inspection`, `laya_auto_finetune`) and standalone `SteeringNotesDto` should go. Do not remove planning/executing/bug-fixing/complete from `ModelsConfigDto`: embedders can patch them even though Mnemo2's Models page deliberately leaves them untouched.

### Per-task model slots and per-turn routing — KEEP

`ModelsConfig` defines `planning`, `executing`, `bug_fixing`, and `complete` at `src/config/general.rs:859-881`. `src/model_resolver.rs:193-211` resolves planning/executing, `:239-247` resolves bug-fixing, and `:197-209` handles complete; `src/agent/dispatch.rs:1797-1801` documents and uses executing→subagent→default for spawned work. `src/agent/loop_impl.rs:1573-1598` performs the live route and can emit `RoutingNotice`.

Mnemo2 sets `skip_turn_routing` (`src-tauri/src/mnemo2/runtime.rs:235-240`) and its model settings tests explicitly preserve these slots (`model_settings.rs:721-770`). That is a host policy choice, not death of the kernel feature. `RoutingConfig` at `general.rs:398` and the public settings patch are likewise KEEP.

### External/off-screen browser — KEEP

`src-tauri/src/mnemo2/browser.rs:145` calls `mnemo::browser::normalize_url`; the browser page/window host is live Mnemo2 code. The factory's external-browser tools and `BrowserManager::run_browser` off-screen path remain general `browser`-feature functionality usable by agents and embedders. Only the WebView/CDP symbols enumerated above are REMOVE. Do not delete `src/browser/mod.rs`, `src/tool/browser/mod.rs`, or the `browser` feature wholesale.

### `SerializableAgentEvent` core contract — KEEP

`src/headless/session.rs:27,174-240` forwards and interprets events; `src-tauri/src/mnemo2/delta_batch.rs:103-116`, `runtime.rs`, and `team/pump.rs` consume the serializable form; `examples/second_consumer.rs:47,230` proves the public embedding use. The enum, conversion implementation, and almost all variants therefore stay. Old `ipc/events.rs` was one forwarder among several, not the owner of the contract.

### `PromptDispatched`, `SkillStarted`, `ChildFinished` — REMOVE

These three are Mnemo1 UI-shaped events with no live producer or consumer:

- `AgentEvent::PromptDispatched` (`src/runtime/channels.rs:385-389`) and its serializable twin (`:776-779`);
- `AgentEvent::SkillStarted` (`:516-525`) and its serializable twin (`:807-811`);
- `AgentEvent::ChildFinished` (`:645-652`) and its serializable twin (`:833-837`).

Current references outside `channels.rs` are only agent conversion tests at `src/agent/tests.rs:16946,17029` and headless serialization tests at `src/headless/session.rs:1693,1752,1767`. Mnemo2 has no match for any variant. Deleted `674702df^:src-tauri/src/ipc/agent.rs:898` constructed `SkillStarted`, `ipc/events.rs:81` forwarded `PromptDispatched`, and `ipc/spawn.rs`/`events.rs` created child-finished notifications.

Removal touches `src/runtime/channels.rs` (six variants, conversion arms, and conversion tests) and deletes/rewrites the three variant-specific tests in `src/agent/tests.rs` and `src/headless/session.rs`. Keep `WorkflowStateChanged`, which core workflow code still emits, and keep all host-tool/policy events.

### `RoutingNotice` — KEEP

Although its comment says Mnemo1 displayed it and Mnemo2 filters it (`src/runtime/channels.rs:441-451`), the core still produces `AgentEvent::RoutingNotice` at `src/agent/loop_impl.rs:1697` and tests the behavior extensively. Headless consumers may observe it. The Mnemo2 filters at `src-tauri/src/mnemo2/runtime.rs:1734` and `team/pump.rs:780` are intentional UI policy, not evidence that the event is unproduced.

### `UNATTENDED_PROMPT_PREFIX` / automatic unattended policy — UNSURE

`src/runtime/channels.rs:159-167` defines the magic prompt prefix; `src/agent/loop_impl.rs:3545-3549` strips it, and `src/app/workflow_policy.rs:350-362` turns it into `PromptHook.unattended`. Deleted `ipc/run_all.rs:1337` is the only identified producer. There is no current Mnemo2/headless/example producer.

The counterweight is the embedding rule: an embedder can send this public prefix through the ordinary prompt API and obtain unattended plan-question behavior today. Removing it also changes `PromptHook.unattended`, plan auto-fork/question behavior, and workflow-policy tests, so this is not just deletion of an unused constant. The lead must decide whether this prompt convention is a supported kernel feature or was Mnemo1 Run-All wire format. See the first question below.

## Settings

Production readers are listed below; serde defaults and tests that only assert persistence are not behavioral readers. "Unknown keys are ignored" follows from the config structs not using `deny_unknown_fields`, so retirement needs no migration of users' `config.toml`.

| Key | Definition and every live reader | Enforced today? | Shown by Mnemo2? | Verdict |
|---|---|---|---|---|
| `[general.budget].enabled`, `max_tokens_per_plan`, `max_escalations_per_plan`, `max_retries_per_lane` | `src/config/general.rs:593-645`; read by `src/agent/budget.rs:127-183`, `src/agent/turn.rs:596-613,1796-1813`; wired by `src/host/brain.rs:1055-1059,1259`, updated by `src/host/rewire.rs:114`, and exposed by Mnemo2 `model_settings.rs:75,161-175,208-213` | **Yes** — pauses/asks at plan iteration boundaries for token, escalation, and failed-cycle retry caps | **Yes**, Models page (`mnemo2/src/lib/modelSettings.ts:71-85,181-195,253-270`; `ModelsPage.svelte:365-390`) | KEEP |
| `[general].auto_compact_on_plan_complete` | `src/config/general.rs:175`; `src/config/settings_dto.rs:428,781-783` only | **No** — no runtime reader remains | No | RETIRE-SETTING |
| `[general].enable_browser_inspection` | `src/config/general.rs:184`; `src/config/settings_dto.rs:424,778-780`; factory gate at `src/agent/factory.rs:988-1003,3197` | The factory gate works, but every live headless/Mnemo2 path forces it `false` (`src/headless/setup.rs:598`); only deleted Mnemo1 startup supplied an on-screen browser | No | RETIRE-SETTING with the WebView/CDP removal |
| `[ui].show_token_usage` | `src/config/general.rs:1108,1186`; no external reader | No | No | RETIRE-SETTING |
| `[ui].show_tool_images` | `src/config/general.rs:1112,1187`; no external reader | No | No | RETIRE-SETTING |
| `[ui].show_tool_activity` | `src/config/general.rs:1121,1188`; no external reader | No | No | RETIRE-SETTING |
| `[ui].show_knowledge_activity` | `src/config/general.rs:1130,1189`; no external reader | No | No | RETIRE-SETTING |
| `[ui].show_delegation_notes` | `src/config/general.rs:1142,1190`; read only by `effective_steering_note_visible` in the same config impl | No — neither that resolver nor the flag has a live caller | No | RETIRE-SETTING |
| `[ui].chat_thread_line` | `src/config/general.rs:1154,1192`; only DTO preservation tests read it | No | No | RETIRE-SETTING |
| `[ui].chat_prose_cap` | `src/config/general.rs:1159,1193`; no external reader | No | No | RETIRE-SETTING |
| `[ui].chat_turn_tint` | `src/config/general.rs:1163,1194`; no external reader | No | No | RETIRE-SETTING |
| `[ui].chat_hover_timestamps` | `src/config/general.rs:1167,1195`; no external reader | No | No | RETIRE-SETTING |
| `[ui.steering_notes]` — `auto_delegated`, `search_nudge`, `shell_tip`, `graph_miss`, `recall_rider`, `read_nudge`, `literal_tip`, `known_memory_hit`, `consolidation_due`, `shell_redirect`, `edit_stale_read` | `src/config/general.rs:1005-1099`; read only by `UiConfig::effective_steering_note_visible` (`:1234`) and its own tests; standalone `SteeringNotesDto` has no live importer | No — model steering text is still emitted regardless; these were UI visibility toggles | No | RETIRE-SETTING |
| `[general.laya].auto_finetune` | `src/config/general.rs:283,360`; patch at `src/config/settings_dto.rs:325,686-688`; Mnemo2 wire at `laya_settings.rs:131,193,231,359`; metadata at `mnemo2/src/lib/layaSettings.ts:24,34,125` | **No** — no training or inference behavior reads it; deleted `ipc/finetune.rs` was the consumer | **Yes**, Tiers renders the "Auto fine-tune" toggle from the metadata (`TiersPage.svelte:328-336`) | RETIRE-SETTING |
| `[ui].theme`, `sound_complete`, `sound_input_needed`, `sound_stopped_errors` | `src/config/general.rs:1106,1170-1181`; Mnemo2 `appearance_settings.rs:53-70,116-139` | Yes — Mnemo2 appearance and sound behavior consumes them | Yes, Appearance | KEEP |
| Other `SettingsSaveDto` fields, including `pricing` and per-task model slots | `src/config/settings_dto.rs:27-447`; public `Setup::patch_settings` at `src/headless/setup.rs:551` plus Mnemo2 settings modules | Yes, or publicly available to an embedder through the validated patch | Some are intentionally not shown | KEEP |

No additional inert key was found in the inspected `SettingsSaveDto`/`UiConfig` surface. A field merely absent from Mnemo2's UI was not called dead when the public headless patch or another live core path still applies it.

## Stale doc comments

These are comments, not reasons to delete the surrounding code. Paths are current file/line locations. Comments already attached to a REMOVE/RETIRE symbol should be deleted with that symbol; the rest are listed here for a documentation pass.

### Deleted `src-tauri/src/ipc/**`, old `main.rs`, or old frontend paths

- `src/host/brain.rs:7,58` — old Tauri `main.rs` construction path (handled by the builder removal).
- `src/host/embed_download.rs:6` — `ipc/embeddings.rs`; `src/host/index_progress.rs:13-17` — old `main.rs`/`ipc/projects.rs`; `src/host/laya.rs:17-22` — `ipc/laya.rs`; `src/host/rewire.rs:5-18` — `ipc/rewire.rs` and an already-completed future move.
- `src/host/runtime.rs:1155` — `frontend/src/lib/state/agents.ts` seed pattern. Its Run-All wording at `:25-44,259-270,1277` should instead describe generic worktree-bound sessions: `src/headless/engine.rs:195-207,371` still constructs `WorktreeBinding` for live sessions.
- `src/browser/mod.rs:10-11,287,346,633` — old `ipc/browser.rs`, startup `main.rs`, and old frontend console listener; `src/webview_args.rs:9-10` — old `mnemo-app` startup (both handled by their removals).
- `src/runtime/channels.rs:13,161-163,286,379,1048-1050,1121` — `ipc/approval.rs`, `ipc/run_all.rs`, GUI `main.rs`, old frontend seed/lane projections. The approval/host-tool documentation should name `src/headless/session.rs` and `src-tauri/src/mnemo2/**` instead.
- `src/runtime/agent.rs:1498,2131` — old `ipc/events.rs` channel forwarding; `src/runtime/mod.rs:310` — old `ipc/events.rs` seed.
- `src/config/settings_dto.rs:13-14,404,910` — old `ipc/settings.rs`; `src/config/endpoints.rs:982` — same; `src/config/mcp.rs:636,701` — `ipc/mcp.rs`; `src/config/general.rs:2229` — `ipc/run_all.rs` (the setting-specific comments are handled by retirement).
- `src/app/workflow/mod.rs:51-52` — old `ipc/workflow.rs` adapter; `src/error.rs:7` — old binary `main.rs` panic hook wording.
- `src/provider/client_factory.rs:9-10,203` and `src/provider/models.rs:33,83` — old GUI `main.rs` / `ipc/agents.rs`; name the headless/Mnemo2 model-change path.
- `src/headless/codegraph_api.rs:8` and `src/headless/memory_api.rs:8` — "moved verbatim" comments still point to deleted IPC files; retain the history in the plan/commit, but make the current contract self-contained.
- `src-tauri/src/mnemo2/runtime.rs:1137` — cites deleted `src-tauri/src/ipc/finetune.rs:585` as though it were a live reference.

### Generic `frontend/**` examples and Run-All-as-current wording

- `src/app/backlog.rs:97` and `src/app/tool/workflow/plan.rs:257,3123` use `frontend/src/**` as path-syntax examples. Change the examples to a live tree (for example `mnemo2/src/**`); the parser behavior is unchanged. Test string literals at `src/app/tool/workflow/backlog.rs:718` and `plan.rs:3134-3135` are fixtures rather than documentation, but should be renamed in the same pass so searches stop finding the deleted tree.
- `src/app/project.rs:356`; `src/app/tool/workflow/backlog.rs:255-258,317,328`; `src/agent/context.rs:123,230,419`; `src/agent/factory.rs:81-103,1093`; `src/agent/loop_impl.rs:611,1401,1873`; `src/codegraph/walk.rs:254`; and `src/codegraph/watcher.rs:295-305` still describe Run-All as the motivating/current caller. Replace with backlog deferral, generic worktree sessions, or Mnemo2 worker language as appropriate; the associated code is KEEP above.
- `src/host/mod.rs:5` still lists the deleted console/Tauri-GUI host set. `src-tauri/src/mnemo2/model_settings.rs:8-11`, `memory_maintenance.rs:11`, `appearance_settings.rs:8-18`, and `indexing.rs:3` describe sharing with Mnemo1 in the present tense; say "legacy/shared Mnemo config" instead. Historical comparisons such as `mnemo2/backlog.rs:80`, `mcp_server.rs:97,335`, `pipeline.rs:5-22`, and the explicit legacy-worktree safety test at `team/tests.rs:2837-2841` are accurate and need no rewrite.

Pure string fixtures such as `src/memory/knowledge_index.rs:411` (`src-tauri/src/main.rs`) and `run_all_dispatch_next` in `src/codegraph/mod.rs:1368,1374` are not doc comments and do not imply a live dependency.

## Proposed removal steps

The file sets below are pairwise disjoint. Steps 1–6 can otherwise land independently; **step 2 must follow step 1**, **step 7 must follow step 1**, and **step 8 is last**. Comments in a file owned by steps 1–7 are fixed in that step; step 8 owns only files not touched earlier. Estimated counts include tests.

### Step 1 — Remove Mnemo1 shell startup and on-screen inspection wiring (~1,000 lines)

- **Files:** `src/host/brain.rs`, `src/host/startup.rs`, `src/agent/factory.rs`, `src/tool/browser/mod.rs`, `src/headless/engine.rs`, `src/headless/setup.rs`.
- **Delete:** `build_brain`, `build_brain_inner`, `BrainOutcome`, `project_flag_from_args`, `BrainMemoryOptions.on_screen_browser`, the six WebView tool structs/registration/gates, `AgentLoopFactory`'s inspection flag/accessors, and associated tests. Keep `wire_brain`, `Brain`, `resolve_startup_provider`, `should_reembed_at_startup`, external-browser tools, and root/worktree support.
- **Rewrite:** the two headless `BrainMemoryOptions` initializers; headless tests that currently prove inspection stays off should instead prove the removed tools are absent. Update Run-All wording in `factory.rs` to generic worktree-session wording.
- **Verify:** `cargo test --workspace --all-features`; search for `build_brain`, `BrainOutcome`, `project_flag_from_args`, `BrowserScreenshotTool`, `BrowserEvalTool`, `BrowserSnapshotTool`, `BrowserNavigateTool`, `BrowserClickTool`, `BrowserTypeTool`, and `set_browser_inspection` and confirm no live references. Confirm `should_reembed_at_startup` still has its `wire_brain` caller.

### Step 2 — Remove the now-unreachable WebView CDP manager surface (~550 lines; after step 1)

- **Files:** `src/browser/mod.rs` only.
- **Delete:** `PageInfo`, `ConsoleEntry`, `ConsoleEvent`, WebView enable/subscription state, child-ensurer/CDP launch/navigation/page/console/evaluate/screenshot methods, and their tests. Trim imports orphaned by those deletions.
- **Keep:** `BrowserManager`, `normalize_url` (`:1903`), external/off-screen browser operations, and their tests.
- **Verify:** `cargo test --workspace --all-features`; search the deleted symbol names and confirm `src-tauri/src/mnemo2/browser.rs:145` still compiles.

### Step 3 — Delete the Mnemo1 WebView2 argument builder (~300 lines)

- **Files:** `src/webview_args.rs` (delete), `src/lib.rs` (remove line 43 module export).
- **Delete:** the entire port-selection/browser-argument module and its unit tests.
- **Verify:** `cargo test --workspace`; search for `webview_args`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, and the deleted helper names.

### Step 4 — Remove Run-All item worktrees, landing, and graph seeding (~1,130 lines)

- **Files:** `src/app/worktrees.rs`, `src/codegraph/mod.rs`, `src-tauri/src/mnemo2/team/tests.rs`.
- **Delete:** all item/runall/landing symbols and landing-only helpers enumerated above; `snapshot_db`, `index_seeded`, their now-dead helpers/imports, and their dedicated tests.
- **Rewrite:** `cleanup_never_touches_mnemo1_runall_worktrees` (`team/tests.rs:2841`) to construct its legacy worktree fixture directly rather than calling the deleted provisioning helper. Keep all `agent_*` worktree functions and ordinary `CodeGraph::index`.
- **Verify:** `cargo test --workspace`; specifically run the Mnemo2 team cleanup tests and ordinary code-graph indexing/watcher tests. Search `provision_item_worktree`, `land_item_branch`, `sweep_merged_runall_branches`, `snapshot_db`, and `index_seeded`.

### Step 5 — Remove shell-only backlog dispatch selectors (~110 lines)

- **Files:** `src/app/backlog.rs` only.
- **Delete:** `pending_item`, `next_pending`, `next_pending_eligible`, `next_pending_eligible_excluding`, and selector-only tests.
- **Rewrite:** live deferral/requeue tests that used `pending_item` as a lookup; update stale Run-All descriptions and the `frontend/src` example without changing the persisted `deferred` shape.
- **Verify:** `cargo test --workspace`; run Mnemo2 backlog tests and confirm `set_deferred`, `set_auto_defer_reason`, `transition`, `set_status`, `requeue`, and `clear_finished` remain.

### Step 6 — Remove three Mnemo1-only UI event variants (~105 lines)

- **Files:** `src/runtime/channels.rs`, `src/agent/tests.rs`, `src/headless/session.rs`.
- **Delete:** both enum forms and conversion arms for `PromptDispatched`, `SkillStarted`, and `ChildFinished`, plus their variant-only tests. Update stale IPC/frontend comments in `channels.rs` while retaining the public event contract.
- **Verify:** `cargo test --workspace`; check `examples/second_consumer.rs` and Mnemo2 runtime/team pumps still compile and their event round-trip tests pass.

### Step 7 — Retire inert config keys and the auto-fine-tune UI (~500 lines; after step 1)

- **Files:** `src/config/general.rs`, `src/config/settings_dto.rs`, `src/memory/classifier.rs` (comment only), `src-tauri/src/mnemo2/laya_settings.rs`, `mnemo2/src/lib/layaSettings.ts`, `mnemo2/src/lib/layaSettings.test.ts`, `mnemo2/src/components/SettingsDialog.test.ts`, `mnemo2/src/components/TiersPage.test.ts`, `mnemo2/src/test/previewLaya.ts`.
- **Delete:** `auto_compact_on_plan_complete`, `enable_browser_inspection`, `laya_auto_finetune`/`auto_finetune`, the nine `[ui]` chat/activity fields, `SteeringNotesCfg`, `UiConfig::STEERING_NOTE_KEYS`, `effective_steering_note_visible`, `SteeringNotesDto`, associated apply code, wire fields, metadata, and dedicated tests.
- **Rewrite:** settings preservation tests to use live theme/sound fields and unrelated Laya flags. `TiersPage.svelte` needs no edit because it consumes the metadata array. Do not touch `BudgetConfigDto`, per-task model slots, pricing, or other Laya flags.
- **Verify:** `cargo test --workspace`, `npm test --prefix mnemo2`, and `npm run check --prefix mnemo2`. Load a config containing every removed key and confirm it is ignored; confirm Tiers no longer shows Auto fine-tune and Appearance still shows theme/sounds.

### Step 8 — Refresh remaining stale comments and fixtures (~100 lines; last)

- **Files:** `src/app/project.rs`, `src/app/tool/workflow/backlog.rs`, `src/app/tool/workflow/plan.rs`, `src/app/workflow/mod.rs`, `src/agent/context.rs`, `src/agent/loop_impl.rs`, `src/codegraph/walk.rs`, `src/codegraph/watcher.rs`, `src/config/endpoints.rs`, `src/config/mcp.rs`, `src/config/mod.rs`, `src/error.rs`, `src/headless/codegraph_api.rs`, `src/headless/memory_api.rs`, `src/host/mod.rs`, `src/host/embed_download.rs`, `src/host/index_progress.rs`, `src/host/laya.rs`, `src/host/runtime.rs`, `src/host/rewire.rs`, `src/provider/client_factory.rs`, `src/provider/models.rs`, `src/runtime/agent.rs`, `src/runtime/mod.rs`, `src-tauri/src/mnemo2/appearance_settings.rs`, `src-tauri/src/mnemo2/indexing.rs`, `src-tauri/src/mnemo2/memory_maintenance.rs`, `src-tauri/src/mnemo2/model_settings.rs`, `src-tauri/src/mnemo2/runtime.rs`.
- **Delete/rewrite:** only comments and stale path fixtures identified in `## Stale doc comments`; no behavior changes. Keep accurate historical comparisons.
- **Verify:** `cargo test --workspace` (fixtures are compiled), then search `src/` and `src-tauri/src/mnemo2/` for `src-tauri/src/ipc`, `src-tauri/src/main.rs`, `frontend/`, and present-tense Mnemo1 host descriptions. Remaining hits should be deliberate history or test data, each with a reason.

If the lead approves removing the UNSURE unattended convention, fold that into **step 6** (it already owns `runtime/channels.rs`) and add `src/app/workflow_policy.rs`, `src/agent/loop_impl.rs`, `src/runtime/agent.rs`, and the related plan-policy tests to that step; in that variant step 8 must not edit those added files.

## Questions for the lead

1. **Is `UNATTENDED_PROMPT_PREFIX` a supported embedding convention or Mnemo1 Run-All wire format?** Options: (a) KEEP it, rename its docs to generic unattended sessions, and retain `PromptHook.unattended`; (b) REMOVE the prefix stripping, automatic policy branch, and now-dead unattended hook/tests as an extension of the UI-event step. **Recommendation: KEEP (option a)** under the plan's public-library rule unless the product explicitly declares the prompt prefix Mnemo1-only. It is observable behavior available through the ordinary public prompt API, unlike the shell-only event DTOs.
2. **Decision-record reconciliation, not an UNSURE verdict:** may the earlier 2026-11-11 note that listed `[general.budget]` for retirement be superseded? **Recommendation: yes** — record that the inventory found live enforcement in `agent/turn.rs`/`agent/budget.rs` and a live Mnemo2 editor, so the section stays. Removing it would be a product feature removal, not dead-code cleanup.

No other summary row is UNSURE.
