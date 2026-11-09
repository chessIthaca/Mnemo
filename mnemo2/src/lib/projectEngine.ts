import { invoke } from "@tauri-apps/api/core";
import { isTauriRuntime } from "./runtime";

/** One real project from the shared registry and its actual saved state. */
export interface ProjectRecord {
  name: string;
  path: string;
  branch?: string;
  last_session_ms?: number | null;
  unfinished_workers?: number;
  review_workers?: number;
  worker_slots?: number;
  lead_model?: string | null;
  available?: boolean;
  state_error?: string | null;
}

export interface ProjectPreferences {
  workbench_open: boolean;
  workbench_tab:
    | "browser"
    | "diff"
    | "plan"
    | "files"
    | "git"
    | "trace"
    | "graph"
    | "memory";
  workbench_width: number;
  browser_url: string;
  diff_worker_id: string | null;
  /** Saved mission-control list width; null (or absent) keeps the default split. */
  board_width: number | null;
  /** Saved width of the wide panel tabs; null (or absent) follows the window. */
  workbench_wide_width?: number | null;
}

export interface ProjectTransportBindings {
  invoke: typeof invoke;
}

export interface InstanceAttachment {
  conflict_pid: number | null;
  conflict_started_at: number | null;
}

export interface CleanupReport {
  removed: string[];
  skipped: string[];
}

export interface ProjectTransport {
  list(configDir?: string | null): Promise<ProjectRecord[]>;
  add(path: string, configDir?: string | null): Promise<ProjectRecord>;
  create(parent: string, name: string, configDir?: string | null): Promise<ProjectRecord>;
  pickFolder(): Promise<string | null>;
  preferences(): Promise<ProjectPreferences>;
  savePreferences(workspaceRoot: string, preferences: ProjectPreferences): Promise<void>;
  attach(workspaceRoot: string, allowConflict: boolean): Promise<InstanceAttachment>;
  cleanup(workspaceRoot: string): Promise<CleanupReport>;
}

export function createTauriProjectTransport(
  bindings: ProjectTransportBindings,
): ProjectTransport {
  return {
    list: (configDir) => bindings.invoke<ProjectRecord[]>("project_list", { configDir: configDir ?? null }),
    add: (path, configDir) => bindings.invoke<ProjectRecord>("project_add", { path, configDir: configDir ?? null }),
    create: (parent, name, configDir) => bindings.invoke<ProjectRecord>("project_create", { parent, name, configDir: configDir ?? null }),
    pickFolder: () => bindings.invoke<string | null>("project_pick_folder"),
    preferences: () => bindings.invoke<ProjectPreferences>("workbench_preferences"),
    savePreferences: (workspaceRoot, preferences) => bindings.invoke<void>("workbench_preferences_set", { workspaceRoot, preferences }),
    attach: (workspaceRoot, allowConflict) => bindings.invoke<InstanceAttachment>("project_attach", { workspaceRoot, allowConflict }),
    cleanup: (workspaceRoot) => bindings.invoke<CleanupReport>("project_cleanup", { workspaceRoot, configDir: null }),
  };
}

export function createProjectTransport(
  bindings?: ProjectTransportBindings,
): ProjectTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriProjectTransport(bindings ?? { invoke });
}
