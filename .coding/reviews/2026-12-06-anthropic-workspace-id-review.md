## Verdict: FINDINGS (0 high, 3 low)

Review of ALL uncommitted changes on `wt/agenticcoder` for plan f0338572 ("Anthropic workspace-id option + settings error dialog"). The core implementation is correct and verified end-to-end by code reading; the three low findings are polish/docs items. No blocking issues.

**Verification note:** I am a read-only reviewer (no shell) — I could not re-run `cargo test`/`tsc` myself. All structural claims below were verified by reading the code; test-execution status relies on the task's stated green runs (1712 passed / 0 failed / 16 ignored, warning-free under `#![deny(warnings)]`; `npx tsc --noEmit` clean).

## What was verified correct

- **Header on every Messages request:** exactly one `.post(&url)` in `src/provider/anthropic.rs` (line 881) and one `.send()` (line 890); `.headers(self.workspace_headers())` sits at line 887 between `anthropic-version` and `Content-Type`. No other HTTP request path exists in the file (all other `.get(`/`/models` matches are serde_json `Value::get` calls) — the claim "applied on all request paths" holds.
- **Never sent empty / header-only:** `workspace_headers()` (anthropic.rs ~110-126) trims and only emits when `Some` + non-empty; `workspace_id` never enters `build_request_json`, so it is headers-only (matches the LiteLLM #29272 finding — a body field would be invalid). `HeaderValue::from_str` guards the wire (invalid chars can never reach the header).
- **Serde back-compat:** `Endpoint.workspace_id` has `#[serde(default, skip_serializing_if = "Option::is_none")]` (endpoints.rs:96) — old toml loads, `None` is skipped on save so hand-edited files stay byte-stable; test `workspace_id_round_trips_and_none_skips_serialization` (endpoints.rs:446-470) asserts both.
- **DTO round-trip both directions:** `EndpointDto.workspace_id` (settings.rs:83, `#[serde(default)]` save-side) → `into_endpoint` passes it as the 11th arg, matching `validate_endpoint`'s signature order (verified against the 11-arg call shape in patch.rs:585-597); `EndpointWire.workspace_id` (settings.rs:362-363, skip+default) �� `endpoint_wire` mapping (settings.rs:624). Test `workspace_id_round_trips_through_dto` (settings.rs:1449-1454).
- **Trim/clear:** `validate_endpoint` (patch.rs:81) trims and clears whitespace-only to `None`; meaningful test `validate_endpoint_trims_and_clears_workspace_id` (patch.rs:582-614). Header-level test `workspace_headers_set_only_when_configured` covers set/absent/whitespace (anthropic.rs ~1295-1360).
- **Frontend save path end-to-end:** `saveEndpoints` serializes the full `EndpointEditable[]` objects (workspace_id included, `null` from `blankEndpoint()`/`?? null` mappings — backend `serde(default)` handles null), and `serializeProviders` JSON-stringifies full objects, so dirty-tracking and the unsaved-changes guard include workspace_id. `onEndpointChange({ workspace_id })` matches the sibling partial-merge pattern.
- **Error-dialog lifecycle:** `handleSave` starts with `setSaveError(null)` (ProvidersSection.tsx:227) — a successful save after a failure clears the strip, and `open={showErrorDialog && saveError != null}` (line 418) force-closes a stale dialog; failure sets the error and pops the dialog (244-247); the inline strip is a clickable `line-clamp-2` button that reopens it (395-407).
- **ErrorDialog.tsx:** message rendered as a plain React child (`{message}`, line 43) — auto-escaped, no `dangerouslySetInnerHTML` anywhere; `whitespace-pre-wrap` + `max-h-72 overflow-y-auto` scrollable block; Radix primitives give focus trap, aria roles, Escape/overlay close via `onOpenChange` (line 37); OK button wired to `onClose`.
- **Security/no leak:** `LlmRequestLog::start` is called with `(model, base_url, provider, body_for_trace)` only (anthropic.rs:845) — headers never enter trace records; repo-wide `workspace_id` scan shows occurrences only in config structs/tests, zero in log/trace statements.
- **Docs/comments/platform:** module doc mentions the header (anthropic.rs:9-10); `workspace_headers` and every new public field have doc comments; README endpoints.toml section documents `workspace_id` accurately; no platform-specific code in the diff (multi-platform neutral).

## Findings

### L1 (low) — Invalid header chars silently drop the workspace header with no signal
`src/provider/anthropic.rs` `workspace_headers()` (~lines 110-126). A workspace id containing characters `HeaderValue::from_str` rejects (control chars, non-ASCII) — reachable via hand-edited `endpoints.toml`, since `validate_endpoint` only trims — makes the header be silently omitted: requests keep succeeding but usage is silently unattributed, with nothing in logs or the save path to explain why. Fail direction is safe (never an invalid header on the wire), hence low.
**Fix:** emit a one-line warning when `from_str` fails (match the file's existing logging convention), and/or validate the header charset in `validate_endpoint` (patch.rs:81) so an invalid id is rejected at Save time with a readable error — which now surfaces nicely in the new ErrorDialog.

### L2 (low) — workspace_id persists invisibly on non-anthropic endpoints
`src/config/patch.rs:81` + `frontend/src/components/settings/sections/EndpointCard.tsx:470`. The form renders the Workspace ID input only for `kind === "anthropic"`, but the value stays in state and is saved for any kind; `validate_endpoint` doesn't clear it for non-anthropic kinds. Functionally harmless (documented "Ignored for openai/local endpoints"; `openai_client_config` has no such field, `anthropic_client_config` is only built for the anthropic kind), but once the field is hidden a stale value is invisible and un-clearable without switching the kind back and forth.
**Fix (optional, trade-off noted):** clear `workspace_id` to `None` in `validate_endpoint` when `kind != Anthropic`. Trade-off: the current preserve-behavior also protects the value against an accidental kind toggle → Save; keeping it is defensible — if so, close as won't-fix.

### L3 (low) — PLAN.md Messages-client row not updated with the new header
`PLAN.md:170` — the LLM-client row enumerates the Messages wire protocol as "`/v1/messages`, `x-api-key` + `anthropic-version`, hoisted `system`, content blocks, `max_tokens` required". The new optional `anthropic-workspace-id` attribution header belongs in that parenthetical; per this project's review expectations PLAN.md is part of docs sync. README is already updated.
**Fix:** add e.g. ", optional `anthropic-workspace-id` workspace attribution" to that cell (one phrase).

## Non-finding observations

- The working tree also carries uncommitted files belonging to the *prior* plan (0aeb92cd, 429-stickiness): `.coding/plans/0aeb92cd.md` (+3 lines), untracked `.coding/knowledge/bug/0aeb92cd.md`, and one `.coding/backlog.jsonl` line. They will ride along in the commit. `.coding/` side-car files travel with git by design, so this is fine — just be aware the commit spans two plan contexts (mention 0aeb92cd in the commit body or commit separately).
- New tests are meaningful: each asserts the exact failure mode it guards (toml skip-serialization byte-stability, trim/clear-to-None, header set/absent/whitespace, DTO round-trip).
