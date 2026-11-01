+++
title = "svelte-check flags conditional bind:this on plain lets in mnemo2 — use $state"
created = "2027-01-11"
+++

In mnemo2 (Svelte 5 runes), a `bind:this` target on a plain `let` compiles clean while its element is always mounted, but wrapping the bound element in `{#if}/{:else}` (conditional mount/unmount) makes svelte-check emit `non_reactive_update` warnings at the declaration ("`scroller` is updated, but is not declared with `$state(...)`") — verified empirically: baseline 0 warnings, one {#if} wrapper → 2 warnings, fixed by `let scroller = $state<HTMLDivElement>()`. All reads must then be guarded (`if (!scroller) return`). Also note: adding a read of such a variable inside an `$effect` counts as a reactive read with the same warning. Baseline `npm run check` in mnemo2 is 0 errors 0 warnings — keep it there (plan acceptance requires no new warnings). To verify a baseline quickly: `git stash push` (tracked only; untracked files stay but are checked separately), run `npm run check`, `git stash pop`.
