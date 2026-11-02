+++
title = "Mnemo2 workers never ask the user; needs-attention surfaces in the Lead view"
created = "2027-01-11"
+++

The user (2026-10-31) said workers asking questions is bad and must be avoided. This is already enforced: src-tauri/src/mnemo2/team.rs executor_guardrails and audit_guardrails exclude ask_user and use QuestionPolicy::Deny, and workers run with PermissionPolicy::OverridePermissions. So in Mnemo2 "needs you" in practice means a failed or conflict worker, not a question. Never add UI for answering worker questions. A worker that needs attention shows as a "Needs you: <name> (<status>)." line in the Lead chat (LeadChat.svelte); clicking it opens the worker in the right-side WorkerPanel.svelte, which offers Continue, Delete, and Open in Mission Control.
