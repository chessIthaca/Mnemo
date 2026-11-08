+++
title = "no model-driven memory verification pass yet"
created = "2026-11-08"
evidence = "mnemo2_memory_eval on the combined build joined with stamped rows on a store copy, 2026-11-08"
+++

A budgeted pass in which a cheap model re-checks records against files and transcripts (the "dreaming" in Anthropic's components-of-agent-memory article) was planned behind a gate: build it only if stale or wrong records actually surface in recall. The gate was not met on 2026-11-08: on missed eval questions, 1 of 115 top-five slots was a stale record, and stale records are now flagged in search and left out of the primer. It also selects records by recall_count, which starts empty, and it would add unattended model spend and automatic supersedes.

Revisit when recall_count has weeks of data and a wrong record has actually misled a session or a worker. The ingredients exist: moved_paths, evidence, session_search and recall_count. Larger recall gains are likelier elsewhere: paraphrased hit@5 is 0.425, and missed questions mostly return plausible neighbours, so look at retrieval (embeddings, query expansion) before verification.
