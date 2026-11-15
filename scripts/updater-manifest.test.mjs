// Tests for scripts/updater-manifest.mjs — the release job's `latest.json`
// writer. Everything runs on throwaway temp dirs; nothing here touches the
// network or a real release.

import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";

import { buildManifest, changelogNotes, findSignatures } from "./updater-manifest.mjs";

/** A throwaway directory, removed when the test ends. */
function tempDir(t) {
  const dir = mkdtempSync(join(tmpdir(), "updater-manifest-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** Write `files` (path relative to `dir` -> contents) under `dir`. */
function writeFiles(dir, files) {
  for (const [rel, contents] of Object.entries(files)) {
    const path = join(dir, rel);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, contents);
  }
}

/** Signed updater artifacts as the release job sees them. */
const SIGNATURES = {
  nsis: { file: "Mnemo2_2.5.0_x64-setup.exe", signature: "sig-nsis" },
  msi: { file: "Mnemo2_2.5.0_x64_en-US.msi", signature: "sig-msi" },
  app: { file: "Mnemo2.app.tar.gz", signature: "sig-app" },
};

/** `buildManifest` with the release job's arguments, overridable per test. */
function manifest(overrides = {}) {
  return buildManifest({
    version: "2.5.0",
    tag: "v2.5.0",
    repo: "chessIthaca/Mnemo",
    notes: "the notes",
    pubDate: "2026-11-15T12:00:00.000Z",
    signatures: SIGNATURES,
    ...overrides,
  });
}

test("builds_urls_and_platforms", () => {
  const built = manifest({
    signatures: {
      ...SIGNATURES,
      // A file name with a space, to prove the URL keeps it one token.
      msi: { file: "Mnemo2 2.5.0 x64.msi", signature: "sig-msi" },
    },
  });
  const base = "https://github.com/chessIthaca/Mnemo/releases/download/v2.5.0";

  assert.equal(built.version, "2.5.0");
  assert.equal(built.notes, "the notes");
  assert.equal(built.pub_date, "2026-11-15T12:00:00.000Z");
  assert.deepEqual(built.platforms["windows-x86_64-nsis"], {
    signature: "sig-nsis",
    url: `${base}/Mnemo2_2.5.0_x64-setup.exe`,
  });
  // Tauri's generic Windows key must point at the NSIS installer, exactly like
  // the explicit nsis key (the MSI has its own key).
  assert.deepEqual(
    built.platforms["windows-x86_64"],
    built.platforms["windows-x86_64-nsis"],
  );
  // A file name with a space is percent-encoded, so the URL stays one token.
  assert.deepEqual(built.platforms["windows-x86_64-msi"], {
    signature: "sig-msi",
    url: `${base}/Mnemo2%202.5.0%20x64.msi`,
  });
  assert.deepEqual(built.platforms["darwin-aarch64"], {
    signature: "sig-app",
    url: `${base}/Mnemo2.app.tar.gz`,
  });
});

test("notes_fall_back_to_the_release_link", () => {
  const built = manifest({ notes: null });
  assert.equal(
    built.notes,
    "See https://github.com/chessIthaca/Mnemo/releases/tag/v2.5.0",
  );
});

test("finds_signatures_in_nested_dirs", (t) => {
  const dist = tempDir(t);
  writeFiles(dist, {
    "windows/nsis/Mnemo2_2.5.0_x64-setup.exe": "",
    "windows/nsis/Mnemo2_2.5.0_x64-setup.exe.sig": "  sig-nsis\n",
    "windows/msi/Mnemo2_2.5.0_x64_en-US.msi": "",
    "windows/msi/Mnemo2_2.5.0_x64_en-US.msi.sig": "sig-msi\n\n",
    "macos/Mnemo2.app.tar.gz": "",
    "macos/Mnemo2.app.tar.gz.sig": "sig-app  \n",
    // Unrelated installers, and an artifact without its sibling .sig: an
    // unsigned installer must never enter the manifest.
    "windows/nsis/Mnemo_9.9.9_x64-setup.exe": "",
    "windows/msi/Mnemo_9.9.9_x64_en-US.msi": "",
    "macos/Mnemo2_2.5.0_aarch64.dmg": "",
  });

  assert.deepEqual(findSignatures(dist), {
    nsis: { file: "Mnemo2_2.5.0_x64-setup.exe", signature: "sig-nsis" },
    msi: { file: "Mnemo2_2.5.0_x64_en-US.msi", signature: "sig-msi" },
    app: { file: "Mnemo2.app.tar.gz", signature: "sig-app" },
  });
});

test("missing_artifacts_are_named", (t) => {
  const dist = tempDir(t);
  writeFiles(dist, {
    "windows/msi/Mnemo2_2.5.0_x64_en-US.msi": "",
    "windows/msi/Mnemo2_2.5.0_x64_en-US.msi.sig": "sig-msi\n",
  });

  assert.throws(
    () => findSignatures(dist),
    (error) => {
      assert.match(error.message, /^missing updater artifact: /);
      assert.match(error.message, /nsis/);
      assert.match(error.message, /app/);
      return true;
    },
  );
});

test("changelog_section_is_extracted", () => {
  const changelog = [
    "# Changelog",
    "",
    "## [2.5.0] — 2026-11-15",
    "",
    "Mnemo2 updates itself.",
    "",
    "### Added",
    "",
    "- It checks for a new release every 6 hours.",
    "",
    "## [2.4.0] — 2026-11-15",
    "",
    "The previous release.",
    "",
  ].join("\n");

  // Only 2.5.0's body: the next `## [` heading ends the section.
  assert.equal(
    changelogNotes(changelog, "2.5.0"),
    "Mnemo2 updates itself.\n\n### Added\n\n- It checks for a new release every 6 hours.",
  );
  // A version the release notes never covered.
  assert.equal(changelogNotes(changelog, "9.9.9"), null);
});
