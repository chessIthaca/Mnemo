+++
title = "a failed rebuild clears its marker, so the next Mnemo call starts a second Mnemo2 beside a stalled quit"
created = "2026-11-09"
evidence = "Observed 2026-11-09 with pids 10796 and 58248: Get-Process, the run directory and the rebuild output."
+++

If `npm run rebuild:mnemo2` fails with "pid N did not exit within 60 s", the old process has already removed its run file and stopped its endpoint, but it stays alive with its window visible and keeps target\release\mnemo2.exe locked. The script then deletes the rebuilding marker, so the next Mnemo tool call or hook makes the helper start a new hidden Mnemo2, because it sees no run file. The run file therefore no longer names the stalled pid: `mnemo2-mcp quit` cannot reach it and the tray's Quit is ignored once an exit is under way (ExitRequested returns when the phase is not running, src-tauri/src/mnemo2/mod.rs). Check `Get-Process mnemo2` before retrying, and avoid Mnemo calls between attempts. Seen once: the stalled process idled about 2 minutes or more with no CPU and an idle main thread, no log or crash file, and was gone later, cause unknown. Shutdown's 10 s deadline (spawn_orderly_shutdown) should have made this impossible, so the stall itself is an open bug.
