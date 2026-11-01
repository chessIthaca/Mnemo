+++
title = "Mnemo2 type-scale rounding and code-font conventions"
created = "2027-01-11"
+++

Mnemo2 typography conventions: (1) When a hard-coded px font-size is not exactly a --text-* token, round to the nearest token; on a tie round UP (10px ties between --text-xs 9px and --text-sm 11px and becomes --text-sm). (2) Commit, branch, path and worktree hashes rendered as <code> elements in UI chrome need an explicit font-family: var(--font-mono) rule — they otherwise inherit Inter, and only Markdown.svelte's scoped .markdown :global(code) rule covers prose code. (3) Markdown reading text gets its Source Serif family from Markdown.svelte's own .markdown rule; consumer components only size it. Spec: .coding/knowledge/spec/2027-01-11-mnemo2-ui-type-scale-and-font-families.md.
