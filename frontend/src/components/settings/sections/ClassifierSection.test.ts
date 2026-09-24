// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

/**
 * Classifier settings section (Laya, opt-in) — source-contract + pure-helper
 * tests in the style of ChatSection.test.ts (vitest runs in `node` env — no
 * React DOM infra).
 *
 * Pins the three surfaces of the opt-in contract: the section is registered
 * (nav + deep-link id), it persists the two config keys through the shared
 * save path, and the frontend bindings stay wired to the backend's exact
 * names (command, event, config + patch fields, snapshot field).
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
  it("renders the enable toggle, the endpoint input and the install hints", () => {
    expect(classifierSource).toContain("Enable the Laya classifier");
    expect(classifierSource).toContain("laya-serve endpoint URL");
    expect(classifierSource).toContain("pip install");
    expect(classifierSource).toContain("laya-serve");
  });

  it("shows the live status (disabled / ready / failed) via the bindings", () => {
    expect(classifierSource).toContain("getClassifierStatus");
    expect(classifierSource).toContain("onClassifierStatus");
    expect(classifierSource).toContain("Disabled — no classifier backend exists");
    expect(classifierSource).toContain("Failed — the last call got no answer");
  });

  it("persists the opt-in through saveSettings (config.toml [general.laya])", () => {
    expect(classifierSource).toContain("laya_enabled: enabled");
    expect(classifierSource).toContain("laya_endpoint: endpoint");
  });

  it("keeps the dialog save contract (forwardRef + dirty callback)", () => {
    expect(classifierSource).toContain("forwardRef<SettingsSectionHandle");
    expect(classifierSource).toContain("onDirtyChange?.(dirty)");
    expect(classifierSource).toContain("save: async () => {");
  });
});

describe("classifier bindings stay wired to the backend names", () => {
  it("the status command targets get_classifier_status", () => {
    expect(tauriSource).toContain('invoke("get_classifier_status")');
  });

  it("the status listener targets the classifier://status event", () => {
    expect(tauriSource).toContain('listen<string>("classifier://status"');
  });

  it("the settings types carry the laya fields (config + save patch)", () => {
    expect(tauriSource).toContain(
      "laya?: { enabled: boolean; endpoint: string | null };",
    );
    expect(tauriSource).toContain("laya_enabled?: boolean;");
    expect(tauriSource).toContain("laya_endpoint?: string;");
  });

  it("the startup snapshot type carries classifier_status", () => {
    expect(tauriSource).toContain("classifier_status: string;");
  });
});

describe("serializeClassifier", () => {
  const base: ClassifierDraft = {
    enabled: true,
    endpoint: "http://127.0.0.1:8000",
  };

  it("serializes identical drafts identically (clean state → not dirty)", () => {
    expect(serializeClassifier(base)).toBe(serializeClassifier({ ...base }));
  });

  it("distinguishes drafts that differ in either field (dirty detection)", () => {
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, enabled: false }),
    );
    expect(serializeClassifier(base)).not.toBe(
      serializeClassifier({ ...base, endpoint: "" }),
    );
  });

  it("matches the persisted opt-in shape (enabled + endpoint)", () => {
    expect(JSON.parse(serializeClassifier(base))).toEqual({
      enabled: true,
      endpoint: "http://127.0.0.1:8000",
    });
  });
});
