+++
title = "Mnemo2 plan review reports carry a per-task Effort table"
created = "2026-11-03"
+++

WorkerRecord (src-tauri/src/mnemo2/team.rs) has #[serde(default)] review_rounds, fix_rounds, audits (counted on the audited target) and models_used (no consecutive duplicates; note_model at start, resume and change_model). ledger.rs render_effort writes "## Effort" after "## Tasks": | Task | Step | Models | Review rounds | Fix rounds | Findings (high) | Audits |, with no rows for audit workers, followed by a Total line. Ledgers written before this render zeros. Use it to judge whether longer specs reduce review and fix rounds.
