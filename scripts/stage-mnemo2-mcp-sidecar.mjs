// Stage the freshly built `mnemo2-mcp` helper for the release installers.
//
// The "mnemo" Claude Code plugin runs `bin/mnemo2-mcp`, and an installed
// Mnemo2 must carry that helper: otherwise leading with Claude Code needs a
// source build (`scripts/install-mnemo2-mcp.mjs`). Tauri ships it as a
// sidecar (`externalBin`), so it has to exist as
// `binaries/mnemo2-mcp-<target triple>[.exe]` under both crates' manifest
// dirs (`src-tauri/mnemo2-app/` and `src-tauri/`) before `npx tauri build`
// runs.
//
// The helper is built from the same package as the app, and tauri's build
// script refuses to compile when a configured sidecar file is missing — so
// the `externalBin` entry lives in `tauri.bundle.conf.json`, which only the
// release build merges in with `npx tauri build --config`. Putting it in
// `tauri.conf.json` instead would break every plain `cargo build` and
// `cargo test`.
//
// It builds `cargo build -p mnemo2-app --bin mnemo2-mcp --release --features
// custom-protocol` (plus `--target <triple>` when asked) and copies the
// result to `src-tauri/mnemo2-app/binaries/` and `src-tauri/binaries/` as
// `mnemo2-mcp-<triple>[.exe]`, for both crates that run `tauri_build` — each
// resolves `externalBin` from its own manifest dir.
//
// Tauri then installs the staged file next to the main executable (Windows:
// the install folder; macOS: Mnemo2.app/Contents/MacOS/mnemo2-mcp), where the
// app picks it up to install the plugin's helper at start.
//
// Usage: node scripts/stage-mnemo2-mcp-sidecar.mjs [--target <triple>]
//   (default: the host triple from `rustc -vV`)

import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// `--target <triple>` asks cargo for a cross build (the macOS job builds
// aarch64-apple-darwin); without it the host triple is what cargo builds.
const flag = process.argv.indexOf("--target");
const explicitTarget = flag === -1 ? undefined : process.argv[flag + 1];
if (flag !== -1 && !explicitTarget) {
  console.error("stage-mnemo2-mcp-sidecar: --target needs a target triple.");
  process.exit(1);
}

function hostTriple() {
  const rustc = spawnSync("rustc", ["-vV"], { encoding: "utf8", shell: false });
  if (rustc.error || rustc.status !== 0 || !rustc.stdout) return undefined;
  const line = rustc.stdout.split(/\r?\n/).find((entry) => entry.startsWith("host: "));
  return line?.slice("host: ".length).trim() || undefined;
}

const triple = explicitTarget ?? hostTriple();
if (!triple) {
  console.error(
    "stage-mnemo2-mcp-sidecar: no target triple — pass --target <triple> or make `rustc -vV` available.",
  );
  process.exit(1);
}

const args = [
  "build",
  "-p",
  "mnemo2-app",
  "--bin",
  "mnemo2-mcp",
  "--release",
  "--features",
  "custom-protocol",
];
if (explicitTarget !== undefined) args.push("--target", explicitTarget);
const build = spawnSync("cargo", args, { stdio: "inherit", shell: false });
if (build.error) {
  console.error(`stage-mnemo2-mcp-sidecar: could not run cargo: ${build.error.message}`);
  process.exit(1);
}
if (build.status !== 0) process.exit(build.status ?? 1);

// The workspace root is the repo root, so cargo builds into <repo>/target —
// unless CARGO_TARGET_DIR points at a shared (warm) target dir. With
// `--target` the profile dir sits under the triple, as cargo lays it out.
const exe = triple.includes("windows") ? ".exe" : "";
const targetRoot = process.env.CARGO_TARGET_DIR || join(root, "target");
const profileDir =
  explicitTarget === undefined
    ? join(targetRoot, "release")
    : join(targetRoot, explicitTarget, "release");
const helper = join(profileDir, `mnemo2-mcp${exe}`);
if (!existsSync(helper)) {
  console.error(`stage-mnemo2-mcp-sidecar: cargo succeeded but ${helper} is missing.`);
  process.exit(1);
}

// Both crates that run tauri_build resolve externalBin relative to their own
// manifest dir, and the Tauri CLI hands the merged config (with externalBin)
// to both build scripts: the app package (src-tauri/mnemo2-app) and the host
// library mnemo-app (src-tauri). Stage the helper for each.
const binDirs = [
  join(root, "src-tauri", "mnemo2-app", "binaries"),
  join(root, "src-tauri", "binaries"),
];
for (const binDir of binDirs) {
  const dest = join(binDir, `mnemo2-mcp-${triple}${exe}`);
  mkdirSync(binDir, { recursive: true });
  copyFileSync(helper, dest);
  console.log(`stage-mnemo2-mcp-sidecar: ${dest}`);
}
