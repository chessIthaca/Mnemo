+++
title = "Forking the built-in OpenSpec schema to disk needs glob-template normalization"
created = "2026-11-05"
+++

OpenSpec `Schema::load` (src/app/openspec/schema.rs) reads each artifact's template as ONE literal file at templates/<template>. The built-in spec-driven schema's `specs` artifact names a GLOB path (`specs/**/*.md`), so the built-in can never be materialized to disk verbatim: Windows forbids `*` in file names, and even on Unix a literal `**`/`*.md` folder would have to exist for load to pass. Any code that writes a built-in fork to disk must normalize glob template paths to a literal file name — the rule implemented in `src-tauri/src/mnemo2/specs_settings.rs::literal_template_name` (artifact id + the glob's extension, e.g. `specs/**/*.md` → `specs.md`), rewriting both the artifact's `template` field in the written schema.yaml and the file it points at. File-based fork sources are copied verbatim (they already passed load on the platform that created them).
