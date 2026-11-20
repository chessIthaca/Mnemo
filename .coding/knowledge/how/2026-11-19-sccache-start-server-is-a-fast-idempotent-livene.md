+++
title = "sccache --start-server is a fast idempotent liveness probe; --show-stats is not"
created = "2026-11-19"
evidence = "Measured on this machine during plan d3336a8a step 7: cold/healthy start-server 0.06 s exit 0 (twice in a row), wedged server 15.3-17.4 s exit 2, show-stats 10.9-11 s exit 0 with 33 stuck sccache wrapper processes under one cargo; after the orphan server was gone, start-server was 0.06 s again."
+++

`sccache --start-server` doubles as a liveness probe: with no server running it starts one and exits 0 in ~0.06 s; with a HEALTHY server it is idempotent and exits 0 in ~0.06 s; against a WEDGED server it prints "Starting the server..." then "Timed out waiting for server startup" and exits 2 after ~15 s (observed on Windows, sccache on PATH at %USERPROFILE%\.cargo\bin\sccache.exe). `--show-stats` is the weaker probe: it exits 0 even against a half-dead server (took ~11 s then), while wrapper invocations (`sccache.exe <rustc> …`) hung — so gating RUSTC_WRAPPER on `--start-server` exit 0, with `--show-stats` only as a fallback, avoids handing out a server that hangs builds. A wedged server is what a server auto-started inside a killed command tree becomes; kill the orphan `sccache.exe` (no args, dead parent) to repair, then `--start-server` is fast again.
