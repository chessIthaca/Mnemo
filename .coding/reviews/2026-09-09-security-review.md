## Verdict: FINDINGS (1 high, 3 low)

Whole-app security review of Mnemo at HEAD c18b5d0 (wt/agenticcoding, clean tree). All eleven scope axes reviewed against the actual tree. One fresh HIGH (agent file tools can write the `.git/` control plane → sandbox escape via git hook / `core.fsmonitor` execution), three LOW tightening items. Prior findings from 2026-12-23 verified: the run-all commit-to-main MEDIUM is fixed and holding (e20b1df branch guard), and the web_fetch redirect SSRF LOW is fixed (manual redirect loop re-runs the gate on every hop — verified in code).

---

## Findings (ranked by exploitability × impact)

### HIGH 1 — Agent file tools can write the `.git/` control plane: sandbox escape via git hook / fsmonitor execution

- **Severity:** HIGH
- **Location:** `src/tool/agent/sandbox.rs:149-190` (`is_protected_write_target` — `.git/` absent from the protected list); `src/agent/approval.rs:124-139` (`is_project_scoped` auto-approves any sandbox-valid path); `src/agent/approval.rs:160-182` (`git status` auto-approved as read-only); `src/tool/agent/git.rs:655-720` (commit path — no `--no-verify` anywhere in the repo, so hooks run).
- **Symptom (BUG):** A prompt-injected agent (via malicious repo content read through `read_files`/`search`, `web_fetch` content, or a hostile MCP server response) can plant executable git configuration inside the project and have it executed with full user privileges, completely bypassing the shell approval gate, the path sandbox, and the protected-dir architecture. Concretely:
  1. `file_write` to `.git/hooks/pre-commit` (or `post-merge`, `pre-push`) with arbitrary script content — `.git/` is inside the project root, so `Sandbox::validate()` accepts it, and `is_protected_write_target` does not list it.
  2. `git commit -m "..."` — approval-gated, but the prompt shows a completely routine-looking commit command. The user has no indication that a hook was planted (under `AutoApproveProject` the file write was auto-approved, so no write was ever shown).
  3. Git executes the hook → arbitrary code execution as the user, outside the sandbox. The payload can exfiltrate `~/.mnemo/keys.toml` (all API keys), rewrite `.coding/safety.toml` to auto-approve everything (permanently disabling every future gate — the hook runs unsandboxed, so the protected-dir refusal that binds the file tools does not bind it), and persist.

  A second, even quieter chain needs **zero approvals** under `AutoApproveProject` (and `Autonomous`): `file_write` to `.git/config` adding `core.fsmonitor = <command>` (auto-approved — path is sandbox-valid), then `git status` (auto-approved as read-only, `is_git_read_only` → `"status"` → `true`). Git spawns the fsmonitor command on the index refresh → arbitrary code execution. `core.sshCommand` similarly executes on `fetch`/`push`.
