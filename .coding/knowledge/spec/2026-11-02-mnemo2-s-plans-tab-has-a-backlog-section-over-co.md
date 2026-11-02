+++
title = "Mnemo2's Plans tab has a Backlog section over .coding/backlog.jsonl"
created = "2026-11-02"
+++

The Plans route (PlansView.svelte) shows BacklogSection.svelte above the plans: pending ideas in order with add, inline edit, reorder, defer, remove and "Clear finished". Backend: src-tauri/src/mnemo2/backlog.rs commands backlog_items / backlog_add / backlog_edit / backlog_remove / backlog_reorder / backlog_set_deferred / backlog_clear_finished, each opening mnemo::app::backlog::BacklogStore fresh on <project>/.coding/backlog.jsonl (no long-lived copy in Mnemo2) and emitting mnemo2://backlog-changed after writes; ideas are capped at 2000 characters and edits keep existing images. The Workbench's Plan tab does not show the backlog; there is no dispatch or run-all from it in Mnemo2.
