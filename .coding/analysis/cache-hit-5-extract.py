# Cache-hit round 5 data extraction (2027-01-09 review round).
# Read-only over .coding/memory.db (request_stats), .coding/logs/traces.jsonl,
# and .coding/logs/provider-errors.jsonl. Writes compact aggregates to
# .coding/analysis/cache-hit-5-aggregates.txt for the read-only reviewer.

import sqlite3
import json
import datetime
import statistics
import collections

OUT = []
def w(s=''):
    OUT.append(s)

def fmt_ts_ms(ms):
    return datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc).strftime('%Y-%m-%d %H:%M:%S')

def fmt_ts(epoch):
    return datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc).strftime('%Y-%m-%d')

def pct(a, b):
    return f'{(100.0 * a / b if b else 0):.1f}%'

def pctl(vals, q):
    if not vals:
        return 0
    s = sorted(vals)
    return s[min(len(s) - 1, int(len(s) * q))]

# R12/R13/R17 batch (heuristic fix, fill_rate 0.3, output budget) landed ~2026-08-31.
R12 = int(datetime.datetime(2026, 8, 31, tzinfo=datetime.timezone.utc).timestamp())

# ---------------- A. request_stats ----------------
c = sqlite3.connect('file:.coding/memory.db?mode=ro', uri=True)
c.execute('PRAGMA busy_timeout=3000')
cur = c.cursor()
rows = cur.execute(
    'SELECT model, session_id, prompt_tokens, completion_tokens, reasoning_tokens, '
    'cached_tokens, ttft_ms, generation_ms, created_at, outcome, purpose FROM request_stats'
).fetchall()
# R21: error rows carry cached_tokens/ttft_ms/generation_ms NULL (the provider
# never reported usage — not a real miss) and outcome='error'; compaction rows
# carry purpose='summarize'. Normalize the NULL numerics to 0 for the
# arithmetic below — the tagged rows are segmented out of the hit aggregates
# first, so the NULL-vs-0 distinction is preserved by the outcome tag.
rows = [(r[0], r[1], r[2], r[3], r[4], r[5] or 0, r[6] or 0, r[7] or 0, r[8], r[9], r[10])
        for r in rows]
c.close()

w('=' * 78)
w('A. request_stats (memory.db, always-on per-request stats)')
w('=' * 78)
tmin = min(r[8] for r in rows); tmax = max(r[8] for r in rows)
w(f'rows: {len(rows)}   window: {fmt_ts(tmin)} .. {fmt_ts(tmax)} UTC (epoch clock)')
w(f'R12/R13/R17 cutoff: {fmt_ts(R12)} UTC (epoch {R12})')
models = sorted({r[0] for r in rows})
w(f'models ({len(models)}): {", ".join(models)}')
w(f'distinct sessions: {len({r[1] for r in rows if r[1]})}')
w(f'ttft_ms=0 rows: {sum(1 for r in rows if r[6] == 0)} (0 = unrecorded)')
# R21 segmentation: the hit/reset aggregates below (A0-A5) run on main-loop
# rows only. Error rows carry no cache signal (the provider never reported
# usage); summarize rows are compaction's own mega-prompt — a fresh single-
# message prefix, structurally unable to hit cache — so including either
# would drag the conversation hit% down for a structural reason, not a
# cache-health signal. Both get their own section (A6).
error_rows = [r for r in rows if r[9] == 'error']
summarize_rows = [r for r in rows if r[10] == 'summarize']
cancelled_rows = [r for r in rows if r[9] == 'cancelled']
main_rows = [r for r in rows if r[9] is None and r[10] is None]
w(f'rows by tag: main={len(main_rows)}  error={len(error_rows)}  summarize={len(summarize_rows)}  cancelled={len(cancelled_rows)}')
rows = main_rows

def hit_of(r):
    return (r[5] / r[2]) if r[2] else 0.0

