// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Sanitized tool-argument error messages.
//!
//! When a tool's argument deserialization fails, the raw serde error used to
//! be fed back to the model verbatim (`format!("invalid arguments: {e}")`).
//! For a weaker model that is exactly the wrong feedback: the serde Display
//! text is parser-shaped ("invalid type: string \"5\", expected u64"),
//! names no tool, and — for the dispatch-layer JSON-syntax failure — echoed the
//! raw arguments blob back into the conversation. A model that is already
//! struggling with argument shapes then has to reverse-engineer serde's
//! vocabulary before it can correct the call.
//!
//! [`sanitize_arguments_error`] rewrites those errors into the instructive
//! shape the model can act on directly:
//!
//! ```text
//! Error: The tool 'file_edit' failed because parameter 'count' must be
//! an integer, not a string. Please try again with the correct type.
//! ```
//!
//! The serde Display text is a single line (never a multi-line trace), and the
//! shapes are stable serde vocabulary, so the sanitizer parses them with
//! small anchored matchers rather than a regex dependency:
//!
//! - `missing field \`X\`` → names the parameter.
//! - `unknown field \`X\`` → names the parameter.
//! - `invalid type: {actual}, expected {expected}` → maps both to
//!   user-facing type words ([`type_word`], [`actual_word`]).
//! - `invalid value: ...` (enum mismatch) → the allowed-values hint.
//! - anything else (including JSON *syntax* errors, which serde renders
//!   with a position, e.g. "expected value at line 1 column 5") → a
//!   generic fallback that still names the tool and carries serde's own
//!   one-line detail — instructive, and never a stack trace.
//!
//! The raw arguments blob is never echoed in full (it can be huge, and it
//! teaches the model nothing the schema does not already say). The one
//! sanctioned exception (backlog 05a0905f): a bounded ~120-character
//! received-tail around the failure point. The enriched producers
//! ([`sanitize_arguments_json_error`], [`sanitize_arguments_error_with`],
//! [`received_args_context`]) echo exactly what the executor received —
//! byte count, keys, and the tail around the break — because a truncated
//! payload is otherwise blind-retryable: the model believes it sent valid
//! JSON, so a bare rejection brings the identical (re-truncated) call
//! back, one full round-trip per retry.

use serde_json::{error::Category, Error};

