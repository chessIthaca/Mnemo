+++
title = "Mnemo2 project startup raced lead open with dependent IPC — FIXED"
created = "2027-01-11"
+++

Symptom: normal project opening could show `Mnemo2 has no open lead; call lead_open first`. Root cause: store.openProject started connectNativeLead and team/board/indexing connections in one Promise.all, so dependent host commands could reach ipc::current before lead_open completed. Fixed by awaiting the automatic lead open, re-checking project ownership, then starting dependent connections; the backend guard remains for genuine misuse. Regression: named function project_open_waits_for_lead_before_dependent_ipc in mnemo2/src/lib/store.startup.test.ts covers deferred team/board/indexing startup and no spurious toast. Verified with 241 Mnemo2 tests and zero Svelte warnings.
