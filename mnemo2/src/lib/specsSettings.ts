import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/**
 * Specs settings for the Specs page: the transport to the
 * `specs_settings` commands, the wire shapes they return (mirroring
 * `specs_settings.rs`), defensive normalization for the page, and the pure
 * helpers the status and artifact panels render with.
 *
 * Per-project data: everything here lives under the open project's
 * `.coding/openspec/` (schemas, `config.yaml`, specs). Without an open
 * project the page shows its empty state; without a Tauri host (a plain
 * browser) the dialog shows its empty state.
 */

// ── Wire shapes (mirroring src-tauri/src/mnemo2/specs_settings.rs) ───────────

/** One capability of the status panel: count or format error. */
export interface CapabilityStatusWire {
  name: string;
  requirements: number;
  format_error: string | null;
}

/** One open plan whose spec changes have not been applied yet. */
export interface PendingSpecPlanWire {
  plan_id: string;
  title: string;
  capabilities: string[];
}

/** The `specs_status` payload. */
export interface SpecsStatusWire {
  capabilities: CapabilityStatusWire[];
  pending_plans: PendingSpecPlanWire[];
  openspec_cli: boolean;
}

/** One schema of the list, loadable or not. */
export interface SchemaSummaryWire {
  name: string;
  reference: string;
  description: string;
  origin: string;
  path: string;
  artifacts: string[];
  warnings: string[];
  error: string | null;
}

/** The `list_schemas` payload, with the active schema key. */
export interface SchemaListWire {
  schemas: SchemaSummaryWire[];
  active: string;
  active_error: string | null;
}

/** One artifact of the template editor. */
export interface ArtifactViewWire {
  id: string;
  generates: string;
  template: string;
  instruction: string;
  requires: string[];
  template_text: string;
}

/** The `read_artifacts` payload for one schema. */
export interface ArtifactsViewWire {
  name: string;
  reference: string;
  origin: string;
  description: string;
  artifacts: ArtifactViewWire[];
}

/** The `read_config` payload. */
export interface SpecsConfigWire {
  schema: string | null;
  context: string;
  rules: Record<string, string[]>;
  warnings: string[];
}

/** One plan section of the preview. */
export interface PlanPreviewSectionWire {
  id: string;
  generates: string;
  requires: string[];
  instruction: string;
  template_text: string;
  required_for_dispatch: boolean;
}

/** The `plan_preview` payload. */
export interface PlanPreviewWire {
  name: string;
  reference: string;
  description: string;
  sections: PlanPreviewSectionWire[];
  required: string[];
  worker_context: string;
}

/** The `run_openspec_validate` payload. */
export interface ValidateOutputWire {
  exit_code: number | null;
  stdout: string;
  stderr: string;
}

// ── Page shapes ──────────────────────────────────────────────────────────────

/** One capability row of the status panel. */
export type CapabilityStatus = CapabilityStatusWire;

/** One pending-plan row of the status panel. */
export type PendingSpecPlan = PendingSpecPlanWire;

/** The status panel's data. */
export type SpecsStatus = SpecsStatusWire;

/** One schema of the list. */
export type SchemaSummary = SchemaSummaryWire;

/** The schema list, with the active schema key. */
export type SchemaList = SchemaListWire;

/** One artifact of the template editor. */
export type ArtifactView = ArtifactViewWire;

/** The artifact editor's data for one schema. */
export type ArtifactsView = ArtifactsViewWire;

/** The project config the page edits. */
export type SpecsConfig = SpecsConfigWire;

/** One plan section of the preview. */
export type PlanPreviewSection = PlanPreviewSectionWire;

/** The plan preview. */
export type PlanPreview = PlanPreviewWire;

/** The `openspec validate` output. */
export type ValidateOutput = ValidateOutputWire;

// ── Transport ────────────────────────────────────────────────────────────────

/** The Tauri bindings the transport invokes through (injected for tests). */
export interface SpecsTransportBindings {
  invoke: typeof invoke;
}

/** The Specs page's backend: one method per `specs_settings` command. */
export interface SpecsTransport {
  status(): Promise<SpecsStatus>;
  listSchemas(): Promise<SchemaList>;
  setActiveSchema(name: string): Promise<SchemaList>;
  addSchema(source: string): Promise<SchemaList>;
  forkSchema(from: string, to: string): Promise<SchemaList>;
  readArtifacts(name: string): Promise<ArtifactsView>;
  saveArtifact(name: string, id: string, template: string, instruction: string): Promise<ArtifactsView>;
  readConfig(): Promise<SpecsConfig>;
  saveConfig(context: string, rules: Record<string, string[]>): Promise<SpecsConfig>;
  planPreview(name: string): Promise<PlanPreview>;
  runValidate(): Promise<ValidateOutput>;
}

