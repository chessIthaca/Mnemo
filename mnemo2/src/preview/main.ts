/**
 * Dev-only entry for `preview.html?scene=<name>`.
 *
 * Renders the real `App` shell against the sample state a scene builds, so a
 * UI change can be reviewed in a browser without the Tauri host. The
 * production entry (`src/main.ts`) never imports this file, and `vite build`
 * only bundles `index.html`, so none of it reaches the shipped app.
 */

import "@fontsource-variable/inter";
import "@fontsource-variable/source-serif-4";
import "../app.css";

import { mount } from "svelte";
import App from "../App.svelte";
import { app } from "../lib/store";
import { previewScene, previewScenes } from "../test/previewScenes";

/**
 * The scene index, shown when `?scene=` names no scene. Built from plain DOM so
 * an unknown parameter does not depend on any app state being valid.
 */
function sceneIndex(requested: string): HTMLElement {
  const page = document.createElement("main");
  page.className = "preview-index";

  const heading = document.createElement("h1");
  heading.textContent = "Mnemo2 preview scenes";
  page.append(heading);

  const intro = document.createElement("p");
  intro.textContent =
    "Dev-only page: each scene renders the real app shell (or a bespoke frame, for scenes the store cannot express) against the sample data in src/test/previewScenes.ts.";
  page.append(intro);

  if (requested !== "") {
    const unknown = document.createElement("p");
    unknown.className = "preview-index-unknown";
    unknown.textContent = `No scene named “${requested}”.`;
    page.append(unknown);
  }

  const list = document.createElement("ul");
  for (const scene of previewScenes) {
    const item = document.createElement("li");
    const link = document.createElement("a");
    link.href = `?scene=${scene.name}`;
    link.textContent = scene.title;
    const description = document.createElement("span");
    description.textContent = scene.description;
    item.append(link, description);
    list.append(item);
  }
  page.append(list);
  return page;
}

const target = document.getElementById("app");
if (!target) throw new Error("preview.html has no #app mount point");

const requested = new URLSearchParams(window.location.search).get("scene");
const scene = previewScene(requested);

if (scene) {
  if (scene.mount) {
    // Bespoke frames own their cleanup; the page runs them for its lifetime.
    scene.mount(target);
  } else {
    app.set(scene.build());
    mount(App, { target });
  }
} else {
  target.append(sceneIndex(requested ?? ""));
}
