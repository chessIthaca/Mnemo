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
 * path, the managed-runtime controls (mode toggle + checkpoint catalog +
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
  it("renders the enable toggle, the mode toggle and the external endpoint field", () => {
    expect(classifierSource).toContain("Enable the Laya classifier");
    expect(classifierSource).toContain("Managed — recommended");
    expect(classifierSource).toContain("External endpoint (advanced)");
    expect(classifierSource).toContain("laya-serve endpoint URL");
  });

  it("keeps the do-it-yourself install hints for external mode", () => {
    expect(classifierSource).toContain("pip install");
    expect(classifierSource).toContain("laya-serve");
  });

  it("wires the managed checkpoint catalog + download through the bindings", () => {
    expect(classifierSource).toContain("listLayaCheckpoints");
    expect(classifierSource).toContain("setupLayaRuntime");
    expect(classifierSource).toContain("Download (~{c.size_mb} MB)");
    expect(classifierSource).toContain('name="laya-checkpoint"');
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
    expect(classifierSource).toContain("Failed — the last call got no answer");
    expect(classifierSource).toContain("Installing — preparing the managed Laya runtime");
    expect(classifierSource).toContain("Starting — launching the local laya-serve sidecar");
  });

  it("persists the opt-in through saveSettings (config.toml [general.laya])", () => {
    expect(classifierSource).toContain("laya_enabled: enabled");
    expect(classifierSource).toContain("laya_auto_type_memories: autoType");
    expect(classifierSource).toContain("laya_failure_triage: failureTriage");
    expect(classifierSource).toContain("laya_failure_triage_knn: failureTriageKnn");
    expect(classifierSource).toContain("laya_auto_finetune: autoFinetune");
    expect(classifierSource).toContain("laya_mode: mode");
    expect(classifierSource).toContain("laya_checkpoint: checkpoint");
    // Managed mode never persists an external endpoint (blank clears it).
    expect(classifierSource).toContain(
      'laya_endpoint: mode === "managed" ? "" : endpoint',
    );
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
    expect(tauriSource).toContain('invoke("laya_setup", { checkpoint })');
  });

  it("the settings types carry the laya fields (config + save patch)", () => {
    expect(tauriSource).toContain(
      `laya?: {
      enabled: boolean;
      endpoint: string | null;
      mode: "external" | "managed";
      checkpoint: string | null;
      /** Whether memory auto-typing is enabled (opt-in). */
      auto_type_memories: boolean;
      /** Whether failure triage is enabled (opt-in; needs a fine-tuned
       *  checkpoint). */
      failure_triage: boolean;
      /** Whether the kNN overlay for failure triage is enabled (opt-in;
       *  local — rides the memory embedder, needs no laya-serve, consulted
       *  only while failure_triage itself is on). */
      failure_triage_knn: boolean;
      /** Whether the startup failure-triage fine-tune is enabled (managed
       *  mode only, opt-in). */
      auto_finetune: boolean;
    };`
    );
    expect(tauriSource).toContain("laya_enabled?: boolean;");
    expect(tauriSource).toContain("laya_endpoint?: string;");
    expect(tauriSource).toContain('laya_mode?: "external" | "managed";');
    expect(tauriSource).toContain("laya_checkpoint?: string;");
    expect(tauriSource).toContain("laya_auto_type_memories?: boolean;");
    expect(tauriSource).toContain("laya_failure_triage?: boolean;");
    expect(tauriSource).toContain("laya_failure_triage_knn?: boolean;");
    expect(tauriSource).toContain("laya_auto_finetune?: boolean;");
  });

  it("the startup snapshot type carries classifier_status", () => {
    expect(tauriSource).toContain("classifier_status: ClassifierStatusWire;");
  });
});

describe("serializeClassifier", () => {
  const base: ClassifierDraft = {
    enabled: true,
    mode: "managed",
    checkpoint: "english",
    endpoint: "",
    autoTypeMemories: false,
    failureTriage: false,
    failureTriageKnn: false,
    autoFinetune: false,
  };

  it("serializes identical drafts identically (clean state → not dirty)", () => {
    expect(serializeClassifier(base)).toBe(serializeClassifier({ ...base }));
  });

  it("distinguishes drafts that differ in any field (dirty detection)", () => {
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, enabled: false }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, mode: "external" as const }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, checkpoint: "multilingual" }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, endpoint: "http://127.0.0.1:8000" }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, autoTypeMemories: true }),
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
  });

  it("matches the persisted opt-in shape (enabled + mode + checkpoint + endpoint + auto-typing + triage + kNN overlay + fine-tune)", () => {
    expect(JSON.parse(serializeClassifier(base))).toEqual({
      enabled: true,
      mode: "managed",
      checkpoint: "english",
      endpoint: "",
      autoTypeMemories: false,
      failureTriage: false,
      failureTriageKnn: false,
      autoFinetune: false,
    });
  });
});
