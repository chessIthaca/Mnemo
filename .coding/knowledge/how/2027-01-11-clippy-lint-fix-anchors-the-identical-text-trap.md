+++
title = "clippy lint-fix anchors — the identical-text trap and count-math verification"
created = "2027-01-11"
+++

Lesson from the clippy-sweep step-2 chunk (plan efd1f4c3, 2027-01-19): fixing lints by text anchor is unsafe when the flagged site's text is identical to an UNFLAGGED site with different types. Live case: src/agent/turn.rs had `memory_context.as_deref(),` twice — line 726 a REAL Option<String>→Option<&str> conversion (not flagged), line 3539 a no-op (flagged derefed-type-is-same-as-origin). A count:1 anchor op matched 726 first and broke the build (E0308, caught by cargo test). Recovery: restore 726, fix 3539 with context-unique anchors. Verification protocol that worked: count math — 36 targeted emissions vs exactly 36 dropped from the gate's `due to N previous errors` (349→313) proves all targeted sites fixed and zero new lints; head/tail of the short-format gate output confirms which families remain. For the redundant_field_names sweep (93 sites): all textual `field: field,` matches ARE flagged sites (the lint is purely textual), so count:N/replace_all is safe there — but still verify by emission-count math, never trust anchors alone.
