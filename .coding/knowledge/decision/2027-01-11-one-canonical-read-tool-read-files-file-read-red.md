+++
title = "one canonical read tool — read_files; file_read redirects, never aliases"
created = "2027-01-11"
+++

2026-10-16, user-ratified (queued as a backlog item): read_files (src/tool/agent/read_files.rs) is the ONE live read tool (registered at src/agent/factory.rs:1522); file_read (src/tool/agent/file_read.rs) is retired — it survives only in test fixtures, registered nowhere live. Landed direction: (1) read_files normalizes a top-level {path, start_line?, max_lines?} with no `files` key to a one-element files array, removing the hint that pointed at the unregistered file_read; (2) unknown-tool redirect for file_read → read_files and file_append → file_write (src/agent/dispatch.rs:240, src/tool/mod.rs:1330); (3) delete FileReadTool, migrate test fixtures. Explicitly REJECTED: a silent dispatchable-but-unlisted alias — it hides the footgun, needs new registry machinery, and the model habit never unlearns; the redirect recovers in one shot and unlearns the name. Root evidence: observed "quite a few failures of file_read but not read_files" = models calling the retired name from priors into the generic unknown-tool error.
