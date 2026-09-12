## Verdict: FINDINGS (0 high, 3 medium, 3 low)

# Code Quality & Maintainability Review — Mnemo at HEAD bf0f71c + uncommitted delta

**Perspective:** Code quality & maintainability (read-only).
**Scope:** full tree at HEAD bf0f71c plus the uncommitted `#[allow(dead_code)]` on `extract_checkpoint_sha` (src-tauri/src/ipc/run_all.rs:851). Prior reviews 2026-12-15 (fixed at f680b88) and 2026-12-17 (fixed at 8957b12) were checked for repeats — every finding below is new. All claims verified against the current tree.

**Summary:** the fresh surface (backlog soft delete/purge, status⇄lifecycle semantics, search auto-delegation, GLM/DeepSeek/claude provider fixes, frameless cards) is in excellent shape — documented, regression-tested, docs synced. The accumulated debt concentrates in the provider layer: four byte-identical helpers duplicated between the OpenAI and Anthropic clients, GLM stop-token constants copy-pasted within openai.rs, and a ~742-line `complete()` method. Plus one uncommitted violation of the project's no-`#[allow]` rule and two smaller items.

### Finding 1 — MEDIUM: four identical helpers + SSE plumbing duplicated between the OpenAI and Anthropic providers

**Evidence:**
- `truncate_raw_stream` — src/provider/openai.rs:2364-2374 and src/provider/anthropic.rs:1044-1054: byte-identical bodies (head-keep, UTF-8 char-boundary backup, "(N more bytes truncated)").
- `header_str` — src/provider/openai.rs:2336-2342 and src/provider/anthropic.rs:1058-1064: identical.
- `error_chain` — src/provider/openai.rs:2349-2358 and src/provider/anthropic.rs:1071-1080: identical (`source()`-chain walk joined with " → ").
- `finish_reason_label` — src/provider/openai.rs:2380-2388 and src/provider/anthropic.rs:1086-1094: identical (FinishReason → snake_case wire form).
- `SseOutcome` enum — src/provider/openai.rs:2700-2707 and src/provider/anthropic.rs:568-575: same two-variant shape (Event/ParseError); both `parse_sse_buffer` fns (openai.rs:2715, anthropic.rs:999) share the drain-based line-splitting core (the Anthropic one additionally threads `StreamState`).
- `parse_data_url` — src/provider/anthropic.rs:557-566 vs src/backlog.rs:908-913: same core, **already drifted** — the Anthropic copy defaults an empty media type to `"image/png"` (anthropic.rs:561-562); the backlog copy returns the empty string.

