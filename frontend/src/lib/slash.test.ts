// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Regression suite for the slash-command Enter bug (plan cb91d6ea): typing a
 * fully-typed argument-less command like "/compact" and pressing Enter never
 * ran it — completeSlashCommand's immediate-run arm was hardcoded to
 * clear|panel|help, so /compact and /new completed to "/compact " (trailing
 * space) and the hasArgs check trimmed that space away, re-completing
 * forever. The fix is data-driven: SlashCommandInfo.takesArgument marks
 * which commands take arguments, and resolveArglessCommand recognizes a
 * fully-typed argument-less command (trailing whitespace tolerated) so the
 * InputBar's Enter branch can run it instead of re-completing.
 */

import { describe, expect, it } from "vitest";
import { parseSlash, SLASH_COMMANDS, resolveArglessCommand } from "./slash";

/**
 * The regression test for the Enter bug (plan cb91d6ea): both stranding
 * states must resolve — "/compact" fully typed by the user, and the
 * completed "/compact " (trailing space) that the old Enter handler
 * re-completed forever instead of running. A named function (not an
 * inline arrow) so the code graph indexes it as a symbol and the bug
 * plan's regression-test gate can point at the actual test function.
 */
function resolvesExactLoopStates(): void {
  expect(resolveArglessCommand("/compact")?.name).toBe("compact");
  expect(resolveArglessCommand("/compact ")?.name).toBe("compact");
  expect(resolveArglessCommand("/new")?.name).toBe("new");
  expect(resolveArglessCommand("/new ")?.name).toBe("new");
}

describe("resolveArglessCommand (Enter runs fully-typed argument-less commands)", () => {
  it(
    "resolves the exact states the pre-fix loop stranded (regression)",
    resolvesExactLoopStates,
  );

  it("resolves every argument-less command, with surrounding whitespace", () => {
    for (const name of ["clear", "compact", "new", "panel", "help"]) {
      expect(resolveArglessCommand(`/${name}`)?.name).toBe(name);
      expect(resolveArglessCommand(`  /${name}  `)?.name).toBe(name);
    }
  });

  it("does not resolve argument-taking commands (Enter completes them for argument entry)", () => {
    expect(resolveArglessCommand("/model")).toBeNull();
    expect(resolveArglessCommand("/provider")).toBeNull();
    expect(resolveArglessCommand("/save")).toBeNull();
    expect(resolveArglessCommand("/load")).toBeNull();
  });

  it("does not resolve partial names, args, or non-slash input", () => {
    expect(resolveArglessCommand("/c")).toBeNull();
    expect(resolveArglessCommand("/comp")).toBeNull();
    expect(resolveArglessCommand("/compact x")).toBeNull();
    expect(resolveArglessCommand("compact")).toBeNull();
    expect(resolveArglessCommand("/")).toBeNull();
    expect(resolveArglessCommand("")).toBeNull();
    // Case-sensitive, mirroring parseSlash's switch ("/Compact" is unknown).
    expect(resolveArglessCommand("/Compact")).toBeNull();
  });
});

describe("SLASH_COMMANDS takesArgument metadata", () => {
  it("marks exactly the argument-less commands", () => {
    const argless = SLASH_COMMANDS.filter((c) => !c.takesArgument).map(
      (c) => c.name,
    );
    expect([...argless].sort()).toEqual(["clear", "compact", "help", "new", "panel"]);
  });

  it("every command declares the flag", () => {
    for (const c of SLASH_COMMANDS) {
      expect(typeof c.takesArgument).toBe("boolean");
    }
  });

  it("parseSlash parses every argument-less command name (completeSlashCommand invokes it directly)", () => {
    // completeSlashCommand's immediate-run arm runs
    // handleSlashCommand(parseSlash(`/${cmd.name}`)) — every argument-less
    // name must parse to its own type or the direct invocation is a no-op.
    for (const c of SLASH_COMMANDS.filter((cmd) => !cmd.takesArgument)) {
      expect(parseSlash(`/${c.name}`)).toEqual({ type: c.name });
    }
  });
});
