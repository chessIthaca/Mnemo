+++
title = "per-model reasoning-effort override (ModelSpec.reasoning_effort)"
created = "2027-01-06"
+++

Per-model reasoning-effort override (backlog 5b099aef, user-reported 2027-01-05), landed in 86b1c21 on wt/agenticcoding (2027-01-06), review PASS (1 low doc finding fixed + verified).

- `ModelSpec.reasoning_effort` (endpoints.toml `[[endpoint.models]]`) — persisted per-model effort default, settable via the "effort" dropdown on the per-model strip in Settings → Providers (options: "endpoint default" sentinel + max/high/medium/low/minimal/off; hidden for anthropic kind, disabled when !supports_reasoning_effort).
- Resolution: `Endpoint::effective_reasoning_effort_for(model_id)` (src/config/endpoints.rs) resolves model-level first → endpoint's value → app default "max". Off-encoding (DeepSeek "off"→"none", others omitted) and the model's own `reasoning_efforts` allow-list clamping apply to the model-level value. Every consumer picks it up automatically (client_factory build_provider_for, config_io resolve_reasoning_effort, console initial_selection, main.rs build_brain_inner).
- Persistence semantics: rides the DTO (ModelSpecDto/ModelSpecWire, null-emitting) like endpoint-level reasoning_effort — authoritative, NO apply_endpoints carry-over (carry-over is only for non-DTO fields: temperature/stop/etc.); importEndpointEditable maps it so re-import is safe. The status-bar toolbar dropdown stays the runtime override on top.
- Tests at every layer: endpoints.rs resolver tests, settings.rs DTO round-trip, patch.rs apply_endpoints round-trip (incl. unset-clears-override), config_io resolver test, openai/tests.rs wire test, FE types.test.ts round-trips.
