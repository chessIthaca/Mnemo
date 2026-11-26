+++
title = "a worker session snapshot's transcript starts with the System head, not KICKOFF"
created = "2026-11-25"
evidence = "The remote e2e test printed the transcript over the real hub↔node link: the System head held \"Write remote.txt at the project root with the line …\". src-tauri/src/mnemo2/runtime/tests.rs:8695-8700."
+++

`WorkerRecord.transcript` (assigned from `WorkerSession::snapshot()`) is the full session transcript: `messages[0]` is a `Role::System` message carrying the pinned task — plan/step spec plus the lead's dispatch brief verbatim — and the KICKOFF text is the first `Role::User` message, followed by the assistant/tool turns. Code that looks for the opening message must filter by role (runtime/tests.rs filters role=="user" and asserts KICKOFF); an is_empty check on the transcript already proves the head and kickoff landed, because the snapshot is atomic.