# R19 tier split: "field absent" (Gemini's OpenAI-compat layer doesn't
# populate prompt_tokens_details.cached_tokens) and "nothing cached" are
# indistinguishable in a single row. A model that NEVER reported
# cached_tokens > 0 in the window is treated as a non-reporting tier —
# its rows are 0% hit / 100% resets by construction and contaminate the
# combined aggregates. The static list from the round-5 review is a
# cross-check only (it goes stale when a provider starts reporting).
REPORTING = {m for m in models if any(r[5] > 0 for r in rows if r[0] == m)}
NON_REPORTING = [m for m in models if m not in REPORTING]
STATIC_NON_REPORTING = ('gemini-vlm-gcp', 'glm-5.3-flash', 'glm-5.2-maas-gcp', 'deepseek-v4-flash')

def static_nonreporting(m):
    return any(m.startswith(p) for p in STATIC_NON_REPORTING)

def tier_rows(rs, reporting):
    return [r for r in rs if (r[0] in REPORTING) == reporting]

def agg_line(rs):
    resets = sum(1 for r in rs if hit_of(r) < 0.5)
    return (f'n={len(rs):<6} hit={pct(sum(r[5] for r in rs), sum(r[2] for r in rs)):>7}  '
            f'resets(<50%)={resets} ({pct(resets, len(rs))})')

w()
w('--- A0. Cache-reporting tier split (R19) ---')
w(f'reporting models ({len(REPORTING)}): {", ".join(sorted(REPORTING))}')
w(f'non-reporting models ({len(NON_REPORTING)}): {", ".join(NON_REPORTING)}')
stale = [m for m in models if m in REPORTING and static_nonreporting(m)]
unlisted = [m for m in NON_REPORTING if not static_nonreporting(m)]
w(f'static-list cross-check: stale (heuristic=reporting, list=non-reporting): '
  f'{", ".join(stale) if stale else "none"}; unlisted non-reporters: '
  f'{", ".join(unlisted) if unlisted else "none"}')
for label, sel in (('all-time', rows), ('post-R12', [r for r in rows if r[8] >= R12])):
    rep = tier_rows(sel, True)
    non = tier_rows(sel, False)
    rep_res = sum(1 for r in rep if hit_of(r) < 0.5)
    non_res = sum(1 for r in non if hit_of(r) < 0.5)
    w(f'{label}:')
    w(f'  {"reporting":<14}{agg_line(rep)}')
    w(f'  {"non-reporting":<14}{agg_line(non)}')
    w(f'  reconciliation: rows {len(rep)}+{len(non)}={len(rep) + len(non)} (of {len(sel)})  '
      f'resets {rep_res}+{non_res}={rep_res + non_res} (of {sum(1 for r in sel if hit_of(r) < 0.5)})')

w()
w('--- A1. Per-model aggregates (all time; t: R=reporting, N=non-reporting tier) ---')
w(f"{'model':<34}{'t':>2}{'n':>6}{'hit%':>7}{'res%':>6}{'avgP':>8}{'p90P':>8}{'maxP':>8}{'avgR':>7}{'ttft50':>8}{'ttft90':>8}{'gen50':>7}{'gen90':>7}")
for m in models:
    rs = [r for r in rows if r[0] == m]
    prompts = [r[2] for r in rs]
    resets = sum(1 for r in rs if hit_of(r) < 0.5)
    ttfts = [r[6] for r in rs if r[6] > 0]
    gens = [r[7] for r in rs if r[7] > 0]
    w(f"{m:<34}{('R' if m in REPORTING else 'N'):>2}{len(rs):>6}{pct(sum(r[5] for r in rs), sum(prompts)):>7}"
      f"{pct(resets, len(rs)):>6}{int(statistics.mean(prompts)) if prompts else 0:>8}"
      f"{pctl(prompts, 0.9):>8}{max(prompts) if prompts else 0:>8}"
      f"{int(statistics.mean([r[4] for r in rs])) if rs else 0:>7}"
      f"{pctl(ttfts, 0.5):>8}{pctl(ttfts, 0.9):>8}{pctl(gens, 0.5):>7}{pctl(gens, 0.9):>7}")