// ── Normalization ────────────────────────────────────────────────────────────

const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string") : [];

const text = (value: unknown): string => (typeof value === "string" ? value : "");

const optionalText = (value: unknown): string | null =>
  typeof value === "string" && value.length > 0 ? value : null;

const number = (value: unknown): number => (typeof value === "number" ? value : 0);

/** Default a `specs_status` payload — hostile or missing fields included — into the page's shape. */
export function normalizeSpecsStatus(wire: unknown): SpecsStatus {
  const raw = (wire ?? {}) as Partial<SpecsStatusWire>;
  return {
    capabilities: Array.isArray(raw.capabilities)
      ? raw.capabilities.map((capability) => ({
          name: text(capability?.name),
          requirements: number(capability?.requirements),
          format_error: optionalText(capability?.format_error),
        }))
      : [],
    pending_plans: Array.isArray(raw.pending_plans)
      ? raw.pending_plans.map((plan) => ({
          plan_id: text(plan?.plan_id),
          title: text(plan?.title),
          capabilities: strings(plan?.capabilities),
        }))
      : [],
    openspec_cli: raw.openspec_cli === true,
  };
}

/** Default a `list_schemas` payload — hostile or missing fields included — into the page's shape. */
export function normalizeSchemaList(wire: unknown): SchemaList {
  const raw = (wire ?? {}) as Partial<SchemaListWire>;
  return {
    schemas: Array.isArray(raw.schemas)
      ? raw.schemas.map((schema) => ({
          name: text(schema?.name),
          reference: text(schema?.reference),
          description: text(schema?.description),
          origin: text(schema?.origin),
          path: text(schema?.path),
          artifacts: strings(schema?.artifacts),
          warnings: strings(schema?.warnings),
          error: optionalText(schema?.error),
        }))
      : [],
    active: text(raw.active) || "spec-driven",
    active_error: optionalText(raw.active_error),
  };
}

/** Default a `read_artifacts` payload — hostile or missing fields included — into the page's shape. */
export function normalizeArtifacts(wire: unknown): ArtifactsView {
  const raw = (wire ?? {}) as Partial<ArtifactsViewWire>;
  return {
    name: text(raw.name),
    reference: text(raw.reference),
    origin: text(raw.origin),
    description: text(raw.description),
    artifacts: Array.isArray(raw.artifacts)
      ? raw.artifacts.map((artifact) => ({
          id: text(artifact?.id),
          generates: text(artifact?.generates),
          template: text(artifact?.template),
          instruction: text(artifact?.instruction),
          requires: strings(artifact?.requires),
          template_text: text(artifact?.template_text),
        }))
      : [],
  };
}

/** Default a `read_config` payload — hostile or missing fields included — into the page's shape. */
export function normalizeConfig(wire: unknown): SpecsConfig {
  const raw = (wire ?? {}) as Partial<SpecsConfigWire>;
  const rules: Record<string, string[]> = {};
  if (raw.rules && typeof raw.rules === "object") {
    for (const [key, value] of Object.entries(raw.rules)) {
      rules[key] = strings(value);
    }
  }
  return {
    schema: optionalText(raw.schema),
    context: text(raw.context),
    rules,
    warnings: strings(raw.warnings),
  };
}

/** Default a `plan_preview` payload — hostile or missing fields included — into the page's shape. */
export function normalizePreview(wire: unknown): PlanPreview {
  const raw = (wire ?? {}) as Partial<PlanPreviewWire>;
  return {
    name: text(raw.name),
    reference: text(raw.reference),
    description: text(raw.description),
    sections: Array.isArray(raw.sections)
      ? raw.sections.map((section) => ({
          id: text(section?.id),
          generates: text(section?.generates),
          requires: strings(section?.requires),
          instruction: text(section?.instruction),
          template_text: text(section?.template_text),
          required_for_dispatch: section?.required_for_dispatch === true,
        }))
      : [],
    required: strings(raw.required),
    worker_context: text(raw.worker_context),
  };
}

/** Default a `run_openspec_validate` payload — hostile or missing fields included — into the page's shape. */
export function normalizeValidateOutput(wire: unknown): ValidateOutput {
  const raw = (wire ?? {}) as Partial<ValidateOutputWire>;
  return {
    exit_code: typeof raw.exit_code === "number" ? raw.exit_code : null,
    stdout: text(raw.stdout),
    stderr: text(raw.stderr),
  };
}

