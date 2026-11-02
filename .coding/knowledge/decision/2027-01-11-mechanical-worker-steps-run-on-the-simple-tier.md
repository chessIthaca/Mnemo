+++
title = "Mechanical worker steps run on the simple tier"
created = "2027-01-11"
+++

The lead picks a worker tier by how hard the edits are, not by how many files they touch. Mechanical steps, where the tool or the brief spells out each change (clippy or lint fixes, renames, CSS token swaps, moving a block), go to the simple tier (currently deepseek-v4.1-flash, about a quarter of the average tier's price), even when they span several files or crates. The average tier is for real implementation steps; the difficult tier is for design, debugging and bug_fixing diagnosis. The user set this rule after a 17-finding clippy cleanup was first dispatched on the average tier.
