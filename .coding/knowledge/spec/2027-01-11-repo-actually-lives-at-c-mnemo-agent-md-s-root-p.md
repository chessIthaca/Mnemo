+++
title = "repo actually lives at C:\\Mnemo — agent.md's root path is stale"
created = "2027-01-11"
+++

The working checkout for this project is C:\Mnemo (vitest reports "C:/Mnemo/frontend"; the ipc contract-fixture path resolves as C:\Mnemo\src-tauri\..\frontend\src\lib\ipc-fixtures\...), NOT the C:\AgenticCoder\AgenticCoder path declared in this project's agent.md ("The project root is C:\AgenticCoder\AgenticCoder" and its `C:\AgenticCoder\AgenticCoder\src\...` example paths). Observed 2026-09-26 during plan 6d8cbe94 (recall_delta): the Rust workspace root, src/, src-tauri/ and frontend/ all live under C:\Mnemo, and `cargo test` / `cargo test -p mnemo-app` / `npx vitest run` (cwd frontend) all run correctly from there. agent.md's Environment section is stale on this point and should be corrected when someone next edits the constitution — nothing else about the environment rules (Windows/PowerShell, never commit to main, the merge_to_main ruleset gate) was contradicted.
