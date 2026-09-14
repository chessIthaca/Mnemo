+++
title = "traces.jsonl mirror rewrote the whole file with fs::write — partial trailing line + permanent fragment"
created = "2027-01-11"
+++

SYMPTOM (found 2027-01-11 while analysing the freshly-enabled trace log): `.coding/logs/traces.jsonl` contained a 444,566+ char line that failed `json.loads` (unterminated string); the round-5 extractor `.coding/analysis/cache-hit-5-extract.py` crashed on it (blind parse).

ROOT CAUSE — the traces.jsonl mirror is NOT an append log. `src/provider/trace.rs`'s writer (the merge step around :1630-1670) does `read_to_string(existing)` → merge the ring → `let mut content = out.join("\n"); content.push('\n'); std::fs::write(path, content)`. `fs::write` opens with truncate+create, so on EVERY writer wakeup the whole file is emptied and rewritten. Consequences: (a) a concurrent reader (analysis tooling reads this file live) can observe an empty file or a half-written final line — the transient half of the symptom; (b) a kill mid-write leaves the truncation on disk permanently; and (c) the merge loop COPIED an unparsable fragment forward into every later rewrite (a fragment has no `id`, so it never matched) — making the corruption permanent.

FIX (plan f69317d4, R6-3): `write_mirror_atomically(path, content)` — write `<name>.tmp` in the same directory, `restrict_log_file` the TEMP (the rename carries the metadata), then `std::fs::rename` over the target (atomic on Unix and Windows via MoveFileEx replace-existing; a failed temp write leaves the previous mirror intact). Plus: a trailing line without a closing LF (the writer always emits one) is dropped on read as a crash fragment.

TESTS: `provider::trace::tests::mirror_drops_a_crash_truncated_trailing_fragment` (verified failing with the fragment-drop neutralized) and `mirror_write_failure_leaves_the_previous_mirror_intact` (temp path occupied by a directory → target must be unchanged). NOTE: a reader-race test (`mirror_write_is_atomic_for_concurrent_readers`) was written and REMOVED — racing a reader thread against 200 rewrites of a 300 KB body never caught a truncate-in-place implementation, so it could not fail without the fix; the transient arm is guarded by construction (rename), documented in the test's stead.

CONSUMER RULE: any tool reading the live mirror must tolerate/skip an unparsable trailing line (the round-6 extractor does; the round-5 one did not).

Amended 2027-01-11: Correction (review round 2, LOW A): the temp file name is `<name>.tmp<pid>` — PID-unique. The FIX paragraph above says `<name>.tmp` (shared, fixed name); the shipped code computes `format!("{file_name}.tmp{}", std::process::id())` inside `write_mirror_atomically`, and both mirror tests key on the PID-suffixed name so producer and tests cannot drift. The PID suffix is what actually closes the two-writer interleaving hole: the double-start conflict dialog warns about a second instance on the same project, it does not lock, so two processes must not stage temp writes at the same path and rename a torn temp into place.
