// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Curated dependency manifest for the About dialog.
 *
 * Lists the DIRECT dependencies of Mnemo across three manifests â€” the
 * library crate (`/Cargo.toml`), the Tauri app shell (`/src-tauri/Cargo.toml`),
 * and the frontend (`/frontend/package.json`) â€” each annotated with its SPDX
 * license label, a registry page (crates.io / npmjs.com), and a link to the
 * license text. Transitive dependencies are intentionally omitted: crediting
 * the direct deps (whose licenses we rely on) is the scope of the About dialog.
 *
 * Each crate is credited exactly once. Several crates are direct deps of BOTH
 * the library and the app shell (tokio, serde, serde_json, anyhow,
 * async-trait, base64); they appear only in the "Rust â€” library" group, and
 * the "Rust â€” app shell (additional)" group lists only the shell-only crates.
 * No crate is left uncredited.
 *
 * License labels mirror what each manifest's package metadata declares. Where a
 * crate is dual-licensed ("MIT OR Apache-2.0") the license-text link points at
 * the first, more permissive option (MIT); the badge still shows the full SPDX
 * expression so the user sees both choices.
 */

/** A single direct dependency with its license + links. */
export interface DependencyEntry {
  /** The package name as it appears in the manifest. */
  name: string;
  /** SPDX license expression (e.g. "MIT", "MIT OR Apache-2.0", "ISC"). */
  license: string;
  /** The package's registry page (crates.io or npmjs.com). */
  registry: string;
  /** A URL showing the license text. */
  licenseUrl: string;
}

/** A titled group of dependencies (one per manifest source). */
export interface DependencyGroup {
  /** Group heading (e.g. "Rust â€” library"). */
  title: string;
  /** The dependencies in this group. */
  entries: DependencyEntry[];
}

/**
 * The license Mnemo itself is published under: MIT (both Cargo.toml files
 * declare `license = "MIT"`). Linked from the About dialog's intro line.
 */
export const APP_LICENSE_URL =
  "https://opensource.org/licenses/MIT";

/**
 * Build a crates.io registry URL for a crate name.
 * @param name - the crate name as declared in Cargo.toml.
 * @returns the `https://crates.io/crates/<name>` URL.
 */
function crates(name: string): string {
  return `https://crates.io/crates/${name}`;
}

/**
 * Build an npm registry URL for a package name.
 * @param name - the package name as declared in package.json (scoped names
 *   keep their leading `@`).
 * @returns the `https://www.npmjs.com/package/<name>` URL.
 */
function npm(name: string): string {
  return `https://www.npmjs.com/package/${name}`;
}

/** The opensource.org license-text URL for the MIT license. */
const MIT = "https://opensource.org/licenses/MIT";
/** The opensource.org license-text URL for the Apache-2.0 license. */
const APACHE = "https://opensource.org/licenses/Apache-2.0";
/** The opensource.org license-text URL for the ISC license. */
const ISC = "https://opensource.org/licenses/ISC";
/** The Creative Commons license-text URL for the CC0 1.0 license (public domain dedication). */
const CC0 = "https://creativecommons.org/publicdomain/zero/1.0/";

/**
 * The direct dependencies, grouped by manifest source. Order is stable so the
 * About dialog renders the same list every time (library â†’ shell â†’ frontend
 * runtime â†’ frontend build/dev).
 */
