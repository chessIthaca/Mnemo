+++
title = "worker hosts are managed in Settings → Worker hosts through the same code as team_hosts"
created = "2026-11-26"
evidence = "plan e8cb5112; hosts_action_matches_the_team_hosts_tool_output; WorkerHostsPage.test.ts; workerHost.test.ts"
+++

The Mnemo2 app's Settings → Worker hosts page (mnemo2/src/components/WorkerHostsPage.svelte, lib/workerHosts.ts, under 'All projects' after Machine) lists, pairs, links, unlinks and revokes worker hosts. It calls the Tauri commands worker_hosts_list/pair/revoke/link/unlink (src-tauri/src/mnemo2/ipc.rs), which run TeamRuntime::hosts_action, the same code as the lead's team_hosts tool, whose output text is unchanged. The commands need an open project (current_team), because check qualification is per project. Workers show their host from the snapshot's record.remote: a host badge in WorkerRow, a 'host:' pill in WorkerDetail, and an away state only for active or interrupted workers (lib/workerHost.ts hostAway, mirroring team_status host_offline_s). preview.html?scene=worker-hosts and ?scene=remote-workers render both without the desktop app. The user guide is docs/WORKER-HOSTS.md, and CI asserts that installers carry mnemo2-node.
