// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5179,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    // Emit into the shared gitignored target/ tree so all build artifacts
    // (Rust + JS) live in one place and frontend/ holds only source.
    outDir: "../target/frontend-dist",
    // outDir is outside the Vite root — tell Vite explicitly so it cleans
    // stale artifacts on rebuild instead of warning.
    emptyOutDir: true,
    // The heavy markdown/highlight stack (react-markdown + remark-gfm +
    // rehype-highlight) is lazy-loaded via dynamic import from
    // MarkdownImpl.tsx, so it lands in a separate chunk and the main bundle
    // stays under Vite's default 500 kB warning. No manualChunks config
    // needed — Vite auto-splits the dynamic import.
  },
});
