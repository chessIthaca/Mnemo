+++
title = "read Mnemo's GitHub security (code-scanning) pages — gh api, not web_fetch"
created = "2027-01-11"
+++

Unauthenticated `web_fetch` on https://github.com/chessIthaca/Mnemo/security/code-scanning returns HTTP 404 (GitHub security/code-scanning pages require auth; the 404 is not "no alerts"). The authenticated path works: `gh` is logged in as chessIthaca with scopes gist/read:org/repo/workflow (verified 2026-09-28).

Working queries (Windows PowerShell, `;` chaining):
- List open alerts: `gh api "repos/chessIthaca/Mnemo/code-scanning/alerts?state=open&per_page=100"`
- Group by rule: add `--jq '[group_by(.rule.id)[] | {rule: .[0].rule.id, sev: .[0].rule.security_severity_level, n: length}]'`
- One alert's details/instances: `gh api repos/chessIthaca/Mnemo/code-scanning/alerts/<n>/instances`

Snapshot 2026-09-28: 13 open / 2 fixed. Open: rust/cleartext-logging x5 (high; src/workflow/mod.rs:1527,1530 logging uuid_hex), rust/insecure-cookie x3 (high; vendor/wry, classified "library"), js/redos x2 (high), js/insecure-randomness x1 (high), actions/missing-workflow-permissions x2 (medium; .github/workflows/build.yml).

## The rest of the GHAS surface (all `gh api`, same auth)

Code scanning is only one of four alert feeds — query all of them when asked for
"all the security defects":

- Dependabot: `gh api "repos/chessIthaca/Mnemo/dependabot/alerts?per_page=100" --paginate --jq ".[]"`
- Secret scanning: `gh api "repos/chessIthaca/Mnemo/secret-scanning/alerts?per_page=100" --paginate`
- Maintainer advisories: `gh api repos/chessIthaca/Mnemo/security-advisories`
- Posture/knobs: `code-scanning/default-setup`, `vulnerability-alerts` (204 = on),
  `automated-security-fixes`, `private-vulnerability-reporting`,
  `actions/permissions/workflow`, `rulesets`.

Verified 2026-09-29 (HEAD a2522a4): code scanning 13 open / 2 fixed / 0 dismissed;
Dependabot 1 open (`rust:glib` GHSA-wrw7-89jp-8q8g, medium) / 2 fixed; secret scanning
0; advisories 0. CodeQL is **default setup** (`dynamic/github-code-scanning/codeql:analyze`,
languages actions+js+ts+python+rust, weekly). Full write-up:
`.coding/analysis/2026-09-29-github-security-defects.md`.

Two traps worth knowing:

- `most_recent_instance.location` is relative to the analysis commit, and
  `gh api .../alerts/<n>/instances` gives that `commit_sha` — check it against HEAD
  before quoting a line number as current. (Here it IS a2522a4 = HEAD.)
- PowerShell 5.1 eats double quotes inside a single-quoted `--jq` program, so
  `select(.state == "open")` reaches jq as `select(.state == open)` and jq dies with
  `function not defined: open/0`. Either use a `.ps1` script file (as
  `.coding/tmp/gh-security-report.ps1` does) or pipe `--jq ".[]"` JSONL into
  `ConvertFrom-Json`.