/// Rewrite a tool-argument deserialization error into a clean, instructive
/// message the model can act on.
///
/// `tool` is the failing tool's name (each call site passes its own
/// literal); `err` is the `serde_json::Error` from
/// `serde_json::from_value` (or `from_str` at the dispatch layer). The
/// result always starts with `Error: The tool '{tool}' failed because`
/// and always ends with a retry instruction — never the raw serde
/// vocabulary alone, never the raw arguments.
pub fn sanitize_arguments_error(tool: &str, err: &Error) -> String {
    // Syntax/EOF errors (malformed JSON — the dispatch layer's
    // `from_str` path) carry a position and serde's own one-line
    // detail. Name the tool and point at well-formed JSON rather than
    // the schema. (EOF is its own category in serde's taxonomy.)
    if matches!(err.classify(), Category::Syntax | Category::Eof) {
        return format!(
            "Error: The tool '{tool}' failed because its arguments \
             are not valid JSON ({err}). Please retry with a \
             correctly-formed JSON object."
        );
    }
    let raw = err.to_string();
    // `missing field \`X\`` — the parameter name sits between the
    // backticks. serde quotes it with a single backtick pair.
    if let Some(field) = between_ticks(&raw, "missing field") {
        return format!(
            "Error: The tool '{tool}' failed because parameter '{field}' is \
             required but was not provided. Please try again with \
             '{field}' included."
        );
    }
    if let Some(field) = between_ticks(&raw, "unknown field") {
        return format!(
            "Error: The tool '{tool}' failed because '{field}' is not a \
             parameter it accepts. Please check the tool's schema and try \
             again with a supported parameter."
        );
    }
    // `invalid type: {actual}, expected {expected} at ...` — the actual
    // description runs to the first comma; the expected type runs to the
    // position suffix (or the end of the line).
    if let Some(rest) = raw.strip_prefix("invalid type: ") {
        let (actual, expected) = split_actual_expected(rest);
        return format!(
            "Error: The tool '{tool}' failed because an argument must be \
             {}, not {}. Please check each parameter's type against the \
             tool's schema and try again with the correct type.",
            type_word(&expected),
            actual_word(&actual)
        );
    }
    // `unknown variant \`X\`, expected one of \`A\`, \`B\`` (an
    // enum-typed parameter given a non-variant value) — name the
    // allowed values so the model can self-correct in one retry.
    if raw.starts_with("unknown variant ") {
        // serde's expected-set wording varies by count: "expected `A`",
        // "expected `A` or `B`", "expected one of `A`, `B`, `C`" — take
        // everything after "expected ", drop the "one of " prefix and
        // the backticks.
        let allowed = raw
            // The LAST "expected " is the real anchor — a variant VALUE
            // containing the substring (e.g. `expected A`) must not
            // mis-anchor the extraction (review LOW 2).
            .rfind("expected ")
            .map(|i| {
                let tail = &raw[i + "expected ".len()..];
                let tail = tail.strip_prefix("one of ").unwrap_or(tail);
                let tail = tail.split(" at ").next().unwrap_or(tail);
                tail.replace('`', "")
            })
            .unwrap_or_default();
        let detail = if allowed.is_empty() {
            String::new()
        } else {
            format!(" (allowed: {allowed})")
        };
        return format!(
            "Error: The tool '{tool}' failed because an argument has a \
             value outside the allowed set{detail}. Please try again with \
             one of the allowed values."
        );
    }
    // `invalid value: ...` (a validated value) — same remedy, but the
    // allowed set is not recoverable from the Display text.
    if raw.starts_with("invalid value: ") {
        return format!(
            "Error: The tool '{tool}' failed because an argument has a \
             value outside the allowed set. Please check the tool's \
             schema for the permitted values and try again."
        );
    }
    // `data did not match any variant of untagged enum \`X\`` — an
    // item matched none of its allowed forms (e.g. a plan step that
    // is neither a string nor a {header, body} object). Point at the
    // list items rather than echoing serde's vocabulary (review
    // LOW 1).
    if raw.starts_with("data did not match any variant of untagged enum") {
        return format!(
            "Error: The tool '{tool}' failed because an item in a list \
             parameter does not match its allowed forms. Please check each \
             list item against the tool's schema and try again."
        );
    }
    // Fallback: still name the tool, keep serde's own one-line detail
    // (positions are instructive; the Display is single-line, so this
    // is never a stack trace), and point the model at the schema.
    format!(
        "Error: The tool '{tool}' failed because its arguments do not \
         match its schema ({raw}). Please check the tool's schema and \
         try again with correctly-typed arguments."
    )
}

/// The remedy appended to every enriched argument rejection: what to do
/// instead of blind-retrying (backlog 05a0905f — a truncated payload
/// re-sent identically fails identically, one full round-trip per retry).
const TRUNCATION_REMEDY: &str = "re-emit a smaller payload (fewer/shorter \
     items), split the work across multiple calls, or — for file changes — \
     use file_write to write the whole file instead";

/// Received-tail window: up to this many characters before the break,
/// plus [`RECEIVED_TAIL_AFTER`] after it (a mid-payload break shows both
/// sides; an EOF break is all tail).
const RECEIVED_TAIL_CHARS: usize = 120;
const RECEIVED_TAIL_AFTER: usize = 40;

