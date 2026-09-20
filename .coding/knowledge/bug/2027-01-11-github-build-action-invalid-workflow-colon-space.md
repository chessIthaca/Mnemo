+++
title = "GitHub build action invalid workflow — colon-space in unquoted YAML scalar (build.yml L52)"
created = "2027-01-11"
+++

Symptom (user report 2027-01-15): GitHub Actions build fails before any step runs — "Invalid workflow file: .github/workflows/build.yml#L52 — You have an error in your yaml syntax on line 52". Local builds were green (the workflow file is never parsed locally).

Root cause: the windows job's step name `Rust tests (workspace: mnemo + mnemo-app)` contained `: ` (colon-space) inside a plain unquoted YAML scalar — YAML parses colon-space as a mapping indicator, so the parser fails with "mapping values are not allowed here" (line 52, column 36; reproduced locally with PyYAML `yaml.safe_load`). File was otherwise valid UTF-8, no tabs, byte-identical to origin.

Fix (commit 56132d3 on main): comma instead of colon — `Rust tests (workspace, mnemo + mnemo-app)` — matching the macos job's own line-95 convention `(workspace, macOS compile validation)`.

Verification / regression guard: `python -c "import yaml; yaml.safe_load(open('.github/workflows/build.yml', encoding='utf-8'))"` → PARSE OK (no cargo test applies — CI YAML isn't compiled by the test suite); GitHub run green after push.

Convention: never put `: ` inside an unquoted YAML scalar (step names, display names) — use a comma/dash or quote the whole scalar.
