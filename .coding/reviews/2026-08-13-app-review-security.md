# Security (Threat-Model) Review — myharness desktop coding agent

**Reviewer:** read-only security subagent
**Scope:** full app (Rust lib `src/`, Tauri shell `src-tauri/`, React frontend `frontend/src/`), reviewed by READING at committed HEAD (working tree clean).
**Method:** focused on real, exploitable escalation paths over theory. A desktop agent app legitimately runs an LLM that executes user-authorized tools with approval gating, so I do NOT flag "the agent did what the user told it to" as a finding. I DO flag unintended escalation: sandbox escapes, secret leaks, approval-gate bypasses, and untrusted-content handling.

---

## VERDICT

**Overall: reasonably well-hardened for a single-user desktop agent; no Critical sandbox-escape or secret-exfiltration hole found.** The path sandbox, URL allow-list, git flag-injection guard, `never_auto_for` core-operation gate, and keys.toml DACL are all correctly implemented and tested. The findings below are defense-in-depth gaps and low/medium hardening issues, not active exploits. Two items rise to **High** (both are *unintended-escalation-adjacent* and cheap to fix): (1) the `describe_image` tool's data-URL exfiltration channel, and (2) trace/error logs persisting request bodies that embed API secrets in reachable form under a plaintext, world-readable `.coding/logs/`.

---

## CRITICAL

No findings.

---

## HIGH

