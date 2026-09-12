# Deep Review — Security (whole codebase @ HEAD)

**Reviewer:** security deep reviewer (read-only). **Date:** 2026-04-18.
**Threat model applied:** single-user Windows desktop app; the LLM is NOT trusted; the human user IS. Only unintended escalation is flagged (sandbox escapes, secret leaks, approval-gate bypasses, injection, untrusted-content handling, unsafe Rust, races/locks). "Agent did what the user asked" and theoretical noise are not reported.

## Verdict

**Ship-blocking findings: 2 High.** The sandbox has a genuine read escape (`search`/`search_read` glob argument is never validated — `AutoRun`, no approval), and the `command_class` auto-approve classifier can be bypassed with PowerShell subexpression/interpolation syntax, silently approving commands the user never saw. Both are exactly the escalation classes this architecture exists to prevent. Everything else scoped in the prompt was verified clean or re-confirmed at its known severity; notably, one known-open item (shell `data` field) turns out to be **less** severe than recorded — it never reaches LLM context.

---

## High

### H1. Sandbox escape: `search` / `search_read` glob argument is unvalidated → arbitrary file read outside the project root, no approval

**Where:** `src/tool/agent/search.rs:150-176` (glob build + `read_to_string`), `src/tool/agent/search_read.rs:118-144` (same construction), `src/tool/agent/search.rs:42-51` (`should_search` fails open), `src/safety_rules.rs` n/a — both tools are `SafetyLevel::AutoRun` (`search.rs:120-122`, `search_read.rs:87-89`).

**The bug:** both tools build the walk root by string concatenation with an LLM-controlled argument and never route it through `Sandbox::validate`:

```rust
let glob_pattern = args.glob.clone().unwrap_or_else(|| "**/*".to_string());
let full_glob = format!("{}/{}", root.to_string_lossy(), glob_pattern);
if let Ok(paths) = glob::glob(&full_glob) {
    for entry in paths.flatten() {
        ...
        let content = match std::fs::read_to_string(&entry) { ... }   // ← no containment check
```

The `glob` crate's pattern language follows literal `..` components (it resolves them against the accumulated scope via `fs::metadata`/`read_dir`), so `glob = "../**/*.toml"` yields and reads files in the project's parent directory and beyond. `should_search` cannot save it: it does `path.strip_prefix(root).unwrap_or(path)` — for an escaped path `strip_prefix` *fails* and the filter silently degrades to inspecting the absolute path's components (only ignored-dir names + a 1 MB size cap), never checking root containment.

**Exploit story:** a prompt-injected or malicious model issues, with **no approval prompt** (AutoRun):

```json
{"pattern": "api_key", "glob": "../../AppData/Roaching/**/*.toml"}
```
(or any `../`-chain reaching the user's profile). `search` reads every matched file and returns up to 100 matching **lines** — e.g. the `api_key = "sk-…"` line of the global `keys.toml` (which lives in the global config dir, outside the sandbox by design), SSH config snippets, `.env` files in sibling projects, anything reachable by `..` + `**` recursion. The entire `Sandbox::validate` machinery that every other file tool routes through is bypassed by a single unvalidated argument. `search_read` shares the construction: its search stage reads out-of-root files the same way (it leaks which files matched + counts, and its auto-read stage then fails `read_one`'s sandbox validation — so it discloses paths/counts rather than contents; the contents leak is via plain `search`).

**Fix (concrete):**
1. Validate the `glob` argument before use: reject absolute patterns, Windows drive prefixes, and any `..` component (`glob.split(['/', '\\']).any(|c| c == "..")`).
2. Defense in depth at the walk: skip any `entry` that does not satisfy `entry.starts_with(&root)` *before* `is_file()`/`read_to_string` (the root is canonicalized, so this lexical check is sound for glob-produced paths).
3. Fix `should_search` to fail closed: `match path.strip_prefix(root) { Ok(rel) => …, Err(_) => return false }`.
Add a regression test: create `outside.txt` in the tempdir's parent, call `search` with `glob: "../**/*.txt"`, assert no match and no read.

---

