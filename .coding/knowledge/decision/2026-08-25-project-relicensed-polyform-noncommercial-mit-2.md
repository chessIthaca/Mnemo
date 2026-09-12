+++
title = "project relicensed PolyForm Noncommercial → MIT"
supersedes = "2026-08-25-project-relicensed-polyform-noncommercial-mit"
created = "2026-08-25"
+++

DECISION (2026-08-25): Mnemo is relicensed from PolyForm Noncommercial 1.0.0 to MIT. Rationale: MIT is permissive, compatible with all direct deps (all MIT/Apache-2.0/ISC/CC0), and the project is single-copyright-holder (Carsten Hess), so no external-consent barrier exists. Applied across: LICENSE file, both Cargo.tomls (`license = "MIT"`), both package.jsons, ~268 source-file copyright headers (SPDX `MIT`, dropped "(non-commercial use only)"), add-copyright-headers.ps1, About dialog (APP_LICENSE_URL → opensource.org/licenses/MIT, body text), README. MERGED into main at b345340 on 2026-08-25 (via develop). The About dialog's dependency manifest was also completed: 7 missing direct deps added (notify=CC0-1.0, image=MIT OR Apache-2.0, tree-sitter/tree-sitter-rust/tree-sitter-typescript=MIT, d3-force=ISC, @types/d3-force=MIT) + a CC0 license constant.
