# Review — Top-Impact Fixes (B1/B2, H1, H2, R1, M1-Sec)

Reviewed all uncommitted changes in the working tree (`git diff HEAD`):
`Cargo.toml`, `Cargo.lock`, `src/tool/agent/describe_image.rs`,
`src/config/keys.rs`, `src/provider/trace.rs`, `src/agent/context.rs`,
`src/tool/agent/sandbox.rs`. (`.coding/plans/stack.json` and the untracked
plan `.md` files are bookkeeping artifacts, not source changes.)

The plan set out five fixes; all five are implemented as described.

---

## Correctness

**No findings.**

- **Magic bytes** (`src/tool/agent/describe_image.rs:92-106`): all signatures
  are correct.
  - PNG `\x89PNG\r\n\x1a\n` (8 bytes) ✓
  - JPEG `\xff\xd8\xff` (3 bytes) ✓
  - GIF `GIF8` (4 bytes) ✓
  - WebP: `RIFF` at `[0..4]` + `WEBP` at `[8..12]`, guarded by
    `bytes.len() >= 12` so the `[8..12]` slice cannot panic on a short input ✓
    (the offset-8 check is correct — WebP is `RIFF` + 4-byte size + `WEBP`.)
  - BMP `BM` (2 bytes) ✓
  - Unknown MIME → `false` (safe default: reject) ✓
  The `magic_matches_known_signatures` test exercises every branch including
  the too-short WebP case and the unknown-MIME case.

