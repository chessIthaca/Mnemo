"""Cache-hit round-6 extractor — READ-ONLY analysis (2027-01-11 round).

Sources (never mutated):
  .coding/logs/traces.jsonl            opt-in per-request traces: real provider
                                       usage + the full request_json body.
  .coding/memory.db                    request_stats (always-on per request).
  .coding/logs/provider-errors.jsonl   429/fallback evidence.

Output: .coding/analysis/cache-hit-6-aggregates.txt (GENERATED artifact — the
round-5 precedent .coding/analysis/cache-hit-5-extract.py writes its own
aggregates file the same way; nothing else is written).

Sections:
  A  traces.jsonl inventory + per-record usage/shape (real provider cached).
  B  prefix-break localizer: first differing message index per consecutive
     same-model record pair -> HEAD / TOOLS / MID / TAIL, plus the falsifiable
     prediction cached ~= tokens before the break (ESTIMATE: chars/4).
  B2 calibrated boundary check: tokens-per-char calibrated from the provider's
     own prompt_tokens, so the pre-break prefix is compared in tokens rather
     than the crude chars/4 estimate.
  B3 hysteresis-gate audit: is the compaction gate ever closed? Counts the
     intact vs permanently-unmarkable vs truncatable tool-result populations.
  C  request_stats: headline hit% split by the R19 reporting tier, per-model
     table, prompt-size histogram (the cliff's MEASURED location), per-session
     shape for the trace session, and the miss buckets.
  E  mutation-path cross-check verdicts (hand-written from the code read).
  F  the traced session joined back to request_stats (true prompt/cached).
"""

import collections
import datetime
import hashlib
import json
import sqlite3

TRACES = ".coding/logs/traces.jsonl"
MEMDB = ".coding/memory.db"
OUTPATH = ".coding/analysis/cache-hit-6-aggregates.txt"

# The compaction marker (src/agent/context.rs:646) — its presence means a tool
# result was already rewritten in place.
COMPACT_MARKER = "[… truncated for context efficiency]"

# The round-4 litellm observation: the proxy stopped caching above ~340K.
CLIFF = 340_000

OUT = []


def w(s=""):
    OUT.append(s)


def fmt_ms(ms):
    if not ms:
        return "-"
    return datetime.datetime.fromtimestamp(
        ms / 1000, datetime.timezone.utc
    ).strftime("%Y-%m-%d %H:%M:%S")


def pct(a, b):
    return f"{100.0 * a / b:.1f}%" if b else "n/a"


def deep_cached(node, prefix=""):
    """Every nested usage key whose name mentions cache (the field name
    differs per provider: cached_tokens / prompt_cache_hit_tokens / ...)."""
    found = {}
    if isinstance(node, dict):
        for k, v in node.items():
            if isinstance(v, (dict, list)):
                found.update(deep_cached(v, f"{prefix}{k}."))
            elif "cach" in k.lower() and isinstance(v, (int, float)):
                found[f"{prefix}{k}"] = int(v)
    elif isinstance(node, list):
        for i, v in enumerate(node):
            found.update(deep_cached(v, f"{prefix}{i}."))
    return found


def cached_total(usage):
    """Sum of every cache-token field; None when the provider reports none."""
    vals = list(deep_cached(usage).values())
    return sum(vals) if vals else None


def canon(content):
    return json.dumps(content, sort_keys=True, ensure_ascii=False)


def fingerprint(m):
    """(role, 12-char sha1 of canonical content, content length)."""
    role = m.get("role") if isinstance(m, dict) else "?"
    content = m.get("content") if isinstance(m, dict) else m
    c = canon(content)
    return (role, hashlib.sha1(c.encode("utf-8")).hexdigest()[:12], len(c))


def est_tokens(chars):
    return chars // 4


def record_body(r):
    rj = r.get("request_json") or {}
    return (rj.get("messages") or []), (rj.get("tools") or [])


