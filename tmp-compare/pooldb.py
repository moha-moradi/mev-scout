import sqlite3
con=sqlite3.connect("core/cache/avalanche-mev-scout.sqlite",timeout=5)
tables=con.execute("SELECT name,type FROM sqlite_master WHERE type='table' ORDER BY name").fetchall()
print("tables:",[r[0] for r in tables])
for n,t in tables:
    if "pool" in n.lower() or "pools" in n:
        try:
            c=con.execute("SELECT COUNT(*) FROM "+n).fetchone()[0]
            print("  %s count=%d"%(n,c))
            cols=con.execute("PRAGMA table_info(%s)"%n).fetchall()
            print("    cols:",[x[1] for x in cols])
        except Exception as e:
            print("  %s ERR %s"%(n,e))
con.close()
