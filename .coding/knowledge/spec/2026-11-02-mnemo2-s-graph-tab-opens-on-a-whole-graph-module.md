+++
title = "Mnemo2's Graph tab opens on a whole-graph module overview with constant-size nodes"
created = "2026-11-02"
+++

With nothing selected, Mnemo2's Graph tab shows a module overview built from EVERY symbol and edge in the code-graph store (src-tauri/src/mnemo2/graph_overview.rs: read_whole_graph via store all_symbols/all_edges, modules_over groups files into directory modules at a level, real symbol counts, cross-module edge bundles with counts, per-module top symbols), with a toggle to "Top symbols" (the capped 300-symbol degree-ranked slice). Clicking a module drills into its symbols; clicking a symbol shows its neighbourhood. GraphView.svelte counter-scales node groups by 1/(viewportScale*zoom) so circles and 10px labels keep a constant on-screen size, uses non-scaling strokes, and shows more labels as you zoom in.
