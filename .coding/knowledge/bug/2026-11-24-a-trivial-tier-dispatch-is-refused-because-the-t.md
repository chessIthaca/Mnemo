+++
title = "a trivial-tier dispatch is refused because the tier's default effort does not fit its first model"
created = "2026-11-24"
evidence = "team_dispatch error on 2026-11-24 for a trivial-tier step; the endpoints.toml entry for \"llama.cpp local\" has supports_reasoning_effort = false; the tier listing shows \"trivial ...; effort low\"."
+++

Symptom (2026-11-24): `team_dispatch` with `tier: "trivial"` and no reasoning_effort failed with "'llama.cpp local/inkling-2-mini' does not support a reasoning effort; omit reasoning_effort". The trivial tier sets a default effort (low) and its first model is the local llama.cpp endpoint, which has supports_reasoning_effort = false in ~/.mnemo/endpoints.toml. The tier default is applied as if the caller had passed it, and the model check then refuses the whole dispatch. Expected: a tier's default effort applies only to models that support it, and is dropped silently for the others (or the next model in the tier is tried). A caller's explicit effort may still be refused. Workaround: pass `model` (e.g. deepseek/deepseek-flash) instead of the tier.
