+++
title = "null-stringify transport defense — optional-property drop at the dispatch seam — MERGED into main (5a86e9d)"
supersedes = "2027-01-11-null-stringify-transport-defense-optional-proper"
created = "2027-01-11"
+++

MERGED into main at 5a86e9d (5a86e9d1157a497a8f6fdc567c29d584dda774eb) on 2027-01-24 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 33ad6ec) — supersedes this record's earlier unmerged marker. WHAT: the transport stringifies JSON null for string/enum params (and auto-fills omitted optional properties with it) into the literal string "null"; since plan 50f36b1e (commit 1ea2940) a central drop_stringified_nulls defense at the dispatch seam drops it from optional properties, keyed on the advertised schema's required list. Knowledge file: .coding/knowledge/spec/2027-01-11-null-stringify-transport-defense-optional-proper.md.