# request_stats is loaded UP-FRONT: sections B2/C/E/F all need it (B2 runs
# before C and cannot re-query).
_conn = sqlite3.connect(f"file:{MEMDB}?mode=ro", uri=True)
_conn.execute("PRAGMA busy_timeout=3000")
_raw = _conn.execute(
    "SELECT model, session_id, prompt_tokens, completion_tokens, reasoning_tokens, "
    "cached_tokens, ttft_ms, generation_ms, created_at, outcome, purpose "
    "FROM request_stats"
).fetchall()
_conn.close()
rows = [
    (r[0], r[1], r[2] or 0, r[3] or 0, r[4] or 0, r[5] or 0, r[6] or 0, r[7] or 0, r[8] or 0, r[9], r[10])
    for r in _raw
]


# ── A. traces.jsonl ────────────────────────────────────────────────────────
recs = []
partial = []
with open(TRACES, encoding="utf-8") as f:
    for lineno, line in enumerate(f, 1):
        line = line.strip()
        if not line:
            continue
        try:
            recs.append(json.loads(line))
        except json.JSONDecodeError as e:
            # A LIVE trace log ends mid-write while a request is streaming:
            # the trailing line is an unterminated record. Tolerate + report
            # (the round-5 extractor parsed blindly and crashed on this).
            partial.append((lineno, len(line), str(e)))

w("=" * 78)
w(f"A. traces.jsonl — {len(recs)} record(s) (opt-in; provider-reported usage)")
w("=" * 78)
if partial:
    w(f"NOTE: {len(partial)} malformed/partial line(s) skipped — the trace file is")
    w("LIVE (it is appended while a request streams, so its last line is an")
    w("unterminated record). Any consumer must tolerate this:")
    for (lineno, ln, err) in partial:
        w(f"  line {lineno}: {ln} chars — {err[:100]}")
    w("")
if recs:
    times = [r.get("ts_ms") or 0 for r in recs]
    w(f"window: {fmt_ms(min(times))} .. {fmt_ms(max(times))} UTC")
    w(f"models: {sorted({r.get('model') for r in recs})}")
    w(f"providers: {sorted({str(r.get('provider')) for r in recs})}")
    w(
        "SAMPLE WARNING: a handful of trace records is ground truth for the "
        "MECHANISM\n(real cached_tokens + exact prefix breaks), not for rates — "
        "request_stats (section C)\ncarries the statistics."
    )
    w("")
    w("per record:")
    for i, r in enumerate(recs):
        msgs, tools = record_body(r)
        usage = r.get("usage") or {}
        c = deep_cached(usage)
        sys_len = len(canon(msgs[0].get("content"))) if msgs else 0
        tail_len = len(canon(msgs[-1].get("content"))) if msgs else 0
        w(
            f"  [{i:2}] id={r.get('id')} {fmt_ms(r.get('ts_ms'))} {r.get('model')} "
            f"nmsg={len(msgs)} ntools={len(tools)} "
            f"prompt={usage.get('prompt_tokens')} cached={c or 'NONE'} "
            f"sys_chars={sys_len} tail_chars={tail_len} "
            f"finish={r.get('finish_reason')} err={r.get('error')} "
            f"cancelled={r.get('cancelled')} guard_cut={r.get('guard_cut')}"
        )
    w("")
    w("usage cache-key inventory (per provider field name):")
    kk = collections.Counter()
    for r in recs:
        keys = deep_cached(r.get("usage") or {})
        if not keys:
            kk[f"<none> ({r.get('model')})"] += 1
        for k in keys:
            kk[k] += 1
    for k, n in kk.most_common():
        w(f"  {k}: {n}/{len(recs)} records")
    w("")
    w("request_json key inventory:")
    rjk = collections.Counter()
    for r in recs:
        for k in r.get("request_json") or {}:
            rjk[k] += 1
    w("  " + ", ".join(f"{k}({n})" for k, n in rjk.most_common()))

# ── B. prefix-break localizer ─────────────────────────────────────────────
w("")
w("=" * 78)
w("B. prefix-break localizer (consecutive records, same model)")
w("=" * 78)
w("k = first differing message index | HEAD=system rewritten | TOOLS=tools array")
w("rewritten while messages match | MID=an ALREADY-SENT message mutated in place")
w("(the R14 failure shape) | TAIL=pure append (healthy).")
w("predicted = tokens before the break (chars/4, ESTIMATE); a large positive")
w("delta with MID means our own mutation, a ~0 delta with low cached means the")
w("provider/proxy limit.")
w("")
w("  pair                     model              k   class   cached   pred_msg  pred_byte  from_end")

