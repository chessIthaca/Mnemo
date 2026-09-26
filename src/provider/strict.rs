// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Strict-schema normalization for OpenAI-compatible providers.
//!
//! OpenAI's strict structured-output mode (`"strict": true` on a function
//! tool) makes the provider CONSTRAIN decoding to the declared schema. That
//! constraint is a large reliability win for the tools models most often
//! malform — but it comes with requirements the schemas in this repo do not
//! naturally satisfy:
//!
//! - every object must set `additionalProperties: false`;
//! - every property of every object must appear in `required`.
//!
//! The tools here are deliberately FORGIVING, so a naive `strict: true` would
//! break the very calls it is meant to fix:
//!
//! - `git` (`src/tool/agent/git.rs`) requires only `subcommand` and accepts
//!   `message`/`branch`/`action` interchangeably;
//! - `complete_step` accepts an integer OR a numeric string for `step_index`;
//! - `file_edit` has several addressing modes (exact / regex / line-range /
//!   fuzzy) whose fields are mutually exclusive in practice.
//!
//! [`normalize_for_strict`] bridges the two: it rewrites a parameters schema
//! into the shape strict mode demands while KEEPING the optional-ness the
//! tools rely on. The trick is that strict mode governs the WIRE SHAPE, not
//! the values: a property that was optional becomes REQUIRED-but-NULLABLE
//! (the compact `type: ["T", "null"]` form for simple-typed properties,
//! `anyOf: [{...}, {"type": "null"}]` for complex ones), so the provider
//! still emits the key on every call and simply sends `null` when the model
//! has nothing to put there. The RECEIVING side must then accept `null` for
//! those keys: `Option<T>` fields do natively, and the non-`Option`
//! optional fields pair `#[serde(default)]` with
//! [`null_to_default`](crate::tool::null_to_default) so `null` maps to the
//! same default absence did (review HIGH 1) — a bare `#[serde(default)]`
//! fires on absence only and would reject the very `null` strict mode
//! forces.
//!
//! Normalization is applied only when the serving endpoint advertises
//! [`Capabilities::supports_strict_schema`](crate::provider::Capabilities) —
//! litellm/vertex reject the `strict` field outright (see the note in
//! `src/provider/openai/request.rs`), so the gate is what keeps a
//! strict-capable endpoint working without breaking the others.

use serde_json::{json, Value};

/// Rewrite a tool's `parameters` schema into a strict-legal one, preserving
/// the acceptance the tool's own deserializer expects.
///
/// The rewrite is recursive and idempotent. For every object schema it:
///
/// 1. records which property names were OPTIONAL (absent from `required`);
/// 2. sets `additionalProperties: false`;
/// 3. widens each optional property to `anyOf: [<original>, {"type": "null"}]`;
/// 4. lists EVERY property name in `required` (strict mode demands the full
///    set; step 3 is what makes that safe).
///
/// Recursion descends into `properties`, `items`, and each `anyOf`/`oneOf`/
/// `allOf` branch, so nested objects (e.g. `create_plan`'s `steps[]` items)
/// are normalized too.
///
/// A non-object schema (or one with no `properties`) is returned with
/// `additionalProperties: false` added when it is an object type — strict
/// mode requires the key on every object, including an empty one.
///
/// Idempotency: an already-normalized schema carries `additionalProperties:
/// false` and a `required` list equal to its property set, so a second pass
/// finds no optional properties to widen and rewrites nothing.
///
/// ```ignore
/// // {properties: {a: {type: "string"}}, required: []}
/// //   -> {properties: {a: {anyOf: [{type: "string"}, {type: "null"}]}},
/// //       required: ["a"], additionalProperties: false}
/// ```
pub fn normalize_for_strict(schema: &Value) -> Value {
    let mut out = schema.clone();
    normalize_in_place(&mut out);
    out
}

