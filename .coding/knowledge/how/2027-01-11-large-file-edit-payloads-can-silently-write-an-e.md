+++
title = "large file_edit payloads can silently write an empty replacement — chunk them"
created = "2027-01-11"
+++

Symptom (2027-01-15, plan ecdd67ac step 1, src/memory/knowledge.rs): a file_edit whose new_string was ~4.5KB returned "edited <path>" but wrote an EMPTY replacement — the anchored old_string was deleted, the new text never landed, and the following test's header was collateral damage (4 lines gone; `supersede_flips_old_and_writes_successor`'s `#[test] fn` line vanished into the previous test's body). The tool's own NOTE ("large edit payload … emission fragility has been observed near this size") starts appearing around ~1.5KB — treat it as a real warning, not a formality.

Detection: `git_read op=diff` — it shows the truncation precisely (only deletions, the intended insertion absent).

Recovery that worked: (1) `git restore` with source=HEAD, target=worktree, paths=[that file] — safe when the diff proves the file's only changes are the damage; (2) re-apply the intended test in three ≤1.8KB chunks, each anchored on the PREVIOUS chunk's unique tail text, then read the region back to confirm assembly before compiling.

Rule: keep file_edit new_string under ~1.5KB; above that, split by anchor-chaining or use file_write. Always read the edited region back (or run the targeted test) before assuming an edit landed.
