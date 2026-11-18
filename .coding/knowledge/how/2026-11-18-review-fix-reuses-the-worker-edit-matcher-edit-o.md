+++
title = "review_fix reuses the worker edit matcher; edit_ops::count_matches is now `pub`"
created = "2026-11-18"
evidence = "Tests mnemo2::team::tests::review_fix_{matches_lf_old_string_in_crlf_file,keeps_crlf_endings_in_new_lines,still_refuses_ambiguous_old_string,lf_file_unchanged_behavior,reports_near_miss_without_the_read_nudge}; commit abc5f297 on wt/agent-rvFixEol (plan f89fd63d step 1)."
+++

Mnemo2's lead `review_fix` (src-tauri/src/mnemo2/team/review.rs, free fns `prepare_review_fix` / `matcher_miss_detail` / `evidence_note`) no longer matches byte-exactly: it builds a `mnemo::tool::agent::file_edit::FileEditArgs` (path, old/new, count=Some(1), no lines/from_read) and calls `prepare_edit`, so an LF old_string hits a CRLF file (and the reverse) and the replacement is written in the file's own ending style. The exactly-once guard uses `mnemo::tool::agent::edit_ops::count_matches`, which is now `pub` instead of `pub(crate)` — keep it pub. Historical message "review_fix old_string must occur exactly once" is kept for miss and ambiguity; a miss appends the matcher's near-miss detail with the worker fresh-read nudge trimmed (cut the nudge tail BEFORE splitting on " — "). Fallback NOTEs go into the finding evidence. Worker edit path itself was already EOL-agnostic; line endings were never the worker bug.
