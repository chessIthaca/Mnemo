<script lang="ts">
  import {
    SOUND_ROWS,
    applySoundToggles,
    playSound,
    type AppearancePatch,
    type AppearanceSettings,
    type AppearanceTransport,
    type SoundKind,
    type SoundToggles,
  } from "../lib/sounds";
  import { errorText } from "../lib/settingsState";
  import { THEME_ROWS, applyTheme, parseTheme, type ThemeChoice } from "../lib/theme";

  /**
   * The Appearance page: the three `[ui]` notification-sound toggles, each
   * with a Test button that plays it, and the `[ui] theme` choice (Light /
   * Dark / System, painted by `lib/theme.ts`).
   *
   * A toggle saves immediately through `appearance_settings_save` and pushes
   * the accepted snapshot into the sound module's live cache, so the very
   * next turn event obeys the new setting — no restart. The Test button calls
   * `playSound` directly (the click is the user's gesture and unlocks audio)
   * and deliberately ignores the toggle, so a muted sound can still be
   * auditioned. A theme click paints at once, before the save returns, so the
   * whole app changes under the pointer; a failed save paints the old palette
   * back.
   */
  let { transport }: { transport: AppearanceTransport } = $props();

  /** The stored snapshot; null until the first read lands. */
  let settings = $state<AppearanceSettings | null>(null);
  let loadError = $state("");
  /** The flag currently saving, or null when the page is idle. */
  let saving = $state<keyof SoundToggles | null>(null);
  /** The theme word currently saving, or null when the page is idle. */
  let themeSaving = $state<ThemeChoice | null>(null);
  let saveError = $state("");
  /** The quiet "Saved." note after a successful save, empty when idle. */
  let savedNote = $state("");
  /** Which group owns `saveError` / `savedNote` — the last one acted on. */
  let noteScope = $state<"sounds" | "theme" | null>(null);

  $effect(() => {
    let cancelled = false;
    transport
      .get()
      .then((loaded) => {
        if (cancelled) return;
        settings = loaded;
        applySoundToggles(loaded);
      })
      .catch((error) => {
        if (!cancelled) loadError = errorText(error);
      });
    return () => {
      cancelled = true;
    };
  });

  /** Flip one toggle: optimistically, then through the save; revert on error. */
  async function toggle(flag: keyof SoundToggles, next: boolean): Promise<void> {
    if (!settings) return;
    const previous = settings;
    settings = { ...settings, [flag]: next };
    saving = flag;
    saveError = "";
    savedNote = "";
    noteScope = "sounds";
    const patch: AppearancePatch = {};
    patch[flag] = next;
    try {
      const accepted = await transport.save(patch);
      settings = accepted;
      applySoundToggles(accepted);
      savedNote = "Saved. The next turn obeys it — no restart.";
    } catch (error) {
      settings = previous;
      saveError = errorText(error);
    } finally {
      saving = null;
    }
  }

  /** Choose a theme: paint it at once, then save it through the same command. */
  async function chooseTheme(choice: ThemeChoice): Promise<void> {
    if (!settings) return;
    const previous = settings.theme;
    settings = { ...settings, theme: choice };
    themeSaving = choice;
    saveError = "";
    savedNote = "";
    noteScope = "theme";
    applyTheme(choice);
    try {
      const accepted = await transport.save({ theme: choice });
      settings = accepted;
      applyTheme(parseTheme(accepted.theme));
      savedNote = "Saved. Every screen repaints — no restart.";
    } catch (error) {
      settings = { ...settings, theme: previous };
      applyTheme(parseTheme(previous));
      saveError = errorText(error);
    } finally {
      themeSaving = null;
    }
  }

  /** Audition one sound regardless of its toggle. */
  function test(kind: SoundKind): void {
    playSound(kind);
  }
</script>

