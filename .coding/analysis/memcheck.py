import sqlite3, datetime

# Read-only snapshot of the memory store: per-tier counts and how many
# distinct sessions still own working-tier rows (= the cleanup op's `total`).
c = sqlite3.connect('file:.coding/memory.db?mode=ro', uri=True)
c.execute('PRAGMA busy_timeout=3000')
cur = c.cursor()
for t, n in cur.execute("SELECT tier, COUNT(*) FROM memories GROUP BY tier ORDER BY tier"):
    print(f'{t}: {n}')
print('total:', cur.execute('SELECT COUNT(*) FROM memories').fetchone()[0])
pend = cur.execute(
    "SELECT COUNT(DISTINCT je.value) FROM memories m, "
    "json_each(m.source_session_ids) je WHERE m.tier='working'"
).fetchone()[0]
print('pending_sessions_with_working_rows:', pend)
recent_epi = cur.execute(
    "SELECT COUNT(*) FROM memories WHERE tier='episodic' AND created_at > ?",
    (int(datetime.datetime.now().timestamp()) - 3600,),
).fetchone()[0]
print('episodic_created_last_hour:', recent_epi)
print('sampled_at:', datetime.datetime.now().strftime('%H:%M:%S'))
c.close()