/// Step back to the nearest UTF-8 char boundary at or before `i`.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Step forward to the nearest UTF-8 char boundary at or after `i`.
fn ceil_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Byte offset of `err` inside the raw payload it came from. serde
/// reports a 1-based `line`/`column` (the column counts bytes within the
/// line): walk the payload to that line's start, add the column, clamp
/// to the payload length. An EOF-at-the-end error's column sits at the
/// last byte (1-based column == len, so the offset is len-1); the clamp
/// absorbs line-count and column overshoots.
fn byte_offset(raw: &str, err: &Error) -> usize {
    let mut offset = 0usize;
    let mut rest = raw;
    for _ in 1..err.line().max(1) {
        match rest.find('\n') {
            Some(i) => {
                offset += i + 1;
                rest = &rest[i + 1..];
            }
            // Fewer lines than the error claims: the break is at (or
            // past) the end — clamp.
            None => return raw.len(),
        }
    }
    (offset + err.column().max(1) - 1).min(raw.len())
}

/// The bounded received-tail around the break — the sanctioned diagnostic
/// exception to "never echo the raw arguments": the full blob stays
/// unechoed (huge, mostly noise), the tail shows where the emission was
/// cut. Char-boundary safe, backtick-quoted.
fn received_tail(raw: &str, offset: usize) -> String {
    let start = floor_boundary(raw, offset.saturating_sub(RECEIVED_TAIL_CHARS));
    let end = ceil_boundary(raw, offset.saturating_add(RECEIVED_TAIL_AFTER));
    if start >= end {
        return "`` (nothing was received)".into();
    }
    format!("`{}`", &raw[start..end])
}

/// The tail of the already-serialized received arguments (schema path —
/// the Value a tool received, re-serialized for the byte count).
fn value_tail(serialized: &str) -> String {
    let start =
        floor_boundary(serialized, serialized.len().saturating_sub(RECEIVED_TAIL_CHARS));
    format!("`{}`", &serialized[start..])
}

/// The top-level keys the received arguments actually carried (or the
/// value's kind when it is not an object — so the model sees why the
/// fields it meant to send could not land).
fn received_keys(received: &serde_json::Value) -> String {
    match received.as_object() {
        Some(map) if map.is_empty() => "none — an empty object".into(),
        Some(map) => map
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        None => format!(
            "none — the arguments are {}, not an object",
            match received {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "a boolean",
                serde_json::Value::Number(_) => "a number",
                serde_json::Value::String(_) => "a string",
                serde_json::Value::Array(_) => "an array",
                // The Object arm matched above; the kind list is
                // exhaustive so the value's shape is always named.
                serde_json::Value::Object(_) => "an object",
            }
        ),
    }
}

/// Whether `err` is a serde missing-field error — the one schema shape
/// with truncation ambiguity: a field dropped in emission looks exactly
/// like an omitted one from the model's side.
fn is_missing_field(err: &Error) -> bool {
    between_ticks(&err.to_string(), "missing field").is_some()
}

/// Rewrite the dispatch-layer JSON-parse rejection so it is
/// self-diagnosing: the failure kind (cut off vs malformed), the byte
/// offset, the exact byte count the executor received, a bounded
/// received-tail around the break, and the truncation remedy. Without
/// this the rejection is blind-retryable — the model believes it sent
/// valid JSON, so a bare "not valid JSON" gets the identical
/// (re-truncated) call back (observed: repeated identical rejections on
/// ~2.5-4 KB multi_edit/file_edit payloads, one full round-trip each).
///
/// Successor of [`sanitize_arguments_error`]'s Syntax/EOF arm for the one
/// rejection path that has the raw payload in scope — the `from_str`
/// failure in `execute_tool_call`. The base text is kept verbatim (its
/// pinned tests stay green); the context is appended. The raw blob is
/// still never echoed in full — only the bounded tail is.
pub fn sanitize_arguments_json_error(tool: &str, err: &Error, raw_args: &str) -> String {
    // from_str failures are Syntax or EOF; anything else keeps the plain
    // shape (defensive: a future caller cannot double-enrich).
    if !matches!(err.classify(), Category::Syntax | Category::Eof) {
        return sanitize_arguments_error(tool, err);
    }
    let offset = byte_offset(raw_args, err);
    let what = if matches!(err.classify(), Category::Eof) {
        format!(
            "A payload that ends mid-document like this was cut off in \
             emission, not mis-drafted — re-sending the identical payload \
             will fail identically; {TRUNCATION_REMEDY}"
        )
    } else {
        format!(
            "A break at that byte is malformed JSON; a large payload \
             damaged in emission breaks exactly there — do not re-send it \
             identically; {TRUNCATION_REMEDY}"
        )
    };
    format!(
        "{} The failure is a JSON-parse break at byte {} of the {} bytes \
         received; the received bytes around the break: {}. {}.",
        sanitize_arguments_error(tool, err),
        offset,
        raw_args.len(),
        received_tail(raw_args, offset),
        what
    )
}