The 2026-12-15 quality review explicitly found "no obvious copy-pasted logic that should be shared" in the provider layer — this has grown in since (or was below that review's threshold); either way it is current and actionable.

**Why it matters:** any fix to one copy silently misses the other — the char-boundary backup in `truncate_raw_stream` and the header-surfacing in `error_chain` are exactly the kind of one-off bug fixes that had to be applied twice. The `parse_data_url` drift is proof the hazard is real: two copies of "parse a base64 data URL" in unrelated modules already disagree on edge-case behavior. The next provider special-case lands in a file that already carries GLM/DeepSeek branches, tempting a third copy.

**Fix sketch:** extract `src/provider/sse_util.rs` (or `provider/shared.rs`) holding `SseOutcome`, `header_str`, `error_chain`, `truncate_raw_stream`, `finish_reason_label` — all five are provider-generic; both clients import from it. Unify `parse_data_url` into one shared util (keep the Anthropic semantics — the png default is the safer behavior; backlog's callers only see well-formed URLs so the default is harmless there). ~60 lines move, zero behavior change, all existing tests keep passing.

### Finding 2 — MEDIUM: GLM-5.3 stop-token constants and model check duplicated within openai.rs

**Evidence:** the same four `const GLM_*_STOP` tag boundaries and the same `starts_with("glm-5.3")` model check exist twice:
- Stream-guard setup — src/provider/openai.rs:945-967: model check at :947-952, four consts at :953-956, dedupe-push loop at :957-966.
- Request builder — src/provider/openai.rs:1792-1837: the same four consts redeclared at :1796-1799, the model check recomputed as `is_glm_53` at :1801-1805, stop-list assembly at :1808-1829.

The two sites intentionally differ in scope — the request also sends the newline cascades `"\n\n\n\n"`/`"\n\n\n"` and `stop_token_ids` `[151329, 151330, 151336]` (:1823), while the guard mirrors only the four tag boundaries (newline cascades would false-positive on legitimate text). But the four tag constants and the model check are pure copy-paste.

**Why it matters:** the stream guard exists precisely to catch what the stop list fails to enforce. When the next GLM revision lands (a fifth boundary token, or a `glm-5.4` prefix), the two sites must be edited in lockstep — miss one and the guard silently stops guarding, with no test failure (the existing tests at openai.rs:6204+ pin each site separately, not their consistency). The magic token ids `[151329, 151330, 151336]` at :1823 are also unexplained inline — nothing ties them to the GLM tokenizer.

**Fix sketch:** hoist a module-level `const GLM_TAG_BOUNDARIES: [&str; 4]` and `fn is_glm_53(model: &str) -> bool`; the guard iterates the const, the builder assembles its stop list from the same const plus the newline cascades and token ids (with a one-line comment naming the tokenizer the ids belong to). Both test suites keep passing unchanged.

### Finding 3 — MEDIUM: openai.rs is the repo's size hotspot — `complete()` is a ~742-line method

**Evidence:**
- src/provider/openai.rs is 6259 lines; `#[cfg(test)] mod tests` starts at :2769 → **2769 production lines**, now the largest production file in the repo. For scale: the 2026-12-15 review flagged memory/mod.rs at ~2180 production lines as MEDIUM (it was since split; it is now 2198 lines *total*).
- `LlmClient for OpenAiClient::complete` spans :710-1451 — a single ~742-line method: parked-timing consumption, message validation, request-build dispatch (chat-completions vs Responses API), trace capture, HTTP send, retry loop, the spawned SSE event loop (tokio::spawn at :968 → while-let over chunks → per-line parse → match on outcome → inner match on event type, with the think-tag filter, repetition guard, and GLM stream guard woven through), tool-call assembly, and fallback-finish synthesis (:1434-1447).
- `build_request_json` spans :1458-1858 (~400 lines) with provider special-casing inline (GLM stops, DeepSeek effort mapping, local Jinja rendering).
- `fetch_models_anthropic` (openai.rs:294) is an Anthropic-endpoint function living in the OpenAI provider file; its only callers are `list_models`/`list_vision_models` in src-tauri/src/ipc/models.rs.

**Why it matters:** a 742-line method is beyond reviewable — every recent provider fix (GLM b3b18a3, DeepSeek a73d38f) landed inside `complete()` or `build_request_json`, so these two functions are the ones still growing; the trend is the finding, not just the current size. `fetch_models_anthropic` in the wrong file misdirects every future reader hunting Anthropic behavior (the natural grep for "anthropic" lands in the wrong module).

**Fix sketch:** (a) move `fetch_models_anthropic` to anthropic.rs — mechanical, no behavior change; (b) split `complete()` into named phases, e.g. `send_with_retries`, `run_sse_loop` (the spawned task body as a standalone fn taking the captured state), `assemble_tool_calls` — each independently readable and testable; (c) longer term, a `provider/openai/` submodule split (request building / stream loop / model listing / think filter) mirroring the memory/mod.rs remediation.

### Finding 4 — LOW: the uncommitted `#[allow(dead_code)]` on `extract_checkpoint_sha` violates the project's no-allow rule; resolve via `#[cfg(test)]` instead

**Evidence:** the uncommitted delta adds `#[allow(dead_code)]` at src-tauri/src/ipc/run_all.rs:851 on `extract_checkpoint_sha`, whose doc comment (:841-850) explains it has been callerless since backlog 45dcf577 removed the rollback arm and is kept as "the manual resume/rollback anchor"; its only users are the tests in the `extract_tests` module. The project constitution is explicit: *"Never add `#[allow(...)]` to silence a warning — fix the root cause (remove the dead code, drop the unused import, …)"* — and committed project code currently contains zero `#[allow]` attributes (verified by tree-wide search; the only other hits are vendor/tao and a doc comment at src-tauri/src/main.rs:894).

**Assessment:** the exception is not justified as-is. A human operator cannot invoke a Rust function inside the Tauri binary — the realistic manual rollback is `git reset --hard <sha>` with the sha read straight from the item note; the function's durable value is exactly its tests, which document the note format (`sha | reason`) and the ≥7-hex-char recovery rule. That value survives without any exemption.

**Fix sketch:** move `extract_checkpoint_sha` together with its `extract_tests` into the file's `#[cfg(test)]` module — it then compiles only under `cargo test`, needs no `allow`, keeps every test green, and the recovery recipe stays documented one screen from the halt path that writes the notes. (Alternative: delete it outright — git history preserves it — but the cfg(test) move loses nothing.) Cheapest to do now, while the change is still uncommitted.

### Finding 5 — LOW: source-contract tests anchor on comments and incidental code text

**Evidence:** 16 tests across four files assert on their own source text via `include_str!`: src-tauri/src/ipc/run_all.rs (10 sites, using the `fn_body` helper at :525-535), src-tauri/src/ipc/agent.rs:916,951,993, src-tauri/src/ipc/backlog_cmds.rs:431,460, src/agent/turn.rs:2182. The pattern is documented as a deliberate compromise (the commands need a Tauri `AppHandle` unit tests can't construct), and pure decision logic *is* properly extracted where possible (`should_stamp_in_flight` is a real behavioral test). But several anchors are incidental text rather than stable contracts:
- run_all.rs:810-812 finds the **comment** string `"Run-All takes priority"` — rewording a comment breaks the test.
- run_all.rs:807-809 finds `"if !closed_loop {"` — inverting the condition to `if closed_loop { … } else { … }` (identical behavior) breaks it.
- run_all.rs:815 asserts the local variable name `iv.item_id`.

**Why it matters:** these tests fail on harmless refactors (noise that trains people to skim test failures) while being structurally unable to catch real regressions in the wired paths — the assertions pass as long as certain strings appear between two markers, even if they end up in dead branches.

**Fix sketch:** keep source-contract tests only for wiring that genuinely cannot be extracted, and anchor them on stable markers (fn signatures, helper names like `finish_captured_item_done` — already used at :829) rather than comments, condition text, or variable names; keep converting decision logic into pure helpers tested behaviorally (the `should_stamp_in_flight` pattern). Minimum viable fix: replace the `"Run-All takes priority"` comment anchor at :811 with the next code construct.

### Finding 6 — LOW: six undocumented `pub async fn` wrappers in git_ops.rs

**Evidence:** project rule — "All public functions must have doc comments." Six `pub async fn` wrappers in src/project/git_ops.rs have none: `checkpoint` (:125), `commit_success` (:157), `rollback` (:178), `prepare_branch` (:577), `ensure_work_branch` (:640), `branch_hint` (:667). Their synchronous `_impl` cores are documented (e.g. `checkpoint_impl`'s doc above :100), so the documentation exists but sits on the wrong function — the public API surface is the wrapper. (Verified tree-wide: these are the only `pub fn`s in the crate not preceded by a doc comment.)

**Why it matters:** rustdoc shows six undocumented public entry points; a reader landing on `rollback` — the scary one — gets no contract (what happens to the worktree on failure, whether it's blocking).

**Fix sketch:** one-line doc comments on each wrapper, e.g. "Async wrapper for [`checkpoint_impl`] — runs the git subprocesses on the blocking pool; returns the checkpoint sha." Mechanical, minutes of work.

---

## Done well

- **Backlog soft delete + 30-day purge (f5be6cf):** `PURGE_AFTER_SECS` is a named, documented const (src/backlog.rs:37-40); six dedicated tests cover the lifecycle — `remove_soft_deletes_line_stays_on_disk` (:1588), `mutators_treat_deleted_id_as_unknown` (:1613), `open_purges_items_deleted_more_than_30_days_ago`, `union_merge_soft_deleted_line_stays_deleted`, `remove_soft_deletes_and_keeps_images_until_purge`, `clear_finished_soft_deletes_and_keeps_images_until_purge` — including the git-union-merge interaction, which is the subtle part.
- **Status ⇄ plan-lifecycle semantics (ba392c1):** `BacklogStatus::allowed_destinations` (src/backlog.rs:115) is a single source of truth for the transition matrix, and the tests are exhaustive in both directions — `transition_allows_every_legal_row_and_persists` (:1121) and `transition_refuses_every_illegal_row_and_unknown_ids` (:1162). Illegal transitions are tested, not just legal ones.
- **Search auto-delegation (6cf10d5):** the search/search_read twins share their helpers instead of copying them (`alternation_nudge` is commented "Shared with search_read"; `try_index` is pub(crate)-shared), and the escape hatch is tested for both tools including the sticky-key replacement semantics (`glob_narrowed_delegation_escapes_on_the_repeat` :2768, `a_different_delegating_query_replaces_the_escape_key` :2800).
- **Every recent provider fix ships its regression test:** GLM stops (request-injection + case-insensitivity tests in the openai.rs tests module; `stream_guard_truncates_and_stops_on_boundary_token` :6204), DeepSeek off→none (`build_request_json_maps_off_effort_to_none_for_deepseek` :3704 plus the non-DeepSeek omission counterpart :3728), claude empty-thinking-block (four tests in anthropic.rs including the only-block-was-empty edge and the redacted-thinking preservation).
- **Docs sync is genuinely excellent:** README:52-53 documents auto-delegation + the escape hatch in detail, README:76 the soft delete/purge, PLAN.md:51 the status semantics, PLAN.md:143-153 the delegation heuristics, PLAN.md:245-246 the GLM/DeepSeek request shaping. Nothing in the fresh surface shipped undocumented.
- **Frontend hygiene:** exactly one `as any` in the whole frontend and it is in a test (useAgentStore.preview.test.ts:170); Message.tsx (1005 lines) is internally well-factored — memo with a hand-written `arePropsEqual` comparator (:37), cohesive sub-components (MemoryEntryCard, VisionEntryCard, ToolCard :664-871, CallDetail) each under ~210 lines, store access via selectors rather than prop drilling.
- **Debt inventory near zero:** one TODO in project code (src/config/patch.rs:291, tracking unfinished F2 wiring), zero `#[allow]` in committed project code, no commented-out code blocks found; `unwrap`/`expect` remain confined to tests and lock-poisoning (consistent with the 2026-12-15 assessment), and the `let _ =` cluster in turn.rs is the documented fire-and-forget event-send pattern.
