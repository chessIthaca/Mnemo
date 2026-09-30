# Security Policy

## Supported versions

The latest [release](https://github.com/chessIthaca/Mnemo/releases) and `main`
receive security fixes. Older tags are frozen — release tags are immutable, so a
fix ships as a new release rather than a rewrite of an existing one.

## Reporting a vulnerability

Use **private vulnerability reporting**: the repository's
[Report a vulnerability](https://github.com/chessIthaca/Mnemo/security/advisories/new)
form (Security → Advisories). Please do not open a public issue for a suspected
vulnerability.

A useful report names the affected version or commit, the impact, and the
smallest reproduction you have (the prompt/tool call that triggers it, a crafted
file, or the endpoint involved). If you are unsure whether something is
exploitable, report it anyway — triage is ours.

This is a small project: reports are handled on a best-effort basis, acknowledged
as soon as someone is available, and credited in the release notes unless you
prefer otherwise. There is no bug bounty.

## Scope

In scope — the Mnemo library/CLI and the Tauri app:

- the tool sandbox and its path checks, the approval gate, the safety modes, the
  per-tool allow-lists and the protected-path rules;
- prompt-injection paths that make the agent call a tool the user did not
  authorize — the approval prompt is a security boundary;
- secrets handling: `keys.toml` permissions, credential redaction in logs, and
  the provider/endpoint configuration surface;
- the local IPC surface and the WebView CSP in `src-tauri/tauri.conf.json`.

Out of scope:

- **vendored dependencies** (`vendor/wry`) — this is a backport
  mirror; report upstream to wry;
- a malicious tool call the user explicitly approved, or content a provider
  returned in response to the user's own request;
- anything about data Mnemo never writes (your repository, your provider account).

## Automated checks

The repository runs CodeQL (advanced setup,
`.github/workflows/codeql.yml`), Dependabot alerts and security updates, and
secret scanning with push protection. Findings, the fixes, and the
dismissal evidence live in `.coding/analysis/` — most recently
`2026-09-29-github-security-defects.md`.
