+++
title = "backlog.jsonl clobbered to empty by stale in-memory state after git ops"
created = "2026-08-31"
+++

BUG: .coding/backlog.jsonl wiped to empty (stale in-memory state persisted over committed entries).

SYMPTOM: After a git operation (merge/checkout/restore/merge_to_main) changes backlog.jsonl on disk, the next backlog mutation wipes the file — items committed at HEAD disappear from the working tree. Recurring "steering-time backlog clobber" (commits 3b99a87, 6e5cf25 repaired symptoms; root cause persisted).

ROOT CAUSE: BacklogStore (src/backlog.rs) loads its items vector ONCE at open() and never re-syncs from disk. Every mutating method (add/remove/reorder/transition/set_status/requeue/edit/clear_finished) calls persist() which writes the in-memory vector unconditionally — never re-reads disk first. When git changes the file externally, the in-memory state is stale; the next mutation persists the stale state, clobbering git's changes. reorder(&[]) on an empty (stale) store always persists, even when empty — the direct path to the empty-file clobber.

FIX: Added reload_from_disk(&mut self) — re-reads the file and replaces self.items (graceful on missing/corrupt: keeps in-memory state). Called at the start of every mutating method → read-modify-write. Safe because every mutation persists immediately (in-memory == last disk write; reload only picks up post-write external changes). NOT added to persist() itself (the open()→migrate_inline_images()→persist() path must not reload, or it would undo the migration).

REGRESSION TEST: backlog::tests::mutation_does_not_clobber_external_disk_changes (src/backlog.rs) — opens a store on an empty file, writes items to disk directly (simulating git restore), calls reorder(&[]) then add(); asserts the disk items survive. Fails without the fix (0 items), passes with it (2→3 items).