/// Enrich a schema-layer argument rejection (a `serde_json::from_value`
/// failure inside a tool's `execute`) with what the executor received:
/// the byte count, the top-level keys, the tail of the received
/// arguments — and, for missing-field failures, the truncation remedy
/// (a model whose large payload was dropped in emission genuinely
/// believes it sent the field, so the base text's "was not provided"
/// reads as false and gets the identical call back). The base text is
/// byte-identical ([`sanitize_arguments_error`]); only context is
/// appended.
pub fn sanitize_arguments_error_with(
    tool: &str,
    err: &Error,
    received: &serde_json::Value,
) -> String {
    let mut msg = format!(
        "{}{}",
        sanitize_arguments_error(tool, err),
        received_args_context(received)
    );
    if is_missing_field(err) {
        msg.push_str(&format!(
            " A required field that is absent on a large call was likely \
             dropped in emission rather than omitted — do not re-send the \
             identical payload; {TRUNCATION_REMEDY}."
        ));
    }
    msg
}

/// The received-arguments context sentence, ready to append to a pinned
/// arg-shape rejection text (the custom validators that fire after
/// deserialization succeeded — e.g. multi_edit's `files is required`).
/// Compute this BEFORE `from_value` consumes the arguments Value, and
/// thread it down to the validator.
pub fn received_args_context(received: &serde_json::Value) -> String {
    let serialized = serde_json::to_string(received).unwrap_or_default();
    format!(
        " The executor received {} bytes of arguments (keys: {}); the \
         received arguments end: {}.",
        serialized.len(),
        received_keys(received),
        value_tail(&serialized)
    )
}

/// The full truncation note for post-parse arg-shape rejections (the
/// custom validators that fire after deserialization succeeded — e.g.
/// multi_edit's `files is required`): the received-arguments context plus
/// the remedy, ready to append to the pinned leading text. Computed in
/// `execute`, where the raw arguments Value is still alive, and threaded
/// down; every other path (approval previews, direct `prepare_edit`
/// callers, tests) passes "" and keeps the historical text unchanged —
/// the bare text is otherwise blind-retryable (backlog 05a0905f: the
/// model believes it sent the payload, so it re-emits the identical call
/// and fails identically, one full round-trip per retry).
pub fn received_truncation_note(received: &serde_json::Value) -> String {
    format!(
        "{ctx} A payload whose content never arrived is almost always \
         emission truncation, not a drafting omission — do not re-send the \
         identical call; {TRUNCATION_REMEDY}.",
        ctx = received_args_context(received)
    )
}

/// Extract the parameter name from a serde Display fragment of the shape
/// `{prefix}\`X\`...` — the text between the first backtick pair after
/// `prefix`. Returns `None` when the shape does not hold (serde changed
/// its wording, or a foreign error text).
fn between_ticks(raw: &str, prefix: &str) -> Option<String> {
    let rest = raw.strip_prefix(prefix)?;
    let start = rest.find('`')? + 1;
    let end = rest[start..].find('`')? + start;
    Some(rest[start..end].to_string())
}