### H2. Approval-gate bypass: `command_class` auto-approve classifier executes PowerShell subexpressions / interpolated strings as "benign", and hides subcommands behind leading flags

**Where:** `src/safety_rules/cmd_class.rs:241-252` (`is_benign_expression`), `:101-126` (`classify`), `:292-331` (`classify_primary` / `with_subcommand`); consumed at `src/agent/dispatch.rs:203-215` (auto-approve executes with **no prompt**).

**The bug.** The classifier's fail-safe contract (module doc, `cmd_class.rs:10-34`: "anything it cannot confidently classify returns None … chaining is closed") is broken by three PowerShell execution constructs it treats as inert, plus one class-collapsing bug:

1. **Subexpression as a standalone statement.** `is_benign_expression` classifies any statement starting with `$` and lacking a top-level `=` as a "bare variable reference" (`cmd_class.rs:248-249`). But `$(command)` in PowerShell is a *subexpression that executes*:
   `cargo test; $(Remove-Item -Recurse -Force $HOME)` → statement 2 is skipped as "benign" → class `"cargo test"` → a saved class rule for `cargo test` **auto-approves** the whole command with no prompt.
2. **Interpolated double-quoted string.** A statement starting with `"` is unconditionally benign (`cmd_class.rs:246-247`). PowerShell interpolates `$(…)` inside double quotes, so `cargo test; "exit $(Remove-Item …)"` also classifies as `"cargo test"`. (Single-quoted strings are genuinely inert; double-quoted are not.)
3. **Subexpression inside arguments.** `with_subcommand` inspects only `tokens[1]`; everything after a recognized subcommand is dropped (`cmd_class.rs:319-331`). `cargo test $(evil)` → class `"cargo test"`; `$(evil)` runs during argument binding.
4. **Leading flags collapse the class.** `git -C ..\other-repo checkout -- .` → `tokens[1]` is `-C` (a flag) → class is just `"git"` — the subcommand never enters the class. If the user ever saved a class rule named `git` (they do, the first time they mark any leading-flag git command safe, e.g. `git -c core.autocrlf=false status` → class `"git"`), subsequent `git -C <anywhere> checkout` calls auto-approve. (`merge`/`push` remain safe: `dispatch.rs:195-208` checks `never_auto_for` *before* `is_safe`, so force-prompt still wins — verified.)

