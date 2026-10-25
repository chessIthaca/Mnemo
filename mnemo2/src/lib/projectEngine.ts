import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./leadEngine";

/** One real project from the shared `projects.toml` registry. */
export interface ProjectRecord {
  name: string;
  path: string;
}

export interface ProjectTransportBindings {
  invoke: typeof invoke;
}

/** The frontend's typed view of the native project commands. */
export interface ProjectTransport {
  list(configDir?: string | null): Promise<ProjectRecord[]>;
  add(path: string, configDir?: string | null): Promise<ProjectRecord>;
  pickFolder(): Promise<string | null>;
}

/** Build the native project transport from real Tauri bindings. */
export function createTauriProjectTransport(
  bindings: ProjectTransportBindings,
): ProjectTransport {
  return {
    list: (configDir) =>
      bindings.invoke<ProjectRecord[]>("project_list", {
        configDir: configDir ?? null,
      }),
    add: (path, configDir) =>
      bindings.invoke<ProjectRecord>("project_add", {
        path,
        configDir: configDir ?? null,
      }),
    pickFolder: () => bindings.invoke<string | null>("project_pick_folder"),
  };
}

/**
 * Create the native project transport, or `null` in a browser-only Vite
 * session. Tests may pass explicit bindings.
 */
export function createProjectTransport(
  bindings?: ProjectTransportBindings,
): ProjectTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriProjectTransport(bindings ?? { invoke });
}