### H1. `describe_image` builds a base64 data-URL from any sandbox image and POSTs it to a network endpoint — an un-gated file→network exfiltration channel
- **Ref:** `src/tool/agent/describe_image.rs:52-70` (`load_image_data_url` reads any sandbox file → base64 data URL), `src/tool/agent/describe_image.rs:78-88, 96-113` (`describe_image_data_url` → vision model), and the network call at `src/provider/vision.rs:139-146`.
- **Why it matters:** The tool is `SafetyLevel::AutoRun` (`describe_image.rs:178-181`) — it never prompts. It reads a file inside the sandbox and sends its full contents (base64) to the configured vision endpoint over the network. The extension gate (`mime_from_ext`, lines 38-47) only checks the *extension*, not the content — and a file named `.png` inside the project can contain anything. So any project file renamed/created with an image extension (the agent's own `file_write` can do this) becomes exfiltratable to the vision endpoint with **no approval**, even under `ApproveEachAction`. This is the one tool that couples "read arbitrary sandbox file" with "send it off-box" and no gate. (The main multimodal path has the same property, but at least it's the user's explicitly-configured chat endpoint receiving their own context; the *vision* endpoint can be a different, third-party endpoint per `config/general.rs:54,72`.)
- **Pragmatic note:** this is partly inherent to a vision tool (the whole point is to send an image). The *unintended* part is that a non-image file with a forged image extension is sent with zero approval and zero content sniffing.
- **Suggested fix:** gate `describe_image` behind `NeedsApproval` (or at minimum `is_project_scoped`-aware auto-approve under `AutoApproveProject`), and/or verify the magic bytes match the claimed MIME before exfiltrating.

### H2. Always-on `provider-errors.jsonl` and opt-in `traces.jsonl` persist full request/response bodies to a plaintext, non-permission-restricted `.coding/logs/`
- **Ref:** `src/provider/trace.rs:345-385` (`maybe_log_error` — always-on, no enable flag), `src/provider/trace.rs:448-487` (`write_record_to_file`), `src-tauri/src/main.rs:500-512` (paths under `.coding/logs/`).
- **Why it matters:** `request_json` (the full chat body — which routinely embeds file contents, tool outputs, and any secrets the user pasted or the agent read) is stored verbatim to `traces.jsonl` when the Trace "Log to file" checkbox is on, and `provider-errors.jsonl` captures every failed request **unconditionally** (`trace.rs:168-178` — "NO enable flag"). Unlike `keys.toml` (which gets a user-only DACL, `src/config/keys.rs:116-123,136-156`), these log files are written with plain `std::fs::write` / `OpenOptions::append` and get **no permission restriction**. On a shared/Windows machine, any local user can read another user's `.coding/logs/*.jsonl` and harvest whatever flowed through the LLM — including echoed API keys if an error body contained one. The docs even acknowledge "persists potentially sensitive request/response bodies to disk in plaintext" (`trace.rs:21-22`) but do not apply the keys.toml hardening to them.
- **Suggested fix:** apply the same `restrict_permissions` used for `keys.toml` to both `.coding/logs/*.jsonl` files at creation, and consider redacting `Authorization`-shaped strings / `api_key` fields from request bodies before persisting.

---

## MEDIUM

### M1. `is_protected_write_target` is case-sensitive and ASCII-only; on Windows a case-variant path bypasses it
- **Ref:** `src/tool/agent/sandbox.rs:170-180`.
- **Why it matters:** The guard compares the normalized relative path with `==` against `.coding/memory.db`, `.coding/safety.toml`, etc. (lines 174-179). Windows file paths are case-insensitive (NTFS default). A write to `.coding/SAFETY.TOML` or `.coding/Plans/stack.json` passes the string check (no match), and `validate()`/`canonicalize()` will resolve it to the *real* on-disk file, which **is** the protected target — so the protection is defeated on Windows by simple case change. The sandbox's `starts_with` root check uses canonicalized paths so *containment* is fine, but the *protected-target* deny-list is lexical and case-sensitive.
- **Suggested fix:** lowercase `rel_str` (e.g. `.to_ascii_lowercase()`) before comparison on Windows, or compare with a case-insensitive match against the protected set. (Low real-world impact — the model isn't adversarial here — but it's a cheap correctness fix to a security guard.)

### M2. TOCTOU window between sandbox `validate()` and the subsequent file write/read
- **Ref:** `src/tool/agent/sandbox.rs:78-109` (validate → canonicalize → return path), consumed later by `std::fs::write`/`read` in `file_write.rs`, `file_read.rs`, etc.
- **Why it matters:** Validation canonicalizes and checks `starts_with(root)`, returns the path, and *then* the tool performs I/O on that returned path in a separate step (and, for async tools, after an await/`spawn_blocking` boundary). A local attacker who can swap a directory for a symlink between the check and the use could redirect the write outside the root. This requires local write access to the project tree and a tight race, so it's low-likelihood for a single-user dev box — but it's the classic validate-then-use gap and worth documenting.
- **Suggested fix:** accept as low risk for a local desktop app; if raised, open the file with `O_NOFOLLOW`-style semantics or re-canonicalize immediately before the syscall within the same `spawn_blocking` closure. (Note the tools already do the validate+use inside one closure, which narrows but does not close the race.)

### M3. Browser `data:` scheme is allow-listed — `browser_eval` on a `data:` page runs JS with no page origin and the eval tool is only `NeedsApproval`, not `never_auto`
- **Ref:** `src/browser/mod.rs:85` (`ALLOWED_SCHEMES = ["http","https","data"]`), `src/browser/mod.rs:418-431` (`eval`), tool safety at `src/tool/browser/mod.rs:171` (EvalTool = NeedsApproval).
- **Why it matters:** Allowing `data:` URLs means `data:text/html,<script>…</script>` is a legal navigate target (used legitimately by the test suite, `mod.rs:763`). On a `data:` page, `browser_eval` executes arbitrary JS in a Chromium context. This is sandboxed by Chromium (no `file://` access, no privileged APIs), so it's contained — but combined with `eval` it is a code-execution surface that is *approval-gated* rather than hard-blocked. Under `AutoApproveProject` or `Autonomous`, `eval` could run without a prompt. The risk is bounded (Chromium sandbox + headless), but the scheme's necessity for production use is questionable.
- **Suggested fix:** consider gating `browser_eval` more strictly (it already requires approval; ensure it is never auto-approved by a broad safety rule), and document why `data:` must remain in the allow-list (or restrict it to test builds).

### M4. `get_api_keys` returns **all** API keys to the frontend over IPC with no per-key scoping
- **Ref:** `src-tauri/src/ipc/settings.rs:155-167`, consumed by `frontend/src/lib/tauri.ts:342` and held in React state `frontend/src/components/settings/sections/ProvidersSection.tsx:34,93`.
- **Why it matters:** Any IPC caller (the webview) can pull the full key map. Tauri's IPC is same-origin to the bundled frontend, so this is not a remote hole; but it means the secrets live in JS memory (`apiKeys` state) for the lifetime of the Providers section being active. The export path (`AdvancedSection.tsx:117-135`) intentionally includes keys only when `includeKeys` is checked (good), and `save_endpoints` correctly drops empty keys (`settings.rs:404-411`). The concern is the breadth of exposure: a single IPC returns every key. The ProvidersSection does clear `apiKeys` to `{}` on deactivate (`ProvidersSection.tsx:93`) — that part is correctly handled.
- **Suggested fix:** return keys only for the endpoint being edited, or return masked values and only accept writes; keep the current "cleared on deactivate" behavior (verified present).

---

## LOW

### L1. reqwest client follows redirects by default; a `base_url` redirect could carry the `Authorization: Bearer` header to a redirected host
- **Ref:** `src/provider/openai.rs:99-119` (client builder — no `.redirect()` policy set → reqwest default of up to 10 redirects), auth header at `openai.rs:394`.
- **Why it matters:** `base_url` is user-configured (intended), but if an endpoint responds with a 3xx, reqwest will follow it. reqwest does strip sensitive headers on cross-host redirects in current versions, but this is implicit and version-dependent. The user controls the endpoint, so this is low-risk / self-inflicted, but a defensive `.redirect(Policy::none())` or a same-host-only policy would make key-leak-on-redirect impossible by construction.
- **Suggested fix:** set an explicit redirect policy (none, or same-host) on the shared `reqwest::Client`.

### L2. Shell tool output is captured fully into `data` (uncapped) while only the *display* string is capped
- **Ref:** `src/tool/agent/shell.rs:186-197` — `cap_tool_output` caps the human-readable `output`, but `data: Some(json!({"exit_code","stdout","stderr"}))` carries the **uncapped** `stdout`/`stderr`.
- **Why it matters:** a command that emits a huge (or binary) stream is bounded in what's *shown*, but the full bytes flow into the structured `data` field and thus into the LLM context on the next request. This is a context-bloat / mild exfil-of-large-data concern rather than a privilege boundary, and the tool already requires approval (`shell.rs:121-123`) and confines `cwd` to the sandbox (`shell.rs:72-85`). Argument injection is N/A — the command is passed as a single string to `powershell -Command` / `sh -c` (the intended shell semantics), and `cwd` is sandbox-validated.
- **Suggested fix:** cap `stdout`/`stderr` before placing them in `data` too (or truncate to the same cap), so the structured field can't smuggle unbounded output into context.

### L3. `browse_markdown_file` is intentionally unsandboxed (native picker) — confirmed by design, noting for completeness
- **Ref:** `src-tauri/src/ipc/files.rs:250-281`.
- **Why it matters:** it reads any file the *user* picks via the OS dialog and returns path+content to the frontend. This is user-driven (the dialog requires a human choice), so it is **not** an agent-reachable read — the agent has no IPC handle to invoke the picker. No finding; listed to confirm the boundary was checked and is sound. Same for `read_file` / `save_conversation` / `load_conversation` / `list_files` / `list_markdown_files` — all route through `sandbox.validate` (`files.rs:26-30, 123-127, 340-342, 367-369`). **No path-traversal finding in the IPC file layer.**

### L4. `write_review_report` is `AutoRun` and writes under `.coding/reviews/` — by design; traversal is correctly rejected
- **Ref:** `src/tool/agent/write_review_report.rs:51-112, 151-155`.
- **Why it matters:** it is the reviewer's only output channel and is intentionally AutoRun. The bare-filename guard (rejects absolute paths, `/`, `\\`, `..`) plus canonicalize + `starts_with(reviews_dir)` is correct (`write_review_report.rs:55-111`). It writes only into the project's own bookkeeping dir. **No finding** — confirmed the guard blocks `../evil.md`, sub-paths, and absolute paths (tests at lines 220-275).

---

## AREAS EXPLICITLY CHECKED AND FOUND CLEAN

- **Path sandbox core** (`src/tool/agent/sandbox.rs`): relative/absolute handling, `..` traversal via canonicalize, symlink resolution, non-existent-parent rejection, and the lexical `validate_for_creation` gap-closure are all correct and well-tested. `normalize_path` (lines 185-203) correctly refuses to pop past root. UNC/drive-letter/ADS: absolute Windows paths are taken as-is and must `starts_with` the canonicalized root, so `C:\…` outside the root and `\\server\share` are rejected by the containment check. **Only the case-sensitivity gap (M1) and the inherent TOCTOU (M2) apply.**
- **Shell tool** (`src/tool/agent/shell.rs`): `cwd` sandbox-validated + must be a dir; `kill_on_drop(true)` + `tokio::time::timeout` bound execution; no shell-string argv splitting bug (single `-Command`/`-c` string is the intended contract); `CREATE_NO_WINDOW` set on Windows. Tool is `NeedsApproval`. Clean apart from L2.
- **Browser URL allow-list** (`src/browser/mod.rs:85, 670-748` + `src-tauri/src/ipc/browser.rs:96-103`): `normalize_url` is the single choke point for BOTH the agent `browser_navigate` tool and the `browser_open` IPC (both call `BrowserManager::navigate`, which calls `normalize_url` first — `mod.rs:228-231`). `file:`, `javascript:`, `about:`, drive paths, and bare `hello world` are all rejected (tests at 856-919). `--disable-extensions` is set (`mod.rs:181`). Scheme allow-list holds. (See M3 re: `data:`.)
- **git tool** (`src/tool/agent/git.rs`): subcommands dispatched to fixed argv (never a shell string); `valid_branch_name` rejects leading `-` and whitespace (flag-injection guard, lines 71-73); `merge`/`push` are `never_auto_for` (lines 61-63, 175-186) → always prompt, even in Autonomous / under a safety rule. Enforced at dispatch (`src/agent/dispatch.rs:193-210`).
- **Approval gate** (`src/agent/approval.rs` + `dispatch.rs:180-275`): `force_prompt` (never_auto / never_auto_for) is checked *before* the safety-rule shortcut, and the safety-rule shortcut is gated on `!force_prompt` (`dispatch.rs:201`) — so a stored rule can **never** auto-approve `git merge`/`git push`. `shell` is never `is_project_scoped` (`approval.rs:99-100`). AutoApproveProject only auto-approves file tools whose path validates in-sandbox + read-only git. No bypass found.
- **Safety-rules classifier** (`src/safety_rules/cmd_class.rs`): fail-safe — chaining (`&&`/`;`), unknown primaries, unknown subcommands, and assignments all classify to `None` → prompt. `SAFE_COMMANDS`/`GIT_SUBS`/etc. are allow-lists; `rm`/`Remove-Item` are absent, so `cargo test && rm -rf x` → `None`. `command_class` rules apply only to `shell` (`safety_rules.rs:262-266`). A broad `^shell:` literal rule is the only way to auto-approve all shell — and that's an explicit user "Allow for project" action, not accidental. Clean.
- **keys.toml** (`src/config/keys.rs`): `KeyStore` has no `Debug` on values (line 45-46, redacted at 289); write is atomic temp+rename then user-only DACL on Windows (`restrict_permissions_windows`, 166-225) / `0600` on Unix; permission failure is warn-not-block (116-122) — acceptable. Key fallback chain reads `OPENAI_API_KEY`/`ANTHROPIC_AUTH_TOKEN`. Clean.
- **Secret leakage in provider errors**: `fetch_models` deliberately omits the response body on 401/403 so a malicious server can't echo the Bearer key back into the UI error (`src/provider/openai.rs:209-217`). Traces record `base_url` "host only … no credentials" by contract (`trace.rs:79`). Key not in trace body. (But see H2 — request *bodies* are persisted.)
- **IPC validation**: `read_file`/`list_files`/`save_conversation`/`load_conversation`/`list_markdown_files` all sandbox-validate; `browser_open` funnels through `normalize_url`; `save_settings`/`save_endpoints` validate fill-rate range, theme enum, safety-mode enum, endpoint uniqueness, default-provider/model cross-refs (`settings.rs:1053-1135, 315-393`). `backlog_*` take only ids/text/images, no paths. Backlog store opens a fixed `.coding/backlog.json` (`main.rs:88-94`). No unvalidated path/URL arg found in the IPC surface.
- **Webview hardening** (`src-tauri/tauri.conf.json:23,27-28`): prod CSP is strict (`default-src 'self'; script-src 'self'; object-src 'none'; connect-src` limited to `ipc:`/`http://ipc.localhost`); the permissive `unsafe-eval`/localhost CSP is `devCsp` only (line 28). `additionalBrowserArgs: --disable-extensions`. Capabilities (`capabilities/default.json`) grant `shell:allow-open` (needed for opening links) but NOT `shell:allow-execute` — the shell plugin (`main.rs:37`) cannot spawn processes from the webview. `openExternal` usage: no direct `shell.open`/`open_url` call found in `src-tauri`. Clean.
- **Memory / SQLite** (`src/memory/mod.rs`): all queries use bound params (`rusqlite::params!`); the FTS5 MATCH query is built by quoting/escaping tokens (lines 435-437) and passed as a bound parameter (449, 453), so no SQL/FTS injection via search terms. The dynamic `IN (...)` clause (386, 636) interpolates only `?N` placeholders, never values. Clean.
- **Temp files**: browser profile uses `tempfile::TempDir` with a random suffix under the system temp dir (`src/browser/mod.rs:170-173`) and stale-profile sweep only removes `myharness-browser-*` older than 1h (201-220) — no symlink-follow on attacker-controlled names. No zip archive extraction anywhere (settings import is a single JSON file parsed in the frontend, `AdvancedSection.tsx:142-258`, with `validateImportBundle` + server-side re-validation). No zip-slip surface. Clean.
- **Prompt injection (browser snapshot → agent)**: `snapshot()` returns `page.content()` (`src/browser/mod.rs:408-413`) straight into the model's context as a tool result, and console output is forwarded verbatim. This is inherent to a browsing agent — web content is untrusted and the model consumes it. It is mitigated at the *action* layer, not the content layer: every mutating tool that content could induce is `NeedsApproval`/`never_auto`, so injected instructions can't act without a prompt (except AutoRun reads + the H1 image exfil + `write_review_report`). No separate content-sanitization exists; acceptable given the action-layer gating, and noted as the reason H1 matters.

---

## Summary of recommended actions (priority order)
1. **(H1)** Make `describe_image` approval-gated (or content-sniff + project-scoped auto-approve).
2. **(H2)** Apply keys.toml-style permission restriction to `.coding/logs/traces.jsonl` + `provider-errors.jsonl`; consider redacting secrets from persisted request bodies.
3. **(M1)** Case-insensitive compare in `is_protected_write_target` on Windows.
4. **(M3)** Ensure `browser_eval` can't be auto-approved by a broad safety rule; reconsider `data:` in the allow-list for production.
5. **(L1/L2)** Explicit reqwest redirect policy; cap shell `data.stdout/stderr`.