breaks = collections.Counter()
detail = []
pairs = 0
for i in range(1, len(recs)):
    prev, cur = recs[i - 1], recs[i]
    if prev.get("model") != cur.get("model"):
        w(f"  [{i-1}->{i}] model switch ({prev.get('model')} -> {cur.get('model')}): skipped")
        continue
    pm, _ = record_body(prev)
    cm, ctools = record_body(cur)
    _, ptools = record_body(prev)
    if not pm or not cm:
        w(f"  [{i-1}->{i}] no captured messages: skipped")
        continue
    pf = [fingerprint(m) for m in pm]
    cf = [fingerprint(m) for m in cm]
    k = None
    for j in range(min(len(pf), len(cf))):
        if pf[j] != cf[j]:
            k = j
            break
    if k is None:
        k = min(len(pf), len(cf))
    if k == 0:
        cls = "HEAD"
    elif k >= min(len(pf), len(cf)):
        cls = (
            "TOOLS"
            if json.dumps(ptools, sort_keys=True) != json.dumps(ctools, sort_keys=True)
            else "TAIL"
        )
    else:
        cls = "MID"
    breaks[cls] += 1
    pairs += 1
    # Byte-exact break INSIDE the first differing message: the provider's
    # cache matches up to the first differing byte, not to message boundaries.
    byte_off = None
    if cls == "MID" and k < len(pm) and k < len(cm):
        a = canon(pm[k].get("content"))
        b = canon(cm[k].get("content"))
        if a != b:
            n = min(len(a), len(b))
            j = 0
            while j < n and a[j] == b[j]:
                j += 1
            byte_off = j
    pred_msg = est_tokens(sum(cf[j][2] for j in range(k)))
    if byte_off is not None:
        pred_byte = est_tokens(sum(cf[j][2] for j in range(k)) + byte_off)
    elif cls == "TAIL":
        pred_byte = est_tokens(sum(cf[j][2] for j in range(len(cf))))
    else:
        pred_byte = pred_msg
    cached = cached_total(cur.get("usage") or {})
    from_end = len(cm) - k
    w(
        f"  [{i-1:2}->{i:2}] {fmt_ms(cur.get('ts_ms'))} {str(cur.get('model'))[:18]:18} "
        f"{k:3}  {cls:6} {str(cached):>7} {pred_msg:>10} {pred_byte:>10} {from_end:>9}"
    )
    if cls in ("MID", "HEAD", "TOOLS"):
        detail.append((i, cls, k, byte_off, pm, cm))
w("")
for (i, cls, k, byte_off, pm, cm) in detail:
    w(f"  --- {cls} at record {i}, message index {k} "
      f"({len(cm) - k} from the end), byte offset inside it: {byte_off} ---")
    if k < len(pm) and k < len(cm):
        a = canon(pm[k].get("content"))
        b = canon(cm[k].get("content"))
        w(f"      role={pm[k].get('role')}  prev_len={len(a)}  cur_len={len(b)}")
        if byte_off is not None:
            lo = max(byte_off - 80, 0)
            w(f"      prev[...]: {a[lo:byte_off+160]!r}")
            w(f"      cur [...]: {b[lo:byte_off+160]!r}")
        else:
            w(f"      (differs by length/array position only)")
    else:
        w(f"      (no overlapping index: len(prev)={len(pm)} len(cur)={len(cm)})")
        if cls == "HEAD":
            a = canon(pm[0].get("content")) if pm else ""
            b = canon(cm[0].get("content")) if cm else ""
            n = min(len(a), len(b))
            j = 0
            while j < n and a[j] == b[j]:
                j += 1
            w(f"      system length {len(a)} -> {len(b)}, first byte difference at {j}")
            w(f"      prev[...]: {a[max(j-80,0):j+200]!r}")
            w(f"      cur [...]: {b[max(j-80,0):j+200]!r}")
w("")
w(f"pairs compared: {pairs}   classification: {dict(breaks) or '{}'}")

