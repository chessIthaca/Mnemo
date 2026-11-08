A fresh Mnemo2 install, and any computer with a Mnemo checkout, starts with the Mnemo project's routing decisions so the kNN overlay works from day one. Only a project that keeps a `.coding/laya/` folder exports its rows, and a copy that conflicts with this computer's own decisions asks before it updates.

WHY. What Laya learns for model choice lives only on one machine: `~/.mnemo/laya/training/dispatch_routing.jsonl` (`dispatch_routing_log_path()`, src-tauri/src/mnemo2/dispatch_routing.rs). The Laya model itself never changes (laya 0.3.20 has no fine-tune surface), and the kNN overlay over this log is what actually improves: 9 of 11 picks matched the final tier versus 17 of 85 for the model on 2026-11-08. The user wants that outcome stored in git so other computers and the installer get it, with a question when versions conflict.

WHAT SHIPS AND WHAT DOES NOT.
- Ships: lines of dispatch_routing.jsonl, verbatim, whose `plan_id` names a plan file in the project's `.coding/plans/`.
- Does not ship: rows without a plan, other projects' rows (the user chose Mnemo-project rows only, because this repo and its installer are public; every row carries a step brief's full text), thresholds (the Laya check computes them on each machine from the hand set compiled into the app plus the checkpoint: `eval_fingerprint` in src-tauri/src/mnemo2/route_eval.rs), Mnemo1's routing.jsonl, tool_choice.jsonl, model files, embeddings. Each machine embeds imported rows with its own embedder: `RouteKnn` (src/agent/route_knn.rs) embeds new log rows itself, so the embedder is untouched.

DECISIONS.
- The opt-in is the folder: a project exports only when `<root>/.coding/laya/` exists. This repo gets the folder; other projects have none, so their rows never leave the machine. No setting.
- The bundle is `<root>/.coding/laya/dispatch_routing.jsonl`. A bundle version is the sha256 of its bytes. Row identity is the exact line text, because export copies lines verbatim.
- Export is a union that never removes a line: existing bundle lines stay in order, new project rows are appended in log order. It runs best-effort after a merge, right after the outcome row is appended. It never runs at project open: an export before the import check would hide a conflict.
- Git merges of the bundle use `merge=union` in .gitattributes, so two computers appending never conflict.
- `.coding/laya/` joins `LEAD_BOOKKEEPING_PREFIXES` (src-tauri/src/mnemo2/phase3.rs), so a refreshed bundle never blocks a merge; it is committed with the lead's other records.
- The installer bundles the file as a resource, `laya/dispatch_routing.jsonl`, through src-tauri/mnemo2-app/tauri.conf.json. The prompt lives in the app, at project open, because an installer page would need separate MSI, NSIS and DMG work.
- Import appends only. `RouteKnn::refresh` tracks the log's byte length and reads only the new bytes, so rewriting the log would corrupt the index. A later label for a worker supersedes an earlier one by file order.
- Conflict rule, like git fast-forward versus diverged. `missing` = bundle lines not in the log. `local_only` = log lines not in the bundle (project source: only rows of this project; installer source: all rows). Nothing missing: up to date. Missing and no local_only: import silently. Missing and local_only: ask. "Update" appends the missing rows and removes nothing. "Not now" records that bundle version as declined, so the same version never asks again; once declined, a later version of that source always asks instead of importing silently.

ACCEPTED RISK. The kNN keys label rows by `worker_id`, so an imported worker id reused on another computer means one vote instead of two. Harmless.

VERIFY (run in your worktree; a fresh worktree first needs `npm ci --no-audit --no-fund` then `npm run build` at the worktree root; the shell tool stops commands at 300 s, so run long cargo builds detached with Start-Process, logs under .coding/tmp/, and poll):
- `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib`
- `cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --all-targets -- -D warnings -A clippy::redundant_guards -A clippy::nonminimal_bool`
- `cargo test --test boundary`
- window steps, in `<worktree>\mnemo2`: `npx vitest run`, and svelte-check through the main checkout's shim: `& C:\mnemo\mnemo2\node_modules\.bin\svelte-check.cmd` (read $LASTEXITCODE)
- `git diff --check`