- **OnceLock BPE cache** (`src/agent/context.rs:249-264`): correct.
  `BPE.get_or_init(|| cl100k_base().ok())` yields `&Option<CoreBPE>`;
  `.as_ref()` narrows to `Option<&CoreBPE>`; `?` extracts `&CoreBPE` (returns
  `None` when the initializer produced `None`, preserving the original
  "tiktoken unavailable → fall back" behavior). `OnceLock` is `Sync`; the
  initializer runs at most once across threads. `CoreBPE` is `Send + Sync`
  (it's what `cl100k_base()` returns), so holding a `&CoreBPE` from a
  `static` is sound. No thread-safety concern.

- **Case-insensitive guard** (`src/tool/agent/sandbox.rs:170-187`): correct.
  `.to_ascii_lowercase()` is applied after the backslash→forward-slash
  normalization, and all comparison literals are lowercase ASCII, so
  `.coding/SAFETY.TOML` → `.coding/safety.toml` matches. The
  `starts_with(".coding/plans/")` check is also lowercase, so
  `.coding/PLANS/stack.json` matches. Legitimate writes under
  `.coding/reviews/` (any case) remain writable — verified by the
  `protected_case_variant_caught` test's negative assertion and the existing
  `allows_writing_review_reports_under_coding` test.

- **Dep removal** (`Cargo.toml`): `syntect` and `markdown` removed cleanly.
  A repo-wide search for `syntect` / `markdown::` / `use markdown` in `.rs`
  files returns no crate references. The `markdown` identifiers in
  `src/config/general.rs` and `src-tauri/src/ipc/settings.rs` are the
  `[markdown]` config-section name and `MarkdownConfig`/`MarkdownWire` types
  (frontend Markdown-viewer settings), unrelated to the removed `markdown`
  crate. `Cargo.lock` drops the transitive deps (`onig`, `onig_sys`,
  `bincode`, `linked-hash-map`, `yaml-rust`, `unicode-id`) consistently. No
  build break.

---

## Bugs

**No findings.**

- **Redaction does not break existing trace tests.** The test
  `file_logging_mirrors_latest_state_to_one_jsonl_row`
  (`src/provider/trace.rs:728-758`) asserts
  `row["response_raw"] == "chunk1 chunk2"`. `redact_text` only rewrites
  `Bearer <token>` and `"api_key"/"authorization":"<value>"` JSON patterns;
  `"chunk1 chunk2"` matches neither, so it passes through verbatim. The
  other existing trace tests (`error_log_appends_on_fail_and_dedupes`,
  `error_log_transport_error_status_zero`) assert on error text like
  `"bad gateway"` / `"connection refused"`, which contain no secret pattern
  and survive redaction unchanged.

- **Both write paths are redacted.**
  - `write_record_to_file` (traces.jsonl, opt-in): clones the record, runs
    `redact_json` on `request_json` and `redact_text` on `response_raw` and
    `error` before serializing. The in-memory record keeps its verbatim
    value (the clone is the only thing mutated). ✓
  - `maybe_log_error` (provider-errors.jsonl, always-on): serializes an
    `LlmRequestSummary` (which excludes `request_json` and `response_raw` —
    confirmed at `src/provider/trace.rs:621-624`), then applies `redact_text`
    to the whole serialized line. The summary's only secret-bearing field is
    `error`, which `redact_text` covers. ✓ No unredacted secret reaches disk
    on either path.

- **`restrict_log_file` is best-effort and non-fatal.** It swallows errors as
  an `eprintln!` warning, so a permission-restriction failure cannot break
  the request path (the data is already written). This mirrors the
  `keys.toml` contract documented on `restrict_permissions`. ✓

---

## Security

**No findings.** All five fixes improve security and none introduce a
regression.

- **H1 — `describe_image` exfiltration channel closed.**
  - `safety()` changed `AutoRun` → `NeedsApproval`
    (`src/tool/agent/describe_image.rs:214-221`): correct. The tool reads an
    arbitrary sandbox file and sends its bytes off-box to the vision
    endpoint; gating it behind approval is the right call. The
    `schema_is_describe_image_with_required_path` test was updated to assert
    `NeedsApproval`. No other code assumes `describe_image` is `AutoRun`
    (the only `describe_image`+`AutoRun` co-occurrence is a doc comment in
    `src/tool/browser/mod.rs:183`, which is descriptive, not behavioral).
  - Magic-byte sniffing (`magic_matches`, lines 92-106) rejects a non-image
    file renamed with an image extension *before* the bytes are base64-encoded
    and sent. The `load_image_data_url_rejects_content_mime_mismatch` test
    confirms a `.png` containing plain text is rejected with
    `Error::InvalidInput`. This closes the rename-exfiltration gap. ✓

- **H2 — provider log files restricted + redacted.**
  - `restrict_permissions` was made `pub(crate)` in `src/config/keys.rs:140`
    and is now applied to both `.coding/logs/*.jsonl` files at write time via
    `restrict_log_file` (Unix `0600` / Windows user-only DACL). ✓
  - Secret redaction (`redact_json` for structured request bodies,
    `redact_text` for raw response/error text) is applied on both write
    paths. `redact_json` recurses into nested objects and arrays and is
    case-insensitive on keys (`api_key`, `authorization`). `redact_text`
    catches `Bearer <token>` and embedded `"api_key"/"authorization":"..."`
    JSON in raw text. The new tests
    (`traces_redact_api_key_in_request_body`,
    `traces_redact_authorization_in_response_body`,
    `traces_redact_json_key_in_response_case_insensitive`,
    `error_log_redacts_bearer_in_error_text`,
    `redact_json_redacts_secret_keys`,
    `redact_text_redacts_bearer_and_json_keys`) verify each pattern is
    scrubbed and that non-secret content survives. ✓

  Minor note (not a finding — acceptable for a debugging aid): the redaction
  is pattern-based, not exhaustive. A secret embedded in an arbitrary field
  name (e.g. `"x-api-key"` or `"token"`) or in a non-JSON, non-Bearer form
  within a raw response body would not be scrubbed. This is a reasonable
  tradeoff for a best-effort debugging log that is already opt-in for the
  full trace and restricted to user-only for the error log; the documented
  intent ("redact secret-shaped substrings") is met.

- **M1-Sec — case-insensitive protected-write guard.** Closes the NTFS
  case-variant bypass (`.coding/SAFETY.TOML` resolving to the real protected
  file). ✓

---

## Constitution compliance

**No findings.**

- **Warning-free build under `#![deny(warnings)]`.** No `#[allow(...)]`
  attributes were added anywhere in the diff. The new items are all used:
  `magic_matches` (called in `load_image_data_url` + tests), `redact_json`
  / `redact_text` / `restrict_log_file` (called in the two write paths +
  tests), the `BPE` static (used in `try_tiktoken_count`). No dead code, no
  unused imports (`regex` is a declared dependency at `Cargo.toml:38`;
  `CoreBPE` and `cl100k_base` are both used). The `restrict_permissions`
  visibility change `fn` → `pub(crate) fn` does not generate a warning.

- **Doc comments on public items.** All new public-facing items are
  documented:
  - `restrict_permissions` (`pub(crate)`) has an expanded doc comment noting
    the shared use by the trace log. ✓
  - `magic_matches` is private (correctly undocumented at the item level,
    though it has a doc comment anyway). ✓
  - `redact_json`, `redact_text`, `restrict_log_file` are private. ✓
  - The `BPE` static and the `try_tiktoken_count` doc comment are present. ✓
  - No new public functions/types were added without docs. The
    `DescribeImageTool::safety` override is an existing trait method. ✓

- **Tests.** The new tests are meaningful and correct:
  - `magic_matches_known_signatures` — covers all five formats + negative
    cases + the too-short WebP guard.
  - `load_image_data_url_rejects_content_mime_mismatch` — end-to-end
    rejection of a renamed non-image.
  - `protected_case_variant_caught` — case-variant protected paths blocked,
    non-protected case-variant paths allowed.
  - The six redaction tests — each asserts the secret is absent AND the
    `[REDACTED]` marker is present, and several assert non-secret content
    survives.
  - `error_log_file_is_user_only` / `traces_file_is_user_only` — assert
    `0600` on Unix (Windows asserts existence only, with a clear comment
    explaining why the DACL isn't asserted).

- **Line-ending style.** The file tools normalize to the file's detected
  style; the diff shows no mixed-ending introduction. The `Cargo.lock`
  CRLF warning is a pre-existing git attribute note, not a code change.

---

## Summary

All five top-impact fixes are correctly implemented, well-tested, and
constitution-compliant. No correctness, bug, security, or
constitution-compliance findings. The diff is clean.
