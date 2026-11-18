+++
title = "worker checks not recorded when run with a trailing \"exit=$LASTEXITCODE\""
created = "2026-11-16"
evidence = "Release worker transcript ran every check with the suffix and got none recorded; after the agent.md change the next worker's four plain runs were recorded automatically."
+++

Symptom: a worker's passing cargo/npm check runs were not recorded as merge checks. Root cause: agent.md told workers to follow a command with `"exit=$LASTEXITCODE"`, so they ran `cargo test --workspace; "exit=$LASTEXITCODE"`; the `;` makes it a compound command, which the exact-match rule for `.coding/checks.toml` rightly refuses (in PowerShell the line then exits with the string's status, 0, even when cargo failed). Fix: agent.md now says to read the shell tool's own `[exit code: N]` and run exit-code-relevant commands alone on their line. Do not loosen the matcher to accept such suffixes.
