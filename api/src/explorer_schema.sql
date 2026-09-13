-- Minimal explorer-store schema mirror for API read endpoints.
-- Used ONLY to seed an in-memory placeholder connection when the explorer
-- DB file does not exist yet, so read endpoints degrade to empty results
-- instead of "no such table" errors. The authoritative schema lives in
-- core/src/explorer/store.rs.

CREATE TABLE IF NOT EXISTS blocks(
  block_number INTEGER PRIMARY KEY,
  block_hash TEXT NOT NULL,
  ts INTEGER NOT NULL,
  producer TEXT,
  base_fee_gwei REAL,
  tx_count INTEGER,
  indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS txs(
  hash TEXT PRIMARY KEY,
  block_number INTEGER NOT NULL,
  tx_index INTEGER NOT NULL,
  "from" TEXT NOT NULL,
  "to" TEXT,
  success INTEGER NOT NULL,
  gas_used INTEGER,
  effective_gas_price_gwei REAL,
  priority_fee_gwei REAL,
  value_native TEXT
);

CREATE TABLE IF NOT EXISTS transfers(
  block_number INTEGER NOT NULL,
  tx_index INTEGER NOT NULL,
  log_index INTEGER NOT NULL,
  token TEXT NOT NULL,
  "from" TEXT NOT NULL,
  "to" TEXT NOT NULL,
  amount TEXT NOT NULL,
  is_native INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(block_number, tx_index, log_index)
);

CREATE TABLE IF NOT EXISTS swaps(
  block_number INTEGER NOT NULL,
  tx_index INTEGER NOT NULL,
  log_index INTEGER NOT NULL,
  pool TEXT NOT NULL,
  dex TEXT,
  amm TEXT,
  token_in TEXT NOT NULL,
  token_out TEXT NOT NULL,
  amount_in TEXT NOT NULL,
  amount_out TEXT NOT NULL,
  sender TEXT,
  PRIMARY KEY(block_number, tx_index, log_index)
);

CREATE TABLE IF NOT EXISTS mev_ops(
  id INTEGER PRIMARY KEY,
  block_number INTEGER NOT NULL,
  tx_index INTEGER,
  tx_hash TEXT NOT NULL,
  ts INTEGER NOT NULL,
  kind TEXT NOT NULL,
  eoa TEXT NOT NULL,
  contract TEXT,
  confidence TEXT NOT NULL,
  canonical_id TEXT,
  profit_token TEXT,
  profit_amount TEXT,
  profit_usd REAL,
  gas_cost_usd REAL,
  net_profit_usd REAL,
  route_json TEXT,
  victim_hashes TEXT,
  details_json TEXT,
  detector TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS mev_ops_ts ON mev_ops(ts);

CREATE TABLE IF NOT EXISTS labels(
  address TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  entity TEXT,
  evidence TEXT,
  first_seen_block INTEGER,
  source TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS prices(
  hour INTEGER NOT NULL,
  token TEXT NOT NULL,
  usd REAL NOT NULL,
  source TEXT NOT NULL,
  PRIMARY KEY(hour, token)
);

CREATE TABLE IF NOT EXISTS sync_state(
  chain_id INTEGER PRIMARY KEY,
  head INTEGER NOT NULL,
  indexed_to INTEGER NOT NULL,
  last_indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS blocks_classified(
  block INTEGER PRIMARY KEY,
  classified_at INTEGER NOT NULL,
  event_count INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS opportunities(
  id INTEGER PRIMARY KEY,
  run_id TEXT NOT NULL,
  chain TEXT NOT NULL,
  block_number INTEGER NOT NULL,
  tx_index INTEGER,
  strategy TEXT NOT NULL,
  pool_a TEXT,
  pool_b TEXT,
  token_in TEXT,
  token_out TEXT,
  input_amount TEXT,
  expected_profit TEXT,
  gas_cost_wei TEXT,
  path TEXT,
  "timestamp" INTEGER,
  mempool_only INTEGER NOT NULL DEFAULT 0,
  confidence TEXT,
  sender TEXT,
  tx_hash TEXT,
  detection_path TEXT,
  canonical_id TEXT
);
CREATE INDEX IF NOT EXISTS opportunities_run ON opportunities(run_id);
CREATE INDEX IF NOT EXISTS opportunities_range ON opportunities(chain, block_number);

CREATE TABLE IF NOT EXISTS rejected_candidates(
  id INTEGER PRIMARY KEY,
  run_id TEXT NOT NULL,
  chain TEXT NOT NULL,
  block_number INTEGER NOT NULL,
  tx_index INTEGER,
  strategy TEXT NOT NULL,
  pool_a TEXT,
  pool_b TEXT,
  path TEXT,
  token_in TEXT,
  token_out TEXT,
  input_amount TEXT,
  expected_profit TEXT,
  expected_profit_usd REAL,
  gas_cost_wei TEXT,
  reject_reason TEXT NOT NULL,
  detail TEXT,
  created_at INTEGER NOT NULL
);
