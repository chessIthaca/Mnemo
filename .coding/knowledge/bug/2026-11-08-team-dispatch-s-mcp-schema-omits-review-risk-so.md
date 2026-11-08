+++
title = "team_dispatch's MCP schema omits review_risk, so the high-risk audit gate is unreachable from a schema-following lead"
created = "2026-11-08"
status = "superseded"
+++

Symptom: a persistence-format dispatch meant as high-risk came back `review_risk: "standard"`, because the tool offers no such parameter. Root cause: src-tauri/src/mnemo2/team/lead_tools.rs declares `worker_id`, `plan_id`, `step`, `brief`, `read_back` in the `team_dispatch` schema (line ~101) while the handler reads `args.get("review_risk")` (line ~277) and the tool description and mnemo-lead skill both say to pass it. So the merge gate that requires a fresh independent audit never arms for an external lead. Not fixed yet. Workaround: call team_audit_dispatch by hand before merging persistence or concurrency work. Fix when taken: declare `review_risk` (enum standard|high) in the schema, with a test that every argument the handler reads is declared.
