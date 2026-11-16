+++
title = "An audit that ends its turn with plain text still delivers its report"
created = "2026-11-16"
+++

Symptom: An audit worker that wrote its findings as its final message instead of calling message_parent (fourthAud1, 2026-11-16) produced only the "finished its read-only audit ... Adjudicate" notice with no report text, stayed idle holding a worker slot for hours, and looked stuck in mission control; the lead had to dig the report out of its transcript. · regression test: an_audit_ending_with_plain_text_delivers_it_as_its_report

Full record for plan ebaac2a3 (see .coding/plans/ebaac2a3.md for the plan file).

regression test: an_audit_ending_with_plain_text_delivers_it_as_its_report · path .coding/plans/ebaac2a3.md · branch wt/mnemo @ 09ea83d1 (unmerged — exists only on this branch)
