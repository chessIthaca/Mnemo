+++
title = "run the Mnemo2 dev preview when another worktree holds port 5180"
created = "2026-11-02"
+++

Mnemo2 UI workers each start `npx vite --port 5180 --strictPort` in their own worktree, so the port is often already held by ANOTHER agent's worktree (seen 2027-11-02: agent-rowColum's vite held 5180 while agent-modelPag's strictPort instance silently exited, and the preview page rendered the OTHER checkout's stale scenes — including a 5-entry scene index where 6 were expected). Before judging preview output stale, check the listener's owner: `Get-NetTCPConnection -LocalPort 5180` → `Get-CimInstance Win32_Process -Filter "ProcessId = <pid>"` shows the serving worktree in CommandLine. Never kill another agent's server — start your own on a free port (`--port 5190 --strictPort`) and verify against that. Start-Process npx.cmd records the WRAPPER's pid; the node child outlives `Stop-Process` on it, so kill the node process owning the listener.
