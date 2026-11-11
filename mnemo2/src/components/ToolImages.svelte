<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import {
    createToolImageTransport,
    dataUrlToBlob,
    imageName,
    loadToolImage,
  } from "../lib/toolImages";
  import Sheet from "./Sheet.svelte";

  let { paths, workerId = null }: { paths: string[]; workerId?: string | null } = $props();

  /** One image of the row: still arriving, ready to show, or unavailable. */
  interface ImageState {
    status: "loading" | "ready" | "failed";
    dataUrl: string;
    error: string;
  }

  const transport = createToolImageTransport();
  let images = $state<Record<string, ImageState>>({});
  let viewer = $state<{ path: string; dataUrl: string } | null>(null);
  let copyLabel = $state<"idle" | "copied" | "failed">("idle");
  let copyTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    if (transport === null) return;
    const wanted = paths;
    const owner = workerId;
    untrack(() => {
      for (const path of wanted) {
        if (images[path] !== undefined) continue;
        images[path] = { status: "loading", dataUrl: "", error: "" };
        void loadToolImage(transport, owner, path).then(
          (dataUrl) => {
            images[path] = { status: "ready", dataUrl, error: "" };
          },
          (error: unknown) => {
            images[path] = {
              status: "failed",
              dataUrl: "",
              error: error instanceof Error ? error.message : String(error),
            };
          },
        );
      }
    });
  });

  onDestroy(() => {
    if (copyTimer !== null) clearTimeout(copyTimer);
  });

  function openViewer(path: string, dataUrl: string): void {
    copyLabel = "idle";
    viewer = { path, dataUrl };
  }

  function closeViewer(): void {
    if (copyTimer !== null) clearTimeout(copyTimer);
    copyTimer = null;
    copyLabel = "idle";
    viewer = null;
  }

  async function copyImage(): Promise<void> {
    if (viewer === null) return;
    if (copyTimer !== null) clearTimeout(copyTimer);
    try {
      const blob = dataUrlToBlob(viewer.dataUrl);
      await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
      copyLabel = "copied";
    } catch {
      copyLabel = "failed";
    }
    copyTimer = setTimeout(() => {
      copyLabel = "idle";
      copyTimer = null;
    }, 1500);
  }

  const copyText = $derived(
    copyLabel === "copied" ? "Copied" : copyLabel === "failed" ? "Copy failed" : "Copy image",
  );

  const viewerName = $derived(viewer === null ? "" : imageName(viewer.path));
</script>

{#if transport !== null}
  <div class="tool-images">
    {#each paths as path (path)}
      {@const image = images[path]}
      {#if image === undefined || image.status === "loading"}
        <p class="tool-image-note">Loading image…</p>
      {:else if image.status === "failed"}
        <p class="tool-image-note">Image unavailable: {image.error}</p>
      {:else}
        <button
          class="tool-image-thumb"
          type="button"
          aria-label={`Open ${imageName(path)}`}
          title={path}
          onclick={() => openViewer(path, image.dataUrl)}
        >
          <img src={image.dataUrl} alt={imageName(path)} />
        </button>
      {/if}
    {/each}
  </div>
  <Sheet open={viewer !== null} title={viewerName} eyebrow="IMAGE" wide onclose={closeViewer}>
    {#if viewer !== null}
      <div class="tool-image-viewer">
        <div class="tool-image-frame">
          <img class="tool-image-full" src={viewer.dataUrl} alt={viewerName} />
        </div>
        <div class="tool-image-actions">
          <button class="button button-primary" type="button" onclick={copyImage}>{copyText}</button>
          <button class="button button-quiet" type="button" onclick={closeViewer}>Close</button>
        </div>
      </div>
    {/if}
  </Sheet>
{/if}

<style>
  .tool-images { display: flex; flex-wrap: wrap; gap: 8px; }
  .tool-image-note { margin: 0; color: var(--muted); font-size: var(--text-meta); }
  .tool-image-thumb { padding: 0; border: 1px solid var(--line); border-radius: 8px; background: var(--surface-soft); cursor: zoom-in; line-height: 0; }
  .tool-image-thumb:hover { border-color: var(--line-strong); }
  .tool-image-thumb img { display: block; max-width: 100%; max-height: 140px; object-fit: contain; border-radius: 7px; }
  .tool-image-viewer { display: flex; flex: 1; min-height: 0; flex-direction: column; gap: 12px; padding: 14px 20px 18px; }
  .tool-image-frame { display: flex; flex: 1; min-height: 0; align-items: center; justify-content: center; overflow: auto; }
  .tool-image-full { max-width: 100%; max-height: 75vh; object-fit: contain; border-radius: 8px; }
  .tool-image-actions { display: flex; justify-content: flex-end; gap: 8px; }
</style>
