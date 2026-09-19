+++
title = "Strict tool schemas + sanitized tool errors — landed design (plan 21118961)"
supersedes = "2027-01-11-strict-tool-schemas-sanitized-tool-errors-code-a"
created = "2027-01-11"
+++

Landed 2027-01-24, plan 21118961, branch wt/mnemo (commits b7a7ba8 + 49008bc), round-2 review PASS.

STRICT TOOL SCHEMAS (capability-gated): src/provider/strict.rs normalizes the 9 mutation/plan tools (STRICT_TOOLS: file_edit, file_write, file_append, convert_line_endings, create_plan, update_plan, complete_step, abandon_plan, finish) to strict-legal form — every property in `required`, `additionalProperties: false`, optionals widened to nullable (compact `type: ["T","null"]` for simple-typed, `anyOf` fallback for complex). ToolRegistry::schemas (src/tool/mod.rs) is the SINGLE decision point: applies normalization + `strict: Some(true)` only when the serving endpoint's Capabilities.supports_strict_schema is true. Per-endpoint `supports_strict_schema: Option<bool>` key in endpoints.toml overrides the kind default (litellm/vertex escape hatch; flows Endpoint → OpenAiClientConfig.strict_schema → capabilities_with_overrides). Both OpenAI request builders defensively strip the flag when caps don't support it (429-fallback guard). Circuit-breaker correction (turn.rs) renders the same normalized schema via strict::apply — advertised array, token estimate, and correction cannot disagree.

RECEIVING-SIDE CONTRACT (review HIGH 1): strict mode forces every key present with null as "no value" — non-Option optional fields MUST pair `#[serde(default)]` with `crate::tool::null_to_default` (null → Default). Applied to file_edit's 6 non-Option optionals, create_plan context/kind, update_plan append; StepInput::Map.body is Option<String>. Any future non-Option optional field on a STRICT_TOOL needs the same treatment (regression tests: strict_mode_shape_all_keys_null_optionals_deserializes in file_edit.rs + plan.rs).

SANITIZED ERRORS: src/tool/error_message.rs::sanitize_arguments_error(tool, &serde_json::Error) rewrites serde failures into instructive messages ("Error: The tool 'X' failed because parameter 'Y' must be an integer, not a string. Please try again with the correct type.") — branches: syntax/EOF, missing field, unknown field, invalid type (type-word mapping, last-anchor split), unknown variant (allowed-set extraction via rfind), untagged-enum, fallback. Never echoes raw serde vocabulary or the arguments blob. Routed through dispatch.rs's malformed-JSON site and all 44 tool arg-parse sites (`self.name()`); read_files' shared invalid_args_error keeps its received-keys/hint context on top.

Budget ceilings raised (factory.rs, dated 2027-01-24): Executing 32_100, PlanFrozen 33_400, Reviewing 27_300. Docs: docs/CONFIGURATION.md (endpoint key), PLAN.md (Capabilities block), ToolSchema.strict doc. Tests: 2445 passed, warning-free.