<div class="appearance-page">
  <div class="appearance-scroll">
    {#if loadError}
      <p class="load-error" role="alert">{loadError}</p>
    {:else if settings === null}
      <p class="load-note">Loading…</p>
    {:else}
      <section class="group" aria-label="Sounds">
        <header>
          <h3>Sounds</h3>
          <p class="micro">
            Short tones synthesized in the app — no sound files. A burst of the same sound plays once, so a whole
            team of workers finishing together is a single chime — and a needs-you ping is never swallowed by one.
          </p>
        </header>
        <div class="sound-list">
          {#each SOUND_ROWS as row (row.kind)}
            <div class="sound-row">
              <label class="sound-toggle">
                <input
                  type="checkbox"
                  aria-label={row.label}
                  checked={settings[row.flag]}
                  disabled={saving !== null}
                  onchange={(event) => toggle(row.flag, event.currentTarget.checked)}
                />
                <span class="sound-text">
                  <strong>{row.label}</strong>
                  <span class="micro">{row.description}</span>
                </span>
              </label>
              <button class="button button-quiet sound-test" onclick={() => test(row.kind)}>Test</button>
            </div>
          {/each}
        </div>
        <div class="section-actions">
          {#if noteScope === "sounds"}
            {#if saveError}
              <p class="section-error" role="alert">{saveError}</p>
            {:else if saving !== null}
              <p class="section-note">Saving…</p>
            {:else if savedNote}
              <p class="section-note">{savedNote}</p>
            {/if}
          {/if}
        </div>
      </section>

      <section class="group" aria-label="Theme">
        <header>
          <h3>Theme</h3>
          <p class="micro">
            The palette for every screen. <strong>System</strong> follows the operating system and switches with it
            while the app is open.
          </p>
        </header>
        <div class="theme-list" role="radiogroup" aria-label="Theme">
          {#each THEME_ROWS as row (row.choice)}
            <label class="theme-row">
              <input
                type="radio"
                name="theme"
                value={row.choice}
                checked={parseTheme(settings.theme) === row.choice}
                disabled={themeSaving !== null}
                onchange={() => chooseTheme(row.choice)}
              />
              <span class="sound-text">
                <strong>{row.label}</strong>
                <span class="micro">{row.description}</span>
              </span>
            </label>
          {/each}
        </div>
        <div class="section-actions">
          {#if noteScope === "theme"}
            {#if saveError}
              <p class="section-error" role="alert">{saveError}</p>
            {:else if themeSaving !== null}
              <p class="section-note">Saving…</p>
            {:else if savedNote}
              <p class="section-note">{savedNote}</p>
            {/if}
          {/if}
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .appearance-page { display: flex; min-height: 0; flex: 1; flex-direction: column; }
  .appearance-scroll { display: grid; flex: 1; align-content: start; gap: 18px; padding: 18px 24px; overflow-y: auto; }
  .load-error { margin: 0; padding: 18px 24px; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .load-note { margin: 0; padding: 18px 24px; color: var(--muted); font-size: var(--text-body); }

  .group { display: grid; gap: 8px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 14px; background: var(--canvas); }
  .group header { display: grid; gap: 4px; }
  .group h3 { margin: 0; font-size: var(--text-title); font-weight: 650; }
  .micro { margin: 0; color: var(--muted); font-size: var(--text-body); line-height: 1.55; }

  .sound-list { display: grid; gap: 4px; }
  .sound-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
  }
  .sound-toggle { display: flex; align-items: center; gap: 10px; cursor: pointer; font-size: var(--text-body); }
  .sound-toggle input { width: 16px; height: 16px; flex-shrink: 0; accent-color: var(--accent); cursor: pointer; }
  .sound-text { display: grid; gap: 1px; }
  .sound-text strong { font-weight: 620; }
  .sound-test { min-height: 32px; padding: 5px 12px; flex-shrink: 0; }

  .theme-list { display: grid; gap: 4px; }
  .theme-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 9px;
    cursor: pointer;
    font-size: var(--text-body);
  }
  .theme-row input { width: 16px; height: 16px; flex-shrink: 0; accent-color: var(--accent); cursor: pointer; }

  .section-actions { display: flex; align-items: center; gap: 12px; min-height: 0; }
  .section-error { margin: 0; color: var(--danger); font-size: var(--text-body); overflow-wrap: anywhere; }
  .section-note { margin: 0; color: var(--muted); font-size: var(--text-body); }
</style>
