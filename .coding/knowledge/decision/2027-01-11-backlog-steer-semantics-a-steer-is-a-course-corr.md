+++
title = "backlog steer semantics — a steer is a course correction, never a requeue"
created = "2027-01-11"
+++

User spec (2026-10-16): steering a session mid-run must NOT return a backlog item to the queue — a steer is a course correction, never an abandonment. An item returns to the queue (Pending) ONLY when the user abandons the plan, plus the legitimate agent-exited dead-run recoveries (main/spawned agent exited while the run was active, src-tauri/src/ipc/run_all.rs:5414/:6821). The status flow already keeps InFlight items in flight on a steer (run_all.rs:7150-7197, the b83e891f + 0296d448 fixes); the defect is the note semantics — the suffix "{reason}, returned to queue" (run_all.rs:7148) is also stamped on Pending items that never left the queue (the :7210-7213 arm), so the user sees "steered by the user, returned to queue" and reads a requeue that must not exist. "Returned to queue" language is reserved for actual requeues only. Queued as a backlog item 2026-10-16; fix together with the sibling observed bug c5feaec4 (steered item stayed InFlight after its plan completed — completion-side auto-resolution skipped).
