+++
title = "the shared [ui] theme defaults to light"
created = "2026-11-10"
evidence = "src/config/general.rs defaults_when_empty; src-tauri/src/mnemo2/appearance_settings.rs read_defaults_when_nothing_is_saved; user: \"change the default of mnemo2 to the light appearance\""
+++

A config without `[ui] theme` now means "light", not "dark": UiConfig::default (src/config/general.rs), Mnemo2's DEFAULT_THEME (mnemo2/src/lib/theme.ts) and the static first paint in mnemo2/index.html (data-theme="light", theme-color #f7f7f4) all agree, so Mnemo2 never flashes dark before settings load. The user asked for Mnemo2 to open light. The engine default had to change because UiConfig deserializes a missing theme to its default, so Mnemo2 cannot tell "unset" from "dark". Mnemo1 shares the word but prefers its own localStorage `mh.theme`, so only a fresh Mnemo1 install starts light (accepted). Explicit dark/system choices are unaffected; app.css keeps light tokens on :root with dark overrides under [data-theme="dark"].
