//! Stage 2 acceptance tests: v13 migration, read-only compatibility views,
//! append-only inventory writer, per-source windows and reorg invalidation.
//!
//! All fixtures are temporary SQLite files; the production cache is untouched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use alloy::primitives::{address, Address};
use mev_scout_core::cache::SqliteStore;
use mev_scout_core::config::Config;
use mev_scout_core::dex_type::DexType;
use mev_scout_core::jobs::load_pool_registry;
use mev_scout_core::pool::discovery::DiscoveredPool;
use mev_scout_core::pool::identity::PoolKey;
use mev_scout_core::pool::state::PoolInfo;
use mev_scout_core::types::ChainName;
use rusqlite::{params, Connection};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_db(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "mev-scout-stage2-{name}-{}-{}.sqlite",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    path
}

/// Build a v13-shaped database from a freshly initialized v14 file, then
/// reopen it so the normal migration path has to rebuild the Master Inventory.
fn v13_fixture(path: &Path, rows: &[PoolInfo], seed_version: bool, sparse: bool) {
    {
        let _store = SqliteStore::open(path).unwrap();
    }
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "
        DROP TRIGGER IF EXISTS pool_info_no_insert;
        DROP TRIGGER IF EXISTS pool_info_no_update;
        DROP TRIGGER IF EXISTS pool_info_no_delete;
        DROP TRIGGER IF EXISTS pool_info_legacy_complete_no_insert;
        DROP TRIGGER IF EXISTS pool_info_legacy_complete_no_update;
        DROP TRIGGER IF EXISTS pool_info_legacy_complete_no_delete;
        DROP VIEW IF EXISTS pool_info;
        DROP VIEW IF EXISTS pool_info_legacy_complete;
        DROP TABLE IF EXISTS pool_role;
        DROP TABLE IF EXISTS pool_observation;
        DROP TABLE IF EXISTS pool_identity;
        DROP TABLE IF EXISTS pool_status;
        DROP TABLE IF EXISTS scan_window;
        DROP TABLE IF EXISTS reorg_event;
        DROP TABLE IF EXISTS registry_meta;
        DROP TABLE IF EXISTS pool_legacy_snapshot;
        ALTER TABLE pool_info_v13_archive RENAME TO pool_info;
        DELETE FROM schema_version;
        ",
    )
    .unwrap();
    if sparse {
        conn.execute_batch(
            "ALTER TABLE pool_info DROP COLUMN dex_name;
             ALTER TABLE pool_info DROP COLUMN tvl_usd;
             ALTER TABLE pool_info DROP COLUMN derivation_version;",
        )
        .unwrap();
    }
    if seed_version {
        conn.execute("INSERT INTO schema_version(version) VALUES (13)", [])
            .unwrap();
    }

    let columns = if sparse {
        "address, token0, token1, fee, dex_type, tick_spacing, creation_block, \
         pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, \
         hook_address, bin_step, maturity_timestamp, token0_symbol, token1_symbol, \
         volume_usd_24h, volume_usd_30d"
    } else {
        "address, token0, token1, fee, dex_type, tick_spacing, creation_block, \
         pool_id, factory, is_stable, underlying_tokens, balancer_pool_type, \
         hook_address, bin_step, maturity_timestamp, dex_name, token0_symbol, \
         token1_symbol, tvl_usd, volume_usd_24h, volume_usd_30d, derivation_version"
    };
    let placeholders = (1..=columns.split(',').count())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("INSERT INTO pool_info ({columns}) VALUES ({placeholders})");

    for pool in rows {
        let mut values: Vec<rusqlite::types::Value> = vec![
            pool.address.as_slice().to_vec().into(),
            pool.token0.as_slice().to_vec().into(),
            pool.token1.as_slice().to_vec().into(),
            (pool.fee as i64).into(),
            (pool.dex_type as i64).into(),
            pool.tick_spacing.map(|v| v as i64).into(),
            (pool.creation_block as i64).into(),
            pool.pool_id.map(|v| v.to_vec()).into(),
            pool.factory.map(|v| v.as_slice().to_vec()).into(),
            pool.is_stable.map(|v| v as i64).into(),
            pool.underlying_tokens
                .as_ref()
                .map(|tokens| {
                    let hexes: Vec<String> = tokens
                        .iter()
                        .map(|a| format!("0x{}", hex::encode(a.as_slice())))
                        .collect();
                    serde_json::to_string(&hexes).unwrap_or_default()
                })
                .into(),
            pool.balancer_pool_type.map(|v| v as i64).into(),
            pool.hook_address.map(|v| v.as_slice().to_vec()).into(),
            pool.bin_step.map(|v| v as i64).into(),
            pool.maturity_timestamp.map(|v| v as i64).into(),
        ];
        if !sparse {
            values.push(pool.dex_name.as_deref().map(str::to_string).into());
        }
        values.push(pool.token0_symbol.as_deref().map(str::to_string).into());
        values.push(pool.token1_symbol.as_deref().map(str::to_string).into());
        if !sparse {
            values.push(pool.tvl_usd.into());
        }
        values.push(pool.volume_usd_24h.into());
        values.push(pool.volume_usd_30d.into());
        if !sparse {
            values.push(pool.derivation_version.map(|v| v as i64).into());
        }
        conn.execute(&sql, rusqlite::params_from_iter(values))
            .unwrap();
    }
    drop(conn);
}

