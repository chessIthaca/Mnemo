+++
title = "Use an OS file lock for cross-process config writes, not a create/rename lock file"
created = "2026-11-04"
+++

A hand-rolled lock file (create_new to acquire, an mtime staleness check, and a rename-aside takeover of stale locks) can always be raced. Three audits in a row on 2026-11-04 found schedules where two processes held it: check-then-rename moved a fresh lock aside, and a third process could create_new into the gap while the fresh lock sat in quarantine. Use the operating system's exclusive file lock instead: std::fs::File::lock / try_lock (stable since Rust 1.89; this repo uses 1.95) on a persistent lock file that is never deleted or renamed. The OS frees the lock when the holder exits or crashes, so there is no staleness, takeover or restore step. Retry try_lock with a short sleep up to a timeout, and surface the timeout as an error.
