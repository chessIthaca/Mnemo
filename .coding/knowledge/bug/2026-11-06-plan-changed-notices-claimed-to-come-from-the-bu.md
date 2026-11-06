+++
title = "plan-changed notices claimed to come from the built-in lead — FIXED"
created = "2026-11-06"
+++

Symptom: a plan decision recorded by Claude Code reached working workers headed "[From the built-in lead]", and the inbox recorded sender lead. Root cause: the board's edit observer in TeamRuntime::attach_board (src-tauri/src/mnemo2/team.rs) hard-coded MessageSender::Lead. Fix: it sends MessageSender::Mnemo ("[From Mnemo2]"). Rule: a message Mnemo2 generates itself is signed Mnemo; the observer only knows plan_id and kinds, so it must not guess a lead, and choosing by whether an external lead is attached would still mislabel an edit from the built-in lead. Sender only selects the delivered header and the inbox label; no code branches on it. Regression test: plan_edit_notifies_only_working_workers asserts the sender.