/// Pre-seed the chain id so the v14 import uses the real chain namespace
/// instead of the default `0` used when opening a brand-new file.
fn seed_chain_id(path: &Path, chain_id: u64) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS registry_meta (
             key   TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );",
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO registry_meta(key, value) VALUES ('chain_id', ?1)",
        params![chain_id.to_string()],
    )
    .unwrap();
}

fn legacy_rows() -> Vec<PoolInfo> {
    vec![
        PoolInfo {
            address: address!("1111111111111111111111111111111111111111"),
            token0: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            token1: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            fee: 500,
            dex_type: DexType::UniswapV3,
            creation_block: 100,
            factory: Some(address!("dddddddddddddddddddddddddddddddddddddddd")),
            ..Default::default()
        },
        PoolInfo {
            address: address!("2222222222222222222222222222222222222222"),
            token0: Address::ZERO,
            token1: address!("cccccccccccccccccccccccccccccccccccccccc"),
            fee: 0,
            dex_type: DexType::Unknown,
            creation_block: 0,
            pool_id: Some([0x77; 32]),
            factory: Some(address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")),
            ..Default::default()
        },
    ]
}

#[test]
fn migration_preserves_every_legacy_row_without_auto_admission() {
    let path = temp_db("full");
    let rows = legacy_rows();
    v13_fixture(&path, &rows, true, false);

    let store = SqliteStore::open(&path).unwrap();
    let listed = store.list_discovered_pools().unwrap();
    assert_eq!(listed.len(), 2, "no legacy row may be dropped");
    assert_eq!(store.registry_revision(), 1);

    let conn = Connection::open(&path).unwrap();
    let archive: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pool_info_v13_archive'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archive, 1);

    let mut stmt = conn.prepare("PRAGMA table_info(pool_info)").unwrap();
    let columns: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(columns.len(), 22, "legacy 22-column view must be preserved");
    drop(stmt);

    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let mut fk_stmt = conn.prepare("PRAGMA foreign_key_check").unwrap();
    let mut fk_rows = fk_stmt.query([]).unwrap();
    let mut violations = 0;
    while fk_rows.next().unwrap().is_some() {
        violations += 1;
    }
    assert_eq!(violations, 0, "foreign_key_check must pass");

    let observations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pool_observation", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        observations, 2,
        "each archived row gets a legacy observation"
    );
    let ready: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pool_status WHERE quoteable != 0 OR simulation_ready != 0 OR execution_ready != 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ready, 0, "migration must not admit any legacy row to Live");
}

#[test]
fn migration_sparse_v13_with_unseeded_version_is_idempotent() {
    let path = temp_db("sparse");
    let rows = legacy_rows();
    v13_fixture(&path, &rows, false, true);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.list_discovered_pools().unwrap().len(), 2);
    drop(store);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.list_discovered_pools().unwrap().len(), 2);

    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 14);
    let restored: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('pool_info') WHERE name IN ('dex_name','tvl_usd','derivation_version')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(restored, 3);
}