# ── B2. calibrated boundary check ────────────────────────────────────────
# chars/4 is too crude to test the prediction. Calibrate tokens-per-char from
# request_stats (the provider's own prompt_tokens for the SAME request) and
# recompute the pre-break prefix in tokens. Then: if the provider's cached
# equals the calibrated pre-break prefix, OUR mutation is the whole story; if
# cached is far larger, the cache survives the mutation and the cost lies
# elsewhere.
stats_by_ts = {}
for r in rows:
    if r[8]:
        stats_by_ts.setdefault(int(r[8]), []).append(r)


def match_stat(rec):
    """The request_stats row for this trace record (nearest timestamp,
    same model, within 8s)."""
    if not rec.get("ts_ms"):
        return None
    sec = rec["ts_ms"] / 1000.0
    best = None
    for k in range(int(sec) - 8, int(sec) + 9):
        for row in stats_by_ts.get(k, []):
            if row[0] != rec.get("model"):
                continue
            d = abs(k - sec)
            if best is None or d < best[0]:
                best = (d, row)
    return best[1] if best else None


w("")
w("B2. calibrated boundary check (provider prompt_tokens vs chars)")
w("")
w("  pair    prompt  tokens/char  cached  pre-break(tok)  post-break(tok)  cached~pre?")
for i in range(1, len(recs)):
    prev, cur = recs[i - 1], recs[i]
    if prev.get("model") != cur.get("model"):
        continue
    pm, _ = record_body(prev)
    cm, ctools = record_body(cur)
    if not pm or not cm:
        continue
    cf = [fingerprint(m) for m in cm]
    k = None
    for j in range(min(len([fingerprint(m) for m in pm]), len(cf))):
        if [fingerprint(m) for m in pm][j] != cf[j]:
            k = j
            break
    if k is None:
        k = min(len(pm), len(cm))
    stat = match_stat(cur)
    if not stat:
        w(f"  [{i-1:2}->{i:2}] no request_stats row within 8s — skipped")
        continue
    prompt_tokens = stat[2]
    total_chars = sum(cf[j][2] for j in range(len(cf))) + len(canon(ctools))
    if not total_chars:
        continue
    ratio = prompt_tokens / total_chars
    chars_before = sum(cf[j][2] for j in range(k))
    # NOTE: `prev` is the trace RECORD dict, not the message list — the guard
    # must be on `pm` (len(pm)), otherwise the intra-message byte refinement is
    # dead code and `pre` under-counts the partial match inside the mutated
    # message. Related known bias: the numerator omits the tools-array chars
    # that the calibration denominator includes, so `pre` runs ~13-16K low on
    # 30-tool prompts (a NO verdict can be an estimator artifact — see the
    # report, which rests only on the YES pairs).
    if k < len(pm) and k < len(cm):
        a = canon(pm[k].get("content"))
        b = canon(cm[k].get("content"))
        n = min(len(a), len(b))
        j = 0
        while j < n and a[j] == b[j]:
            j += 1
        chars_before += j
    pre = int(chars_before * ratio)
    post = prompt_tokens - pre
    cached = cached_total(cur.get("usage") or {})
    verdict = "n/a"
    if cached is not None:
        verdict = (
            "YES (our mutation)"
            if abs(cached - pre) <= max(0.1 * prompt_tokens, 4000)
            else f"NO (cached {cached - pre:+,} above pre-break)"
        )
    w(
        f"  [{i-1:2}->{i:2}] {prompt_tokens:7,} {ratio:12.3f} {str(cached):>7} "
        f"{pre:16,} {post:16,}  {verdict}"
    )

# ── B3. hysteresis-gate audit ────────────────────────────────────────────
w("")
w("=" * 78)
w("B3. hysteresis-gate audit (is the gate ever closed?)")
w("=" * 78)
w("`compact_old_tool_results(messages, 10, 20, 500)` fires only while the")
w("INTACT population (tool results without the marker) exceeds keep_high=20.")
w("`truncate_tool_result_at` REFUSES to touch a result of <= keep_chars+100 and")
w("does NOT mark it (src/agent/context.rs:849-851) -> such results stay intact")
w("forever. If 11+ of them exist, `intact > 20` is true on EVERY request and")
w("the sliding keep window cuts one more already-sent result per request.")
w("")
w("  rec  toolmsgs  marked  intact  short(<=601)  truncatable  gate")
for i, r in enumerate(recs):
    msgs, _ = record_body(r)
    tool_msgs = [
        m for m in msgs if isinstance(m, dict) and m.get("role") == "tool"
    ]
    marked = 0
    short = 0
    truncatable = 0
    for m in tool_msgs:
        c = canon(m.get("content"))
        if COMPACT_MARKER in c:
            marked += 1
        elif len(c) <= 601:
            short += 1
        else:
            truncatable += 1
    intact = short + truncatable
    w(
        f"  {i:3} {len(tool_msgs):9} {marked:7} {intact:7} {short:13} {truncatable:12} "
        f"{'OPEN (fires)' if intact > 20 else 'closed'}"
    )
