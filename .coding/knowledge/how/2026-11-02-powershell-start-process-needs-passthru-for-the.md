+++
title = "PowerShell Start-Process needs -PassThru for the spawned pid in test commands"
created = "2026-11-02"
+++

In PowerShell 5.1, `Start-Process` WITHOUT `-PassThru` returns nothing — `$p = Start-Process ...` leaves `$p` as $null, so `$p.Id` is $null and `Set-Content <file> $p.Id` writes an EMPTY file (a subsequent `parse::<u32>()` fails with ParseIntError kind: Empty). When a test command needs the spawned pid, always pass `-PassThru` — e.g. `$p = Start-Process -NoNewWindow -PassThru ping -ArgumentList '-n','60','127.0.0.1'; Set-Content grandchild.pid $p.Id`. Discovered 2026-11-02 while writing shell-tool process-tree regression tests: an empty pid file looked like a shell/timeout race at first, but it reproduced even solo with a 10 s timeout.

Related Windows test gotchas in the same area: `cmd /c timeout /t N` exits almost immediately under redirected stdin (use `ping -n N` as the long-lived child — see separate record), and `Out-File` writes UTF-16LE with a BOM while `Set-Content` (PS 5.1 "Default" ANSI) writes plain ASCII digits — prefer `Set-Content` when the test reads the file back with `read_to_string` and parses it (or strip non-digit chars defensively).
