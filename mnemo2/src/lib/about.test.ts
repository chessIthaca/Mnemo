import { describe, expect, it } from "vitest";
import packageJson from "../../package.json";
import { ABOUT_GROUPS, createAboutTransport, licenseUrl } from "./about";

describe("licenseUrl", () => {
  it("links the curated MIT page", () => {
    expect(licenseUrl("MIT")).toBe("https://opensource.org/licenses/MIT");
  });

  it("strips parentheses and links the first choice of a parenthesised dual license", () => {
    expect(licenseUrl("(MPL-2.0 OR Apache-2.0)")).toBe("https://www.mozilla.org/en-US/MPL/2.0/");
  });

  it("links the first choice of a dual license", () => {
    expect(licenseUrl("Apache-2.0 OR MIT")).toBe("https://www.apache.org/licenses/LICENSE-2.0");
  });

  it("falls back to the SPDX listing for an id without a curated page", () => {
    expect(licenseUrl("Zlib")).toBe("https://spdx.org/licenses/Zlib.html");
  });
});

describe("ABOUT_GROUPS", () => {
  it("drops the React frontend groups and adds Mnemo2's interface groups", () => {
    expect(ABOUT_GROUPS.filter((group) => group.title.startsWith("Frontend"))).toEqual([]);
    expect(ABOUT_GROUPS.map((group) => group.title)).toContain("Mnemo2 interface — runtime");
  });

  it("gives every entry a name, a license and https links", () => {
    for (const group of ABOUT_GROUPS) {
      for (const entry of group.entries) {
        expect(entry.name, group.title).not.toBe("");
        expect(entry.license, group.title).not.toBe("");
        expect(entry.registry, group.title).toMatch(/^https:\/\//);
        expect(entry.licenseUrl, group.title).toMatch(/^https:\/\//);
      }
    }
  });

  it("credits every runtime dependency of mnemo2/package.json", () => {
    // The real manifest is imported as JSON (resolveJsonModule) rather than read
    // with node:fs: the app tsconfig only loads "vite/client" types and the
    // workspace carries no @types/node, so node:fs imports fail svelte-check.
    const runtime = ABOUT_GROUPS.find((group) => group.title === "Mnemo2 interface — runtime");
    expect(runtime).toBeDefined();
    const credited = new Set(runtime!.entries.map((entry) => entry.name));
    for (const name of Object.keys(packageJson.dependencies)) {
      expect(credited.has(name), name).toBe(true);
    }
  });
});

describe("createAboutTransport", () => {
  it("is null outside the desktop app", () => {
    expect(createAboutTransport()).toBeNull();
  });
});
