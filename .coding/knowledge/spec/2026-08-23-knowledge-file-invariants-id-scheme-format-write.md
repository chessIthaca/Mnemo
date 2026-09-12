+++
title = "knowledge-file invariants (id scheme, format, writer)"
created = "2026-08-23"
+++

SPEC: knowledge-file invariants (impl): source_key = knowledge:<type>/<slug> WITHOUT .md; knowledge_id(rel) strips .md so tool-reported id == row id. Front matter: title/status(live|superseded)/supersedes/created. Files: .coding/knowledge/{spec,decision,bug,how}/<date>-<slug>.md. Writer: KnowledgeStore (write/update/supersede/delete/write_at). .coding/knowledge/ protected from file tools. PLAN/REVIEW have no knowledge home.
