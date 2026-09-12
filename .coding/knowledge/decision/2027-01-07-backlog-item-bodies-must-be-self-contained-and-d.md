+++
title = "Backlog item bodies must be self-contained and detailed (lesser-model dispatch)"
created = "2027-01-07"
+++

User directive (2027-01-07): backlog item descriptions must be a LOT more detailed so a model with less reasoning effort (or a lesser model) dispatched on the item succeeds. Every item body carries: (1) problem/symptom with dates + repro, (2) exact file paths + symbol names, (3) fix direction, (4) acceptance criteria, (5) pointers to related memories/commits/reviews. Rationale: run-all dispatches to cheaper models; terse pointer-style bodies flounder. Headline+body shape already enforced at both write paths (568d405); an enforcement + dispatch-enrichment work item is queued in the backlog. Applies to every backlog_add from now on.