/// Split an `invalid type: {actual}, expected {expected}...` remainder
/// into its actual-description and expected-type words. The actual runs to
/// the first comma (serde renders e.g. `string "5", expected u64`);
/// the expected runs from there to the position suffix or the end.
fn split_actual_expected(rest: &str) -> (String, String) {
    // Split on the LAST ", expected ": the actual VALUE may itself
    // contain the anchor substring (a model-supplied string like
    // "a, expected b"), and the real anchor is the final one
    // (review LOW 2).
    let (actual, expected) = match rest.rfind(", expected ") {
        Some(i) => (rest[..i].trim(), &rest[i + ", expected ".len()..]),
        None => (rest.trim(), ""),
    };
    // Drop serde's position suffix (" at line 1 column 20") from the
    // expected side — a type name never contains " at ", so the first
    // match there is always the suffix.
    let expected = match expected.find(" at ") {
        Some(i) => expected[..i].trim(),
        None => expected.trim(),
    };
    (actual.to_string(), expected.to_string())
}

/// Map a serde EXPECTED type name to the user-facing word for it.
/// Serde names the Rust type: `u64`, `usize`, `i64`, `String`,
/// `bool`, `Vec`, `f64`, `map`... The model thinks in JSON Schema
/// vocabulary, so translate.
fn type_word(expected: &str) -> String {
    match expected {
        "u64" | "u32" | "usize" | "i64" | "i32" | "isize" | "u128"
        | "i128" | "u8" | "i8" | "u16" | "i16" => "an integer".into(),
        "f64" | "f32" => "a number".into(),
        "String" | "str" => "a string".into(),
        "bool" => "a boolean".into(),
        "Vec" | "seq" | "sequence" => "an array".into(),
        "map" | "Map" => "an object".into(),
        "char" => "a single character".into(),
        // A unit (Option::None / ()) deserializes as null.
        "unit" | "()" => "null".into(),
        other => other.to_string(),
    }
}

