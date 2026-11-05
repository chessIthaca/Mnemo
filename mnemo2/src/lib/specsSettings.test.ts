import { afterEach, describe, expect, it, vi } from "vitest";
import {
  artifactDirty,
  capabilityLine,
  createSpecsTransport,
  createTauriSpecsTransport,
  normalizeArtifacts,
  normalizeConfig,
  normalizePreview,
  normalizeSchemaList,
  normalizeSpecsStatus,
  normalizeValidateOutput,
  planSectionPreview,
  pendingPlanLine,
  schemaBy,
  setPreviewSpecsTransport,
  validateLine,
  type ArtifactView,
  type SchemaListWire,
  type SpecsTransport,
} from "./specsSettings";

/** A schema list wire: the built-in active, one project schema, one broken. */
const SCHEMAS: SchemaListWire = {
  schemas: [
    {
      name: "spec-driven",
      reference: "spec-driven@1",
      description: "Default OpenSpec workflow",
      origin: "built-in",
      path: "(built-in)",
      artifacts: ["proposal", "specs", "design", "tasks"],
      warnings: [],
      error: null,
    },
    {
      name: "my-flow",
      reference: "my-flow@1",
      description: "The fork we edit",
      origin: "project",
      path: "C:/proj/.coding/openspec/schemas/my-flow",
      artifacts: ["proposal"],
      warnings: [],
      error: null,
    },
    {
      name: "broken",
      reference: "",
      description: "",
      origin: "project",
      path: "C:/proj/.coding/openspec/schemas/broken",
      artifacts: [],
      warnings: [],
      error: "could not read 'schema.yaml'",
    },
  ],
  active: "spec-driven",
  active_error: null,
};

/** One artifact wire as the backend sends it. */
const ARTIFACT: ArtifactView = {
  id: "proposal",
  generates: "proposal.md",
  template: "proposal.md",
  instruction: "Explain why.",
  requires: [],
  template_text: "# Why\n",
};

/** A mock invoke answering one command payload. */
function invokeAnswering(payload: unknown): ReturnType<typeof vi.fn> {
  return vi.fn(async () => payload);
}

