# Read-only diagnosis: why did the last trace calls show a big cache reset?
# (plan 83fb316a, user question 2027-01-11). Sections:
#   A tail table    - last N records: usage, shape, head/tail fingerprints,
#                     fold-vendor tail-in-head markers, in-place compaction markers
#   B break locator - per pair: first differing message index, classification,
#                     approx tokens before the break vs the actual cached value
#   C request_stats - the always-on rows: session identity + summarize/error tags
# Never prints a raw request body (a record is capped at 256 KiB ~ 65k tokens):
# everything is projected to lengths, sha1s and 300-char previews.
# Writes .coding/analysis/cache-reset-tail.txt
import datetime, hashlib, json, sqlite3

TRACES = ".coding/logs/traces.jsonl"
MEMDB = ".coding/memory.db"
OUTPATH = ".coding/analysis/cache-reset-tail.txt"
N = 6
COMPACT_MARKER = "[\u2026 truncated for context efficiency]"
FOLD_MARKERS = ("WORKFLOW STATE", "RECALLED MEMORIES", "CONTEXT FOOTER", "PROGRESS:")
OUT, BRIEF = [], []

def w(s=""):
    OUT.append(s)

def b(s=""):
    BRIEF.append(s)

def fmt_ms(ms):
    if not ms:
        return "-"
    return datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc).strftime("%Y-%m-%d %H:%M:%S")