/// Map the ACTUAL-value word from serde's `invalid type:` description to a
/// user-facing noun phrase. The actual is serde's `Unexpected` display,
/// e.g. `string "5"` — take the leading word.
fn actual_word(actual: &str) -> String {
    let word = actual.split_whitespace().next().unwrap_or(actual);
    match word {
        "string" => "a string".into(),
        "integer" => "an integer".into(),
        "float" => "a number".into(),
        "boolean" => "a boolean".into(),
        "null" => "null".into(),
        "sequence" | "seq" => "an array".into(),
        "map" => "an object".into(),
        "char" => "a character".into(),
        "unit" => "null".into(),
        "byte" | "byte_buf" => "a byte string".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    /// The typed struct the probe errors come from — mirrors how the real
    /// tools deserialize (a `from_value` into a per-tool args struct).
    #[derive(Debug, Deserialize)]
    struct Probe {
        path: String,
        count: u64,
    }

    /// A deny_unknown_fields probe — the source of real "unknown field"
    /// errors (some tools enable the attribute).
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Strict {
        path: String,
    }

    /// An enum-typed probe — the source of real "unknown variant" errors.
    #[derive(Debug, Deserialize)]
    enum Mode {
        A,
        B,
    }

    #[derive(Debug, Deserialize)]
    struct WithEnum {
        mode: Mode,
    }

    /// Generate a REAL serde error (not a hand-typed Display string) so
    /// the matchers are pinned against serde's actual vocabulary.
    fn real_error(value: serde_json::Value) -> Error {
        serde_json::from_value::<Probe>(value)
            .expect_err("probe value must fail to deserialize")
    }

    #[test]
    fn probes_deserialize_valid_input() {
        // Sanity: the probes deserialize cleanly on valid input. This also
        // READS every field, keeping the dead-code lint honest — the
        // error cases below are then provably error cases, not a broken
        // probe.
        let ok = serde_json::from_value::<Probe>(json!({"path": "a", "count": 5}))
            .expect("valid input must deserialize");
        assert_eq!(ok.path, "a");
        assert_eq!(ok.count, 5);

        let ok = serde_json::from_value::<Strict>(json!({"path": "a"}))
            .expect("valid input must deserialize");
        assert_eq!(ok.path, "a");

        let ok = serde_json::from_value::<WithEnum>(json!({"mode": "A"}))
            .expect("valid input must deserialize");
        assert!(matches!(ok.mode, Mode::A));
    }

    #[test]
    fn missing_field_names_the_parameter() {
        let err = real_error(json!({"count": 5}));
        let msg = sanitize_arguments_error("file_edit", &err);
        assert_eq!(
            msg,
            "Error: The tool 'file_edit' failed because parameter 'path' \
             is required but was not provided. Please try again with \
             'path' included."
        );
    }

    #[test]
    fn unknown_field_names_the_parameter() {
        let err = serde_json::from_value::<Strict>(json!({"path": "a", "foo": 1}))
            .expect_err("extra key must fail under deny_unknown_fields");
        let msg = sanitize_arguments_error("file_edit", &err);
        assert_eq!(
            msg,
            "Error: The tool 'file_edit' failed because 'foo' is not a \
             parameter it accepts. Please check the tool's schema and try \
             again with a supported parameter."
        );
    }

    #[test]
    fn invalid_type_maps_serde_types_to_user_words() {
        let err = real_error(json!({"path": "a", "count": "5"}));
        let msg = sanitize_arguments_error("file_edit", &err);
        assert_eq!(
            msg,
            "Error: The tool 'file_edit' failed because an argument must \
             be an integer, not a string. Please check each parameter's \
             type against the tool's schema and try again with the \
             correct type."
        );
    }

    #[test]
    fn invalid_type_boolean_actual() {
        let err = real_error(json!({"path": "a", "count": true}));
        let msg = sanitize_arguments_error("complete_step", &err);
        assert!(
            msg.contains("must be an integer, not a boolean"),
            "boolean actual must map to 'a boolean': {msg}"
        );
    }

    #[test]
    fn unknown_variant_gets_the_allowed_set_hint() {
        let err = serde_json::from_value::<WithEnum>(json!({"mode": "x"}))
            .expect_err("non-variant value must fail");
        let msg = sanitize_arguments_error("convert_line_endings", &err);
        assert_eq!(
            msg,
            "Error: The tool 'convert_line_endings' failed because an \
             argument has a value outside the allowed set (allowed: A or B). \
             Please try again with one of the allowed values."
        );
    }

    #[test]
    fn syntax_error_names_tool_and_points_at_well_formed_json() {
        // The dispatch-layer shape: a from_str syntax error carries a
        // position. The message keeps serde's one-line detail (never a
        // trace) and names the tool. The exact position wording is
        // serde's own, so assert the stable prefix/suffix only.
        let err = serde_json::from_str::<Probe>("{")
            .expect_err("truncated JSON must fail to parse");
        let msg = sanitize_arguments_error("file_write", &err);
        assert!(
            msg.starts_with(
                "Error: The tool 'file_write' failed because its arguments \
                 are not valid JSON (EOF while parsing"
            ),
            "syntax branch must name the tool and carry the one-line \
             detail: {msg}"
        );
        assert!(msg.ends_with(
            "Please retry with a correctly-formed JSON object."
        ));
    }

    #[test]
    fn sanitized_output_never_carries_raw_serde_vocabulary() {
        // The contract the sanitizer exists for: whatever the input
        // shape, the output must read as an instruction — the raw
        // serde phrases ("invalid type:", "expected u64") must not
        // leak through as the leading text.
        let cases = vec![
            real_error(json!({"count": 5})),
            real_error(json!({"path": "a", "count": "5"})),
            serde_json::from_str::<Probe>("{").expect_err("must fail"),
        ];
        for err in cases {
            let msg = sanitize_arguments_error("file_edit", &err);
            assert!(msg.starts_with("Error: The tool 'file_edit' failed"));
            assert!(!msg.contains("invalid type:"));
            assert!(!msg.contains("missing field"));
            assert!(!msg.contains("expected u64"));
        }
    }

    #[test]
    fn untagged_enum_mismatch_points_at_list_items() {
        // Review LOW 1: an item matching none of an untagged enum's
        // forms must not fall through to the raw-vocabulary fallback —
        // it names the list items instead.
        #[derive(Debug, Deserialize)]
        #[serde(untagged)]
        enum Item {
            Text(String),
        }
        // Sanity (also reads the field, keeping the dead-code lint
        // honest): a string DOES match the variant.
        let ok = serde_json::from_value::<Vec<Item>>(serde_json::json!(["x"]))
            .expect("a string matches the Text variant");
        assert!(matches!(ok.as_slice(), [Item::Text(s)] if s == "x"));
        let err = serde_json::from_value::<Vec<Item>>(serde_json::json!([42]))
            .expect_err("an integer matches no untagged variant");
        let msg = sanitize_arguments_error("create_plan", &err);
        assert_eq!(
            msg,
            "Error: The tool 'create_plan' failed because an item in a \
             list parameter does not match its allowed forms. Please check \
             each list item against the tool's schema and try again."
        );
    }

    #[test]
    fn anchor_substrings_inside_values_do_not_mis_split() {
        // Review LOW 2: a model-supplied value containing the anchor
        // substring must not cut the actual description — the split
        // takes the LAST ", expected ".
        let err = serde_json::from_value::<Probe>(serde_json::json!({
            "path": "ok",
            "count": "a, expected b"
        }))
        .expect_err("a string in an integer field must fail");
        let msg = sanitize_arguments_error("file_edit", &err);
        assert!(
            msg.contains("must be an integer, not a string"),
            "the anchor-in-value must not leak raw serde text: {msg}"
        );
    }

    #[test]
    fn anchor_substring_inside_variant_name_does_not_mis_extract() {
        // Review LOW 2: a variant value containing "expected " must not
        // mis-anchor the allowed-set extraction — the real anchor is the
        // last one.
        let err =
            serde_json::from_value::<WithEnum>(serde_json::json!({"mode": "expected A"}))
                .expect_err("non-variant value must fail");
        let msg = sanitize_arguments_error("convert_line_endings", &err);
        assert!(
            msg.contains("(allowed: A or B)"),
            "the allowed set must come from the real anchor: {msg}"
        );
    }

    #[test]
    fn truncated_json_mid_string_names_kind_offset_bytes_tail_and_remedy() {
        // Backlog 05a0905f regression: a payload cut mid-string used to
        // reject with only "not valid JSON (EOF while parsing a string at
        // line 1 column 69)" — no byte count, no tail, nothing showing
        // WHERE the emission stopped, so the model re-sent the identical
        // (re-truncated) call, one full round-trip per retry.
        let raw = r#"{"files":[{"path":"src/tool/agent/multi_edit.rs","ops":[{"i":"b","pay"#;
        let err = serde_json::from_str::<serde_json::Value>(raw)
            .expect_err("payload cut mid-string must fail to parse");
        let msg = sanitize_arguments_json_error("multi_edit", &err, raw);
        // (1) the failure kind + (2) the byte offset and byte count — an
        // EOF-at-the-end break sits at the LAST received byte (serde's
        // 1-based column == len, so the 0-based offset is len-1):
        assert!(
            msg.contains(&format!(
                "JSON-parse break at byte {} of the {} bytes received",
                raw.len() - 1,
                raw.len()
            )),
            "{msg}"
        );
        // (2) the received tail shows the literal cut point:
        assert!(msg.contains(r#""pay"#), "the tail must show the cut: {msg}");
        assert!(msg.contains("cut off in emission"), "{msg}");
        // (4) the remedy line:
        assert!(msg.contains("re-emit a smaller payload"), "{msg}");
        assert!(msg.contains("use file_write"), "{msg}");
        // The pinned base opening is kept:
        assert!(
            msg.starts_with("Error: The tool 'multi_edit' failed"),
            "{msg}"
        );
    }

    #[test]
    fn truncated_json_mid_brace_names_kind_offset_bytes_tail_and_remedy() {
        // The mid-BRACE cut: EOF while parsing an object — the received
        // tail ends at the opening brace the emission never closed.
        let raw = r#"{"files":[{"path":"src/x.rs","ops":[{"#;
        let err = serde_json::from_str::<serde_json::Value>(raw)
            .expect_err("payload cut mid-brace must fail to parse");
        let msg = sanitize_arguments_json_error("multi_edit", &err, raw);
        assert!(
            msg.contains(&format!(
                "JSON-parse break at byte {} of the {} bytes received",
                raw.len() - 1,
                raw.len()
            )),
            "{msg}"
        );
        assert!(msg.contains(r#"ops":[{"#), "the tail must show the cut: {msg}");
        assert!(msg.contains("cut off in emission"), "{msg}");
        assert!(msg.contains("use file_write"), "{msg}");
    }

    #[test]
    fn malformed_json_mid_payload_marks_the_break_byte() {
        // A mid-payload SYNTAX break (not EOF): the kind wording switches
        // to "malformed JSON" while the offset/bytes/tail/remedy stay —
        // the failure kind is part of the diagnostic.
        let raw = r#"{"files": [{path: "src/x.rs"}]}"#;
        let err = serde_json::from_str::<serde_json::Value>(raw)
            .expect_err("unquoted key must fail to parse");
        let msg = sanitize_arguments_json_error("multi_edit", &err, raw);
        assert!(msg.contains("malformed JSON"), "{msg}");
        assert!(msg.contains("bytes received"), "{msg}");
        assert!(msg.contains("received bytes around the break"), "{msg}");
        assert!(msg.contains("do not re-send it identically"), "{msg}");
        assert!(msg.contains("re-emit a smaller payload"), "{msg}");
    }

    #[test]
    fn schema_missing_field_error_carries_received_bytes_and_remedy() {
        // Backlog 05a0905f regression: the missing-field arm used to end
        // at "was not provided" — on a truncated call the model believes
        // it sent the field, so that text invites the identical retry.
        // The with-variant appends the received byte count, keys, tail,
        // and the truncation remedy (gated to missing-field errors).
        let received = json!({"count": 5});
        let err = real_error(received.clone());
        let msg = sanitize_arguments_error_with("file_edit", &err, &received);
        assert!(
            msg.contains("parameter 'path' is required but was not provided"),
            "{msg}"
        );
        // The received args serialize to exactly `{"count":5}`:
        assert!(msg.contains("received 11 bytes"), "{msg}");
        assert!(msg.contains("keys: count"), "{msg}");
        assert!(
            msg.contains("received arguments end: `{\"count\":5}`"),
            "{msg}"
        );
        assert!(msg.contains("dropped in emission"), "{msg}");
        assert!(msg.contains("re-emit a smaller payload"), "{msg}");
    }

    #[test]
    fn schema_type_error_gets_context_without_truncation_remedy() {
        // Non-missing-field schema failures (a type error) get the
        // received context but NOT the truncation remedy — a wrong type
        // is a drafting error, not emission decay.
        let received = json!({"path": "a.rs", "count": "5"});
        let err = real_error(received.clone());
        let msg = sanitize_arguments_error_with("file_edit", &err, &received);
        assert!(msg.contains("bytes of arguments"), "{msg}");
        assert!(!msg.contains("dropped in emission"), "{msg}");
    }
}
