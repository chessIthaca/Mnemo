// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Classifier settings section (Laya, opt-in) — source-contract + pure-helper
 * tests in the style of ChatSection.test.ts (vitest runs in `node` env — no
 * React DOM infra).
 *
 * Pins the surfaces of the opt-in contract: the section is registered
 * (nav + deep-link id), it persists the config keys through the shared save
 * path, the managed-runtime controls (checkpoint catalog +
 * download progress) stay wired, and the frontend bindings stay wired to the
 * backend's exact names (commands, event, config + patch fields, snapshot
 * field).
 */

import { describe, expect, it } from "vitest";
import classifierSource from "./ClassifierSection.tsx?raw";
import dialogSource from "../SettingsDialog.tsx?raw";
import tauriSource from "../../../lib/tauri.ts?raw";
import {
  SETTINGS_NAV,
  isSettingsSectionId,
  serializeClassifier,
  type ClassifierDraft,
} from "../types";

describe("Classifier settings section placement", () => {
  it("registers 'classifier' as a valid Settings section id", () => {
    expect(isSettingsSectionId("classifier")).toBe(true);
  });

  it("places Classifier right after Embeddings in the nav", () => {
    const ids = SETTINGS_NAV.map((n) => n.id);
    expect(ids.indexOf("classifier")).toBe(ids.indexOf("embeddings") + 1);
  });

  it("classifier nav keywords surface the feature in Settings search", () => {
    const keywords =
      SETTINGS_NAV.find((n) => n.id === "classifier")?.keywords ?? [];
    const hay = keywords.join(" ");
    expect(hay).toContain("laya");
    expect(hay).toContain("classifier");
    expect(hay).toContain("system 1");
  });

  it("the dialog renders the section with a save handle", () => {
    expect(dialogSource).toContain(
      'import { ClassifierSection } from "./sections/ClassifierSection";',
    );
    expect(dialogSource).toContain("<ClassifierSection");
    expect(dialogSource).toContain("classifier: classifierRef,");
  });
});

describe("ClassifierSection owns the opt-in controls", () => {
  it("renders the enable toggle and the managed install card", () => {
    expect(classifierSource).toContain("Enable the Laya classifier");
    expect(classifierSource).toContain("Mnemo downloads and runs it");
  });


  it("wires the managed checkpoint catalog + download through the bindings", () => {
    expect(classifierSource).toContain("listLayaCheckpoints");
    expect(classifierSource).toContain("setupLayaRuntime");
    expect(classifierSource).toContain("Download (~{c.size_mb} MB)");
    expect(classifierSource).toContain("{c.id}</code> checkpoint");
  });

  it("shows download progress from the downloading status payload", () => {
    expect(classifierSource).toContain("typeof status === \"object\"");
    expect(classifierSource).toContain("fmtPct");
    expect(classifierSource).toContain(
      "next.downloading.progress",
    );
  });

  it("surfaces the honest disk-size note for the managed runtime", () => {
    expect(classifierSource).toContain("~0.8–1 GB");
  });

  it("shows the live status (incl. installing / starting) via the bindings", () => {
    expect(classifierSource).toContain("getClassifierStatus");
    expect(classifierSource).toContain("onClassifierStatus");
    expect(classifierSource).toContain("Disabled — no classifier backend exists");
    expect(classifierSource).toContain("Failed — the sidecar did not start");
    expect(classifierSource).toContain("Installing — preparing the managed Laya runtime");
    expect(classifierSource).toContain("Starting — launching the local laya-serve sidecar");
  });

  it("persists the opt-in through saveSettings (config.toml [general.laya])", () => {
    expect(classifierSource).toContain("laya_enabled: enabled");
    expect(classifierSource).toContain("laya_auto_type_memories: autoType");
    expect(classifierSource).toContain("laya_failure_triage: failureTriage");
    expect(classifierSource).toContain("laya_failure_triage_knn: failureTriageKnn");
    expect(classifierSource).toContain("laya_auto_finetune: autoFinetune");
  });

  it("keeps the dialog save contract (forwardRef + dirty callback)", () => {
    expect(classifierSource).toContain("forwardRef<SettingsSectionHandle");
    expect(classifierSource).toContain("onDirtyChange?.(dirty)");
    expect(classifierSource).toContain("save: async () => {");
  });

  it("renders the auto-typing opt-in toggle with the fine-tuned caveat", () => {
    expect(classifierSource).toContain("Auto-type memory records");
    expect(classifierSource).toContain("fine-tuned checkpoint");
  });

  it("renders the failure-triage, kNN-overlay + startup fine-tune opt-in toggles", () => {
    expect(classifierSource).toContain("Classify failures to steer retries");
    expect(classifierSource).toContain(
      "Learn from logged failures between fine-tunes (kNN)",
    );
    expect(classifierSource).toContain(
      "Fine-tune on startup from logged failures",
    );
    expect(classifierSource).toContain("checked={failureTriage}");
    expect(classifierSource).toContain("checked={failureTriageKnn}");
    expect(classifierSource).toContain("checked={autoFinetune}");
  });

  it("renders the pre-prompt routing block (opt-in + shadow/enforce + targets + gate)", () => {
    expect(classifierSource).toContain("Route turns by task complexity");
    expect(classifierSource).toContain(
      "Switch the model on a confident decision",
    );
    expect(classifierSource).toContain("Confidence gate");
    expect(classifierSource).toContain("Trivial tasks");
    expect(classifierSource).toContain("Architectural tasks");
    // The target pickers reuse the Models section's endpoint/model/effort
    // control; the shadow-first contract is spelled out in the block.
    expect(classifierSource).toContain("ModelPickerBody");
    expect(classifierSource).toContain("routing.jsonl");
    expect(classifierSource).toContain("Shadow-first");
  });

  it("renders the fine-tuning status shape (the startup fine-tune hot-swap)", () => {
    expect(classifierSource).toContain(
      "fine-tuning ${status.finetuning.label}",
    );
    expect(classifierSource).toContain(
      "Fine-tuning ${status.finetuning.label} on the logged failure",
    );
  });
});

