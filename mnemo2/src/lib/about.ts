/**
 * The About page's data: the running version, where Mnemo's source and license
 * live, and the open-source projects Mnemo2 is built on.
 *
 * The Rust groups are shared with Mnemo1's About dialog
 * (`frontend/src/components/about/dependencies.ts`) — the same crates build
 * both apps. That manifest's "Frontend —" groups cover the React app and do
 * not apply to Mnemo2, so they are filtered out and Mnemo2's own interface
 * groups (`MNEMO2_INTERFACE_GROUPS`, mirroring mnemo2/package.json) are added.
 */

import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";
import {
  APP_LICENSE_URL,
  DEPENDENCY_GROUPS,
  type DependencyEntry,
  type DependencyGroup,
} from "../../../frontend/src/components/about/dependencies";

export { APP_LICENSE_URL };
export type { DependencyEntry, DependencyGroup };

/** Where Mnemo's source code lives (the About page's "Source code" link). */
export const REPOSITORY_URL = "https://github.com/chessIthaca/Mnemo";

/** License-text pages for the SPDX ids Mnemo2's own dependencies declare. */
const LICENSE_URLS: Readonly<Record<string, string>> = {
  MIT: "https://opensource.org/licenses/MIT",
  ISC: "https://opensource.org/license/isc-license-txt",
  "Apache-2.0": "https://www.apache.org/licenses/LICENSE-2.0",
  "OFL-1.1": "https://openfontlicense.org/open-font-license-official-text/",
  "BSD-3-Clause": "https://opensource.org/license/bsd-3-clause",
  "MPL-2.0": "https://www.mozilla.org/en-US/MPL/2.0/",
};

/**
 * The license-text page for one SPDX license expression.
 *
 * Parentheses are stripped and a choice (`A OR B`) links its first option, so
 * dompurify's `(MPL-2.0 OR Apache-2.0)` links the MPL-2.0 text. An id without
 * a curated page falls back to its SPDX listing.
 */
export function licenseUrl(license: string): string {
  const id = license.replaceAll("(", "").replaceAll(")", "").split(" OR ")[0].trim();
  return LICENSE_URLS[id] ?? `https://spdx.org/licenses/${id}.html`;
}

/** The npm registry page of a package (scoped names keep their leading `@`). */
const npm = (name: string): string => `https://www.npmjs.com/package/${name}`;

/**
 * One entry of Mnemo2's own interface groups: name and license from
 * mnemo2/package.json (read from each package's metadata), plus its npm page
 * and license-text link.
 */
function npmEntry(name: string, license: string): DependencyEntry {
  return { name, license, registry: npm(name), licenseUrl: licenseUrl(license) };
}

/**
 * mnemo2/package.json's direct dependencies. The "runtime" group is what the
 * shipped interface runs on (Svelte compiles the app but remains its runtime);
 * "build & test" covers the tooling around it. Order is stable so the page
 * renders the same list every time.
 */
export const MNEMO2_INTERFACE_GROUPS: DependencyGroup[] = [
  {
    title: "Mnemo2 interface — runtime",
    entries: [
      npmEntry("@fontsource-variable/inter", "OFL-1.1"),
      npmEntry("@fontsource-variable/source-serif-4", "OFL-1.1"),
      npmEntry("@lucide/svelte", "ISC"),
      npmEntry("@tauri-apps/api", "Apache-2.0 OR MIT"),
      npmEntry("d3-force", "ISC"),
      npmEntry("dompurify", "(MPL-2.0 OR Apache-2.0)"),
      npmEntry("highlight.js", "BSD-3-Clause"),
      npmEntry("markdown-it", "MIT"),
      npmEntry("svelte", "MIT"),
    ],
  },
  {
    title: "Mnemo2 interface — build & test",
    entries: [
      npmEntry("vite", "MIT"),
      npmEntry("@sveltejs/vite-plugin-svelte", "MIT"),
      npmEntry("typescript", "Apache-2.0"),
      npmEntry("svelte-check", "MIT"),
      npmEntry("vitest", "MIT"),
      npmEntry("jsdom", "MIT"),
      npmEntry("@testing-library/svelte", "MIT"),
      npmEntry("@types/d3-force", "MIT"),
    ],
  },
];

/**
 * What the About page lists: Mnemo's Rust crates (library, app shell, vendored
 * patches) followed by Mnemo2's own interface groups. Mnemo1's React "Frontend
 * —" groups are dropped.
 */
export const ABOUT_GROUPS: DependencyGroup[] = [
  ...DEPENDENCY_GROUPS.filter((group) => group.title.startsWith("Rust")),
  ...MNEMO2_INTERFACE_GROUPS,
];

/** What the About page needs from the desktop shell. */
export interface AboutTransport {
  /** The running app's version (tauri.conf.json). */
  version(): Promise<string>;
  /** Open a URL in the system browser. */
  open(url: string): Promise<void>;
}

/**
 * The desktop-shell transport: the version comes from the Tauri app API, and
 * links go through the existing `browser_open_external` command. Null in a
 * plain browser — the page then hides the version chip and opens links
 * itself.
 */
export function createAboutTransport(): AboutTransport | null {
  if (!isTauriRuntime()) return null;
  return {
    version: () => getVersion(),
    open: (url) => invoke<void>("browser_open_external", { url }),
  };
}