**Exploit story:** user clicks "Mark Safe (same operation)" once on `cargo test 2>&1 | Select-String x` (the flagship example in `safety.toml`'s own header). From then on, any prompt-injected tool call of the form `cargo test; "…" $(curl … | iex)` or `cargo test $(evil)` is classified `"cargo test"`, matches the rule, and **executes without any human seeing the command** — a silent approval bypass that the chaining split was specifically built to prevent.

**Fix (concrete), all in `cmd_class.rs`, conservative direction only:**
- Return `None` from `classify` if the command contains `$(` anywhere at top level of any statement.
- `is_benign_expression`: allow only *single-quoted* string literals and bare `$name` variables (reject `$(` and any statement starting with `"`).
- `with_subcommand`/`npm_class`/`npx_class`: if `tokens[1]` is a flag (starts with `-`), return `None` instead of the bare command name (the flags' meaning, e.g. `git -C`, is unknown to the classifier).
- Reject any token containing `(`, `)`, or a backtick in a classified statement.
Add table-driven tests for all four vectors (assert `classify(...) == None` / rule does not match).

---

## Medium

### M1. `cap_tool_output` can panic on untrusted output (non-char-boundary `String::truncate`) — crashes the agent loop

**Where:** `src/tool/agent/mod.rs:40-46`; callers `src/tool/agent/shell.rs:188` and `src/tool/agent/git.rs:116-118`.

`String::truncate` panics unless the new length lies on a UTF-8 char boundary. `cap_tool_output` truncates at exactly 100 KiB with no boundary check, while its input is `String::from_utf8_lossy(&output.stdout/stderr)` — perfectly valid UTF-8 that routinely contains multi-byte characters. The codebase already knows this hazard: `read_files.rs:35-48` exists precisely because "a naive `String::truncate` panics". Any command (or git output) whose combined stdout+stderr has a multi-byte character straddling byte 102400 panics inside the tool's `execute`, which unwinds the agent task — killing the turn/agent. This is trivially triggerable by untrusted content (e.g. a dependency's build script emitting non-ASCII output).

**Fix:** reuse `truncate_to_boundary` (make it `pub(crate)` at the `mod.rs` level and call it from `cap_tool_output`), i.e. back up to the nearest char boundary before truncating. Add a test with a 200 KB string of `"é"`s through `cap_tool_output`.

### M2. 401/403 response-body suppression exists only on `/models` discovery — the main chat path and the vision path still embed raw error bodies (regression-check finding)

**Where:** `src/provider/openai.rs:487-496` (main `chat/completions`: `format!("stream request failed: {status} — {text}")` with no status check), `src/provider/vision.rs:154-159` (`describe_image`: same). The suppression exists only at `openai.rs:205-222` (`fetch_models_with_vision`).

The original fix's own comment ("a misconfigured or malicious server can echo the `Authorization: Bearer {key}` header back in its response body") applies identically to these two paths: a hostile endpoint returns 401 with the bearer token echoed in the body, and the raw body lands in the `Error::Provider` string surfaced through `get_startup_error` / turn errors / UI, and (unredacted) anywhere those strings are persisted. `trace.rs` redaction covers only the log files (`redact_text` at `:806-817`), not the in-flight error string. The user-configured gateway is inside this threat model (the `/models` fix was accepted on exactly that basis).

**Fix:** extract the `fetch_models_with_vision` 401/403 branch into a shared helper (`suppress_auth_body(status, url, text)`) and apply it in the main streaming error path and in `VisionClient::describe_image`. Add tests asserting the returned error string contains no body text for 401/403 on both paths.

---

## Low

### L1. Subagent allowlist fails open when the parent loop is missing

**Where:** `src-tauri/src/ipc/spawn.rs:143-148` (`if let Some(list) = allowlist { … }`) and `:264-274` (`let parent = loops.get(&parent_id)?;` → `compute_subagent_allowlist` returns `None`).

A parent id that is no longer in `agent_loops` (parent exited/cleaned up in the race window between id allocation and allowlist computation, or a stale `parent_id`) yields `None`, which the caller treats as "apply **no** restriction" — an unrestricted child where the permission model promises "subagent ⊆ parent". Mitigating factors (verified): the child still gets `set_plan_mutations_allowed(false)` (`spawn.rs:121-131`) and its own workflow state starts in Planning, which hides mutation tools at dispatch (`dispatch.rs:95-108`), so the practical blast radius is bounded. Still, the fail direction is wrong for a security check.

**Fix:** on a *known* parent id with a missing loop, return `Some(Vec::new())` (empty allowlist — deny everything) instead of `None`; keep `None` only for genuinely parentless (UI/main) spawns. Update `missing_parent_loop_returns_none` (`spawn.rs:676-688`) accordingly.

### L2. NTFS alternate data streams can attach data to protected files

**Where:** `src/tool/agent/sandbox.rs:170-187` (`is_protected_write_target` compares exact relative-path strings) + `src/tool/agent/file_write.rs:86-148`.

The protected matrix (memory.db, safety.toml, backlog.json, `.coding/plans/`) matches exact names. A path like `.coding/memory.db:evil` passes `validate` (the parent canonicalizes; the ADS path does not exist yet) and fails the protected check lexically (`".coding/memory.db:evil"` ≠ `".coding/memory.db"`); `fs::write` on the canonicalized `\\?\`-prefixed path passes the colon through to `CreateFileW`, creating an *alternate data stream* on the protected file. The main content is not clobbered (I verified the trailing-dot variant is neutralized by the `\\?\` prefix canonicalization produces, which disables Win32 dot-stripping), so impact is limited to smuggled/attached data on live-state files. Note `git commit`'s `add -A` would also happily commit such streams.

**Fix:** in `Sandbox::validate`/`validate_for_creation` (or `is_protected_write_target`), reject any Windows path whose final component contains `:` (after the drive prefix), or compare the protected name against the component before the first `:`. Cheap and closes the class.

### L3. Known-open items — current state re-confirmed (no expansion)

- **shell `data` field (shell.rs:197) — severity DOWN, not context bloat.** Verified at HEAD: the tool message fed back to the LLM is built from `result.output` only (`src/agent/turn.rs:1119-1127`), and working-memory capture stores the capped output (`turn.rs:1107-1115`). The uncapped `{stdout, stderr}` JSON crosses IPC to the webview for the tool card (`turn.rs:1134-1139`) — a UI/event payload bloat (a multi-MB stdout serializes per event), not an LLM-context leak. Recommend capping `data` at the same 100 KiB for symmetry, but the previously-recorded framing ("carries uncapped stdout/stderr into context") is factually wrong at HEAD.
- **browser `data:` scheme in `ALLOWED_SCHEMES` (`src/browser/mod.rs:89`) — unchanged, accepted-low stands.** A `data:text/html` page runs in a null origin inside the throwaway headless profile; it cannot read local files, and the agent's ability to run JS there is separately gated (`browser_eval` is `NeedsApproval`, `src/tool/browser/mod.rs:528`). Keep as accepted.
- **Sandbox validate→use TOCTOU — unchanged, accepted-low stands.** `file_write`/`file_edit` still canonicalize-then-write with no re-open guard (`file_write.rs:100-148`, `file_edit.rs:533-562`); single-user, non-adversarial-FS assumption documented.
- **AgentManager single mutex — unchanged, accepted-low stands** (spawn/register under one lock, `src-tauri/src/ipc/spawn.rs:95-165`; short critical sections, no lock held across awaits that could deadlock with the fan-in channel).

---

## Regression check (known-fixed items re-verified at HEAD)

| Item | Status | Evidence |
|---|---|---|
| `describe_image` NeedsApproval + magic-byte sniffing | ✅ holds | `src/tool/agent/describe_image.rs:214-221` (`SafetyLevel::NeedsApproval`, doc: "never auto-run… a broad safety rule cannot auto-approve"), `:70-106` (`magic_matches` on PNG/JPEG/GIF/WebP/BMP before base64/encode). Extension→MIME is extension-driven but content-verified — rename exfil closed. |
| Logs user-only perms | ✅ holds | `src/provider/trace.rs:663` (traces.jsonl) and `:756-762` (provider-errors.jsonl) both call `restrict_permissions` (DACL via `keys.rs:140-229`). |
| reqwest redirect policy | ✅ holds | `src/provider/openai.rs:123` (main client) and `:193` (`fetch_models_with_vision`); vision reuses the main client's `http_client` (`vision.rs:122,144-146`) → covered. |
| keys.toml DACL + redacted Debug | ✅ holds | `src/config/keys.rs:140-229` (user-only DACL, `PROTECTED_DACL_SECURITY_INFORMATION`), `:288-296` (manual `Debug` prints `"<redacted>"`), `:111-123` (save → `write_atomic` → restrict). `.bak` snapshots use `CopyFileW` semantics (inherit source DACL); accepted. |
| 401/403 body suppression | ⚠️ **partial** — only `/models` | See **M2** above: `openai.rs:487-496` and `vision.rs:154-159` still include raw bodies. |
| `protected_case_variant_caught` | ✅ holds | Test exists (`sandbox.rs:391-421`) and matches the implementation (`:170-187` lowercases the relative path before comparison). 8.3 short-name variants are neutralized because `validate` canonicalizes existing paths back to long names, and non-existent short-name targets are simply new files. (New, narrower gap: ADS — see **L2**.) |
| shell runs arbitrary commands behind approval gate | ✅ by design, unchanged | `shell.rs:121-123` NeedsApproval; `is_project_scoped` returns false for shell (`approval.rs:118-133`), so even `AutoApproveProject` prompts. |

---

## Clean areas (checked deeply, no findings)

- **Sandbox core (`sandbox.rs`)** — canonicalize + `starts_with` root check, traversal rejection, symlink resolution, parent-canonicalize for non-existent targets, lexical `..` normalization in `validate_for_creation`; protected-write matrix enforced in **all** of `file_write` (three sites incl. the creation gap), `file_edit` (execute + approval preview), `file_append`; reviewer's `.coding/reviews/` deliberately writable.
- **`file_read` / `read_files` / `read_one`** — every path through `Sandbox::validate`; per-file line/byte caps with char-boundary-safe truncation; `search_read`'s auto-read stage re-validates through `read_one`.
- **git tool** — argv-only execution (no shell string), `valid_branch_name` rejects leading `-`/whitespace (flag injection guard), subcommand allow-list, `commit` message required, `merge`/`push` forced-prompt via `never_auto_for` which dispatch checks **before** safety rules (`dispatch.rs:195-208`) — the precedence is correct.
- **Approval/dispatch layer** — ToolFilter re-enforced at dispatch (`dispatch.rs:95-108`) so schema hiding can't be bypassed by hallucinated names; plan-mutation tools main-agent-only (`:114-126`); state-transition gate while descendants run; DenyAll latch; `never_auto_for` beats both Autonomous mode and saved rules.
- **`write_review_report`** — bare-filename-only (no separators/`..`), canonicalize containment against `.coding/reviews/`, non-empty content; correctly `AutoRun`.
- **spawn/allowlist (`ipc/spawn.rs`)** — reviewer role validated at the tool (`spawn_agent.rs:166-176`), unknown role → *empty* allowlist at `:324-333`; subset computed from the parent's real `(category, safety)` tuples, not names; `ALWAYS_SAFE_ROLE_TOOLS` additions are genuinely read-only (`git_diff`) or containment-guarded (`write_review_report`). (Fail-open on missing parent → **L1**.)
- **Memory store** — all SQL parameterized (`rusqlite::params!` everywhere; no string-built statements found); FTS5 query built with proper token quoting (embedded `"` doubled, `cmd_class`-style operators neutralized, `memory/mod.rs:550-565`); string caps on stored working events.
- **Browser URL gate** — single choke point `normalize_url` (`browser/mod.rs:289-305`, `:972-1001`): scheme allow-list enforced after omnibox autodetection; `file:`, `javascript:`, `about:`, `C:\…` rejected (drive letters caught by `is_scheme_like`); per-tool safety levels sensible (navigate/eval/click/type/switch = NeedsApproval; list/screenshot/console/snapshot = AutoRun; `game_eval` = NeedsApproval — arbitrary JS in the app's own webview is correctly gated, `game_snapshot` AutoRun is read-only).
- **IPC surface (`src-tauri/src/ipc/*`)** — `read_file`, `list_files`, `list_markdown_files`, `save_conversation`, `load_conversation` all route paths through `Sandbox::validate`; `get_config`/`get_settings` never serialize the key store; `get_api_keys` is the only secret-bearing command and is webview-only; `save_endpoints` validates endpoint shape and rejects partial batches.
- **Tauri config & capabilities** — `tauri.conf.json` CSP is strict for production (`script-src 'self'`, no `unsafe-eval`/`unsafe-inline` scripts, `connect-src ipc:` only, `object-src 'none'`), which structurally blunts any webview XSS → key-exfil chain; `capabilities/default.json` is minimal (no `shell:allow-execute`, only `shell:allow-open` + dialog/core defaults); `additionalBrowserArgs --disable-extensions` on both the app window and the headless profile.
- **Frontend** — no `dangerouslySetInnerHTML`/`innerHTML` anywhere in `frontend/src` (React escaping covers LLM-controlled content), so untrusted markdown cannot become script in the webview.
- **Secrets hygiene in Rust** — `KeyStore` Debug redacted; `LlmRequestRecord.base_url` host-only; trace file mirror redacts `api_key`/`authorization` keys and `Bearer` tokens in both JSON and raw text before disk (`trace.rs:701-817`), with tests.
- **Unsafe Rust** — the only `unsafe` blocks reviewed are the Windows SID/DACL FFI in `keys.rs` and trace's use of it: buffer lifetimes are owned (`sid_bytes.to_vec()` before the token handle closes), allocated ACLs freed with `LocalFree`, handles closed on both paths. No soundness issues found.