w()
w('--- A2. Daily trend (UTC) ---')
w('rep* = reporting-tier rows only (the prompt-shape baseline); nonN = non-reporting rows (cached')
w('always 0). Read day dips as nonN share + fallback-induced cold caches + over-cliff requests,')
w('not prompt-shape regression (R19).')
w(f"{'day':<12}{'n':>6}{'hit%':>7}{'avgP':>8}{'maxP':>8}{'resets':>7} | {'repN':>6}{'repHit%':>8}{'repRes%':>8}{'nonN':>6}")
days = collections.defaultdict(list)
for r in rows:
    days[datetime.datetime.fromtimestamp(r[8], datetime.timezone.utc).strftime('%m-%d')].append(r)
for day in sorted(days):
    rs = days[day]
    drep = tier_rows(rs, True)
    drep_res = sum(1 for r in drep if hit_of(r) < 0.5)
    rep_cols = (f"{pct(sum(r[5] for r in drep), sum(r[2] for r in drep)):>8}{pct(drep_res, len(drep)):>8}"
                if drep else f"{'-':>8}{'-':>8}")
    w(f"{day:<12}{len(rs):>6}{pct(sum(r[5] for r in rs), sum(r[2] for r in rs)):>7}"
      f"{int(statistics.mean([r[2] for r in rs])):>8}{max(r[2] for r in rs):>8}"
      f"{sum(1 for r in rs if hit_of(r) < 0.5):>7} | {len(drep):>6}"
      f"{rep_cols}"
      f"{len(rs) - len(drep):>6}")

w()
BUCKETS = [(0, 50_000), (50_000, 100_000), (100_000, 200_000), (200_000, 300_000),
           (300_000, 340_000), (340_000, 400_000), (400_000, 10**12)]

def emit_buckets(label, sel):
    w(f'--- A3. Hit rate by prompt-size bucket ({label}): all-time | post-R12 ---')
    w(f"{'bucket':>14}{'n':>7}{'hit%':>7}{'res%':>6} | {'n':>6}{'hit%':>7}{'res%':>6}  post-R12")
    for lo, hi in BUCKETS:
        def agg(rs):
            if not rs:
                return f"{'0':>6}{'-':>7}{'-':>6}"
            resets = sum(1 for r in rs if hit_of(r) < 0.5)
            return f"{len(rs):>6}{pct(sum(r[5] for r in rs), sum(r[2] for r in rs)):>7}{pct(resets, len(rs)):>6}"
        lab = f'>{lo // 1000}K' if hi > 10**11 else f'{lo // 1000}K-{hi // 1000}K'
        w(f"{lab:>14}{agg([r for r in sel if lo <= r[2] < hi])} | {agg([r for r in sel if lo <= r[2] < hi and r[8] >= R12])}")

emit_buckets('all rows', rows)
w()
emit_buckets('reporting tier', tier_rows(rows, True))
w()
emit_buckets('non-reporting tier', tier_rows(rows, False))

w()
post = [r for r in rows if r[8] >= R12]

def emit_post_r12(label, sel, note='real provider-reported cached tokens'):
    w(f'--- A4. Post-R12 subset, {label} ({note}) ---')
    w(f'rows: {len(sel)}  hit: {pct(sum(r[5] for r in sel), sum(r[2] for r in sel))}  '
      f'resets(<50%): {sum(1 for r in sel if hit_of(r) < 0.5)} ({pct(sum(1 for r in sel if hit_of(r) < 0.5), len(sel))})')
    hi90 = sum(1 for r in sel if hit_of(r) >= 0.9)
    mid = sum(1 for r in sel if 0.5 <= hit_of(r) < 0.9)
    w(f'hit distribution: >=90%: {pct(hi90, len(sel))}   50-90%: {pct(mid, len(sel))}   <50%: {pct(sum(1 for r in sel if hit_of(r) < 0.5), len(sel))}')
    big = [r for r in sel if r[2] > 340_000]
    if big:
        w(f'rows with prompt>340K: {len(big)}  their hit: {pct(sum(r[5] for r in big), sum(r[2] for r in big))}  '
          f'resets: {sum(1 for r in big if hit_of(r) < 0.5)}')
    else:
        w('rows with prompt>340K: 0')
    w('per-model (post-R12):')
    for m in sorted({r[0] for r in sel}):
        rs = [r for r in sel if r[0] == m]
        w(f"  {m:<34} n={len(rs):<6} hit={pct(sum(r[5] for r in rs), sum(r[2] for r in rs)):>7}  "
          f"resets={pct(sum(1 for r in rs if hit_of(r) < 0.5), len(rs)):>6}  avgP={int(statistics.mean([r[2] for r in rs]))}")

