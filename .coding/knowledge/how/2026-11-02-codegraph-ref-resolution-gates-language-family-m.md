+++
title = "codegraph ref resolution gates — language family + method-call-only-to-Method"
created = "2026-11-02"
+++

Codegraph bare-name ref resolution has two target gates (since the `join` incident — one Rust test helper was credited with every `.join(` call site in the repo across Python/TS/JS). Gate 1: a ref only ever resolves within its own language family — `Lang::family()` (src/codegraph/walk.rs) folds Ts/Tsx/Js/Html into the Js family and C/Cpp into C; every other language is isolated, so a Python `.join(` can never reach a Rust `fn join`. Gate 2: a method-call ref (`x.join(...)` — receiver syntax, recorded as `RefKind::MethodCall` by every language walk; bare callees and `new`/path calls stay `RefKind::Call`) only resolves to a Method-kind symbol, never a free fn; unresolvable refs stay unresolved. The gates live in BOTH resolvers and must stay in sync: `resolve_one` in src/codegraph/extract.rs (tests/in-memory) and the private `resolve` in src/codegraph/store.rs `rebuild_edges` (the live persisted path). `cg_refs.kind` persists the shape as "call"/"method_call"/"import" — no schema break; rows from older extractors read as "call" until re-parsed.
