+++
title = "backlog plan chip — plan title + short id replace the raw sha headline"
created = "2027-01-07"
+++

The Backlog tab's in-flight items identify the working plan by TITLE + 8-char short-id chip (backlog f45513b2, plan 33383b61) — clickable → switches to the working agent's chat tab while in-flight (inert once done/failed, or no main agent); the pre-work git checkpoint sha is demoted to a copyable detail row + tooltip (manual resume/rollback anchor, full sha stays machine-copyable).

Recording chain: `stamp_backlog_in_flight` (src-tauri/src/ipc/run_all.rs) fires on EVERY main-agent Executing entry and reads the plan file's `# Plan: <title>` heading via `plan_title_from_file` (BufReader, first line only — plan bodies can be large), passing title + ROOT plan id to `BacklogItem::set_plan_id` (src/backlog.rs) — id and title always re-link together (a fresh plan replacing an abandoned one mid-dispatch re-links both; sub-plan pushes are no-op refreshes since top_plan_id is the root). Plan files are the canonical title source because `WorkflowStateChanged` carries only {state, top_plan_id}.

Storage/payload: `plan_title: Option<String>` on `BacklogItem` with `#[serde(default, skip_serializing_if = "Option::is_none")]` — old JSONL lines deserialize as None (backward compatible). `BacklogItemView` (src-tauri/src/ipc/backlog_cmds.rs) flattens it into the backlog-changed payload; TS `BacklogItem` (frontend/src/lib/types.ts) declares `plan_id`/`plan_title` optional; dto-backlog-changed-payload.json pins present + absent cases.

UI: `planChipTarget(status, mainAgentId)` helper in frontend/src/components/views/BacklogView.tsx decides clickability (in_flight + main agent present → `setActiveAgent(mainId)`); the SSR suite (renderToStaticMarkup) cannot render the clickable case (zustand getServerSnapshot reads initial store state) — pinned instead by `planChipTarget` unit tests + a source-contract test asserting the `setActiveAgent(chipTarget)` wiring.

Shipped: commits 82466c3 (feature + round-1 review fixes: doc re-attachment, README bullet, tooltip wording, heading-only read) + 0da8f09 (round-2 PASS report .coding/reviews/2027-01-07-backlog-plan-chip-round2-verification.md).
