// Install the freshly built `mnemo2-mcp` helper for the "mnemo" Claude Code
// plugin.
//
// - `mnemo2/plugin/bin/` is what the plugin's MCP server and hooks run
//   (`${CLAUDE_PLUGIN_ROOT}/bin/mnemo2-mcp`). `claude plugin install` copies
//   the plugin into Claude Code's plugin cache, so an installed plugin keeps
//   the helper it was installed with.
// - `<config>/bin/` ($MNEMO_HOME or ~/.mnemo) holds the current helper. Any
//   older copy (e.g. the cached plugin's) hands its arguments to this one, so
//   a rebuild takes effect without reinstalling the plugin.
// - A running Claude session keeps the old helper open, and Windows refuses to
//   overwrite a running executable. The old file is renamed aside first;
//   leftovers from earlier runs are removed once they are no longer locked.
// - The helper starts Mnemo2 itself when no instance serves the project, so
//   `mnemo2-path.txt` records where the matching `mnemo2` executable lives.
//
// Usage: node scripts/install-mnemo2-mcp.mjs [release|debug]   (default release)

import { copyFileSync, existsSync, mkdirSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const profile = process.argv[2] === "debug" ? "debug" : "release";
const exe = process.platform === "win32" ? ".exe" : "";
// src-tauri is a workspace member, so cargo builds into the workspace target.
const targetDir = join(process.env.CARGO_TARGET_DIR || join(root, "target"), profile);
const helper = join(targetDir, `mnemo2-mcp${exe}`);
const app = join(targetDir, `mnemo2${exe}`);
const configDir = process.env.MNEMO_HOME || join(homedir(), ".mnemo");

if (!existsSync(helper)) {
  console.error(`install-mnemo2-mcp: ${helper} is missing; build it with --bin mnemo2-mcp first.`);
  process.exit(1);
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
  const dest = join(binDir, `mnemo2-mcp${exe}`);
  try {
    if (existsSync(dest)) {
      try {
        rmSync(dest);
      } catch {
        renameSync(dest, `${dest}.old-${Date.now()}`);
      }
    }
    copyFileSync(helper, dest);
    writeFileSync(join(binDir, "mnemo2-path.txt"), `${app}\n`);
    console.log(`install-mnemo2-mcp: ${dest}`);
    return true;
  } catch (error) {
    console.error(`install-mnemo2-mcp: could not install ${dest}: ${error.message}`);
    return false;
  }
}

// Install both before failing, so one locked folder does not skip the other.
const results = [install(join(root, "mnemo2", "plugin", "bin")), install(join(configDir, "bin"))];
console.log(`  starts ${app}`);
if (results.includes(false)) process.exit(1);