emit_post_r12('all rows', post)
w()
emit_post_r12('reporting tier (the prompt-shape baseline)', tier_rows(post, True))
w()
emit_post_r12('non-reporting tier', tier_rows(post, False), note='cached_tokens field absent — 0 is not a miss')

w()

def emit_worst_sessions(label, sel):
    w(f'--- A5. 10 worst sessions by token-weighted hit rate, {label} (>=20 requests) ---')
    sess = collections.defaultdict(list)
    for r in sel:
        if r[1]:
            sess[r[1]].append(r)
    ranked = sorted(((sum(r[5] for r in rs) / sum(r[2] for r in rs), sid, rs)
                     for sid, rs in sess.items() if len(rs) >= 20 and sum(r[2] for r in rs) > 0))
    for hr, sid, rs in ranked[:10]:
        w(f'{sid[:8]}  n={len(rs):<5} hit={hr * 100:.1f}%  models={",".join(sorted({r[0] for r in rs}))[:60]}')

emit_worst_sessions('all rows', rows)
w()
emit_worst_sessions('reporting tier only', tier_rows(rows, True))

w()
w('--- A6. R21 tagged rows (outcome/purpose) ---')
w('error rows: our estimated prompt tokens, cached_tokens NULL (the provider never reported')
w('usage — not a real miss). summarize rows: compaction\'s own mega-prompt — a fresh single-')
w('message prefix, structurally unable to hit cache; excluded from the hit aggregates above')
w('so they do not drag the conversation hit% down for a structural reason.')
w(f'error rows: {len(error_rows)}')
for m in sorted({r[0] for r in error_rows}):
    rs = [r for r in error_rows if r[0] == m]
    w(f'  {m:<34} n={len(rs):<6} estPromptAvg={int(statistics.mean([r[2] for r in rs]))}')
w(f'summarize rows: {len(summarize_rows)}')
for m in sorted({r[0] for r in summarize_rows}):
    rs = [r for r in summarize_rows if r[0] == m]
    w(f'  {m:<34} n={len(rs):<6} promptAvg={int(statistics.mean([r[2] for r in rs]))}  '
      f'completionAvg={int(statistics.mean([r[3] for r in rs]))}')
w(f'cancelled rows (D1: the consumer dropped the stream mid-flight on a user interrupt — our')
w(f'estimated prompt tokens, completion 0, cached_tokens NULL; excluded from the hit aggregates):')
w(f'cancelled rows: {len(cancelled_rows)}')
for m in sorted({r[0] for r in cancelled_rows}):
    rs = [r for r in cancelled_rows if r[0] == m]
    w(f'  {m:<34} n={len(rs):<6} estPromptAvg={int(statistics.mean([r[2] for r in rs]))}')