/// The recursive worker behind [`normalize_for_strict`].
fn normalize_in_place(node: &mut Value) {
    // Recurse into the composition keywords first, so a schema that is only a
    // union of branches (no top-level `properties`) still has its branches
    // normalized.
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(branches)) = node.get_mut(key) {
            for branch in branches.iter_mut() {
                normalize_in_place(branch);
            }
        }
    }
    if let Some(items) = node.get_mut("items") {
        // `items` may be a single schema or (draft-04 style) an array of them.
        match items {
            Value::Array(schemas) => {
                for s in schemas.iter_mut() {
                    normalize_in_place(s);
                }
            }
            other => normalize_in_place(other),
        }
    }
    // Recurse into each property's own schema, so nested objects (and their
    // items/unions) are normalized too — without this, only the top-level
    // object gets the treatment and `steps[].body` stays optional.
    if let Some(Value::Object(props)) = node.get_mut("properties") {
        for (_name, prop) in props.iter_mut() {
            normalize_in_place(prop);
        }
    }

    let Some(obj) = node.as_object_mut() else {
        return;
    };

    // Only an object-typed schema gets the object treatment. A schema with
    // `properties` but no explicit `type` is treated as an object (JSON Schema
    // infers it, and every tool schema here declares `type: "object"`
    // explicitly anyway).
    let is_object = obj.get("type").and_then(|t| t.as_str()) == Some("object")
        || obj.contains_key("properties");
    if !is_object {
        return;
    }

    // Which names were optional? (`required` may be absent entirely.)
    let previously_required: Vec<String> = obj
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Widen each optional property to also accept null. Sorted iteration keeps
    // the output byte-stable for a given input, which the prefix-cache
    // constraint on the advertised tools array depends on.
    if let Some(Value::Object(props)) = obj.get_mut("properties") {
        let mut names: Vec<String> = props.keys().cloned().collect();
        names.sort();
        for name in names {
            let was_optional = !previously_required.iter().any(|r| r == &name);
            if !was_optional {
                continue;
            }
            let Some(prop) = props.get_mut(&name) else {
                continue;
            };
            // Already nullable (a previous pass, or hand-written) — leave it.
            if is_nullable(prop) {
                continue;
            }
            // Compact nullability: a property that declares a simple
            // string `type` widens to the type-array form
            // (`["string", "null"]`) — the OpenAI-documented nullable
            // shape, and far cheaper in schema tokens than an anyOf
            // wrapper (the tools array has a measured char budget; see
            // factory's tools_array_stays_within_context_budget).
            // Complex schemas (enum unions, no declared type) fall
            // back to the anyOf wrapper.
            if let Some(t) = prop
                .get("type")
                .and_then(|v| v.as_str())
                .map(String::from)
            {
                let mut widened = prop.take();
                widened["type"] = json!([t, "null"]);
                *prop = widened;
            } else {
                let original = prop.take();
                *prop = json!({
                    "anyOf": [original, { "type": "null" }]
                });
            }
        }
    }

    // Strict mode: every property required, and no additional properties.
    let all_names: Vec<String> = obj
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default();
    obj.insert(
        "required".to_string(),
        Value::Array(all_names.into_iter().map(Value::String).collect()),
    );
    obj.insert("additionalProperties".to_string(), Value::Bool(false));
}

/// Whether a property schema already accepts `null` — either a literal
/// `type: "null"`, or a union with a null branch. Used to keep
/// [`normalize_for_strict`] idempotent.
fn is_nullable(schema: &Value) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    match obj.get("type") {
        // `type: ["string", "null"]` (draft-07 array form).
        Some(Value::Array(types)) => types.iter().any(|t| t.as_str() == Some("null")),
        Some(Value::String(t)) if t == "null" => true,
        _ => {
            // A union with a null branch.
            ["anyOf", "oneOf"].iter().any(|key| {
                obj.get(*key)
                    .and_then(|u| u.as_array())
                    .map(|branches| branches.iter().any(is_nullable))
                    .unwrap_or(false)
            })
        }
    }
}

/// Return `schema` unchanged (no `strict` normalization) — the identity
/// counterpart of [`normalize_for_strict`], used for endpoints whose
/// capabilities do not include strict schema support.
///
/// Kept as an explicit function so the caller reads as a two-branch decision
/// rather than a bare clone, and so the intent ("do NOT normalize") is
/// greppable.
pub fn pass_through(schema: &Value) -> Value {
    schema.clone()
}

/// Build the schema map used for a tool's `parameters` field: normalized when
/// the endpoint supports strict schemas, verbatim otherwise.
///
/// This is the single decision point — call it wherever the advertised tools
/// array is assembled, so the strict/non-strict choice cannot drift between
/// the request builder and any other consumer of the schemas.
pub fn parameters_for(strict_supported: bool, schema: &Value) -> Value {
    if strict_supported {
        normalize_for_strict(schema)
    } else {
        pass_through(schema)
    }
}

