# Review — plan 1c50659f "Show images from image commands in chat + Chat settings toggle"

Reviewed: `git status --short` + full `git diff HEAD` + the new files (ToolImage.tsx, ToolImage.test.ts, plan .md) + surrounding code (image tool schemas in `src/tool/agent/image_tools/tools.rs`, browser screenshot data shapes, sandbox validation, settings IPC wire, ChatSection, store, contract fixtures). .coding/ harness files (backlog.json, plans/stack.json, plan .md) skimmed only.

Overall: the change is well-structured and mostly correct. The Rust IPC is sandbox-validated, allowlisted, size-capped, spawn_blocking with owned data (no State across await); the toggle is default-ON and consistent across config default, store init, and `!== false` hydration; the frontend component is fail-quiet with a disposed flag; tests are registered in vitest.config.ts; no Windows-only APIs; doc comments present; no `#[allow]`. Findings below, no High severity.

## Medium

**M1 — `image_ui_diff` never renders its images; the test masks this with an impossible arg shape.**
`frontend/src/components/chat/ToolImage.tsx:27-37` — `toolImagePath` only looks for an `image` key for `image_*` tools, but `image_ui_diff`'s schema (`src/tool/agent/image_tools/tools.rs:451-506`) requires `image_a`/`image_b` — there is no `image` property, and an `image` key would fail serde deserialization ("unknown field"). So in real usage `toolImagePath("image_ui_diff", …)` always returns `null` and one of the 7 advertised image tools never shows its image.
`frontend/src/components/chat/ToolImage.test.ts:21-22` masks this: the test calls `toolImagePath("image_ui_diff", JSON.stringify({ image: "a/b.png", image_b: "c.png" }), …)` — a shape the real tool can never produce (the plan's own doc comment for image_ui_diff says the schema is `image_a`/`image_b` and the test even names the second key `image_b`, confirming the author knew the real key). The test passes but asserts nothing about real image_ui_diff calls.
Fix: handle `image_a`/`image_b` for `image_ui_diff` (e.g. prefer `image_a`, or render both thumbs), and change the test to use the real arg shape `{ image_a: …, image_b: … }`. As written, the headline feature silently no-ops for 1 of the 7 tools.

**M2 — UX gap: the image only renders inside the expanded ToolCard, and cards are collapsed by default.**
`frontend/src/components/chat/Message.tsx:419` (`useState(false)`) + `:558-571` (image renders only in the `expanded &&` block) — the tool card's collapsed default state means "always show the image in the agent window" (backlog item 73) is only true after the user clicks the card header. The image lives inside CallDetail alongside args/result, so it is consistent with the existing card UX, but the literal requirement ("always show the image") suggests at least a thumbnail visible in the collapsed state (or auto-expanding image cards). Low-Medium; decide deliberately and document in the plan.

## Low

**L1 — Size cap TOCTOU: metadata check then untracked `fs::read`.**
`src-tauri/src/ipc/files.rs:83-91` — `meta.len()` is checked, then `std::fs::read` pulls the whole file; if the file grows between the stat and the read (or is replaced), more than 20 MiB is read/encoded into the data URL. The path is sandbox-confined and the consumer is the app's own renderer, so impact is minor — but a bounded read (e.g. `File::take(MAX+1)`/read-up-to-cap-then-reject) would make the cap airtight. Also note the size-cap branch has no regression test (only the happy path, wrong ext, missing file, sandbox escape, and .jpg→image/jpeg are covered — `read_image_data_url_validates_and_encodes`); consider adding an over-20-MiB fixture.

**L2 — Doc comment inaccuracy: "sandbox-validated BEFORE the spawn".**
`src-tauri/src/ipc/files.rs:104-105` — the comment says the path is validated before the spawn, but `read_image_data_url_sync` performs `sandbox.validate` inside the spawned closure (first statement). The behavior is correct and safe (owned `Sandbox` clone + owned `String` moved in, no `State` across await), and validating inside the blocking thread is strictly better; the comment just doesn't match the code. Reword ("the closure validates the path first, inside spawn_blocking").

**L3 — no magic-byte sniffing, deviating from the existing image loader.**
`src-tauri/src/ipc/files.rs:71-95` — `load_image_data_url` in `src/tool/agent/image_tools/mod.rs:63-86` verifies magic bytes (`magic_matches`) so a non-image file renamed with an image extension is rejected; the new IPC allowlists only the extension. Here the bytes go only to the app's own `<img>` renderer (never to a network endpoint, and the model never sees them), so there is no exfiltration channel and no scripting risk — the impact is a broken-image thumbnail for mislabeled files. Low; reusing `magic_matches` (also for `ico`, which the existing matcher lacks) would keep the two loaders consistent.

## Checked and clean (no findings)

- **Sandbox security**: `validate()` canonicalizes + `starts_with` check (symlink/traversal safe, `sandbox.rs:82-100`); the escape test (files.rs:326-331) exercises it.
- **Extension allowlist**: lowercased (`str::to_ascii_lowercase` — `.PNG`/`.Png` handled; `"png "` trailing space correctly rejected, no false accept). MIME mapping png/jpg/jpeg/gif/webp/bmp/ico correct, incl. `image/x-icon`.
- **spawn_blocking**: owned sandbox clone + path String moved in; `.await` on the join handle; mirrors `read_file` (N2 review 2026-06-14).
- **Frontend IPC wrapper + registration**: `readImageDataUrl` (tauri.ts:1128-1134) with doc comment; registered in main.rs invoke_handler (main.rs:692).
- **Toggle plumbing**: config default `true` (general.rs:308), store init `true` (useAgentStore.ts:548), App hydration `!== false` absent-field tolerance (App.tsx:253-261), ChatDraft + serializeChat dirty detection (types.ts:288-297), ChatSection save sends `show_tool_images` matching `SettingsSaveDto` field (settings.rs:887), apply in both save_settings (settings.rs:1153) and lib `apply_settings_patch` (patch.rs:378).
- **IPC wire contract**: `GetSettingsUi` (settings.rs:493-509), Rust fixture (contract_fixtures.rs:220), JSON fixture (dto-get-settings.json), and TS contract test (ipc-contract.test.ts:370) all agree.
- **Fallback regex**: `\.coding\/browser\/screenshots\/[\w.-]+\.png` — safe class, no backtracking risk, matches both "screenshot saved: …" and "game screenshot saved: …" outputs (SCREENSHOT_DIR constant, forward slashes on both OSes).
- **Screenshot data**: `data.path` already additive from prior work (browser/mod.rs:242,770 — unchanged); the diff only strengthens the existing test; fallback covers edge cases.
- **ToolImage component**: fail-quiet catch, disposed flag on cleanup, re-fetch on path change, 20 MiB cap backend-enforced; unmounts on collapse (bounded memory).
- **Message.tsx wiring**: prop threading ToolCard→CallDetail correct; other tools unaffected; running/failed calls render nothing.
- **Vitest registration**: ToolImage.test.ts registered in vitest.config.ts; tests cover parse failure, data.path, text fallback, wrong tool, failed result (with the M1 caveat).
- **Multi-platform neutrality**: portable Rust + pure TS; game_screenshot stays behind its pre-existing Windows/WebView2 gate; no cfg(windows) additions.
- **Constitution**: doc comments on all new public Rust fns; no `#[allow]`; no dead code; README bullet added and accurate; no unrelated source changes (backlog/stack.json are harness state).