# ---------------- B. traces.jsonl ----------------
w()
w('=' * 78)
w('B. traces.jsonl (opt-in full request traces)')
w('=' * 78)
with open('.coding/logs/traces.jsonl', encoding='utf-8') as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        t = json.loads(line)
        w(f"id={t.get('id')}  ts={fmt_ts_ms(t.get('ts_ms', 0))}  model={t.get('model')}  provider={t.get('provider')}")
        w(f"top-level keys: {sorted(t.keys())}")
        rq = t.get('request_json') or {}
        msgs = rq.get('messages') or []
        tools = rq.get('tools') or []
        w(f"max_completion_tokens={rq.get('max_completion_tokens')}  nMessages={len(msgs)}  nTools={len(tools)}")
        roles = collections.Counter(m.get('role') for m in msgs)
        w(f'role counts: {dict(roles)}')
        if msgs:
            head = msgs[0].get('content') or ''
            w(f'system head len: {len(head)} chars')
            sizes = sorted(((len(str(m.get("content") or "")), m.get('role'), i) for i, m in enumerate(msgs)), reverse=True)[:8]
            w('largest messages (chars, role, idx): ' + ', '.join(f'{n}@{role}[{i}]' for n, role, i in sizes))
            tail = msgs[-1]
            w(f"last msg: role={tail.get('role')} len={len(str(tail.get('content') or ''))} "
              f"head80={str(tail.get('content') or '')[:80]!r}")
        for k in ('usage', 'response', 'ttft_ms', 'generation_ms', 'send_ms', 'finish_reason', 'error'):
            if k in t:
                v = t[k]
                w(f'{k}: {str(v)[:300]}')

# ---------------- C. provider-errors.jsonl ----------------
w()
w('=' * 78)
w('C. provider-errors.jsonl')
w('=' * 78)
errs = []
with open('.coding/logs/provider-errors.jsonl', encoding='utf-8') as f:
    for line in f:
        line = line.strip()
        if line:
            errs.append(json.loads(line))
w(f'total: {len(errs)}   window: {fmt_ts_ms(min(e["ts_ms"] for e in errs))} .. {fmt_ts_ms(max(e["ts_ms"] for e in errs))} UTC')

def err_sig(e):
    try:
        inner = json.loads(e.get('error') or '{}')
        msg = (inner.get('error') or {}).get('message') or inner.get('message') or ''
        return msg[:110].replace('\n', ' ')
    except Exception:
        return (e.get('error') or '')[:110].replace('\n', ' ')

w()
w('--- C1. Histogram by (http_status, model) ---')
hist = collections.Counter((e.get('http_status'), e.get('model')) for e in errs)
for (status, model), n in hist.most_common():
    ex = next(e for e in errs if e.get('http_status') == status and e.get('model') == model)
    w(f'status={status:<5} n={n:<5} model={model}')
    w(f'   e.g. {err_sig(ex)}')

w()
w('--- C2. Per-day counts ---')
edays = collections.Counter(
    datetime.datetime.fromtimestamp(e['ts_ms'] / 1000, datetime.timezone.utc).strftime('%m-%d') for e in errs)
for day in sorted(edays):
    w(f'{day}: {edays[day]}')

w()
w('--- C3. Top bursts (errors within 120s of each other, size>=5) ---')
seq = sorted(errs, key=lambda e: e['ts_ms'])
bursts = []
cur_burst = []
for e in seq:
    if cur_burst and e['ts_ms'] - cur_burst[-1]['ts_ms'] > 120_000:
        if len(cur_burst) >= 5:
            bursts.append(cur_burst)
        cur_burst = []
    cur_burst.append(e)
if len(cur_burst) >= 5:
    bursts.append(cur_burst)
bursts.sort(key=len, reverse=True)
for b in bursts[:6]:
    span = (b[-1]['ts_ms'] - b[0]['ts_ms']) / 1000
    w(f"n={len(b):<4} span={span:.0f}s  start={fmt_ts_ms(b[0]['ts_ms'])}  "
      f"statuses={dict(collections.Counter(e.get('http_status') for e in b))}  model={b[0].get('model')}")

w()
w('--- C4. Post-R12 subset ---')
post_errs = [e for e in errs if e['ts_ms'] / 1000 >= R12]
w(f'rows: {len(post_errs)}')
phist = collections.Counter((e.get('http_status'), e.get('model')) for e in post_errs)
for (status, model), n in phist.most_common(12):
    w(f'status={status:<5} n={n:<5} model={model}')

with open('.coding/analysis/cache-hit-5-aggregates.txt', 'w', encoding='utf-8', newline='\n') as f:
    f.write('\n'.join(OUT) + '\n')
print(f'wrote .coding/analysis/cache-hit-5-aggregates.txt ({len(OUT)} lines)')
