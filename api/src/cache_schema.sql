-- Minimal scanner-cache schema mirror for API read endpoints.
-- Used ONLY to seed an in-memory placeholder connection when the cache DB
-- file does not exist yet, so read endpoints degrade to empty results
-- instead of "no such table" errors. The authoritative schema lives in
-- core/src/cache/store/mod.rs.

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS run_manifests (
    run_id     TEXT PRIMARY KEY,
    chain      TEXT NOT NULL,
    start_block INTEGER NOT NULL,
    end_block  INTEGER NOT NULL,
    resolved_at INTEGER NOT NULL,
    range_mode TEXT NOT NULL,
    strategies TEXT NOT NULL,
    flash_loan_provider TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pool_info (
    address    BLOB PRIMARY KEY,
    token0     BLOB NOT NULL,
    token1     BLOB NOT NULL,
    fee        INTEGER NOT NULL,
    dex_type   INTEGER NOT NULL,
    tick_spacing INTEGER,
    creation_block INTEGER NOT NULL,
    pool_id    BLOB,
    factory    BLOB,
    is_stable  INTEGER,
    underlying_tokens TEXT,
    balancer_pool_type INTEGER,
    hook_address BLOB,
    bin_step INTEGER,
    maturity_timestamp INTEGER,
    dex_name TEXT,
    token0_symbol TEXT,
    token1_symbol TEXT,
    tvl_usd REAL,
    volume_usd_24h REAL,
    volume_usd_30d REAL
);
