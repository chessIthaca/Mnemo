+++
title = "Startup window-geometry clamp (frontend/src/lib/windowRestore.ts) — MERGED into main"
supersedes = "2027-01-11-startup-window-geometry-clamp-frontend-src-lib-w"
created = "2027-01-11"
+++

MERGED into main — commit c9da825 (plan 9ff59133) has been in main's history since the earlier wt/mnemo landing, pre-dating dcc7a93 (2027-01-16); this record's "(branch wt/mnemo)" hint was stale. WHAT: the startup restore of the persisted window geometry clamps the size to a minimum (800×560 logical, mirroring min_inner_size) and to the anchor monitor's work area, and adjusts the position to keep the window fully on-screen; fully-visible geometries pass through unchanged. Implementation: frontend/src/lib/windowRestore.ts; the knowledge record behind this digest holds the detail.