export const DEPENDENCY_GROUPS: DependencyGroup[] = [
  {
    title: "Rust â€” library",
    entries: [
      { name: "tokio", license: "MIT", registry: crates("tokio"), licenseUrl: MIT },
      { name: "tokio-stream", license: "MIT", registry: crates("tokio-stream"), licenseUrl: MIT },
      { name: "futures", license: "MIT OR Apache-2.0", registry: crates("futures"), licenseUrl: MIT },
      { name: "async-trait", license: "MIT OR Apache-2.0", registry: crates("async-trait"), licenseUrl: MIT },
      { name: "similar", license: "MIT", registry: crates("similar"), licenseUrl: MIT },
      { name: "serde", license: "MIT OR Apache-2.0", registry: crates("serde"), licenseUrl: MIT },
      { name: "serde_json", license: "MIT OR Apache-2.0", registry: crates("serde_json"), licenseUrl: MIT },
      { name: "toml", license: "MIT OR Apache-2.0", registry: crates("toml"), licenseUrl: MIT },
      { name: "thiserror", license: "MIT OR Apache-2.0", registry: crates("thiserror"), licenseUrl: MIT },
      { name: "anyhow", license: "MIT OR Apache-2.0", registry: crates("anyhow"), licenseUrl: MIT },
      { name: "clap", license: "MIT OR Apache-2.0", registry: crates("clap"), licenseUrl: MIT },
      { name: "directories", license: "MIT OR Apache-2.0", registry: crates("directories"), licenseUrl: MIT },
      { name: "glob", license: "MIT OR Apache-2.0", registry: crates("glob"), licenseUrl: MIT },
      { name: "uuid", license: "MIT OR Apache-2.0", registry: crates("uuid"), licenseUrl: MIT },
      { name: "regex", license: "MIT OR Apache-2.0", registry: crates("regex"), licenseUrl: MIT },
      { name: "tiktoken-rs", license: "MIT", registry: crates("tiktoken-rs"), licenseUrl: MIT },
      { name: "rusqlite", license: "MIT", registry: crates("rusqlite"), licenseUrl: MIT },
      { name: "fastembed", license: "MIT OR Apache-2.0", registry: crates("fastembed"), licenseUrl: MIT },
      { name: "reqwest", license: "MIT OR Apache-2.0", registry: crates("reqwest"), licenseUrl: MIT },
      { name: "sha2", license: "MIT OR Apache-2.0", registry: crates("sha2"), licenseUrl: MIT },
      { name: "base64", license: "MIT OR Apache-2.0", registry: crates("base64"), licenseUrl: MIT },
      { name: "chromiumoxide", license: "MIT", registry: crates("chromiumoxide"), licenseUrl: MIT },
      { name: "tempfile", license: "MIT OR Apache-2.0", registry: crates("tempfile"), licenseUrl: MIT },
      { name: "windows-sys", license: "MIT OR Apache-2.0", registry: crates("windows-sys"), licenseUrl: MIT },
      { name: "notify", license: "CC0-1.0", registry: crates("notify"), licenseUrl: CC0 },
      { name: "image", license: "MIT OR Apache-2.0", registry: crates("image"), licenseUrl: MIT },
      { name: "tree-sitter", license: "MIT", registry: crates("tree-sitter"), licenseUrl: MIT },
      { name: "tree-sitter-rust", license: "MIT", registry: crates("tree-sitter-rust"), licenseUrl: MIT },
      { name: "tree-sitter-typescript", license: "MIT", registry: crates("tree-sitter-typescript"), licenseUrl: MIT },
      { name: "tree-sitter-javascript", license: "MIT", registry: crates("tree-sitter-javascript"), licenseUrl: MIT },
      { name: "tree-sitter-python", license: "MIT", registry: crates("tree-sitter-python"), licenseUrl: MIT },
      { name: "tree-sitter-go", license: "MIT", registry: crates("tree-sitter-go"), licenseUrl: MIT },
      { name: "tree-sitter-java", license: "MIT", registry: crates("tree-sitter-java"), licenseUrl: MIT },
      { name: "tree-sitter-c", license: "MIT", registry: crates("tree-sitter-c"), licenseUrl: MIT },
      { name: "tree-sitter-cpp", license: "MIT", registry: crates("tree-sitter-cpp"), licenseUrl: MIT },
      { name: "tree-sitter-c-sharp", license: "MIT", registry: crates("tree-sitter-c-sharp"), licenseUrl: MIT },
      { name: "tree-sitter-ruby", license: "MIT", registry: crates("tree-sitter-ruby"), licenseUrl: MIT },
      { name: "tree-sitter-php", license: "MIT", registry: crates("tree-sitter-php"), licenseUrl: MIT },
      { name: "tree-sitter-html", license: "MIT", registry: crates("tree-sitter-html"), licenseUrl: MIT },
    ],
  },
  {
    title: "Rust â€” app shell (additional)",
    entries: [
      { name: "tauri", license: "MIT OR Apache-2.0", registry: crates("tauri"), licenseUrl: MIT },
      { name: "tauri-build", license: "MIT OR Apache-2.0", registry: crates("tauri-build"), licenseUrl: MIT },
      { name: "tauri-plugin-shell", license: "MIT OR Apache-2.0", registry: crates("tauri-plugin-shell"), licenseUrl: MIT },
      { name: "tauri-plugin-dialog", license: "MIT OR Apache-2.0", registry: crates("tauri-plugin-dialog"), licenseUrl: MIT },
      { name: "log", license: "MIT OR Apache-2.0", registry: crates("log"), licenseUrl: MIT },
    ],
  },
  {
    title: "Rust — vendored patches",
    entries: [
      // Patched forks carried in vendor/ ([patch.crates-io] in the root
      // Cargo.toml) — see vendor/*/PATCHES.md for what changed.
      { name: "tao (vendored)", license: "Apache-2.0", registry: "https://github.com/tauri-apps/tao", licenseUrl: APACHE },
      { name: "wry (vendored)", license: "MIT OR Apache-2.0", registry: "https://github.com/tauri-apps/wry", licenseUrl: MIT },
    ],
  },
  {
    title: "Frontend â€” runtime",
    entries: [
      { name: "@radix-ui/react-dialog", license: "MIT", registry: npm("@radix-ui/react-dialog"), licenseUrl: MIT },
      { name: "@radix-ui/react-tabs", license: "MIT", registry: npm("@radix-ui/react-tabs"), licenseUrl: MIT },
      { name: "@tauri-apps/api", license: "MIT OR Apache-2.0", registry: npm("@tauri-apps/api"), licenseUrl: MIT },
      { name: "@tauri-apps/plugin-shell", license: "MIT OR Apache-2.0", registry: npm("@tauri-apps/plugin-shell"), licenseUrl: MIT },
      { name: "lucide-react", license: "ISC", registry: npm("lucide-react"), licenseUrl: ISC },
      { name: "react", license: "MIT", registry: npm("react"), licenseUrl: MIT },
      { name: "react-dom", license: "MIT", registry: npm("react-dom"), licenseUrl: MIT },
      { name: "react-markdown", license: "MIT", registry: npm("react-markdown"), licenseUrl: MIT },
      { name: "remark-breaks", license: "MIT", registry: npm("remark-breaks"), licenseUrl: MIT },
      { name: "rehype-highlight", license: "MIT", registry: npm("rehype-highlight"), licenseUrl: MIT },
      { name: "remark-gfm", license: "MIT", registry: npm("remark-gfm"), licenseUrl: MIT },
      { name: "zustand", license: "MIT", registry: npm("zustand"), licenseUrl: MIT },
      { name: "d3-force", license: "ISC", registry: npm("d3-force"), licenseUrl: ISC },
    ],
  },
  {
    title: "Frontend â€” build & dev",
    entries: [
      { name: "@tailwindcss/typography", license: "MIT", registry: npm("@tailwindcss/typography"), licenseUrl: MIT },
      { name: "@tauri-apps/cli", license: "MIT OR Apache-2.0", registry: npm("@tauri-apps/cli"), licenseUrl: MIT },
      { name: "@types/react", license: "MIT", registry: npm("@types/react"), licenseUrl: MIT },
      { name: "@types/react-dom", license: "MIT", registry: npm("@types/react-dom"), licenseUrl: MIT },
      { name: "@vitejs/plugin-react", license: "MIT", registry: npm("@vitejs/plugin-react"), licenseUrl: MIT },
      { name: "autoprefixer", license: "MIT", registry: npm("autoprefixer"), licenseUrl: MIT },
      { name: "postcss", license: "MIT", registry: npm("postcss"), licenseUrl: MIT },
      { name: "tailwindcss", license: "MIT", registry: npm("tailwindcss"), licenseUrl: MIT },
      { name: "typescript", license: "Apache-2.0", registry: npm("typescript"), licenseUrl: APACHE },
      { name: "vite", license: "MIT", registry: npm("vite"), licenseUrl: MIT },
      { name: "vitest", license: "MIT", registry: npm("vitest"), licenseUrl: MIT },
      { name: "@types/d3-force", license: "MIT", registry: npm("@types/d3-force"), licenseUrl: MIT },
    ],
  },
];
