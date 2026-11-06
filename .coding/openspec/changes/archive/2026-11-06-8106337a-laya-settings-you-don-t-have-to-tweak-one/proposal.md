In Settings > Tiers the user decides only how much Laya helps. The routing eval becomes an automatic "Laya check" with visible progress, like indexing, and it applies its own gates. The tuning knobs and the false "unsaved changes" prompt are gone.

WHY: the user finds the Laya part of Settings > Tiers (mnemo2/src/components/TiersPage.svelte) too much to tweak: it is unclear when eval is needed, eval is hidden under collapsed "Routing details" with no progress, it always asks to save with nothing to save, and nobody knows what the boxes under Nudges are for.

ROOT CAUSES FOUND BY THE LEAD:
1. False save prompt: `RoutingSettings` in src-tauri/src/mnemo2/dispatch_routing.rs has NO `effort` field, but TiersPage seeds `dispatchDraft = {...settings, effort: {...settings.effort}}`, so the draft has `effort: {}` and `routingStatus.settings` has none. `dispatchDirty` compares JSON, so it is always true. Test fixtures include `effort: {}`, which hid it.
2. The "trivial/routine/medium/high effort" text boxes under Nudges write that same dead field. They do nothing. The real per-tier effort lives on the ladder.
3. A second false prompt: after `saveLadder`, `draft` keeps its old `resolves_to` while `tiers` becomes the stored copy, so `ladderChanged` can stay true after a save.
4. The eval result lives only in component state, `run_routing_eval` blocks with no progress, and a dead classifier still writes a record.

DECISIONS ALREADY TAKEN (do not reopen):
- The check runs when a project opens, like the indexing check: it compares a fingerprint and only re-runs when the test set or Laya's checkpoint changed, because the eval is deterministic for those inputs. It also runs when a Laya choice other than off is picked, when an install finishes, and on a "Check again" button.
- Its progress rides the existing indexing status: a third pass `laya` on `IndexingSnapshot`, broadcast on the existing `mnemo2://indexing` channel. No new channel, no new store state. The first-run project hold still looks at the code pass only.
- The check applies its suggested gates itself. A `None` suggestion leaves that gate as stored, exactly what the old Apply button did. The suggestion formula (`suggested_gate`, 90% floor) is NOT changed in this plan.
- A check where Laya mostly did not answer fails visibly and writes nothing.
- The UI drops the gates, min words, fallback tier, kNN, nudges and effort inputs, the eval tables, the Force buttons and the Mnemo1 gates. Those values stay in `mnemo2.toml` / `config.toml` and keep working; Mnemo1's own Classifier settings still edit its gates. No config field is removed or migrated.
- Laya's other features (the 6 flags) stay available under a collapsed section and save on click. No Save button.
- One tiers page; do not add a rail page. Keep the four choices (off, shadow, suggest, auto). Locked choices are no longer locked: they show a hint.

CONSTRAINTS:
- Tests must never read or write the real `~/.mnemo/laya/training/route_eval.json`: production passes `route_eval_path()`, tests pass a temp path.
- `#![deny(warnings)]` applies, and CI uses clippy-clean code. svelte-check must stay at 0 errors AND 0 warnings, so remove CSS that loses its markup.
- Never start a real sidecar or use the network in tests.

VERIFY:
- `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::route_eval`
- `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::indexing`
- `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::laya`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cd mnemo2 && npx vitest run`
- `cd mnemo2 && npx svelte-check --tsconfig ./tsconfig.json`
