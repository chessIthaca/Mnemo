+++
title = "live-pid test fixtures need a child that survives redirected stdin (not `timeout /t`)"
created = "2026-11-02"
+++

HOW (repo convention, src-tauri tests): a test fixture that must present a "live pid" has to be a child that genuinely stays alive with stdin redirected — `cmd /c timeout /t N` exits in ~163 ms once stdin is redirected (it refuses input redirection), and `sh -c "sleep N"` has the same class of hazard under a harness. Use loopback `ping -n 30 127.0.0.1` on Windows (stdin/stdout/stderr set to Stdio::null(), ~30 s lifetime) and `sleep 30` elsewhere, and wrap the Child in a drop guard that kills and waits so a failing assertion cannot leave the process behind.

Why it matters: src-tauri/src/instance_marker.rs `pid_alive` checks GetExitCodeProcess == STILL_ACTIVE, so an exited child whose handle is still held reads as dead and any assertion expecting a conflict from that pid fails — intermittently, only when suite load delays the asserts past the child's short life. Symptom shape to recognise: test passes in isolation, fails under the full lib suite with `left: None, right: Some(<pid>)` in mnemo2::project.

Reproduce deterministically without relying on load: keep the old helper and inject a temporary ~800 ms sleep between spawn and the first assert (red), then swap in the ping helper under the same sleep (green). Remove the sleep before committing.
