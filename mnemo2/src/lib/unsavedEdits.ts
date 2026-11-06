import { getContext, onDestroy, setContext } from "svelte";

/**
 * The settings dialog's shared answer to "would leaving now drop an edit?".
 *
 * Every mounted page registers one check; the dialog asks the registry before
 * closing or switching pages. Checks are plain functions over the pages' own
 * deriveds, so they are read fresh at question time — no runes needed here.
 */
export interface UnsavedRegistry {
  /** Add a page's check; the returned function removes it again. */
  register(check: () => boolean): () => void;
  /** Whether any registered check currently reports unsaved edits. */
  hasUnsaved(): boolean;
}

/** The context key shared by the dialog and its pages. */
const KEY = Symbol("unsaved-edits");

/** A fresh registry with no pages registered. */
export function createUnsavedRegistry(): UnsavedRegistry {
  const checks = new Set<() => boolean>();
  return {
    register(check) {
      checks.add(check);
      return () => {
        checks.delete(check);
      };
    },
    hasUnsaved() {
      for (const check of checks) {
        if (check()) return true;
      }
      return false;
    },
  };
}

/** Create the registry and hand it to the subtree; call during component init. */
export function provideUnsavedRegistry(): UnsavedRegistry {
  const registry = createUnsavedRegistry();
  setContext(KEY, registry);
  return registry;
}

/**
 * Report this page's unsaved edits to the enclosing dialog for as long as the
 * page is mounted. A page rendered without a registry (a test, a preview)
 * simply has nothing to report.
 */
export function trackUnsaved(check: () => boolean): void {
  const registry = getContext<UnsavedRegistry | null>(KEY);
  if (!registry) return;
  onDestroy(registry.register(check));
}
