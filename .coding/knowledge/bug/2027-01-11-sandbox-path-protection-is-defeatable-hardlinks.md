+++
title = "sandbox path protection is defeatable — hardlinks, Win32 8.3 aliases, ladder step-5 lexical fallback"
created = "2027-01-11"
+++

Three PRE-EXISTING ways to defeat `Sandbox`'s path-prefix write protection (surfaced by review rounds 2–4 of plan ee65fd4b, 2027-01-11; not fixed there — recorded, see the backlog item).

1. HARDLINKS (the dangerous one). `canonicalize` cannot see a hardlink (there is no link to follow; the path IS the file), so both `is_artifact_write_target` and `is_protected_write_target` pass on `.coding/x.toml` hardlinked to `.coding/safety.toml` / `memory.db`, and on `.coding/x.rs` hardlinked to `src/lib.rs`. Any plan kind can then truncate the shared inode — i.e. write a protected file (safety.toml, memory.db, plans, reviews, knowledge) or source through an unprotected-looking path. Creating the link needs `shell` (approval-gated), but a committed or pre-existing link needs no new privilege. src/tool/agent/sandbox.rs (`is_protected_write_target`, new `is_artifact_write_target`), tools in src/tool/agent/{file_write,file_append,file_edit,convert_line_endings}.rs.

2. Win32 8.3 SHORT-NAME ALIASES. The refusal-side normalization trims case + trailing dots/spaces per component but not 8.3 aliases, so `.coding/KNOWLED~1/new/x.md` (knowledge is 9 chars → gets an alias; plans/reviews/.git fit 8.3 and get none) reads as `.coding` + second component + NOT protected → research artifact grant; the ladder's step 4 `create_dir_all` resolves the alias and plants a new subtree inside the real `.coding/knowledge/`. Overwriting existing protected files stays impossible (an alias of an existing file canonicalizes to the long name). Environment-gated: check with `fsutil 8dot3name query C:` or `dir /x .coding`. Fix direction: re-run `is_protected_write_target` on step 5's CANONICAL result in `validate_for_write` (src/tool/agent/sandbox.rs), not just on its lexical input.

3. LADDER STEP-5 LEXICAL FALLBACK. `validate_for_write` (src/tool/agent/sandbox.rs:362-387) returns the LEXICAL path when the final revalidation fails, so a write through a link that escapes the root lands outside it — reachable for implementation/bug_fixing plans (the research route is closed by plan ee65fd4b's artifact verdict). Fix direction: never fall back to the lexical path when the revalidation failed for a reason other than "parent created".

Authored: 2027-01-11. Evidence: .coding/reviews/2026-09-14-research-artifacts-subplan-review-round2.md (§Q5 residual), …-round4.md (§Q5), .coding/analysis/.
