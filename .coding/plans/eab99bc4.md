# Plan: Stream guard debug telemetry: record boundary cuts (trace field + warn log + Trace tab badge)

## Goal
Make stream-guard terminations observable: every boundary cut is recorded in the trace record (Trace tab + traces.jsonl) with the matched boundary, cut position, and cut size, plus a warn log — so guard cuts are distinguishable from natural stops and empty completions, confirming or refuting the false-positive hypothesis behind this session's stalls.

## Kind
implementation

## Context
Session diagnosis (2026-12-31/01-03): four empty-output stalls, all on turns whose natural content quoted GLM boundary-tag strings; zero provider errors from this session's calls (clean HTTP completions); the stall trace records were evicted by the ~1MB traces.jsonl mirror cap, so guard-cut vs clean-empty stayed indistinguishable. The guard (plan 5ce79c00, merged 720b039) terminates silently: on find_boundary_cutoff match it keeps only the text prefix, sets stream_stopped, breaks (openai.rs ~1162-1189) — no log, no trace marker. Suspected false-positive: legitimate quotation of boundary strings in code/analysis cuts the turn. This plan is observability ONLY; the behavior fix (smarter matching / config-driven stop_boundary_strings) is pending backlog f322277c. Follow the H1 delivered-args tap precedent (plan b696ac35): debugging evidence rides the trace record.

## Steps
- [x] 1. **Capture the guard hit in the stream loop (openai.rs ~1162-1189): on find_boundary_cutoff match, record matched boundary string, byte idx, accumulated answer chars, prefix-empty flag; thread to where stream_stopped synthesizes the stop (~1214-1330). Add guard_cut field to the trace record (trace.rs), populated where finish_reason/usage are recorded. Warn log per cut + debug log of active boundaries. Trace tab badge. Tests + README line. Verify with cargo test.**