#[test]
fn migration_crash_rollback_leaves_original_v13_readable() {
    let path = temp_db("rollback");
    let rows = legacy_rows();
    v13_fixture(&path, &rows, true, false);
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE pool_info SET underlying_tokens = 'not-json' WHERE address = ?1",
            params![rows[0].address.as_slice().to_vec()],
        )
        .unwrap();
    }

    assert!(
        SqliteStore::open(&path).is_err(),
        "a malformed legacy row must abort the v14 import"
    );

    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 13, "schema stamp must roll back");
    let is_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pool_info'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(is_table, 1, "the original v13 table must survive");
    let row_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pool_info", [], |r| r.get(0))
        .unwrap();
    assert_eq!(row_count, 2);
    let archive: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pool_info_v13_archive'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(archive, 0, "partial rename must be rolled back");
}

#[test]
fn legacy_singleton_rows_stay_address_keyed_with_provisional_role() {
    let path = temp_db("singleton");
    let singleton = address!("dddddddddddddddddddddddddddddddddddddddd");
    let synthetic = Address::repeat_byte(0x77);
    let pool_id = [0x77u8; 32];
    let rows = vec![PoolInfo {
        address: synthetic,
        token0: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        token1: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        fee: 0,
        dex_type: DexType::UniswapV4,
        creation_block: 200,
        pool_id: Some(pool_id),
        factory: Some(singleton),
        ..Default::default()
    }];
    v13_fixture(&path, &rows, true, false);
    seed_chain_id(&path, 43114);

    let store = SqliteStore::open(&path).unwrap();
    let key = PoolKey::EvmPool {
        chain_id: 43114,
        address: synthetic,
    }
    .to_string();
    let conn = store.raw_connection_for_test();
    let (namespace, state): (String, String) = conn
        .query_row(
            "SELECT namespace, identity_state FROM pool_identity WHERE pool_key = ?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(namespace, "evm_pool_address");
    assert_eq!(state, "provisional");

    let candidate: String = conn
        .query_row(
            "SELECT value FROM pool_role WHERE pool_key = ?1 AND role = 'singleton_candidate'",
            params![key],
            |r| r.get(0),
        )
        .unwrap();
    let singleton_hex = format!("0x{}", hex::encode(singleton.as_slice()));
    let pool_id_hex = format!("0x{}", hex::encode(pool_id));
    assert_eq!(candidate, format!("{singleton_hex}:{pool_id_hex}"));

    let ready: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pool_status WHERE quoteable != 0 OR simulation_ready != 0 OR execution_ready != 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ready, 0);
}

#[test]
fn old_readers_read_migrated_view_without_auto_admission() {
    let path = temp_db("old-readers");
    let rows = legacy_rows();
    v13_fixture(&path, &rows, true, false);
    seed_chain_id(&path, 43114);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.pool_count().unwrap(), 2);
    assert_eq!(store.list_discovered_pools().unwrap().len(), 2);
    assert!(store
        .get_discovered_pool(&rows[0].address)
        .unwrap()
        .is_some());
    assert_eq!(
        store.earliest_creation_block_by_factory().unwrap().len(),
        1,
        "remote-style zero-block rows must not seed the start-block guard"
    );
    assert_eq!(store.discovery_watermark().unwrap(), None);

    let mut config = Config::default();
    config.output.db_path = path.to_string_lossy().into_owned();
    let registry = load_pool_registry(&config, &ChainName::Avalanche);
    assert!(registry.tokens.contains_key(&rows[0].address));
    assert_eq!(registry.health.repaired, 0);

    let conn = store.raw_connection_for_test();
    let ready: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pool_status WHERE quoteable != 0 OR simulation_ready != 0 OR execution_ready != 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ready, 0);
}

#[test]
fn compatibility_views_reject_direct_writes() {
    let store = SqliteStore::open(":memory:").unwrap();
    store
        .put_discovered_pool(&PoolInfo {
            address: address!("1111111111111111111111111111111111111111"),
            token0: Address::repeat_byte(0xaa),
            token1: Address::repeat_byte(0xbb),
            fee: 30,
            dex_type: DexType::UniswapV2,
            creation_block: 1,
            ..Default::default()
        })
        .unwrap();
    let conn = store.raw_connection_for_test();
    for view in ["pool_info", "pool_info_legacy_complete"] {
        let insert = format!(
            "INSERT INTO {view}(address, token0, token1, fee, dex_type, creation_block)
             VALUES (X'11', X'22', X'33', 1, 1, 1)"
        );
        assert!(
            conn.execute_batch(&insert).is_err(),
            "{view} INSERT must fail"
        );
        let update = format!("UPDATE {view} SET fee = 2");
        assert!(
            conn.execute_batch(&update).is_err(),
            "{view} UPDATE must fail"
        );
        let delete = format!("DELETE FROM {view}");
        assert!(
            conn.execute_batch(&delete).is_err(),
            "{view} DELETE must fail"
        );
    }
}

