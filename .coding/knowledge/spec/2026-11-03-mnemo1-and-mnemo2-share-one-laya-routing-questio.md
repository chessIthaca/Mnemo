+++
title = "Mnemo1 and Mnemo2 share one Laya routing question in src/agent/route_question.rs"
created = "2026-11-03"
+++

src/agent/route_question.rs holds the routing question both apps ask Laya:
- three neutral labels (trivial, routine, hard) with keyword descriptions, as byte-stable constants;
- folding of un-offered labels (trivial folds up, hard folds down) and a question only when two or more labels are offered;
- default gates 0.475 for 3 labels and 0.60 for 2, provisional until the routing eval;
- label_above, a canonical tool-call signature (keys sorted, id keys dropped) and a Strikes counter that fires once per streak of 3 failed or identical calls.
Never use tier or model names as labels. Changing a description means re-running the routing eval. Design: .coding/design/mnemo2-laya-routing.md.
