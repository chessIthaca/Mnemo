+++
title = "mission control draggable split — bounds, board_width preference, test trap"
created = "2026-11-02"
+++

Mission Control's list/output split is divider-driven: `ControlRoom.svelte` puts the grid columns in a `--board-columns` custom property from `$app.boardWidth` (null = the default 0.88fr/1.12fr proportion), with a `.pane-divider` button between the panes (pointer-capture drag, ArrowLeft/Right ±24, double-click resets to null, hidden in the ≤760px stacked layout; no separate ≤980px media variant).

Bounds live in `mnemo2/src/lib/store.ts` next to the workbench pair: `MIN_BOARD_WIDTH` 300 (the WorkerRow 152px status column needs room), `MIN_BOARD_OUTPUT_WIDTH` 420, `BOARD_DIVIDER_WIDTH` 9, `DEFAULT_BOARD_SHARE` 0.44; `maxBoardWidth(w) = max(300, w - 420 - 9)`, plus `clampBoardWidth`, `defaultBoardWidth`, `setBoardWidth`, `reclampBoardWidth` (a null default re-adapts itself, so re-clamp only touches saved widths).

Persisted as `board_width: Option<u32>` in `ProjectPreferences` (`src-tauri/src/mnemo2/project.rs`, struct-level `#[serde(default)]`, validated 300..=8192) and `board_width: number | null` in `projectEngine.ts`; `AppState.boardWidth` sits at the end of the interface in `types.ts`.

Trap worth remembering: a project.rs preferences test that writes `preferences.json` straight into `state_dir` fails with "was not created by Mnemo2 on this machine" — call `save_preferences` once first so the dir carries its owner stamp, then overwrite the file.