describe("classifier bindings stay wired to the backend names", () => {
  it("the status command targets get_classifier_status", () => {
    expect(tauriSource).toContain('invoke("get_classifier_status")');
  });

  it("the status listener targets the classifier://status event", () => {
    expect(tauriSource).toContain(
      'listen<ClassifierStatusWire>("classifier://status"',
    );
  });

  it("the managed-runtime commands target laya_catalog + laya_setup", () => {
    expect(tauriSource).toContain('invoke("laya_catalog")');
    expect(tauriSource).toContain('invoke("laya_setup")');
  });

  it("the settings types carry the laya fields (config + save patch)", () => {
    expect(tauriSource).toContain(
      `laya?: {
      enabled: boolean;
      /** Whether memory auto-typing is enabled (opt-in). */
      auto_type_memories: boolean;
      /** Whether Laya tool-choice steering is enabled (opt-in; the
       *  search/search_read tools let a confident classifier pick the
       *  delegation class; needs a fine-tuned checkpoint). */
      steer_tool_choice: boolean;
      /** Whether failure triage is enabled (opt-in; needs a fine-tuned
       *  checkpoint). */
      failure_triage: boolean;
      /** Whether the kNN overlay for failure triage is enabled (opt-in;
       *  local — rides the memory embedder, needs no laya-serve, consulted
       *  only while failure_triage itself is on). */
      failure_triage_knn: boolean;
      /** Whether the startup failure-triage fine-tune is enabled (managed
       *  runtime only, opt-in). */
      auto_finetune: boolean;
      /** Whether pre-prompt model routing is enabled (opt-in; backlog
       *  091e694d; needs a fine-tuned checkpoint — base models are near-chance
       *  on this task). Shadow-first: decisions are classified + logged, and
       *  the turn's model only switches once \`[general.routing] enforce\` is
       *  on. */
      routing: boolean;
    };`
    );
    expect(tauriSource).toContain("laya_enabled?: boolean;");
    expect(tauriSource).toContain("laya_auto_type_memories?: boolean;");
    expect(tauriSource).toContain("laya_steer_tool_choice?: boolean;");
    expect(tauriSource).toContain("laya_failure_triage?: boolean;");
    expect(tauriSource).toContain("laya_failure_triage_knn?: boolean;");
    expect(tauriSource).toContain("laya_auto_finetune?: boolean;");
    expect(tauriSource).toContain("laya_routing?: boolean;");
    expect(tauriSource).toContain("enforce?: boolean;");
  });

  it("the startup snapshot type carries classifier_status", () => {
    expect(tauriSource).toContain("classifier_status: ClassifierStatusWire;");
  });
});

describe("serializeClassifier", () => {
  const base: ClassifierDraft = {
    enabled: true,
    autoTypeMemories: false,
    steerToolChoice: false,
    failureTriage: false,
    failureTriageKnn: false,
    autoFinetune: false,
    routing: false,
    routingEnforce: false,
    routingCheap: null,
    routingCapable: null,
    routingThreshold: 0.8,
  };

  it("serializes identical drafts identically (clean state → not dirty)", () => {
    expect(serializeClassifier(base)).toBe(serializeClassifier({ ...base }));
  });

  it("distinguishes drafts that differ in any field (dirty detection)", () => {
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, enabled: false }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, autoTypeMemories: true }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, steerToolChoice: true }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, failureTriage: true }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, failureTriageKnn: true }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, autoFinetune: true }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, routing: true }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, routingEnforce: true }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({
        ...base,
        routingCheap: { endpoint: "local", model: "tiny" },
      }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, routingThreshold: 0.9 }),
    );
  });

  it("matches the persisted shape (enabled + auto-typing + tool-choice + triage + kNN overlay + fine-tune + routing)", () => {
    expect(JSON.parse(serializeClassifier(base))).toEqual({
      enabled: true,
      autoTypeMemories: false,
      steerToolChoice: false,
      failureTriage: false,
      failureTriageKnn: false,
      autoFinetune: false,
      routing: false,
      routingEnforce: false,
      routingCheap: null,
      routingCapable: null,
      routingThreshold: 0.8,
    });
  });
});
