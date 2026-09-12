+++
title = "BacklogStore mutations are read-modify-write (reload-from-disk before every mutation)"
created = "2026-08-31"
+++

DECISION: BacklogStore mutations are read-modify-write — every mutating method (add, remove, reorder, transition, set_status, requeue, edit, clear_finished) calls reload_from_disk() before mutating+persisting, so external (git merge/checkout/restore/merge_to_main) changes to .coding/backlog.jsonl are never clobbered by a stale in-memory state.

Rationale: BacklogStore is a single Arc<Mutex<>> loaded once at open(); git can change the file underneath it. Without reload, the next mutation persists stale state and wipes git's changes (the recurring "steering-time backlog clobber", commits 3b99a87/6e5cf25 patched symptoms; root-cause fix landed 99ab9f7, plan 0a226a1a).

Constraint: reload_from_disk is deliberately NOT in persist() itself — the open() → migrate_inline_images() → persist() path must stay reload-free (a reload there would re-read the still-unmigrated disk and undo the migration). Any NEW mutating method added to BacklogStore MUST call self.reload_from_disk() as its first statement. Graceful on missing/corrupt file (keeps in-memory state); an empty file is treated as a valid empty backlog (clears in-memory).

Regression test: backlog::tests::mutation_does_not_clobber_external_disk_changes (src/backlog.rs).
