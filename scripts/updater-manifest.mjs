// Write the Tauri updater's `latest.json` for a tagged release.
//
// The release job builds Mnemo2 with updater artifacts enabled
// (src-tauri/mnemo2-app/tauri.updater.conf.json), so tauri writes a minisign
// `.sig` next to every artifact the updater installs: the NSIS `-setup.exe`,
// the `.msi` and macOS `Mnemo2.app.tar.gz`. The installed app polls
// `https://github.com/chessIthaca/Mnemo/releases/latest/download/latest.json`
// (src-tauri/mnemo2-app/tauri.conf.json `plugins.updater`), so the release job
// runs this script over the downloaded installers and publishes the manifest
// with them (the assets go up while the release is still a draft — this repo
// uses immutable releases).
//
// Usage (the release job passes exactly this):
//   node scripts/updater-manifest.mjs --tag v2.5.0 --repo chessIthaca/Mnemo \
//     --dist dist --changelog CHANGELOG.md --out dist/latest.json
//
// Platform keys are Tauri's: windows-x86_64-nsis and windows-x86_64 both point
// at the NSIS installer, windows-x86_64-msi at the MSI, darwin-aarch64 at the
// macOS tarball. Every entry carries the trimmed contents of the artifact's
// `.sig` file and the tag's download URL.
// Tests: scripts/updater-manifest.test.mjs (`node --test`).

import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

/** Every regular file under `dir`, recursively, as `{ name, path }`, sorted. */
function walk(dir) {
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      found.push(...walk(path));
    } else if (entry.isFile()) {
      found.push({ name: entry.name, path });
    }
  }
  return found.sort((a, b) => a.path.localeCompare(b.path));
}

/** The three updater artifacts and the file name each one is recognized by. */
const UPDATER_KINDS = [
  {
    kind: "nsis",
    pattern: "*-setup.exe with a sibling <file>.sig",
    matches: (name) => name.endsWith("-setup.exe"),
  },
  {
    kind: "msi",
    pattern: "*.msi with a sibling <file>.sig",
    matches: (name) => name.endsWith(".msi"),
  },
  {
    kind: "app",
    pattern: "Mnemo2.app.tar.gz with a sibling <file>.sig",
    matches: (name) => name === "Mnemo2.app.tar.gz",
  },
];

/**
 * Find the signed updater artifacts under `distDir`: `{ nsis, msi, app }`,
 * each `{ file, signature }` with the artifact's basename and the trimmed
 * contents of its sibling `<file>.sig`. An artifact without its `.sig` does
 * not count — an unsigned installer must never enter the manifest. Throws one
 * `missing updater artifact: …` error naming every missing kind when an
 * artifact is absent, so a release fails instead of publishing a partial
 * manifest.
 */
export function findSignatures(distDir) {
  const files = existsSync(distDir) ? walk(distDir) : [];
  const paths = new Set(files.map((file) => file.path));
  const signatures = {};
  const missing = [];
  for (const { kind, pattern, matches } of UPDATER_KINDS) {
    const artifact = files.find(
      (file) => matches(file.name) && paths.has(`${file.path}.sig`),
    );
    if (!artifact) {
      missing.push(`${kind} (${pattern})`);
      continue;
    }
    signatures[kind] = {
      file: artifact.name,
      signature: readFileSync(`${artifact.path}.sig`, "utf8").trim(),
    };
  }
  if (missing.length > 0) {
    throw new Error(`missing updater artifact: ${missing.join(", ")}`);
  }
  return signatures;
}

/**
 * The body under `## [<version>]` in a Keep-a-Changelog file: every line up to
 * (not including) the next `## [` heading, trimmed. Null when that section is
 * missing or empty — the manifest then falls back to the release-tag link.
 */
export function changelogNotes(text, version) {
  const lines = text.split(/\r?\n/);
  const heading = `## [${version}]`;
  const start = lines.findIndex((line) => line.startsWith(heading));
  if (start === -1) {
    return null;
  }
  const body = [];
  for (const line of lines.slice(start + 1)) {
    if (line.startsWith("## [")) {
      break;
    }
    body.push(line);
  }
  const notes = body.join("\n").trim();
  return notes === "" ? null : notes;
}

/**
 * The Tauri updater manifest: the version, its notes, the publish date and one
 * `platforms` entry per target — each the artifact's release-asset URL (the
 * file name percent-encoded) and its signature. Notes fall back to the
 * release-tag link when the CHANGELOG has no section for this version.
 */
export function buildManifest({ version, tag, repo, notes, pubDate, signatures }) {
  const url = (file) =>
    `https://github.com/${repo}/releases/download/${tag}/${encodeURIComponent(file)}`;
  const platform = ({ file, signature }) => ({ signature, url: url(file) });
  const nsis = platform(signatures.nsis);
  return {
    version,
    notes: notes ?? `See https://github.com/${repo}/releases/tag/${tag}`,
    pub_date: pubDate,
    platforms: {
      "windows-x86_64-nsis": nsis,
      "windows-x86_64": nsis,
      "windows-x86_64-msi": platform(signatures.msi),
      "darwin-aarch64": platform(signatures.app),
    },
  };
}

/** Parse `--key value` pairs into `{ key: value }`. */
function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index] ?? "";
    const value = argv[index + 1];
    if (!flag.startsWith("--") || value === undefined) {
      throw new Error(
        `expected --tag/--repo/--dist/--changelog/--out pairs, got ${JSON.stringify(flag)}`,
      );
    }
    args[flag.slice(2)] = value;
  }
  return args;
}

/**
 * The CLI: read the version from the tag, take that release's CHANGELOG
 * section, find the signed updater artifacts and write the manifest JSON
 * (2-space indent, trailing newline) to `--out`.
 */
function writeManifest(argv) {
  const args = parseArgs(argv);
  for (const required of ["tag", "repo", "dist", "changelog", "out"]) {
    if (!args[required]) {
      throw new Error(`missing --${required}`);
    }
  }
  const tag = args.tag;
  if (!/^v\d+\.\d+\.\d+/.test(tag)) {
    throw new Error(`--tag must look like vX.Y.Z (got ${JSON.stringify(tag)})`);
  }
  const version = tag.slice(1);
  const manifest = buildManifest({
    version,
    tag,
    repo: args.repo,
    notes: changelogNotes(readFileSync(args.changelog, "utf8"), version),
    pubDate: new Date().toISOString(),
    signatures: findSignatures(args.dist),
  });
  writeFileSync(args.out, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`updater-manifest: wrote ${args.out} for ${version}`);
}

const invokedDirectly =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(process.argv[1]).href;

if (invokedDirectly) {
  try {
    writeManifest(process.argv.slice(2));
  } catch (error) {
    console.error(`updater-manifest: ${error.message}`);
    process.exitCode = 1;
  }
}
