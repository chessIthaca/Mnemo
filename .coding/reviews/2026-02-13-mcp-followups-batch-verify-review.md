## Verdict: PASS

Round-2 verification of plan 844180a8 ("MCP follow-ups batch") on wt/agenticcoder. All five round-1 findings (2 HIGH + 3 LOW) are fixed correctly and comment-accurately, each with a real regression test that fails without the fix. The specific no-regression concerns (no resurrection of deliberately-deleted globals/endpoint keys, 401 retry bounded at once, dispatch/deferral gates untouched) all hold. No new issues found.

## Fix verification

### HIGH 1 — Split-save deletes shadowed global servers → FIXED

`restore_shadowed_globals(new_global, project, current_global)` (src/config/mcp.rs) is present, pure, and correct: it appends each current-global def whose name is in the project set and absent from the new global set. The owned `out_names: BTreeSet<String>` (cloned before the pushes, with the borrow comment) is sound — and safe despite going stale, since `load_or_default` dedups current_global names so each is examined once. Wired into `mcp_save_servers` exactly as specified: current global loaded (`load_or_default(&global_path)`) → restore → `validate_set` re-validation → merged-view re-validation → write. Test `restore_shadowed_globals_keeps_shadowed_baselines` asserts all three cases: shadowed twin restored (`[brand-new, shared]`), user-dropped non-shadowed `other` does NOT resurrect, and no-overrides is a no-op (`restore(&[], &[], &[shared])` → empty). I hand-traced the test's expected outputs against the implementation — they match.

**No-resurrection check:** a deliberately-deleted global is only ever re-added when a project def currently claims its name (`project_names.contains` gate); with no project override the function is identity on `new_global`. A deletion of a project override plus its twin also propagates (neither set contains the name → not restored). Correct.

### HIGH 2 — OAuth tokens wiped by save_all → FIXED

`Config::save_all` now merges on-disk keys.toml entries into the keys payload during the serialization phase (before any disk touch): `name.starts_with("mcp-") && keys.get(name).is_none()` → insert. Memory-wins for everything else, so endpoint-key edits and deletions stick. `KeyStore::iter()` is the new documented accessor. The only Settings write path (`save_endpoints` → `persist_and_reload` → `save_all`, verified at src-tauri/src/ipc/config_io.rs:37-43 and settings.rs:209) is covered. Test `save_all_preserves_mcp_oauth_tokens_on_disk` asserts both directions: memory `openai=k1` beats disk `stale`, and disk `mcp-fs` blob survives. I traced the merge: `{openai: k1, mcp-fs: blob}` is exactly what gets serialized.

**No-resurrection check:** only the `mcp-` prefix is restored from disk — a deleted endpoint key absent from memory is not re-added; a manually-deleted on-disk mcp-* key is also not resurrected (there is nothing left on disk to merge; memory never holds mcp-* keys since the OAuth flow writes the store directly). The other keys.toml writers (McpTokenStore::store — load-then-insert, preserves siblings; KeyStore::save callers are tests only) cannot clobber the namespace.

### LOW 3 — Bearer not gated on auth + name-only token key → FIXED

`HttpMcpClient::new` wires the store only when `def.auth.as_deref() == Some("oauth")` (removing auth stops old-token injection immediately); `expected_origin` comes from the new pure `oauth::origin_of` (scheme://host[:port], unit-tested incl. port + bad-url error). Every load goes through `stored_tokens()`, which requires the store, a parseable expected origin, and an exact `origin` match — so re-pointing a url invalidates old tokens (no foreign host sees them, and `refresh_tokens` can only run after the same origin check, so the refresh token never goes to a re-pointed host's discovery). `OAuthTokens.origin` is `#[serde(default)]`, so legacy blobs parse with `origin: None` and fail the filter → treated as absent, one-time re-auth, exactly as documented. Both exchange paths stamp it: the ipc `mcp_oauth_start` task via `origin_of(&server_url)`, and `HttpMcpClient::refresh_tokens` via `expected_origin.clone()`. `from_response` (oauth.rs:121) stamps `origin: None` for the caller to fill — the "fixed along the way" item is present and correct.

### LOW 4 — Double-encoded OAuth code → FIXED

`oauth::urldecode` is pure and correct — I hand-traced the byte walk: valid `%XX` hex pairs expand (indices i+1/i+2 checked via `i + 2 < b.len()`), stray/truncated `%` (`%2`, `%zz`) stays literal, and `+` passes through untouched. `wait_for_code` decodes BOTH `code` and `state` before use (state decode is identity for the hex UUID — symmetric robustness as specified). The decoded code is then single-encoded by `build_token_request`, fixing the `%2B` → `%252B` double-encode. Test `urldecode_expands_percent_escapes` covers `%2B`, `%C3%A4` (multi-byte), and truncated/invalid escapes; the existing `wait_for_code_parses_and_validates_state` still passes a literal code through.

### LOW 5 — Empty project file materialized → FIXED

`mcp_save_servers` gates the project write on `!project_defs.is_empty() || project_path.exists()` — a purely-global save no longer creates `.coding/mcp.toml` in projects that never had one; existing files (even with an emptied set) are still rewritten. Matches the fix description exactly.

## Specific no-regression checks (all hold)

- **restore_shadowed_globals** cannot resurrect a deliberately-deleted global when no project override claims it (name-gated; asserted by the test's `other` case).
- **save_all's mcp-* merge** cannot resurrect a deleted ENDPOINT key (`mcp-` prefix gate; asserted by the test's `openai` case).
- **401 retry bounded at once**: `request()` refreshes + retries exactly once, then routes straight to `read_response` — a second 401 surfaces as an error, no loop.
- **Dispatch/deferral gates untouched**: `load_tools.rs` diff is test-only (FakeClient trait impls); `McpTool` category/safety unchanged (not in the diff); research-prefix exclusion and `mcp__` naming untouched.
- **restore/merge/sync consistency**: the restored twins land in `config.mcp` (memory) too, so a later `save_all` round-trips the same global set — no divergence between the save path and the save_all path. Merge order (project-first) matches `mcp_list_servers` source tagging.
- **stdio decline path**: still sound — mutex-serialized writes, pending→stdin lock order never nested, the "stray-brace" fix left the reader task syntactically complete (green suite confirms).

## Tests

The three new regression tests are real (hand-traced, they fail without their fixes): `restore_shadowed_globals_keeps_shadowed_baselines`, `save_all_preserves_mcp_oauth_tokens_on_disk`, `urldecode_expands_percent_escapes` (+ the existing origin test `origin_of_extracts_scheme_host_port`). Main agent reports the full suite green after the fixes (cargo test 1615 passed / 0 failed / 1 ignored; src-tauri build green; frontend unchanged since its green run) — consistent with my read of the code.

## Observations (non-blocking, pre-existing — no action required)

1. `token_key` sanitization can collide for names differing only in non-alphanumerics (`a.b` vs `a-b` → same `mcp-a-b` key); same-host collisions would share tokens. Pre-existing from round 1 and mitigated for the cross-host case by the new origin validation — noted for completeness only.
2. save_all's disk merge silently skips when keys.toml is unreadable/unparseable (`if let Ok(disk)`); in that pathological state the OAuth store itself would also fail to read the tokens, so behavior is consistent graceful degradation, strictly better than the pre-fix always-wipe.
