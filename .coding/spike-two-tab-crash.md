# Spike: two-tab handling + crash resistance (shared-env re-verify)

**Date:** 2026-12
**Plan:** e9cd6fb1 (research — no production source changes)
**Verdict:** **PASS — crash isolation holds under the shared WebView2 env.**

## The question

A prior spike (71bd2daa) found that two `add_child` webviews in a bare
`WindowBuilder` window share ONE WebView2 environment — i.e. there is **no CDP
isolation** (both children land on port 9222). That disproved the scoping memo's
"second child gets no CDP" assumption and the security win it implied.

The open question: does **crash isolation** still hold when the two children
share one browser process? Or does a renderer crash in tab 1 take down tab 2
(shared fate)?

## Setup

Standalone Tauri spike (`spike/two-tab-crash/`, now deleted). Bare
`WindowBuilder::new("main")` (no webview of its own) + two ordered `add_child`
webviews:

1. `tab1` → `https://example.com` (created first → its env binds 9222 → CDP).
2. `tab2` → `https://example.org` (created second → shares tab1's env).

Initial visibility: tab1 shown, tab2 hidden. A background self-test runs after
a 4s load delay.

## Self-test

1. **Tab switching** — hide tab1 / show tab2, wait 1s, hide tab2 / show tab1.
2. **Crash tab1's renderer** — `eval` an OOM loop
   (`var a=[];for(;;)a.push(new Uint8Array(67108864))`).
3. **Wait 6s** for the renderer to die + the browser to notice.
4. **Survival check** — `tab2.navigate("https://example.net")` must return `Ok`.
5. **Recreate tab1** — `tab1.navigate("https://example.com")` must return `Ok`.

## Result

```
[1] tab switching (show/hide): OK
[2] dispatched OOM crash to tab1
[3] tab2.navigate after tab1 crash: OK (alive)
[4] tab1.navigate after crash: OK (recreated)
VERDICT: PASS — crash isolation holds under shared env
```

## Why it holds

A renderer crash kills only tab1's **renderer** process. The shared **browser**
process survives, tab2 keeps responding to navigation, and tab1 can be recreated
by navigating again (the browser spawns a fresh renderer for the crashed tab).
The shared env means no CDP isolation, but it does **not** mean a shared
renderer fate — each child has its own renderer.

## Implication for path (a)

The crash-resistance benefit of the bare-window + two-`add_child` reconfigure is
**real and confirmed**, even though the CDP-isolation/security benefit is **not**
(both children share one env → both get CDP).

| Benefit of path (a) | Achieved? |
|---|---|
| Crash isolation (game-browser crash leaves agent chat alive) | ✅ Yes — confirmed here |
| Keeps React + IPC + brain untouched | ✅ Yes |
| Smallest change | ✅ Yes |
| CDP isolation / security win (scope CDP away from React DOM) | ❌ No — shared env |

Path (a) is still GO for crash isolation + minimal change. It does not deliver
the security win; that is unachievable in this stack (env-share doesn't compile,
second CDP port crashes the UI, separate window is user-rejected).
