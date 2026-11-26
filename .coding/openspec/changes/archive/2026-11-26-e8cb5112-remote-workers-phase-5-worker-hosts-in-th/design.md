## Decisions

- 2026-11-26: no spec impact: external-lead, tier-ladder and worker-brief are untouched: step 1 only moves team_hosts' body into a shared function with byte-identical tool output; the dispatch-routing delta covers the step 2 behaviour change.
- 2026-11-26: added step 7 (Integration check) because steps 1 and 4 ran in parallel
- 2026-11-26: host_offline_s in team_status and the UI away state cover the same statuses: is_active (Dispatching, Working, NeedsYou, Fixing) plus Interrupted. team.rs is_active itself is unchanged, because host-restart handling depends on it; worker_status_view adds Interrupted explicitly. team_status_shows_host now builds its worker as NeedsYou instead of InReview (one-line test change accepted).

## Interfaces

- 2026-11-26: Step 1 owns team_hosts, hosts_action and the five team_hosts_* helpers in src-tauri/src/mnemo2/team/lead_tools.rs. Step 2 owns only the continue arm (~:1076-1088), continue_agent_answer and worker_status_view's host_offline_s block in the same file. Both add new tests at the end of team/tests.rs; whichever merges second rebases and keeps both.
- 2026-11-26: Tauri commands worker_hosts_list / worker_hosts_pair(name?) / worker_hosts_revoke(name) / worker_hosts_link(name, ssh) / worker_hosts_unlink(name) return the team_hosts JSON objects defined in the plan context; step 3's lib/workerHosts.ts types follow them exactly.
- 2026-11-26: Worker.host = {name, os, offlineSinceMs, ssh} | null (step 4, lib/types.ts), mapped from TeamWorkerRecord.remote; later UI reads host state only through it and lib/workerHost.ts hostAway.
- 2026-11-26: File ownership for steps 1 and 2: step 2 does not edit src-tauri/src/mnemo2/ipc.rs or src-tauri/src/mnemo2/mod.rs (step 1 owns both). In team/tests.rs, each step appends only its own new test functions at the end of the file and changes no existing test; the second step to merge rebases and keeps both blocks.
- 2026-11-26: `src-tauri/src/mnemo2/team/tests.rs` is shared by steps 1 and 2: each step only appends its own new test functions at the end of the file and changes no existing test; the second to merge rebases and keeps both blocks.
- 2026-11-26: `mnemo2/src/components/WorkerHostsPage.svelte` is owned by step 3; step 5 only reads it (via git show from step 3's branch) to quote its UI labels in docs/WORKER-HOSTS.md and never edits it.