describe("specsSettings", () => {
  afterEach(() => setPreviewSpecsTransport(null));

  it("lists the schemas and marks the active pick", async () => {
    const invoke = invokeAnswering(SCHEMAS);
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    const list = await transport.listSchemas();
    expect(invoke).toHaveBeenCalledWith("list_schemas");
    expect(list.active).toBe("spec-driven");
    expect(list.schemas.map((schema) => schema.name)).toEqual([
      "spec-driven",
      "my-flow",
      "broken",
    ]);
    // The pick helpers: the schema by key, loadable ones for pickers.
    expect(schemaBy(list, "my-flow")?.origin).toBe("project");
    expect(schemaBy(list, "missing")).toBeUndefined();
    expect(list.schemas.filter((schema) => !schema.error).length).toBe(2);
  });

  it("picks the active schema through set_active_schema", async () => {
    const invoke = invokeAnswering(SCHEMAS);
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    const list = await transport.setActiveSchema("my-flow");
    expect(invoke).toHaveBeenCalledWith("set_active_schema", { name: "my-flow" });
    expect(list.active).toBe("spec-driven");
  });

  it("shows the add-validation error and copies nothing", async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === "add_schema") {
        throw "C:/in/schema.yaml: artifact 'research' template 'templates/research.md': not found";
      }
      return SCHEMAS;
    });
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    // The transport surfaces the backend error untouched so the page can
    // show it; the schema list is left as it was.
    await expect(transport.addSchema("C:/in")).rejects.toContain("research.md");
    expect(invoke).toHaveBeenCalledWith("add_schema", { source: "C:/in" });
    const after = await transport.listSchemas();
    expect(after.schemas.some((schema) => schema.name === "research")).toBe(false);
  });

  it("forks through fork_schema with both names", async () => {
    const invoke = invokeAnswering(SCHEMAS);
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    await transport.forkSchema("spec-driven", "my-flow");
    expect(invoke).toHaveBeenCalledWith("fork_schema", {
      from: "spec-driven",
      to: "my-flow",
    });
  });

  it("reads and saves artifacts, mapping every command field", async () => {
    const invoke = invokeAnswering({
      name: "my-flow",
      reference: "my-flow@1",
      origin: "project",
      description: "The fork",
      artifacts: [ARTIFACT],
    });
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    const view = await transport.readArtifacts("my-flow");
    expect(invoke).toHaveBeenCalledWith("read_artifacts", { name: "my-flow" });
    expect(view.artifacts[0]).toEqual(ARTIFACT);

    await transport.saveArtifact("my-flow", "proposal", "# Why\n\nMore", "Explain.");
    expect(invoke).toHaveBeenCalledWith("save_artifact", {
      name: "my-flow",
      id: "proposal",
      template: "# Why\n\nMore",
      instruction: "Explain.",
    });
  });

  it("tracks the template edit and previews the plan section", async () => {
    const artifact = ARTIFACT;
    // The edit is dirty before saving, clean after the drafts match.
    expect(artifactDirty(artifact, "# Why\n\nMore", "Explain why.")).toBe(true);
    expect(artifactDirty(artifact, "# Why\n", "Explain.")).toBe(true);
    expect(artifactDirty(artifact, "# Why\n", "Explain why.")).toBe(false);
    // The preview shows the section the plan would render, not just the
    // raw template.
    expect(planSectionPreview("proposal", "# Why\n\nMore")).toBe(
      "## proposal\n\n# Why\n\nMore",
    );
  });

  it("round-trips the config context and rules", async () => {
    const invoke = invokeAnswering({
      schema: "my-flow",
      context: "The conventions.",
      rules: { research: ["Cite sources."] },
      warnings: [],
    });
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    const loaded = await transport.readConfig();
    expect(invoke).toHaveBeenCalledWith("read_config");
    expect(loaded.context).toBe("The conventions.");
    expect(loaded.rules).toEqual({ research: ["Cite sources."] });
    await transport.saveConfig("New.", { proposal: ["Name the risk."] });
    expect(invoke).toHaveBeenCalledWith("save_config", {
      context: "New.",
      rules: { proposal: ["Name the risk."] },
    });
  });

  it("previews a schema's plan sections and worker context", async () => {
    const invoke = invokeAnswering({
      name: "my-flow",
      reference: "my-flow@1",
      description: "The fork",
      sections: [
        { ...ARTIFACT, requires: [], template_text: "# Why\n", required_for_dispatch: true },
      ],
      required: ["proposal"],
      worker_context: "\n## Project context\n\nThe conventions.\n",
    });
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    const preview = await transport.planPreview("my-flow");
    expect(invoke).toHaveBeenCalledWith("plan_preview", { name: "my-flow" });
    expect(preview.sections[0].id).toBe("proposal");
    expect(preview.sections[0].required_for_dispatch).toBe(true);
    expect(preview.worker_context).toContain("## Project context");
  });

  it("shapes the status panel and its one-liners", async () => {
    const invoke = invokeAnswering({
      capabilities: [
        { name: "auth", requirements: 2, format_error: null },
        { name: "broken", requirements: 0, format_error: "spec.md is missing" },
      ],
      pending_plans: [
        { plan_id: "abcdef12", title: "Rate limit", capabilities: ["auth"] },
      ],
      openspec_cli: true,
    });
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    const status = await transport.status();
    expect(invoke).toHaveBeenCalledWith("specs_status");
    expect(status.openspec_cli).toBe(true);
    expect(capabilityLine(status.capabilities[0])).toBe("auth — 2 requirements");
    expect(capabilityLine(status.capabilities[1])).toBe("broken — spec.md is missing");
    expect(pendingPlanLine(status.pending_plans[0])).toBe("Rate limit (auth)");
  });

  it("runs openspec validate only through the command", async () => {
    const invoke = invokeAnswering({ exit_code: 1, stdout: "", stderr: "no changes" });
    const transport = createTauriSpecsTransport({ invoke: invoke as never });
    const output = await transport.runValidate();
    expect(invoke).toHaveBeenCalledWith("run_openspec_validate");
    expect(validateLine(output)).toBe("openspec validate exited with code 1");
  });

  it("has no transport outside the native shell", () => {
    expect(createSpecsTransport()).toBeNull();
  });

  it("serves the planted preview transport outside the native shell", async () => {
    const seeded: SpecsTransport = {
      status: vi.fn(async () => normalizeSpecsStatus(null)),
      listSchemas: vi.fn(async () => normalizeSchemaList(null)),
      setActiveSchema: vi.fn(async () => normalizeSchemaList(null)),
      addSchema: vi.fn(async () => normalizeSchemaList(null)),
      forkSchema: vi.fn(async () => normalizeSchemaList(null)),
      readArtifacts: vi.fn(async () => normalizeArtifacts(null)),
      saveArtifact: vi.fn(async () => normalizeArtifacts(null)),
      readConfig: vi.fn(async () => normalizeConfig(null)),
      saveConfig: vi.fn(async () => normalizeConfig(null)),
      planPreview: vi.fn(async () => normalizePreview(null)),
      runValidate: vi.fn(async () => normalizeValidateOutput(null)),
    };
    setPreviewSpecsTransport(seeded);
    expect(createSpecsTransport()).toBe(seeded);
    await expect(seeded.listSchemas()).resolves.toEqual({
      schemas: [],
      active: "spec-driven",
      active_error: null,
    });
  });

  it("defaults a hostile wire into the page shape", () => {
    const status = normalizeSpecsStatus({
      capabilities: [
        // A capability with nothing but a name still renders.
        { name: "auth" },
      ],
      pending_plans: [{ title: "Rate limit" }],
    });
    expect(status.capabilities[0]).toEqual({
      name: "auth",
      requirements: 0,
      format_error: null,
    });
    expect(status.capabilities[0].requirements).toBe(0);
    expect(status.openspec_cli).toBe(false);

    const list = normalizeSchemaList(null);
    expect(list.active).toBe("spec-driven");
    expect(list.schemas).toEqual([]);

    const config = normalizeConfig(null);
    expect(config.context).toBe("");
    expect(config.rules).toEqual({});
    expect(config.schema).toBeNull();

    const preview = normalizePreview(null);
    expect(preview.sections).toEqual([]);
    expect(preview.worker_context).toBe("");

    const output = normalizeValidateOutput(null);
    expect(output.exit_code).toBeNull();
    expect(output.stdout).toBe("");

    const artifacts = normalizeArtifacts(null);
    expect(artifacts.artifacts).toEqual([]);
  });
});
