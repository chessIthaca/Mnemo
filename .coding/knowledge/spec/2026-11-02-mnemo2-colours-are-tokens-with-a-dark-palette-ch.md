+++
title = "Mnemo2 colours are tokens with a dark palette chosen by the shared [ui] theme"
created = "2026-11-02"
+++

Every Mnemo2 component colour is a CSS token in mnemo2/src/app.css (light values on :root, dark overrides under :root[data-theme="dark"]; code blocks keep their own dark palette in both themes); components must not carry literal colours. mnemo2/src/lib/theme.ts maps the [ui] theme word (light | dark | system; default dark, shared with Mnemo1's config.toml) to data-theme on <html>, follows prefers-color-scheme live for system, and mirrors the choice in localStorage so main.ts paints before the first frame (the app CSP forbids an inline pre-paint script). The choice lives on Settings > Appearance & sounds; changing it there also changes Mnemo1's theme.
