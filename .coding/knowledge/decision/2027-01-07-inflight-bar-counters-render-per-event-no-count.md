+++
title = "inflight bar counters render per event — no count-up interpolation"
created = "2027-01-07"
+++

User preference (2027-01-07, backlog d9b560a0): the inflight bar's token counters (↑/↓/🧠/ctx) render their raw values directly — updating exactly when streaming data arrives, like the trace graph (e20b1df precedent) — never via interpolated count-up animations. useCountUp was deleted (commit c53d22e on wt/agenticcoding). If a future counter seems to need smoothing, justify it explicitly per-counter rather than reintroducing a blanket animation hook.
