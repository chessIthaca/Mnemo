# Read-only verification of .coding/memory.db (auto-recall decay investigation).
# Usage: python .coding/analysis/memverify.py
import sqlite3
from datetime import datetime, timezone

c = sqlite3.connect("file:.coding/memory.db?mode=ro", uri=True)


def ts(v):
    return datetime.fromtimestamp(v, tz=timezone.utc).strftime("%Y-%m-%d %H:%M") if v else "?"

print("== counts per tier ==")
for tier, n in c.execute("select tier, count(*) from memories group by tier"):
    print(f"  {tier:10} {n}")

print("\n== newest write per tier ==")
for tier, mx in c.execute("select tier, max(created_at) from memories group by tier"):
    print(f"  {tier:10} {ts(mx)}")

print("\n== writes in last 7 / 30 days per tier ==")
for tier, w7, w30 in c.execute(
    "select tier,"
    " sum(case when created_at > strftime('%s','now')-7*86400 then 1 else 0 end),"
    " sum(case when created_at > strftime('%s','now')-30*86400 then 1 else 0 end)"
    " from memories group by tier"
):
    print(f"  {tier:10} 7d={w7}  30d={w30}")

print("\n== Grammarly memory ==")
for r in c.execute(
    "select tier, strength, access_count,"
    " datetime(created_at,'unixepoch'), datetime(last_accessed_at,'unixepoch'),"
    " length(content), title"
    " from memories where title like '%Grammarly%'"
):
    print(f"  tier={r[0]} strength={r[1]:.3f} access_count={r[2]}")
    print(f"  created={r[3]} last_accessed={r[4]} content_len={r[5]}")
    print(f"  title={r[6]!r}")

print("\n== non-working memories ordered by last_accessed_at desc (top 25) ==")
for r in c.execute(
    "select tier, strength, access_count,"
    " datetime(created_at,'unixepoch'), datetime(last_accessed_at,'unixepoch'), title"
    " from memories where tier != 'working'"
    " order by last_accessed_at desc limit 25"
):
    print(f"  [{r[0]:9}] s={r[1]:.2f} acc={r[2]:<4} created={r[3][:10]} last={r[4]} {r[5][:70]!r}")
