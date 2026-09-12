## Verdict: PASS

The hardening of the `file_edit` and tool-emission pipeline successfully addresses the 2026-12-20 incident. The implementation is robust, follows the five specified hardening asks (H1–H5), and preserves safety in the lexer and trace-tap logic.

### H1: Raw Delivery Tap
- **Race Safety**: The use of `last_record_id` within the provider clients (OpenAI/Anthropic) ensures that delivered tool calls are attributed to the correct trace record even under concurrent agent activity.
- **Verbatim Capture**: The tap in `turn.rs` captures arguments immediately after finalizing the accumulator, ensuring the recorded state is truly "raw" (pre-normalization, pre-bad-JSON filter).
- **Safe Caps**: `trace.rs` implements a per-call 4KiB cap and 32-call count cap. The UTF-8 char-boundary walk (`while !arguments.is_char_boundary(cut)`) correctly prevents panics on truncation.

### H2: Emission Artifact Validation
- **Robust Lexer**: `rust_brace_deficit` in `file_edit.rs` correctly skips nestable comments (`/* */`), string/char literals (handling escapes), and raw strings (`r#"..."#`) with arbitrary hash counts. Lifetimes/labels are properly disambiguated from char literals.
- **Delta Scoping**: The validation only rejects if the brace deficit *grows*, ensuring that files with pre-existing syntax errors are not rejected.
- **Correct Steering**: Artifact rejections use `Error::InvalidInput` and steer toward re-emission/smaller fragments rather than the stale-read drift nudge, which is appropriate as a re-read cannot fix model decay.
- **Exemptions**: Markdown files (.md) and non-code extensions are correctly excluded from the markdown-heading check.

### H3/H4: Freshness Contract & Telemetry
- **Strict Gate**: `EDIT_GATE_THRESHOLD = 1` enforces the machine-checkable freshness contract. A single drift failure now requires a fresh read before the next edit.
- **Telemetry**: `note_edit_gated` in `dispatch.rs` correctly increments the `gated` counter without feeding the `fired` count, providing a clear signal of emission decay versus content drift in the Trace panel.

### H5: Large Payload Advisory
- **Advisory Only**: Payloads >= 700 chars correctly trigger a `NOTE` on success without rejecting the edit, providing a useful nudge for long sessions where emission fragility has been observed.

### Frontend Sanity
- Mechanical field additions in `lib/types.ts` and `LlmTraceView.tsx` correctly surface the `gated` counts and the "Delivered tool calls" section for generation-vs-harness comparison.
