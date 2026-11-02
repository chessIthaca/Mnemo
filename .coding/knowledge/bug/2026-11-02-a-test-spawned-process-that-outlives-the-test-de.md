+++
title = "A test-spawned process that outlives the test delays the whole test binary's teardown"
created = "2026-11-02"
+++

When a test spawns a real OS process that OUTLIVES the test (a leaked grandchild holding inherited pipe handles / the tempdir as its CWD), the libtest binary can linger in teardown until that process dies: observed twice on 2026-11-02 (Windows, solo runs, no parallel-test noise) — a test panicking at ~20-30 s while a `ping -n 60` grandchild survived made the test binary report "finished in ~59.9 s", matching the ping's full 60 s lifetime almost exactly. The test itself had already failed at the panic; the extra ~30-40 s were pure post-panic teardown wait. Exact blocking mechanism not root-caused (candidates: runtime-drop/IO-driver pipe wait, cargo's capture-pipe EOF held via the child, TempDir removal against a CWD-held directory — all released only when the leaked process exits).

Practical consequence for tests that spawn processes: a leaked 60 s process turns a fast failing test into a ~60 s test-binary run. Kill the tree before the test returns (the shell tool's TreeKillGuard / Job Object does this once wired through the real tool; when spawning directly in tests, `Start-Process -PassThru` gives you the pid to kill) and keep grandchildren short-lived (`ping -n 60`, `sleep 60`) so even a bug cannot linger longer than that. If a solo test inexplicably takes ~N seconds where N = the spawned process's lifetime, look for a leaked descendant before blaming timers.