def fmt_epoch(ts):
    if ts is None:
        return "-"
    v = float(ts)
    if v > 1e12:
        v /= 1000.0
    try:
        return datetime.datetime.fromtimestamp(v, datetime.timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
    except Exception:
        return "raw=%s" % ts

def pct(a, n):
    return "%.1f%%" % (100.0 * a / n) if n else "n/a"

def canon(c):
    return json.dumps(c, sort_keys=True, ensure_ascii=False)

def deep_cached(node, prefix=""):
    found = {}
    if isinstance(node, dict):
        for k, v in node.items():
            if isinstance(v, (dict, list)):
                found.update(deep_cached(v, prefix + k + "."))
            elif "cach" in k.lower() and isinstance(v, (int, float)):
                found[prefix + k] = int(v)
    elif isinstance(node, list):
        for i, v in enumerate(node):
            found.update(deep_cached(v, "%s%d." % (prefix, i)))
    return found

def cached_total(u):
    vals = list(deep_cached(u or {}).values())
    return sum(vals) if vals else None

def record_body(r):
    rj = r.get("request_json") or {}
    if isinstance(rj, str):
        try:
            rj = json.loads(rj)
        except ValueError:
            return [], []
    if not isinstance(rj, dict):
        return [], []
    return rj.get("messages") or [], rj.get("tools") or []

def msg_text(m):
    return canon(m) if not isinstance(m, dict) else canon(m.get("content"))

def fp_pair(m):
    c = msg_text(m)
    tc = canon(m.get("tool_calls")) if isinstance(m, dict) and m.get("tool_calls") is not None else ""
    role = m.get("role") if isinstance(m, dict) else "?"
    return (role, hashlib.sha1((c + "\x00" + tc).encode("utf-8")).hexdigest()[:12], len(c))

def tools_fp(ts):
    return [hashlib.sha1(canon(t).encode("utf-8")).hexdigest()[:12] for t in ts]

def one_line(s, n=300):
    return " ".join(str(s).split())[:n]

def load():
    recs, partial = [], []
    with open(TRACES, encoding="utf-8") as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                recs.append(json.loads(line))
            except ValueError as e:
                partial.append((i, len(line), str(e)))
    return recs, partial

def section_a(recs, partial, tail):
    w("=" * 78)
    w("A. traces.jsonl - %d parseable record(s), %d partial" % (len(recs), len(partial)))
    w("=" * 78)
    for lineno, ln, err in partial:
        w("  partial line %d: %d chars - %s" % (lineno, ln, err[:90]))
    if recs:
        ts = [r.get("ts_ms") or 0 for r in recs]
        w("window: %s .. %s UTC" % (fmt_ms(min(ts)), fmt_ms(max(ts))))
        w("models: %s" % sorted({r.get("model") for r in recs}))
        w("providers: %s" % sorted({str(r.get("provider")) for r in recs}))
    w("")
    b("records=%d partial=%d tail=%d models=%s" % (len(recs), len(partial), len(tail), sorted({r.get("model") for r in tail})))
    w("per record (last %d - index 0 is the OLDEST of the tail):" % len(tail))
    for i, r in enumerate(tail):
        msgs, tools = record_body(r)
        usage = r.get("usage") or {}
        cached = cached_total(usage)
        prompt = usage.get("prompt_tokens")
        head = msg_text(msgs[0]) if msgs else ""
        last = msgs[-1] if msgs else {}
        ltxt = msg_text(last)
        folds = [mk for mk in FOLD_MARKERS if mk in head]
        ncomp = sum(1 for m in msgs if COMPACT_MARKER in msg_text(m))
        w("  [%d] id=%s %s %s/%s" % (i, r.get("id"), fmt_ms(r.get("ts_ms")), r.get("model"), r.get("provider")))
        w("      session=%s nmsg=%d ntools=%d prompt=%s cached=%s hit=%s" % (r.get("session_id") or "-", len(msgs), len(tools), prompt, cached if cached is not None else "NONE", pct(cached or 0, prompt or 0)))
        w("      finish=%s err=%s cancelled=%s guard_cut=%s" % (r.get("finish_reason"), str(r.get("error"))[:60], r.get("cancelled"), r.get("guard_cut")))
        w("      head: len=%d sha=%s fold_markers=%s" % (len(head), hashlib.sha1(head.encode("utf-8")).hexdigest()[:12], folds or "none"))
        w("      head300: %s" % one_line(head))
        w("      last: role=%s len=%d sha=%s" % (last.get("role") if isinstance(last, dict) else "?", len(ltxt), hashlib.sha1(ltxt.encode("utf-8")).hexdigest()[:12]))
        w("      last300: %s" % one_line(ltxt))
        w("      in-place compaction marker in request: %d message(s)" % ncomp)
        b("  [%d] id=%s %s %s prompt=%s cached=%s hit=%s nmsg=%d folds=%s compact=%d" % (i, r.get("id"), fmt_ms(r.get("ts_ms")), r.get("model"), prompt, cached if cached is not None else "NONE", pct(cached or 0, prompt or 0), len(msgs), folds or "-", ncomp))
    w("")

def section_b(tail):
    w("=" * 78)
    w("B. prefix-break localizer (consecutive pairs)")
    w("=" * 78)
    for i in range(len(tail) - 1):
        a, c = tail[i], tail[i + 1]
        ma, ta = record_body(a)
        mb, tb = record_body(c)
        same = (a.get("session_id") == c.get("session_id")) and (a.get("model") == c.get("model"))
        tools_same = tools_fp(ta) == tools_fp(tb)
        k = None
        for j in range(min(len(ma), len(mb))):
            if fp_pair(ma[j]) != fp_pair(mb[j]):
                k = j
                break
        if k is None and len(ma) != len(mb):
            k = min(len(ma), len(mb))
        if not same:
            cls = "UNRELATED (session or model differs)"
        elif k is None:
            cls = "IDENTICAL" if tools_same else "TOOLS (messages identical, tools differ)"
        elif k >= len(ma):
            cls = "TAIL (pure append)" if tools_same else "TAIL (pure append, tools differ)"
        elif k == 0:
            cls = "HEAD (messages[0] changed)"
        else:
            cls = "MID (already-sent message mutated in place)"
        cut = k if k is not None else min(len(ma), len(mb))
        predicted = sum(len(msg_text(m)) for m in ma[:min(cut, len(ma))]) // 4
        cc = cached_total(c.get("usage") or {})
        delta = (cc - predicted) if cc is not None else None
        w("  pair [%d]->[%d] id %s -> %s" % (i, i + 1, a.get("id"), c.get("id")))
        w("      class=%s k=%s nmsg %d->%d ntools %d->%d tools_same=%s" % (cls, k, len(ma), len(mb), len(ta), len(tb), tools_same))
        w("      tokens_before_k~=%d (chars/4 APPROXIMATION, tools excluded) actual_cached=%s delta=%s" % (predicted, cc if cc is not None else "NONE", delta))
        if k is not None and k < len(ma) and k < len(mb):
            w("      prev msg[%d]=%s  cur msg[%d]=%s" % (k, fp_pair(ma[k]), k, fp_pair(mb[k])))
        b("  pair [%d]->[%d] %s k=%s predicted~%d cached=%s delta=%s" % (i, i + 1, cls.split(" ")[0], k, predicted, cc if cc is not None else "NONE", delta))
    w("")

def section_c():
    w("=" * 78)
    w("C. request_stats (always-on) - 15 newest rows")
    w("=" * 78)
    con = sqlite3.connect("file:%s?mode=ro" % MEMDB, uri=True)
    try:
        con.execute("PRAGMA busy_timeout=3000")
        rows = con.execute("SELECT created_at, model, session_id, prompt_tokens, cached_tokens, ttft_ms, outcome, purpose FROM request_stats ORDER BY created_at DESC LIMIT 15").fetchall()
    finally:
        con.close()
    kinds, mains = {}, []
    for idx, (created, model, sid, prompt, cached, ttft, outcome, purpose) in enumerate(rows):
        if purpose == "summarize":
            kind = "SUMMARIZE"
        elif outcome == "error":
            kind = "ERROR"
        elif outcome == "cancelled":
            kind = "CANCELLED"
        elif outcome is None and purpose is None:
            kind = "main"
            mains.append((idx, (sid or "-")[:12], prompt, cached))
        else:
            kind = "%s/%s" % (outcome, purpose)
        kinds[kind] = kinds.get(kind, 0) + 1
        w("  [%2d] %s %s %s prompt=%s cached=%s ttft=%s %s" % (idx, fmt_epoch(created), model, (sid or "-")[:12], prompt, "NULL" if cached is None else cached, "NULL" if ttft is None else ttft, kind))
    w("")
    w("kinds in window: %s" % kinds)
    w("newest main-loop rows (idx, session, prompt, cached): %s" % mains[:5])
    b("C: kinds=%s newest_mains=%s" % (kinds, mains[:5]))
    w("")

def main():
    recs, partial = load()
    tail = recs[-N:]
    if not tail:
        b("NO TRACE RECORDS - the ring may have rotated everything out; check traces-history.jsonl")
    section_a(recs, partial, tail)
    section_b(tail)
    section_c()
    text = "\n".join(OUT) + "\n"
    with open(OUTPATH, "w", encoding="utf-8") as f:
        f.write(text)
    print("\n".join(BRIEF))
    print("\n[artifact] %s - %d chars, %d lines" % (OUTPATH, len(text), len(OUT)))

if __name__ == "__main__":
    main()
