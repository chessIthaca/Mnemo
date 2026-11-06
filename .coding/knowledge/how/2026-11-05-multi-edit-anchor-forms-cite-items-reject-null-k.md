+++
title = "multi_edit anchor forms — cite items reject null keys; never hand-type dash runs"
created = "2026-11-05"
+++

multi_edit/file_edit ops-array anchor forms (extends the existing anchor-shape HOW): the VERBATIM form ({old_string, new_string}) accepts null placeholders for from_read/lines, but the READ-RECEIPT CITE form ({from_read, lines, new_string}) rejects null old_string/count/fuzzy_whitespace with "invalid type: null, expected a string" — omit those keys entirely when citing. Never hand-type long runs of identical characters (section-header dash lines) as old_string: a 2-dash miscount cost one failed call ("matched 75/126 chars"); anchor on a nearby unique line instead. A fuzzy (whitespace-normalized) match still splices the new_string verbatim, so the replacement's own whitespace is what lands. When a call fails, read the error and fix the named parameter — never resend identical arguments; after two identical failures, stop and report.
