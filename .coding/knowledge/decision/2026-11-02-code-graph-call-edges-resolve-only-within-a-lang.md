+++
title = "Code-graph call edges resolve only within a language family, and method-call syntax only to methods"
created = "2026-11-02"
+++

The code graph's call resolver (src/codegraph/store.rs rebuild_edges → resolve, mirrored by extract.rs resolve_one) links a call to a definition only when both are in the same language family (lang_family: Ts/Tsx/Js/Html together, C/Cpp together, every other language alone) and, for receiver calls (`x.f(...)`, recorded as RefKind::MethodCall / persisted kind "method_call"), only to SymbolKind::Method targets; anything else stays unresolved rather than guessed. Why: bare-name matching credited a two-line Rust test helper `join` with 1489 callers from Python, TS and `.join(` method calls, making it a top "most-connected" symbol. Rust functions inside impl/trait blocks are Method, so Rust method calls still resolve. schema::EXTRACTOR_VERSION plus cg_extract_meta per-file stamps force one re-parse of old files after an extractor change. Also: Mnemo2's graph neighbourhood always keeps its centre node first under the 150-node cap, and the id-taking graph commands refresh the index once before reporting an unknown symbol.
