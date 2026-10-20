import sqlite3, os
p=os.path.join('cache','explorer-avalanche.sqlite')
con=sqlite3.connect(p)
print("size", os.path.getsize(p))
print("sync", con.execute("select chain_id, head, indexed_to, last_indexed_at from sync_state").fetchall())
print("prices", con.execute("select hour, token, usd, source from prices order by hour desc limit 8").fetchall())
print("maxblock", con.execute("select max(block), count(*) from blocks_classified").fetchone())
print("ops", con.execute("select count(*) from mev_ops").fetchone())
print("recent", con.execute("select block_number, kind, tx_hash, profit_usd, net_profit_usd from mev_ops order by block_number desc limit 15").fetchall())
