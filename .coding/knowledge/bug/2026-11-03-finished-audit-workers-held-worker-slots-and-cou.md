+++
title = "Finished audit workers held worker slots and could be revived from mission control"
created = "2026-11-03"
+++

Symptom: audit workers stayed 'idle' after reporting and held worker slots ('all N worker slots are busy'). Stopped audits came back when Resume, Continue or Compact was clicked in mission control. Root cause: slots count live sessions (team.rs live map), and an audit's session stayed live after its turn; the UI and backend allowed reviving finished audits. Fix: WorkerRecord.report_delivered is set at the audit's message_parent report; at its Finished event the pump releases the session (release_finished_audit, status Completed, record and worktree kept). audit_finished() refuses resume, compact, change_model and message with 'already delivered its report; dispatch a new audit', and mission control hides those buttons for finished audits. Regression: finished_audit_releases_its_slot.