- **Root cause:** `is_protected_write_target` protects `.coding/reviews/`, `.coding/plans/`, the memory/codegraph DBs, `safety.toml`, `backlog.jsonl`, and the plan stack — but not the `.git/` directory, which is the git control plane (hooks, config, index). The sandbox treats "inside the project root" as safe-to-write, and under `AutoApproveProject` `is_project_scoped` returns true for any sandbox-valid path, so `.git/` writes are auto-approved. The git tool never passes `--no-verify` (verified: zero matches repo-wide), so planted hooks execute on the next approval-gated (or auto-approved read) git operation. The only "git" mention in sandbox.rs is a migration comment — the omission is an oversight, not a documented design decision.
- **Exploitability × impact:** The security model explicitly assumes prompt injection is possible (that is why the sandbox and approval gates exist). This path defeats the entire containment stack: impact is full user-privilege code execution (secret exfiltration + permanent safety-rule tampering); exploitability is one successful prompt injection plus zero approvals (`AutoApproveProject`/`Autonomous`, fsmonitor chain) or one routine-looking commit approval (`ApproveEachAction`, hook chain).
- **Suggested fix:** Add `.git/` to `is_protected_write_target` in `src/tool/agent/sandbox.rs` — refuse writes whose project-relative path starts with `.git/` (case-insensitive, matching the existing protected-target style; also cover the `.git` plain file used by linked worktrees, which would let a run-all item rewrite its parent repo's gitdir pointer). This automatically closes `file_write`, `file_edit`, and `convert_line_endings` (all three route through `refuse_if_protected`), and the IPC `write_file` command which mirrors it. Add regression tests: `file_write` to `.git/hooks/pre-commit` and `.git/config` refused; `read_files` on `.git/config` still allowed (reads are harmless and useful). Optional defense-in-depth: pass `--no-verify` on commit/merge in the git tool (note the tradeoff — users may legitimately want their own hooks; the file-tool gate is the real fix). The shell residual (LOW 2) still reaches `.git/`, same as it still reaches `.coding/` — that is the documented standing residual class.

### LOW 1 — web_fetch DNS rebinding TOCTOU (documented limitation; concrete tightening available)

- **Severity:** LOW
- **Location:** `src/tool/agent/web_fetch.rs` (`ensure_public_http_url` + the manual redirect loop at ~line 287).
- **Symptom:** The SSRF gate resolves the hostname and validates the IP (loopback/link-local/private/multicast/IPv4-mapped-IPv6 all blocked — verified holding, including on every redirect hop), but `reqwest` then re-resolves the hostname at connect time. A rebinding DNS server can answer the gate's lookup with a public IP and the connection's lookup with `127.0.0.1`/`169.254.169.254`, reaching local/metadata services. The module docs already record this as a known limitation.
- **Root cause:** Resolve-then-connect gap — validation and connection use two independent DNS lookups.
- **Suggested fix:** Pin the validated IP: resolve manually, connect to the IP with a `Host`/`:authority` header override (and TLS SNI set to the original hostname), or install a `reqwest` custom resolver that re-runs the IP gate per connection. Until then, the documented limitation stands; risk is bounded by the attacker needing to control DNS for a URL the agent fetches.

### LOW 2 — Shell residual: an approved shell command can write protected dirs and `.git/` (known, documented)

- **Severity:** LOW (documented standing residual)
- **Location:** `src/tool/agent/sandbox.rs:180-182` (module docs record the residual for `.coding/plans/` and `.coding/reviews/`).
- **Symptom:** The sandbox and protected-dir refusal bind only the file tools. A shell command the user approves can write anywhere the user can write — including `.coding/reviews/` (forging a review report), `.coding/plans/`, `safety.toml`, and `.git/hooks/`. The approval prompt does show the full command line, so a vigilant user can catch it; the residual is inherent to shell approval and is already documented.
- **Root cause:** By design — shell execution cannot be statically confined without a container/seatbelt, which the app does not bundle.
- **Suggested fix (tightening):** None required beyond the existing documentation; optionally surface a warning badge in the approval UI when the command text mentions `.coding/`, `safety.toml`, or `.git/` (cheap heuristic, catches the obvious cases). Note this residual also covers the `.git/` path of HIGH 1 — fixing HIGH 1 closes the file-tool chain, not the shell chain.

### LOW 3 — API keys are plaintext at rest (DACL-restricted; OS keychain would tighten)

- **Severity:** LOW
- **Location:** `src/config/keys.rs` (module docs + `restrict_permissions`).
- **Symptom:** `~/.mnemo/keys.toml` stores all endpoint API keys in plaintext. Mitigations verified: on Windows the file is created with a user-only DACL (no inherited group/Users ACE), on Unix mode 0600; `KeyStore` deliberately does not derive `Debug` so keys cannot leak into logs; trace files and `provider-errors.jsonl` get the same restriction. Any process already running as the user (including a payload from HIGH 1) can read it.
- **Root cause:** Deliberate tradeoff — no OS-keyring dependency, documented in the module.
- **Suggested fix:** Optional tightening: store keys via Windows Credential Manager / macOS Keychain (`keyring` crate), keeping `keys.toml` as a migration source only. Not urgent on its own; it becomes worthwhile once HIGH 1 is fixed (which removes the easiest in-app path to the file).

---

## Verified solid at HEAD c18b5d0 (no findings)

- **Tauri config:** CSP is strict (`default-src 'self'; script-src 'self'; object-src 'none'; base-uri 'self'`; connect-src limited to the IPC bridge — the frontend cannot make arbitrary network requests; `style-src 'unsafe-inline'` is required by React inline styles). `devCsp` is dev-only. Capabilities (`capabilities/default.json`) grant only `core:default`, `shell:allow-open`, and window position/size to the `main` window. No asset-protocol scope, no `withGlobalTauri`, no remote-domain IPC access.
- **IPC surface (~110 commands across `src-tauri/src/ipc/*`):** file reads/writes are sandbox-validated; `write_file` mirrors the agent tools' protected-target refusal; image reads are magic-byte-checked and size-capped; browser/webview commands normalize URLs before use; run-all commands operate on app-managed worktrees. The IPC bridge is reachable only from the app's own webview (CSP + capabilities), not from remote content.
- **Browser child webview:** created as label `browser-child` (`src-tauri/src/ipc/browser_webview.rs:105`) with no capability grant — pages loaded in the Browser tab **cannot invoke Tauri commands**. `normalize_url` enforces a scheme allow-list (javascript:/data: rejected); `is_app_url` guards the app's own origin from CDP targeting; agent CDP inspection is opt-in via Settings (dev-only port otherwise); the whole surface is Windows-gated by design (the sanctioned exception). The old `game_*` tools are consolidated into the browser module behind the same opt-in.
- **Path sandbox:** canonicalization-based containment (symlinks resolved, absolute paths checked, parent-canonicalization for not-yet-existing files so `root/newdir/../..` cannot escape), NTFS ADS guard, case-insensitive protected-target matching. Glob patterns in `search` are validated against escape.
- **Approval architecture:** five safety modes; `AutoApproveProject` auto-approves only sandbox-valid file tools, `search`, and read-only git; shell is never auto-approved; `merge`/`push` are `never_auto_for` even in Autonomous; unknown/unclassifiable commands fail closed to approval (cmd_class is fail-safe).
- **Git tool:** argv-based (no shell string), commit/merge messages passed as single argv elements (no argument injection), branch names validated (no leading `-`, no whitespace), unsafe read-query flags rejected (`--output`/`-o`/`--exec`/`--no-index`/`--ext-diff`/`--textconv`), all mutating ops approval-gated. Run-all checkpoints commit on the item's own worktree branch — the e20b1df main-branch guard holds; landings are serialized `--no-ff` merges.
- **Secrets in logs/traces:** `redact_json` (api_key/authorization/token keys) + `redact_text` (Bearer values, JSON key patterns) applied to every trace record and to `provider-errors.jsonl` before disk; log files DACL/0600-restricted; extensive tests pin the masking. The LlmTraceView renders bodies through `JsonView` (pure React) — no HTML rendering of raw JSON.
- **Frontend XSS:** exactly one `dangerouslySetInnerHTML` mention in the codebase is a comment saying JsonView does *not* use it. Markdown is react-markdown + remark-gfm + rehype-highlight **without rehype-raw** — raw HTML in assistant output is escaped, not rendered. Links route through `MarkdownLink` (sanctioned openers only).
- **MCP:** config lives in `~/.mnemo` (outside the agent sandbox — the agent's file tools cannot rewrite it to spawn commands); secrets are env-var *names* resolved at connect time, never stored in `mcp.toml`; tool results join text content only; server→client requests are declined; per-server `trusted` auto-approval is explicit user opt-in.
- **backlog.jsonl union merge:** tampered/garbage lines are skipped without data loss, soft-deletes survive, same-id collapse is deterministic — injection via crafted backlog lines cannot smuggle state (verified by tests at `src/backlog.rs`).

## Prior-findings verification (2026-12-23 security review)

- **MEDIUM run-all checkpoint can commit to main** — FIXED and holding: checkpoints commit on the item's own `wt/runall-*` worktree branch; landing is a serialized `--no-ff` merge (e20b1df).
- **LOW web_fetch redirect hops bypass SSRF blocklist** — FIXED and holding: redirects are followed manually (max 5 hops) with the full gate re-run on every URL before requesting it, scheme re-checked per hop.
- **LOW `#[allow(dead_code)]` constitution violation** — no `#[allow]` remains in the run-all path (spot-checked; the constitution's warning-free build is enforced by `#![deny(warnings)]`).

## Bug list (for the executive summary)

1. **HIGH — `.git/` control plane writable by agent file tools** (`sandbox.rs:149-190`): prompt-injected agent plants `.git/hooks/pre-commit` or `.git/config` `core.fsmonitor` via auto-approved `file_write`; the next routine/auto-approved git op executes it → unsandboxed arbitrary code execution, API-key exfiltration, and permanent safety-rule tampering. Fix: add `.git/` to `is_protected_write_target` + regression tests.