#[test]
fn adapter_writes_preserve_authority_precedence() {
    let store = SqliteStore::open(":memory:").unwrap();
    store.set_chain_id(43114).unwrap();
    let address = address!("1234567890123456789012345678901234567890");
    let onchain = PoolInfo {
        address,
        token0: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        token1: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        fee: 500,
        dex_type: DexType::UniswapV3,
        creation_block: 123,
        ..Default::default()
    };
    store
        .put_discovered_pool_with_source(&onchain, "receipts", "factory_scan", "onchain")
        .unwrap();

    let mut lower = onchain.clone();
    lower.fee = 100;
    lower.tvl_usd = Some(42.0);
    store
        .put_discovered_pool_with_source(&lower, "geckoterminal", "enrich", "external")
        .unwrap();

    let got = store.get_discovered_pool(&address).unwrap().unwrap();
    assert_eq!(
        got.fee, 500,
        "external source must not overwrite onchain identity"
    );
    assert_eq!(
        got.tvl_usd,
        Some(42.0),
        "lower authority may refine metadata"
    );

    let conn = store.raw_connection_for_test();
    let observations: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pool_observation WHERE pool_key = ?1",
            params![PoolKey::from_pool_info(43114, &onchain).to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        observations, 2,
        "conflicting evidence is appended, not replaced"
    );
}

