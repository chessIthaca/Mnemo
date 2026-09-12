## Verdict: FINDINGS (0 high, 1 medium, 2 low)

Security review of Mnemo at HEAD bf0f71c + uncommitted delta (run_all.rs `#[allow(dead_code)]`). All eight axes reviewed against the actual tree; the 8957b12 SSRF protection verified holding on the initial fetch; prior findings from 2026-12-15/2026-12-17 not re-reported. One fresh MEDIUM (Run-All checkpoint can commit to main), one residual LOW in the SSRF area (redirect hops), one constitution LOW in the uncommitted delta.


## Findings

### MEDIUM — Run-All pre-item checkpoint can commit directly to `main`

**Evidence:**
- `src-tauri/src/ipc/run_all.rs:962-964` — `run_all_dispatch_next` calls `checkpoint(root, item.clone()).await` with no branch check before dispatching the item.
- `src/project/git_ops.rs:104-123` — `checkpoint_impl` runs `git add -A` + `git commit -m "backlog: pre-item checkpoint …"` on **whatever branch is currently checked out**; it only skips when the tree is clean.
- `src-tauri/src/ipc/backlog_cmds.rs:368-402` — `backlog_run_all` (the loop's entry command) has no branch guard either.
- `src/project/git_ops.rs:577/640` — `prepare_branch`/`ensure_work_branch` exist but are never called on this path.

**Why it matters:** The constitution's hard rule "Never commit to main" is enforced for agent work by `create_plan`'s auto-fork — but that fork happens only *after* the item's prompt is dispatched. The checkpoint runs *before* dispatch, on the current branch. Concrete reachable sequence: `merge_to_main` deletes the `wt/*` branch and leaves the session on `main`; the user then adds backlog items (dirtying the tracked `.coding/backlog.jsonl` — exactly the state of the current uncommitted delta in this tree) and starts Run-All overnight; the first pre-item checkpoint commits the dirty tree straight to `main`, unattended. `commit_success` (git_ops.rs:140-155) shares the exposure for any item whose turn never creates a plan (e.g. a research/answer-only item), since the auto-fork never fires.

**Fix sketch:** In `run_all_dispatch_next` (or once in `backlog_run_all` before the loop starts), read the current branch (`git rev-parse --abbrev-ref HEAD`); if it is `main`, call `prepare_branch`/`ensure_work_branch` to fork/reuse the per-directory `wt/*` branch — the same auto-fork `create_plan` performs — so both the pre-item checkpoint and `commit_success` always land on the working branch. Add a regression test with a mock `GitRunner` asserting no `commit` is issued while `HEAD` is on `main` without a prior `checkout -b`/`switch`.

### LOW — web_fetch redirect hops bypass the SSRF blocklist (documented residual)

**Evidence:**
- `src/tool/agent/web_fetch.rs:186` — `reqwest::Client::builder()…redirect(reqwest::redirect::Policy::limited(5))`: redirects are followed up to 5 hops.
- `src/tool/agent/web_fetch.rs:154` — inline comment documents the residual: redirects are "bounded to 5 but not re-checked against the blocklist".
- The 8957b12 protection (`ensure_public_http_url`: scheme filter, IPv4/IPv6/IPv4-mapped literal blocklist, DNS-resolution check, fail-closed) is applied **only to the initial URL**.

**Why it matters:** The task asked to verify the SSRF protection on every fetch path including redirects — this is the one path it does not cover. Attack: the model fetches an attacker-influenced *public* URL; that server replies `302 Location: http://169.254.169.254/…` (or `http://localhost:<port>/…`); reqwest follows it without re-validation and the internal response body is returned to the model. This is a working bypass of an explicit security control, albeit one the fix deliberately documented as a known limitation.

**Fix sketch:** Build the client with `redirect::Policy::none()` and implement a manual redirect loop: on each 3xx, resolve the `Location` header against the current URL, run it through the same `ensure_public_http_url` gate, and only then follow (bounded to 5 hops). If closing it now is out of scope, promote the inline comment to a tracked backlog item so the residual is a decision, not an accident.

### LOW — `#[allow(dead_code)]` on `extract_checkpoint_sha` violates the project constitution (uncommitted delta)

**Evidence:**
- Uncommitted change in `src-tauri/src/ipc/run_all.rs` — adds `#[allow(dead_code)]` to `extract_checkpoint_sha`, with a comment noting the function is "kept deliberately callerless" and that the tests below are its only users.
- Project constitution: "Never add `#[allow(...)]` to silence a warning — fix the root cause (remove the dead code, drop the unused import, drop the unneeded `mut`, etc.)."

**Why it matters:** The constitution makes `#[allow]`-silencing a hard finding, and the root cause is clear: the function has had no production caller since backlog item 45dcf577 removed the rollback arm — only its unit tests use it, and `#[cfg(test)]` items don't count for dead-code analysis in non-test builds. Landing this as-is sets a precedent for silencing future warnings rather than fixing them.

**Fix sketch:** Mark the function `#[cfg(test)]` (the tests are the only users, so the warning disappears without `#[allow]`), or delete it together with its tests if the sha-preservation feature is considered dead. No security impact — policy compliance only.


## Done well

- **SSRF protection (8957b12) verified holding on the initial fetch**: scheme filter (http/https only), IP-literal blocklist covering IPv4, IPv6, and IPv4-mapped forms, DNS-resolution check against the blocklist, and fail-closed on `spawn_blocking` failure (`src/tool/agent/web_fetch.rs`). No other HTTP egress exists outside it — provider clients hit user-configured `base_url`s only, and the browser child webview goes through `normalize_url` (`src/browser/mod.rs:1386`), which rejects `javascript:`/`about:` and non-`file://`-authority forms.
- **Path sandboxing is uniform and canonicalization-based**: `Sandbox::validate` (canonicalize + `starts_with` root) is applied by every file tool, the shell tool's `cwd` (`shell.rs:142`), the IPC file commands (`files.rs`), and git pathspecs; protected write targets (`.coding/reviews/`, `.coding/plans/`, `backlog.jsonl`) are refused case-insensitively with an NTFS alternate-data-stream guard; git invocations use the `--` separator and scrub `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` (no flag injection).
- **SQL is fully parameterized**: memory store queries use bare `?` placeholders with `params_from_iter`; FTS tokens are double-quoted with embedded quotes doubled (`src/memory/mod.rs:1163-1233`). No string-interpolated SQL anywhere in memory or backlog stores.
- **Shell tool**: argv-based spawn (no shell interpolation beyond the intended `-Command`), flag-injection guards, timeout + `kill_on_drop`, output capping.
- **Secrets hygiene**: `KeyStore` has no `Debug` impl leaking keys; `keys.toml` is written atomically with user-only DACL (Windows) / 0600 (Unix) via sound, well-commented `unsafe`; the provider trace redacts `api_key`/`Authorization` in both JSON and text forms and permission-restricts `.coding/logs/*.jsonl`; IPC `get_config` never includes secrets (separate `get_api_keys` gated on the settings dialog).
- **Backlog union merge is robust against hostile lines**: `parse_jsonl` skips malformed lines gracefully, soft-delete `deleted_at` markers survive merges, the 30-day purge and resurrection dedup are guarded, and image sidecar writes reject unsafe item ids (path separators, `..`) with `starts_with(image_root)` containment (`src/backlog.rs:633-776`).
- **Search auto-delegation (6cf10d5) is strictly read-only**: the memory arm calls `store.recall` (same read path as `memory_search`) and the symbol arm does exact-match graph lookups; the glob is validated before any use; the escape hatch is per-pattern. It cannot reach a write path or bypass the tool filter — dispatch re-resolves every call by exact registry name and re-checks the workflow `ToolFilter` (`src/agent/dispatch.rs:79-106`), so tool-name spoofing from model output cannot reach a filtered tool.
- **Reviewer read-only enforcement is layered**: `ToolFilter::Reviewer` is a strict allow-list; `write_review_report` is visible under no other filter, and every base state and Skill allow-list denies it (`src/tool/mod.rs`).
- **Tauri surface is tight**: strict CSP (`default-src 'self'`, `script-src 'self'`, `object-src 'none'`, `base-uri 'self'`; the relaxed `devCsp` is dev-only for Vite HMR), minimal capability set, the `browser-child` webview sits outside the `windows: ["main"]` capability so model-navigated content has no IPC access, the markdown pipeline (react-markdown without rehype-raw) escapes raw HTML, and no `dangerouslySetInnerHTML` exists in the frontend.
- **`unsafe` is confined and justified**: keys.rs (DACL/SID hardening), console.rs and watchdog.rs (read-only Windows diagnostics), all `cfg(windows)`-gated with sound SAFETY reasoning; no unsafe in the core library or IPC layer.
- **ReDoS is structurally absent**: every user/model-supplied regex (search tool, safety rules, shell filter) compiles through the linear-time `regex` crate, which has no catastrophic backtracking by design.
- **Fresh provider surface is clean**: the GLM-5.3 stream guard (`find_boundary_cutoff`, applied in the streaming loop at `openai.rs:1116`, regression-tested at :6204) and the DeepSeek `reasoning_effort` off→none wire mapping (`policy::reasoning_effort_off_wire_value`, tested at :3706-3742) are request/response shaping only — no URL, secret, or injection surface.
