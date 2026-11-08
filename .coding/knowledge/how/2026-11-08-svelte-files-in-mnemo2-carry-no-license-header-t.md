+++
title = "Svelte files in mnemo2 carry no license header; .ts files do"
created = "2026-11-08"
+++

In `mnemo2/src/**`, `.svelte` components start directly with `<script lang="ts">` — they carry NO `// Copyright (c) …` / `// SPDX-License-Identifier: MIT` header (a `//` line outside a script block is rendered as literal text in the template, so it is both wrong and visible). `.ts` modules in `mnemo2/src/lib/**` DO carry the 3-line MIT header at the top; `.test.ts` files do not. Copy an existing sibling file's head rather than assuming the header is repo-wide.