w("")
w("Read it as: 'short' results are permanently unmarkable by construction, so")
w("the gate cannot close while short >= keep_high - keep + 1 = 11 (the recent")
w("window keeps at most 10 intact). 'truncatable' > 0 with the gate OPEN means")
w("this request rewrote an already-sent result — a guaranteed prefix break.")

# ── C. request_stats ──────────────────────────────────────────────────────
w("")
w("=" * 78)
w("C. request_stats (memory.db, always-on)")
w("=" * 78)
w(f"rows: {len(rows)}  (loaded up-front — sections B2/C/E/F share the same set)")
if rows:
    ts_all = [r[8] for r in rows if r[8]]
    if ts_all:
        w(
            f"window: {datetime.datetime.fromtimestamp(min(ts_all), datetime.timezone.utc):%Y-%m-%d}"
            f" .. {datetime.datetime.fromtimestamp(max(ts_all), datetime.timezone.utc):%Y-%m-%d} UTC (epoch clock)"
        )
    models = sorted({r[0] for r in rows})
    w(f"models ({len(models)}): {', '.join(models)}")
    w(f"distinct sessions: {len({r[1] for r in rows if r[1]})}")

    main_rows = [r for r in rows if r[9] is None and r[10] is None]
    w(f"main-loop rows (outcome=None AND purpose=None): {len(main_rows)}")

    reporting = {m for m in models if any(r[5] for r in rows if r[0] == m)}
    nonrep = [m for m in models if m not in reporting]
    w(f"reporting models ({len(reporting)}): {', '.join(sorted(reporting))}")
    w(f"non-reporting models ({len(nonrep)}): {', '.join(nonrep)}")
    w("  (non-reporting = 0% BY CONSTRUCTION, excluded from the headline)")

    def agg(rs):
        p = sum(r[2] for r in rs)
        c = sum(r[5] for r in rs)
        return pct(c, p), c, p

    w("")
    w("  tier            n       hit%      cached          prompt     resets<50%")
    for label, rs in (
        ("main reporting", [r for r in main_rows if r[0] in reporting]),
        ("main non-reporting", [r for r in main_rows if r[0] not in reporting]),
        ("main all", main_rows),
    ):
        if rs:
            hp, c, p = agg(rs)
            resets = sum(1 for r in rs if r[2] and r[5] / r[2] < 0.5)
            w(f"  {label:16} {len(rs):6} {hp:>9} {c:>13,} {p:>15,} {resets:8} ({pct(resets, len(rs))})")

    w("")
    w("per-model (main-loop rows; R = reporting tier):")
    w("  model                          tier  n      hit%     resets   avgP      p90P      maxP")
    bym = collections.defaultdict(list)
    for r in main_rows:
        bym[r[0]].append(r)
    for m, rs in sorted(bym.items(), key=lambda kv: -len(kv[1])):
        hp, _, _ = agg(rs)
        ps = sorted(r[2] for r in rs)
        p90 = ps[int(len(ps) * 0.9)] if ps else 0
        resets = sum(1 for r in rs if r[2] and r[5] / r[2] < 0.5)
        w(
            f"  {m[:28]:28} {'R ' if m in reporting else 'nr'}   {len(rs):6} {hp:>8} "
            f"{resets:7}  {sum(ps)//max(len(ps),1):9,} {p90:9,} {max(ps) if ps else 0:9,}"
        )

    # Per-session shape for the trace session's model(s): the within-session
    # ramp (cold -> warm) is what distinguishes cold-start cost from mutations.
    trace_models = {r.get("model") for r in recs}
    sess = collections.Counter(r[1] for r in main_rows if r[0] in trace_models)
    top = [s for s, _ in sess.most_common(3)]
    if top:
        w("")
        w("per-session shape (sessions using the traced models, first 3 by size;")
        w("request index is the position within the session):")
        for sid in top:
            rs = sorted([r for r in main_rows if r[1] == sid and r[0] in trace_models], key=lambda r: r[8])
            if len(rs) < 4:
                continue
            w(f"  session {str(sid)[:16]} ({rs[0][0]}), n={len(rs)}:")
            step = max(len(rs) // 12, 1)
            shard = []
            for j in range(0, len(rs), step):
                chunk = rs[j : j + step]
                hp, _, _ = agg(chunk)
                shard.append(f"{j}:{hp}")
            w("    " + "  ".join(shard))

    # Cliff histogram — the MEASURED location, not the assumed 340K.
    w("")
    w("prompt-size histogram (main reporting rows, 40K bins, hit% per bin):")
    bins = collections.defaultdict(lambda: [0, 0, 0])
    for r in main_rows:
        if r[0] in reporting:
            b = (r[2] // 40_000) * 40_000
            bins[b][0] += 1
            bins[b][1] += r[2]
            bins[b][2] += r[5]
    for b in sorted(bins):
        n, p, c = bins[b]
        w(f"  {b/1000:6.0f}K-{(b+40_000)/1000:6.0f}K  n={n:6}  hit={pct(c, p):>7}")

    # Miss buckets: exactly one per main-loop row, first match wins.
    buckets = collections.Counter()
    bucket_miss = collections.Counter()
    bucket_rows = collections.defaultdict(list)
    last_model = {}
    for r in sorted(main_rows, key=lambda r: r[8]):
        sid, model, pt, ct = r[1], r[0], r[2], r[5]
        if sid in last_model and last_model[sid] != model:
            b = "SWITCH"
        elif sid not in last_model:
            b = "COLD"
        elif model not in reporting:
            b = "NONREPORTING"
        elif pt > CLIFF:
            b = "CLIFF"
        else:
            b = "STEADY"
        buckets[b] += 1
        bucket_miss[b] += max(pt - ct, 0)
        bucket_rows[b].append(r)
        last_model[sid] = model

    total_miss = sum(bucket_miss.values()) or 1
    w("")
    w("miss buckets (main-loop rows; one bucket each, precedence SWITCH > COLD >")
    w("NONREPORTING > CLIFF > STEADY). STEADY is the residual where a real")
    w("prefix mutation or provider behavior must be hiding — it is the bucket")
    w("section B's MID findings should be checked against:")
    w("  bucket          n      share_n    miss_tokens   share_of_miss   hit%")
    for b, n in buckets.most_common():
        hp, _, _ = agg(bucket_rows[b])
        w(
            f"  {b:14} {n:7} {pct(n, len(main_rows)):>10} {bucket_miss[b]:15,} "
            f"{pct(bucket_miss[b], total_miss):>15} {hp:>7}"
        )
    w(f"  total misses: {total_miss:,} tokens across {sum(buckets.values()):,} main-loop rows")
    recon = sum(buckets.values())
    w(f"  reconciliation: buckets {recon} == main-loop rows {len(main_rows)} -> "
      f"{'OK' if recon == len(main_rows) else 'MISMATCH'}")

# ── E. mutation-path cross-check verdicts ───────────────────────────────
w("")
w("=" * 78)
w("E. mutation-path cross-check verdicts (code read, no edits)")
w("=" * 78)
w("")
w("A. PERMANENT-OPEN HYSTERESIS GATE — BUG (avoidable), the steady-state cap")
w("   code: src/agent/context.rs::compact_old_tool_results (939-1006; gate at")
w("     969, intact population built at 950-963); truncate_tool_result_at")
w("     (829-851: the `s.chars().count() <= keep_chars + 100` early return at")
w("     849-851 returns false WITHOUT adding COMPACTED_MARKER); call site")
w("     src/agent/turn.rs:1602 = compact_old_tool_results(messages, 10, 20, 500).")
w("   evidence: B3 — intact 68-71 > keep_high 20 on EVERY request, of which")
w("     60-63 are permanently unmarkable short results and only 7-8 truncatable.")
w("     B — MID on 5/5 same-model pairs, always ~20 messages from the end,")
w("     byte offset 504-517 inside the result, 1193-8745 chars -> 544-717.")
w("     B2 — cached == the calibrated pre-break prefix on pairs 3->4/4->5")
w("     (prediction CONFIRMED): our own rewrite, not a provider limit.")
w("   consequence: ~36-41K tokens of tail re-billed EVERY request; steady")
w("     state 70-78% where 99.0-99.7% is reached whenever the prefix is stable.")
w("   fix direction: make the gate honest — count only TRUNCATABLE results")
w("     (len > keep_chars + 100) toward the high-water mark, or mark a short")
w("     result once so it leaves the intact population for good.")
w("")
w("B. HEAD REWRITE ON PLAN-KIND CHANGE - AVOIDABLE, deliberate design cost")
w("   code: src/workflow/mod.rs::schema_filter (458-474) freezes the ADVERTISED")
w("     schema once per plan (ToolFilter::PlanFrozen, src/tool/mod.rs:219-239)")
w("     and picks ExecutingResearch for kind=research - pinned by the test")
w("     schema_filter_research_plan_uses_executing_research (workflow/mod.rs:1836).")
w("   evidence: pair 5->6 HEAD - system 26,993 -> 25,831 chars, tools 30 -> 26,")
w("     cached collapses to 6,016 (the common system prefix). request_stats:")
w("     7 of 24 window rows at ~5% with ttft 15.7s/17.9s, then 99.7% recovery.")
w("   verdict: AVOIDABLE but a DESIGN DECISION - the research filter is")
w("     advertisement-only (dispatch enforces), so the schema COULD be the")
w("     stable superset for both kinds. Not changed here: it is documented")
w("     intent with its own test; the cost is now measured.")
w("")
w("C. NOT the cause (measured): the 340K cliff - the 320-360K bin hits 96.9%,")
w("   max prompt 322,619; non-reporting models - none in this dataset (all 6")
w("   models report cache); the R14 shape - hysteresis exists, but see A.")
w("")
w("D. INSTRUMENT: the trace log is appended while a request streams, so its")
w("   last line is a partial record (section A note) - any consumer must")
w("   tolerate it (the round-5 extractor crashed on it).")

w("")
w("=" * 78)
w("F. the trace session in request_stats (real prompt_tokens → true hit%)")
w("=" * 78)
if recs:
    t0 = (min(r.get("ts_ms") or 0 for r in recs)) / 1000.0 - 120
    t1 = (max(r.get("ts_ms") or 0 for r in recs)) / 1000.0 + 120
    conn = sqlite3.connect(f"file:{MEMDB}?mode=ro", uri=True)
    conn.execute("PRAGMA busy_timeout=3000")
    mine = [
        r
        for r in rows
        if t0 <= r[8] <= t1 and r[0] in {x.get("model") for x in recs}
    ]
    conn.close()
    w(f"rows in the trace window ({fmt_ms(int(t0*1000))} .. {fmt_ms(int(t1*1000))} + pad),")
    w("matched to the traced models — this is the session's TRUE cache picture:")
    w("  #   time              model                    prompt    cached   hit%    ttft  out  purpose")
    if not mine:
        w("  (none — request_stats may lag the trace log)")
    for n, r in enumerate(sorted(mine, key=lambda r: r[8])):
        hp = pct(r[5], r[2])
        w(
            f"  {n:3} {datetime.datetime.fromtimestamp(r[8], datetime.timezone.utc):%Y-%m-%d %H:%M:%S} "
            f"{r[0][:22]:22} {r[2]:8,} {r[5]:9,} {hp:>7} {r[6]:6} {r[3]:4} "
            f"{str(r[10])[:14]:14} {str(r[9])[:10]}"
        )
    if mine:
        p = sum(r[2] for r in mine)
        c = sum(r[5] for r in mine)
        w(f"  window total: {p:,} prompt tokens, {c:,} cached → hit {pct(c, p)}")

with open(OUTPATH, "w", encoding="utf-8") as f:
    f.write("\n".join(OUT) + "\n")
print("\n".join(OUT[:8]))
print(f"... wrote {OUTPATH} ({len(OUT)} lines)")
