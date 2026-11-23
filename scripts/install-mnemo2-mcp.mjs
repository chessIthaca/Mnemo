// Install the freshly built `mnemo2-mcp` helper and the `mnemo-models`
// shared-models host.
//
// - `mnemo2/plugin/bin/` is what the plugin's MCP server and hooks run
//   (`${CLAUDE_PLUGIN_ROOT}/bin/mnemo2-mcp`). `claude plugin install` copies
//   the plugin into Claude Code's plugin cache, so an installed plugin keeps
//   the helper it was installed with.
// - `<config>/bin/` ($MNEMO_HOME or ~/.mnemo) holds the current helper — and
//   `mnemo-models` (plan d3336a8a step 2), the machine-wide shared
//   embedding-model host the instances spawn from this dir when it is not a
//   sibling of their own executable. The plugin never runs the models host,
//   so it is not copied into the plugin bin. Any older helper copy (e.g. the
//   cached plugin's) hands its arguments to this one, so a rebuild takes
//   effect without reinstalling the plugin.
// - A running Claude session keeps the old helper open, a running models
//   host keeps serving from the old host binary, and Windows refuses to
//   overwrite a running executable. The old file is renamed aside first;
//   leftovers from earlier runs are removed once they are no longer locked.
// - The helper starts Mnemo2 itself when no instance serves the project, so
//   `mnemo2-path.txt` records where the matching `mnemo2` executable lives.
//
// Usage: node scripts/install-mnemo2-mcp.mjs [release|debug]   (default release)

import { copyFileSync, cpSync, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const profile = process.argv[2] === "debug" ? "debug" : "release";
const exe = process.platform === "win32" ? ".exe" : "";
// src-tauri is a workspace member, so cargo builds into the workspace target.
const targetDir = join(process.env.CARGO_TARGET_DIR || join(root, "target"), profile);
const helper = join(targetDir, `mnemo2-mcp${exe}`);
const host = join(targetDir, `mnemo-models${exe}`);
const app = join(targetDir, `mnemo2${exe}`);
const configDir = process.env.MNEMO_HOME || join(homedir(), ".mnemo");

for (const [binary, hint] of [
  [helper, "cargo build -p mnemo2-app --bin mnemo2-mcp"],
  [host, "cargo build -p mnemo2-app --bin mnemo-models"],
]) {
  if (!existsSync(binary)) {
    console.error(`install-mnemo2-mcp: ${binary} is missing; build it with ${hint} first.`);
    process.exit(1);
  }
}

// Copy `src` to `dest` with the rename-aside rollover: Windows refuses to
// overwrite a running executable, so the old file is moved out of the way
// (a running session or host keeps using it) and copied over fresh.
function copyWithRollover(src, dest) {
  try {
    if (existsSync(dest)) {
      try {
        rmSync(dest);
      } catch {
        renameSync(dest, `${dest}.old-${Date.now()}`);
      }
    }
    copyFileSync(src, dest);
    console.log(`install-mnemo2-mcp: ${dest}`);
    return true;
  } catch (error) {
    console.error(`install-mnemo2-mcp: could not install ${dest}: ${error.message}`);
    return false;
  }
}

function install(binDir) {
  mkdirSync(binDir, { recursive: true });
  for (const name of readdirSync(binDir)) {
    if (name.includes(".old-")) {
      try {
        rmSync(join(binDir, name));
      } catch {
        // Still held by a running session; a later install removes it.
      }
    }
  }
  if (!copyWithRollover(helper, join(binDir, `mnemo2-mcp${exe}`))) return false;
  try {
    writeFileSync(join(binDir, "mnemo2-path.txt"), `${app}\n`);
    return true;
  } catch (error) {
    console.error(`install-mnemo2-mcp: could not write ${join(binDir, "mnemo2-path.txt")}: ${error.message}`);
    return false;
  }
}

// Install both before failing, so one locked folder does not skip the other.
// The models host goes only into <config>/bin — the plugin never runs it
// (the .old- sweep in install() above already cleaned that directory).
const results = [
  install(join(root, "mnemo2", "plugin", "bin")),
  install(join(configDir, "bin")),
  copyWithRollover(host, join(configDir, "bin", `mnemo-models${exe}`)),
];
console.log(`  starts ${app}`);

// The command-line `claude` runs the Mnemo plugin from the copy
// `claude plugin install` made in its plugin cache, so refresh that copy's
// hooks, skills and manifest too (plan 4d920eec decision 9); the Claude
// desktop app reads the plugin straight from this repository. Best effort:
// a missing cache or file is skipped silently.
function refreshInstalledPlugin() {
  const registry = join(homedir(), ".claude", "plugins", "installed_plugins.json");
  if (!existsSync(registry)) return;
  let installs;
  try {
    installs = JSON.parse(readFileSync(registry, "utf8")).plugins ?? {};
  } catch {
    return;
  }
  const source = join(root, "mnemo2", "plugin");
  for (const [key, entries] of Object.entries(installs)) {
    if (!key.startsWith("mnemo@")) continue;
    for (const entry of Array.isArray(entries) ? entries : [entries]) {
      const target = entry?.installPath;
      if (!target || !existsSync(target) || resolve(target) === resolve(source)) continue;
      for (const name of ["hooks", "skills", ".claude-plugin", ".mcp.json", "README.md"]) {
        const from = join(source, name);
        if (!existsSync(from)) continue;
        try {
          cpSync(from, join(target, name), { recursive: true, force: true });
        } catch (error) {
          console.error(`install-mnemo2-mcp: could not refresh ${join(target, name)}: ${error.message}`);
        }
      }
      console.log(`install-mnemo2-mcp: refreshed the installed plugin copy ${target}`);
    }
  }
}
refreshInstalledPlugin();

if (results.includes(false)) process.exit(1);
