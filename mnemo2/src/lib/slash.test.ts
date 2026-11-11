// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import { describe, expect, it } from "vitest";
import {
  filterSlashCommands,
  parseSlash,
  resolveModel,
  SLASH_COMMANDS,
  type SlashCommand,
} from "./slash";
import type { LeadModel } from "./types";

const model = (overrides: Partial<LeadModel> = {}): LeadModel => ({
  endpoint: "test",
  model: "scripted-a",
  spec: "test/scripted-a",
  is_default: false,
  ...overrides,
});

describe("parseSlash", () => {
  it("parses the argument-less commands and folds /clear into /new", () => {
    expect(parseSlash("/compact")).toEqual<SlashCommand>({ kind: "compact" });
    expect(parseSlash("/NEW ")).toEqual<SlashCommand>({ kind: "new" });
    expect(parseSlash("  /New  ")).toEqual<SlashCommand>({ kind: "new" });
    expect(parseSlash("/clear")).toEqual<SlashCommand>({ kind: "new" });
    expect(parseSlash("/stop")).toEqual<SlashCommand>({ kind: "stop" });
    expect(parseSlash("/help")).toEqual<SlashCommand>({ kind: "help" });
  });

  it("rejects extra text on a command that takes no arguments", () => {
    expect(parseSlash("/compact now")).toEqual<SlashCommand>({
      kind: "invalid",
      message: "/compact takes no arguments.",
    });
    expect(parseSlash("/new please")).toEqual<SlashCommand>({
      kind: "invalid",
      message: "/new takes no arguments.",
    });
  });

  it("parses /model with no name, a name, and a name plus effort", () => {
    expect(parseSlash("/model")).toEqual<SlashCommand>({
      kind: "model",
      name: "",
      effort: null,
    });
    expect(parseSlash("/model glm")).toEqual<SlashCommand>({
      kind: "model",
      name: "glm",
      effort: null,
    });
    expect(parseSlash("/MODEL glm High")).toEqual<SlashCommand>({
      kind: "model",
      name: "glm",
      effort: "High",
    });
  });

  it("rejects /model with more than a name and an effort", () => {
    expect(parseSlash("/model a b c")).toEqual<SlashCommand>({
      kind: "invalid",
      message: "Usage: /model <name> [effort]",
    });
  });

  it("leaves anything else as an ordinary message", () => {
    expect(parseSlash("/usr/bin is missing")).toBeNull();
    expect(parseSlash("/unknown")).toBeNull();
    expect(parseSlash("hello")).toBeNull();
    expect(parseSlash("")).toBeNull();
    expect(parseSlash("/")).toBeNull();
  });
});

describe("filterSlashCommands", () => {
  it("offers every command for a bare slash", () => {
    expect(filterSlashCommands("/")).toEqual(SLASH_COMMANDS);
    expect(SLASH_COMMANDS.map((command) => command.name)).toEqual([
      "new",
      "clear",
      "compact",
      "model",
      "stop",
      "help",
    ]);
  });

  it("narrows to the command names still being typed", () => {
    expect(filterSlashCommands("/c")).toEqual(
      SLASH_COMMANDS.filter((command) => command.name.startsWith("c")),
    );
    expect(filterSlashCommands("/c").map((command) => command.name)).toEqual([
      "clear",
      "compact",
    ]);
    expect(filterSlashCommands("/h").map((command) => command.name)).toEqual(["help"]);
  });

  it("steps aside once the draft is not a command name anymore", () => {
    expect(filterSlashCommands("/model x")).toEqual([]);
    expect(filterSlashCommands("hi")).toEqual([]);
    expect(filterSlashCommands(" /c")).toEqual([]);
    expect(filterSlashCommands("")).toEqual([]);
  });
});

describe("resolveModel", () => {
  const models = [
    model({ model: "scripted-a", spec: "test/scripted-a" }),
    model({ endpoint: "other", model: "glm", spec: "other/glm" }),
    model({ endpoint: "other", model: "glm-air", spec: "other/glm-air" }),
  ];

  it("resolves an exact spec first", () => {
    expect(resolveModel("test/scripted-a", models)).toEqual({ spec: "test/scripted-a" });
    expect(resolveModel("TEST/SCRIPTED-A", models)).toEqual({ spec: "test/scripted-a" });
  });

  it("resolves a bare model id when exactly one model carries it", () => {
    expect(resolveModel("scripted-a", models)).toEqual({ spec: "test/scripted-a" });
    expect(resolveModel("GLM", models)).toEqual({ spec: "other/glm" });
  });

  it("resolves a unique spec substring", () => {
    expect(resolveModel("glm-air", models)).toEqual({ spec: "other/glm-air" });
    expect(resolveModel("air", models)).toEqual({ spec: "other/glm-air" });
  });

  it("reports an ambiguous name with every matching spec", () => {
    expect(resolveModel("gl", models)).toEqual({
      error: `"gl" matches several models: other/glm, other/glm-air — type more of the name.`,
    });
  });

  it("reports an unknown name with every configured spec", () => {
    expect(resolveModel("nothing", models)).toEqual({
      error: `No model matches "nothing". Models: test/scripted-a, other/glm, other/glm-air`,
    });
  });

  it("reports when no model is configured at all", () => {
    expect(resolveModel("glm", [])).toEqual({ error: "No models are configured." });
  });
});