#[test]
fn protocol_identifiers_survive_in_inventory_observations() {
    let store = SqliteStore::open(":memory:").unwrap();
    store.set_chain_id(43114).unwrap();
    let pool_id = [0x77u8; 32];
    let pool = DiscoveredPool::new(
        address!("1234567890123456789012345678901234567890"),
        Address::repeat_byte(0xaa),
        Address::repeat_byte(0xbb),
        0,
        DexType::UniswapV4,
        10,
    )
    .with_pool_id(Some(pool_id))
    .with_protocol_params(serde_json::json!({ "parameters": "0x0100" }));
    store
        .record_discovered_pool(&pool, "discovery", "scan", "onchain", Some(10))
        .unwrap();

    let conn = store.raw_connection_for_test();
    let payload: String = conn
        .query_row("SELECT payload_json FROM pool_observation", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(payload.contains("parameters"));

    let pool_id_role: String = conn
        .query_row(
            "SELECT value FROM pool_role WHERE role = 'pool_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pool_id_role, format!("0x{}", hex::encode(pool_id)));

    let param_role: String = conn
        .query_row(
            "SELECT value FROM pool_role WHERE role = 'protocol:parameters'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(param_role, "\"0x0100\"");
}

#[test]
fn failed_and_partial_windows_stay_retryable() {
    let store = SqliteStore::open(":memory:").unwrap();
    let args = (43114u64, "receipts", "backfill", "0xabc", 100u64, 200u64);
    let claim = store
        .claim_window(args.0, args.1, args.2, args.3, args.4, args.5)
        .unwrap();
    assert_eq!(claim.state, "running");

    store
        .mark_window(
            &claim.window_key,
            "failed",
            "provider_limited",
            Some(150),
            None,
            Some("429"),
        )
        .unwrap();
    assert_eq!(store.retry_failed_windows(43114, "receipts").unwrap(), 1);

    let retried = store
        .claim_window(args.0, args.1, args.2, args.3, args.4, args.5)
        .unwrap();
    assert_eq!(retried.state, "running");

    store
        .mark_window(
            &retried.window_key,
            "complete",
            "exhaustive",
            Some(200),
            None,
            None,
        )
        .unwrap();
    assert_eq!(store.retry_failed_windows(43114, "receipts").unwrap(), 0);
}

#[test]
fn provisional_rows_are_retained_without_a_persistence_cap() {
    let store = SqliteStore::open(":memory:").unwrap();
    for i in 0..40u8 {
        let mut addr = [0x11u8; 20];
        addr[19] = i;
        store
            .put_discovered_pool(&PoolInfo {
                address: Address::from(addr),
                token0: if i % 2 == 0 {
                    Address::ZERO
                } else {
                    Address::repeat_byte(0xaa)
                },
                token1: Address::repeat_byte(0xbb),
                fee: i as u32,
                dex_type: DexType::Unknown,
                creation_block: 0,
                ..Default::default()
            })
            .unwrap();
    }
    assert_eq!(store.list_discovered_pools().unwrap().len(), 40);

    let conn = store.raw_connection_for_test();
    let admitted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pool_status WHERE quoteable != 0 OR simulation_ready != 0 OR execution_ready != 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(admitted, 0);
}

#[test]
fn status_upgrade_bumps_revision_but_repeated_evidence_does_not() {
    let store = SqliteStore::open(":memory:").unwrap();
    store.set_chain_id(43114).unwrap();
    let pool = PoolInfo {
        address: address!("1234567890123456789012345678901234567890"),
        token0: Address::repeat_byte(0xaa),
        token1: Address::repeat_byte(0xbb),
        fee: 500,
        dex_type: DexType::UniswapV3,
        creation_block: 120,
        ..Default::default()
    };

    store
        .put_discovered_pool_with_source(&pool, "geckoterminal", "enrich", "external")
        .unwrap();
    let initial = store.registry_revision();

    store
        .put_discovered_pool_with_source(&pool, "dexscreener", "enrich", "external")
        .unwrap();
    assert_eq!(
        store.registry_revision(),
        initial,
        "same-state metadata refinement must not bump the revision"
    );

    store
        .put_discovered_pool_with_source(&pool, "receipts", "factory_scan", "onchain")
        .unwrap();
    assert!(
        store.registry_revision() > initial,
        "provisional -> canonical must bump the revision"
    );

    let conn = store.raw_connection_for_test();
    let (identity, validation): (String, String) = conn
        .query_row(
            "SELECT i.identity_state, s.validation_state
             FROM pool_identity i JOIN pool_status s USING (pool_key)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(identity, "canonical");
    assert_eq!(validation, "observed");
}

#[test]
fn reorg_orphans_observations_and_bumps_revision() {
    let store = SqliteStore::open(":memory:").unwrap();
    store.set_chain_id(43114).unwrap();
    store
        .put_discovered_pool_with_source(
            &PoolInfo {
                address: address!("1234567890123456789012345678901234567890"),
                token0: Address::repeat_byte(0xaa),
                token1: Address::repeat_byte(0xbb),
                fee: 500,
                dex_type: DexType::UniswapV3,
                creation_block: 120,
                ..Default::default()
            },
            "receipts",
            "factory_scan",
            "onchain",
        )
        .unwrap();
    let before = store.registry_revision();

    assert_eq!(store.orphan_after_block(43114, 100).unwrap(), 1);
    assert!(store.registry_revision() > before);

    let conn = store.raw_connection_for_test();
    let valid: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pool_observation WHERE valid = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(valid, 0);
}

#[test]
fn canonical_pool_key_text_forms_are_stable() {
    let address_key = PoolKey::EvmPool {
        chain_id: 43114,
        address: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    };
    assert_eq!(
        address_key.to_string(),
        "43114:evm_pool_address:0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(
        address_key.to_string().parse::<PoolKey>().unwrap(),
        address_key
    );

    let singleton = PoolKey::EvmSingletonPool {
        chain_id: 43114,
        singleton: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        pool_id: [0xcc; 32],
    };
    let text = singleton.to_string();
    assert!(text.contains(&format!("0x{}", hex::encode([0xcc; 32]))));
    assert_eq!(text.parse::<PoolKey>().unwrap(), singleton);

    let typed = PoolKey::EvmSingletonTypedPool {
        chain_id: 43114,
        singleton: address!("dddddddddddddddddddddddddddddddddddddddd"),
        pool_type: "0x0100".to_string(),
        pool_id: [0xee; 32],
    };
    assert_eq!(typed.to_string().parse::<PoolKey>().unwrap(), typed);
    assert!("43114:evm_pool_address:0x1234".parse::<PoolKey>().is_err());
}