/// The tool names whose schemas request strict enforcement when the endpoint
/// supports it.
///
/// Scope is deliberate: these are the tools whose malformed calls are the
/// observed failure mode (a dropped `old_string`, a missing `message`, a
/// mistyped `step_index`). Read-only tools are excluded — constraining them
/// buys nothing, and each normalized schema costs tokens on every request.
pub const STRICT_TOOLS: &[&str] = &[
    "file_edit",
    "multi_edit",
    "file_write",
    "file_append",
    "convert_line_endings",
    "create_plan",
    "update_plan",
    "complete_step",
    "abandon_plan",
    "finish",
];

/// Whether `name` is one of the [`STRICT_TOOLS`].
pub fn wants_strict(name: &str) -> bool {
    STRICT_TOOLS.contains(&name)
}

/// Apply strict-mode normalization to a tool schema when BOTH conditions hold:
/// the tool is a [`STRICT_TOOLS`] member AND the endpoint advertises
/// `supports_strict_schema`. Returns the schema unchanged otherwise.
///
/// Sets `strict: Some(true)` alongside the normalized parameters, so the
/// request builder emits the field. The two must move together: a `strict`
/// flag on an un-normalized schema is exactly the 400 that litellm/vertex (and
/// strict-capable providers) reject.
pub fn apply(
    schema: &mut crate::provider::ToolSchema,
    strict_supported: bool,
) {
    if !strict_supported || !wants_strict(&schema.name) {
        return;
    }
    schema.parameters = normalize_for_strict(&schema.parameters);
    schema.strict = Some(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_strict_schema_is_unchanged() {
        // A schema that is already strict-legal must survive a pass
        // byte-identical — otherwise every request would re-normalize (and
        // re-bill) a schema that was fine, and the advertised array would not
        // be stable across turns.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "p"}
            },
            "required": ["path"],
            "additionalProperties": false
        });
        assert_eq!(normalize_for_strict(&schema), schema);
    }

    #[test]
    fn optional_property_becomes_required_and_nullable() {
        // The core transformation: an optional property must stay ABSENT-able
        // for the tool, while strict mode demands it in `required`. Nullable
        // is how both hold at once.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "count": {"type": "integer"}
            },
            "required": ["path"]
        });
        let out = normalize_for_strict(&schema);
        // Both keys are required now…
        let required = out["required"].as_array().unwrap();
        assert_eq!(required.len(), 2);
        assert!(required.iter().any(|v| v == "path"));
        assert!(required.iter().any(|v| v == "count"));
        // …and the previously-optional one accepts null (the compact
        // type-array form), keeping the original schema verbatim
        // otherwise.
        assert_eq!(
            out["properties"]["count"],
            json!({"type": ["integer", "null"]})
        );
        // The already-required one is untouched.
        assert_eq!(out["properties"]["path"], json!({"type": "string"}));
        // additionalProperties is forced false.
        assert_eq!(out["additionalProperties"], json!(false));
    }

    #[test]
    fn additional_properties_added_when_absent() {
        let schema = json!({
            "type": "object",
            "properties": {}
        });
        let out = normalize_for_strict(&schema);
        assert_eq!(out["additionalProperties"], json!(false));
        assert_eq!(out["required"], json!([]));
    }

    #[test]
    fn normalization_recurses_into_nested_objects_and_array_items() {
        // create_plan's `steps` is an array of objects — its items need the
        // same treatment, or strict mode rejects the nested schema.
        let schema = json!({
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": {"type": "string"},
                            "body": {"type": "string"}
                        },
                        "required": ["header"]
                    }
                }
            },
            "required": ["steps"]
        });
        let out = normalize_for_strict(&schema);
        let item = &out["properties"]["steps"]["items"];
        assert_eq!(item["additionalProperties"], json!(false));
        assert_eq!(item["required"].as_array().unwrap().len(), 2);
        assert_eq!(
            item["properties"]["body"],
            json!({"type": ["string", "null"]})
        );
    }

    #[test]
    fn normalization_is_idempotent() {
        // Running the pass twice must equal running it once — the advertised
        // tools array has to be byte-stable, and a double-wrapped
        // `anyOf: [{anyOf: [...]}, null]` would both grow the schema and
        // change its bytes between turns.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "mode": {"type": "string", "enum": ["a", "b"]}
            },
            "required": []
        });
        let once = normalize_for_strict(&schema);
        let twice = normalize_for_strict(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn enum_survives_widening() {
        // The widened schema must carry the original constraints — an
        // `enum` lost in normalization would silently drop the value
        // constraint that makes the tool's validation errors impossible
        // in the first place.
        let schema = json!({
            "type": "object",
            "properties": {
                "level": {"type": "string", "enum": ["a", "b"], "description": "d"}
            }
        });
        let out = normalize_for_strict(&schema);
        let level = &out["properties"]["level"];
        assert_eq!(level["type"], json!(["string", "null"]));
        assert_eq!(level["enum"], json!(["a", "b"]));
        assert_eq!(level["description"], json!("d"));
    }

    #[test]
    fn complex_property_falls_back_to_anyof_wrapping() {
        // A property with no simple `type` (an enum union, a bare oneOf)
        // cannot take the type-array shortcut — it wraps in anyOf with
        // the original schema verbatim inside.
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": {"enum": ["a", "b"]}
            }
        });
        let out = normalize_for_strict(&schema);
        assert_eq!(
            out["properties"]["mode"],
            json!({"anyOf": [{"enum": ["a", "b"]}, {"type": "null"}]})
        );
    }

    #[test]
    fn non_object_schema_is_left_alone() {
        // A bare string schema (no object shape) has no properties to require
        // and must not gain an `additionalProperties` key.
        let schema = json!({"type": "string"});
        assert_eq!(normalize_for_strict(&schema), schema);
    }

    #[test]
    fn parameters_for_passes_through_when_strict_unsupported() {
        // The gate: an endpoint without strict support (litellm/vertex) must
        // receive the ORIGINAL schema bytes — normalizing there would change
        // the request for no benefit and break the prefix cache.
        let schema = json!({
            "type": "object",
            "properties": {"path": {"type": "string"}}
        });
        assert_eq!(parameters_for(false, &schema), schema);
        // …and normalizes when supported.
        let normalized = parameters_for(true, &schema);
        assert_ne!(normalized, schema);
        assert_eq!(normalized["additionalProperties"], json!(false));
    }

    #[test]
    fn apply_sets_strict_only_for_listed_tools_on_supporting_endpoints() {
        use crate::provider::ToolSchema;

        let params = json!({
            "type": "object",
            "properties": {"path": {"type": "string"}}
        });

        // A listed tool on a supporting endpoint: normalized + strict flag.
        let mut listed = ToolSchema::new("file_edit", "edit", params.clone());
        apply(&mut listed, true);
        assert_eq!(listed.strict, Some(true));
        assert_eq!(listed.parameters["additionalProperties"], json!(false));

        // A listed tool on an UNSUPPORTING endpoint: untouched.
        let mut unsupported = ToolSchema::new("file_edit", "edit", params.clone());
        apply(&mut unsupported, false);
        assert_eq!(unsupported.strict, None);
        assert_eq!(unsupported.parameters, params);

        // An unlisted tool on a supporting endpoint: untouched (read-only
        // tools are deliberately out of scope).
        let mut unlisted = ToolSchema::new("file_read", "read", params.clone());
        apply(&mut unlisted, true);
        assert_eq!(unlisted.strict, None);
        assert_eq!(unlisted.parameters, params);
    }

    #[test]
    fn strict_tools_cover_the_mutation_and_plan_surface() {
        // Guard against a rename silently dropping a tool out of scope: the
        // set must name the file-mutation and plan tools explicitly.
        for name in [
            "file_edit",
            "multi_edit",
            "file_write",
            "file_append",
            "create_plan",
            "update_plan",
            "complete_step",
            "finish",
        ] {
            assert!(wants_strict(name), "{name} must be in STRICT_TOOLS");
        }
        // Read-only tools stay out of scope.
        assert!(!wants_strict("file_read"));
        assert!(!wants_strict("search"));
    }

    #[test]
    fn draft07_type_array_null_is_recognized_as_nullable() {
        // `type: ["string", "null"]` is already nullable — wrapping it would
        // double-wrap on the second pass and break idempotency.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": ["string", "null"]}
            }
        });
        let once = normalize_for_strict(&schema);
        assert_eq!(once["properties"]["path"], json!({"type": ["string", "null"]}));
        assert_eq!(once, normalize_for_strict(&once));
    }
}
