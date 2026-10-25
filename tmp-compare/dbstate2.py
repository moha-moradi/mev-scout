import sqlite3, os, glob

paths = {
    'avalanche': 'cache/explorer-avalanche.sqlite',
}
for name, f in paths.items():
    full = os.path.abspath(f)
    if not os.path.exists(full):
        print(name, 'MISSING', f)
        continue
    print('===', name, full, 'size', os.path.getsize(full))
    with open(f, 'rb') as fh:
        print('  first bytes:', fh.read(16).hex(), fh.read(0))
    try:
        con = sqlite3.connect(f, timeout=5)
        rows = con.execute("SELECT name, type, sql FROM sqlite_master").fetchall()
        print('  sqlite_master rows:', len(rows))
        for r in rows[:80]:
            print('    ', r[1], '|', r[0])
        con.close()
    except Exception as e:
        print('  OPEN ERR:', e)