// ── Pure helpers the page renders with ────────────────────────────────────────

/** One-line labels for the three schema origins. */
export const ORIGIN_META: Record<string, { label: string; hint: string }> = {
  "built-in": { label: "Built-in", hint: "Ships with Mnemo — read-only, fork it to edit." },
  project: { label: "This project", hint: "Lives in .coding/openspec/schemas/ — editable." },
  user: { label: "User schemas", hint: "Shared across projects — fork it to edit here." },
};

/** The one-line status of a capability for the status panel. */
export function capabilityLine(capability: CapabilityStatus): string {
  if (capability.format_error) {
    return `${capability.name} — ${capability.format_error}`;
  }
  const count = capability.requirements;
  return `${capability.name} — ${count} ${count === 1 ? "requirement" : "requirements"}`;
}

/** The one-line status of a pending plan for the status panel. */
export function pendingPlanLine(plan: PendingSpecPlan): string {
  const capabilities = plan.capabilities.join(", ");
  return `${plan.title} (${capabilities})`;
}

/** The schema summary with the given key, when listed. */
export function schemaBy(list: SchemaList, name: string): SchemaSummary | undefined {
  return list.schemas.find((schema) => schema.name === name);
}

/** Whether an artifact edit differs from what is stored. */
export function artifactDirty(
  artifact: ArtifactView,
  template: string,
  instruction: string,
): boolean {
  return artifact.template_text !== template || artifact.instruction !== instruction;
}

/**
 * The preview of a template edit: the plan section the lead would see —
 * the section heading the board renders under, plus the template body
 * the section starts from.
 */
export function planSectionPreview(id: string, templateText: string): string {
  return `## ${id}\n\n${templateText}`;
}

/** The one-line summary of a `openspec validate` run. */
export function validateLine(output: ValidateOutput): string {
  if (output.exit_code === null) return "openspec validate could not be waited for";
  return `openspec validate exited with ${output.exit_code === 0 ? "success" : `code ${output.exit_code}`}`;
}

// ── Transport construction ───────────────────────────────────────────────────

/** The real IPC transport over the Tauri bindings. */
export function createTauriSpecsTransport(bindings: SpecsTransportBindings): SpecsTransport {
  return {
    status: () => bindings.invoke("specs_status").then(normalizeSpecsStatus),
    listSchemas: () => bindings.invoke("list_schemas").then(normalizeSchemaList),
    setActiveSchema: (name) =>
      bindings.invoke("set_active_schema", { name }).then(normalizeSchemaList),
    addSchema: (source) =>
      bindings.invoke("add_schema", { source }).then(normalizeSchemaList),
    forkSchema: (from, to) =>
      bindings.invoke("fork_schema", { from, to }).then(normalizeSchemaList),
    readArtifacts: (name) => bindings.invoke("read_artifacts", { name }).then(normalizeArtifacts),
    saveArtifact: (name, id, template, instruction) =>
      bindings
        .invoke("save_artifact", { name, id, template, instruction })
        .then(normalizeArtifacts),
    readConfig: () => bindings.invoke("read_config").then(normalizeConfig),
    saveConfig: (context, rules) =>
      bindings.invoke("save_config", { context, rules }).then(normalizeConfig),
    planPreview: (name) => bindings.invoke("plan_preview", { name }).then(normalizePreview),
    runValidate: () => bindings.invoke("run_openspec_validate").then(normalizeValidateOutput),
  };
}

// ── Preview seam (mirrors lib/layaSettings.ts) ──────────────────────────────

/** The dev-only transport `src/test` seeds install in a browser. */
let previewSpecsTransport: SpecsTransport | null = null;

/** Install the preview transport (dev-only; no Tauri host in a browser). */
export function setPreviewSpecsTransport(transport: SpecsTransport | null): void {
  previewSpecsTransport = transport;
}

/**
 * The Specs page's transport: the Tauri one inside the desktop app, the
 * preview one when a test-side seed installed it, null in a plain browser
 * (the dialog shows its empty state).
 */
export function createSpecsTransport(bindings?: SpecsTransportBindings): SpecsTransport | null {
  if (previewSpecsTransport) return previewSpecsTransport;
  if (!isTauriRuntime()) return null;
  return createTauriSpecsTransport(bindings ?? { invoke });
}
