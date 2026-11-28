//! Stage 2 inventory writer.
//!
//! This is the only module allowed to write Master Inventory tables. Legacy
//! `put_discovered_pool` callers keep working through an adapter that records a
//! low-authority observation and refreshes the compatibility snapshot.

use anyhow::Context;
use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::pool::discovery::DiscoveredPool;
use crate::pool::identity::PoolKey;
use crate::pool::state::PoolInfo;

use super::v14;

pub struct Observation<'a> {
    pub pool_key: PoolKey,
    pub authority: &'a str,
    pub source: &'a str,
    pub method: &'a str,
    pub run_id: Option<&'a str>,
    pub endpoint: Option<&'a str>,
    pub block_number: Option<u64>,
    pub block_hash: Option<&'a str>,
    pub tx_hash: Option<&'a str>,
    pub log_index: Option<u64>,
    pub raw_identifier: Option<&'a str>,
    pub payload_json: String,
    pub roles: Vec<(String, String)>,
    pub legacy: &'a PoolInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowClaim {
    pub window_key: String,
    pub state: String,
    pub cursor_block: Option<u64>,
    pub coverage: String,
}

impl super::SqliteStore {
    // ---- registry metadata -------------------------------------------------

    pub fn chain_id(&self) -> u64 {
        self.meta_u64("chain_id").unwrap_or(0)
    }

    pub fn set_chain_id(&self, chain_id: u64) -> anyhow::Result<()> {
        self.set_meta("chain_id", &chain_id.to_string())
    }

    pub fn registry_revision(&self) -> u64 {
        self.meta_u64("registry_revision").unwrap_or(1)
    }

    fn meta_u64(&self, key: &str) -> anyhow::Result<u64> {
        let conn = self.conn();
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM registry_meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO registry_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn bump_revision(&self, conn: &Connection) -> anyhow::Result<u64> {
        let current: Option<String> = conn
            .query_row(
                "SELECT value FROM registry_meta WHERE key = 'registry_revision'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let next = current.and_then(|v| v.parse::<u64>().ok()).unwrap_or(1) + 1;
        conn.execute(
            "INSERT INTO registry_meta(key, value) VALUES ('registry_revision', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![next.to_string()],
        )?;
        Ok(next)
    }

    // ---- observation writer ------------------------------------------------

    pub fn record_observation(&self, chain_id: u64, obs: &Observation<'_>) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> anyhow::Result<()> {
            let observed_at = v14::now_string();
            let key_text = obs.pool_key.to_string();
            let namespace = match &obs.pool_key {
                PoolKey::EvmPool { .. } => "evm_pool_address",
                PoolKey::EvmSingletonPool { .. } => "evm_singleton_pool",
                PoolKey::EvmSingletonTypedPool { .. } => "evm_singleton_typed_pool",
            };
            let rank = authority_rank(obs.authority);
            let state = if rank >= 3 {
                "canonical"
            } else {
                "provisional"
            };
            let validation = if rank >= 3 { "observed" } else { "unverified" };
            // Decision 10.4: advance the registry version on reorgs and on
            // material status upgrades. The upserts below are monotonic, so
            // repeated evidence at the same state is a metadata refinement
            // and does not consume a revision. New rows are creation, not a
            // status change.
            let current_identity: Option<String> = conn
                .query_row(
                    "SELECT identity_state FROM pool_identity WHERE pool_key = ?1",
                    params![key_text],
                    |row| row.get(0),
                )
                .optional()?;
            let current_validation: Option<String> = conn
                .query_row(
                    "SELECT validation_state FROM pool_status WHERE pool_key = ?1",
                    params![key_text],
                    |row| row.get(0),
                )
                .optional()?;
            let status_upgraded = current_identity.is_some()
                && ((state == "canonical" && current_identity.as_deref() != Some("canonical"))
                    || (validation == "observed"
                        && current_validation.as_deref() != Some("observed")));
            let revision: i64 = if status_upgraded {
                self.bump_revision(&conn)? as i64
            } else {
                let current: Option<String> = conn
                    .query_row(
                        "SELECT value FROM registry_meta WHERE key = 'registry_revision'",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?;
                current.and_then(|v| v.parse::<i64>().ok()).unwrap_or(1)
            };

            conn.execute(
                "INSERT INTO pool_identity
                 (pool_key, chain_id, namespace, family, version, legacy_address,
                  created_block, first_seen_at, identity_state, registry_revision)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(pool_key) DO UPDATE SET
                   identity_state = CASE
                     WHEN excluded.identity_state = 'canonical' THEN 'canonical'
                     ELSE pool_identity.identity_state END,
                   legacy_address = COALESCE(pool_identity.legacy_address, excluded.legacy_address),
                   registry_revision = excluded.registry_revision",
                params![
                    key_text,
                    chain_id as i64,
                    namespace,
                    obs.legacy.dex_type.to_string(),
                    obs.pool_key.legacy_address().as_slice().to_vec(),
                    obs.legacy.creation_block as i64,
                    observed_at,
                    state,
                    revision,
                ],
            )?;

            let event_key = match (obs.run_id, obs.tx_hash, obs.log_index, obs.raw_identifier) {
                (Some(run), Some(tx), Some(log), Some(raw)) => {
                    format!("{chain_id}:{run}:{tx}:{log}:{raw}")
                }
                _ => format!(
                    "{}:{}:{}:{}:{}:{}",
                    chain_id,
                    key_text,
                    obs.source,
                    obs.method,
                    v14::now_nanos(),
                    obs.block_number.unwrap_or(0)
                ),
            };
            conn.execute(
                "INSERT OR IGNORE INTO pool_observation
                 (pool_key, event_key, chain_id, source, method, authority, run_id,
                  endpoint, block_number, block_hash, tx_hash, log_index,
                  observed_at, raw_identifier, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15)",
                params![
                    key_text,
                    event_key,
                    chain_id as i64,
                    obs.source,
                    obs.method,
                    obs.authority,
                    obs.run_id,
                    obs.endpoint,
                    obs.block_number.map(|v| v as i64),
                    obs.block_hash,
                    obs.tx_hash,
                    obs.log_index.map(|v| v as i64),
                    observed_at,
                    obs.raw_identifier,
                    obs.payload_json,
                ],
            )?;
            // Resolve by event_key instead of `last_insert_rowid()`: an
            // `INSERT OR IGNORE` leaves that value pointing at whichever table
            // was written last, which can violate the pool_role foreign key.
            let observation_id: i64 = conn.query_row(
                "SELECT id FROM pool_observation WHERE event_key = ?1",
                params![event_key],
                |row| row.get(0),
            )?;

            for (role, value) in &obs.roles {
                conn.execute(
                    "INSERT OR IGNORE INTO pool_role(pool_key, role, value, observation_id)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![key_text, role, value, observation_id],
                )?;
            }

            conn.execute(
                "INSERT INTO pool_status
                 (pool_key, validation_state, quoteable, simulation_ready,
                  execution_ready, admission_reason, registry_revision, updated_at)
                 VALUES (?1, ?2, 0, 0, 0, ?3, ?4, ?5)
                 ON CONFLICT(pool_key) DO UPDATE SET
                   validation_state = CASE
                     WHEN excluded.validation_state = 'observed' THEN 'observed'
                     ELSE pool_status.validation_state END,
                   registry_revision = excluded.registry_revision,
                   updated_at = excluded.updated_at",
                params![
                    key_text,
                    validation,
                    if rank >= 3 {
                        None
                    } else {
                        Some("requires on-chain verification")
                    },
                    revision,
                    observed_at,
                ],
            )?;

            refresh_snapshot(&conn, obs, rank, &observed_at, &key_text)?;
            conn.execute_batch("COMMIT;")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
        result
    }

    /// Compatibility adapter: preserves the old `put_discovered_pool` signature.
    pub fn put_discovered_pool(&self, pool: &PoolInfo) -> anyhow::Result<()> {
        self.put_discovered_pool_with_source(pool, "compat", "legacy", "legacy")
    }

    pub fn put_discovered_pool_with_source(
        &self,
        pool: &PoolInfo,
        source: &str,
        method: &str,
        authority: &str,
    ) -> anyhow::Result<()> {
        let chain_id = self.chain_id();
        let key = PoolKey::from_pool_info(chain_id, pool);
        let payload = serde_json::to_string(pool).context("serialize pool for observation")?;
        let roles = roles_from_pool_info(&key, pool);
        let raw_identifier = format!("0x{}", hex::encode(pool.address.as_slice()));
        self.record_observation(
            chain_id,
            &Observation {
                pool_key: key,
                authority,
                source,
                method,
                run_id: None,
                endpoint: None,
                block_number: Some(pool.creation_block),
                block_hash: None,
                tx_hash: None,
                log_index: None,
                raw_identifier: Some(&raw_identifier),
                payload_json: payload,
                roles,
                legacy: pool,
            },
        )
    }

    /// Discovery-specific writer: keeps protocol parameters in the observation.
    pub fn record_discovered_pool(
        &self,
        pool: &DiscoveredPool,
        source: &str,
        method: &str,
        authority: &str,
        block_number: Option<u64>,
    ) -> anyhow::Result<()> {
        let info: PoolInfo = pool.clone().into();
        let chain_id = self.chain_id();
        let key = PoolKey::from_pool_info(chain_id, &info);
        let payload = serde_json::to_value(pool).context("serialize discovered pool")?;
        let mut roles = roles_from_pool_info(&key, &info);
        if let Some(params) = &pool.protocol_params {
            if let Some(obj) = params.as_object() {
                for (name, value) in obj {
                    roles.push((format!("protocol:{name}"), value.to_string()));
                }
            }
        }
        let raw_identifier = format!("0x{}", hex::encode(pool.address.as_slice()));
        self.record_observation(
            chain_id,
            &Observation {
                pool_key: key,
                authority,
                source,
                method,
                run_id: None,
                endpoint: None,
                block_number: block_number.or(Some(pool.creation_block)),
                block_hash: None,
                tx_hash: None,
                log_index: None,
                raw_identifier: Some(&raw_identifier),
                payload_json: serde_json::to_string(&payload)?,
                roles,
                legacy: &info,
            },
        )
    }

    // ---- scan windows ------------------------------------------------------

    pub fn claim_window(
        &self,
        chain_id: u64,
        source: &str,
        scan_type: &str,
        subject: &str,
        from: u64,
        to: u64,
    ) -> anyhow::Result<WindowClaim> {
        let window_key = format!("{chain_id}:{source}:{scan_type}:{subject}:{from}:{to}");
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> anyhow::Result<WindowClaim> {
            let now = v14::now_string();
            let existing: Option<(String, Option<i64>, String)> = conn
                .query_row(
                    "SELECT state, cursor_block, coverage FROM scan_window WHERE window_key = ?1",
                    params![window_key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let state = match existing.as_ref() {
                None => "running".to_string(),
                Some((state, _, _))
                    if state == "pending" || state == "failed" || state == "partial" =>
                {
                    "running".to_string()
                }
                Some((state, _, _)) => state.clone(),
            };
            let cursor_block = if state == "running" {
                None
            } else {
                existing.as_ref().and_then(|e| e.1)
            };
            let coverage = existing
                .as_ref()
                .map(|e| e.2.clone())
                .unwrap_or_else(|| "unknown".to_string());
            conn.execute(
                "INSERT INTO scan_window
                 (window_key, chain_id, source, scan_type, subject, from_block, to_block,
                  state, coverage, cursor_block, attempts, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?11)
                 ON CONFLICT(window_key) DO UPDATE SET
                   state = excluded.state,
                   attempts = CASE WHEN excluded.state = 'running'
                     THEN scan_window.attempts + 1 ELSE scan_window.attempts END,
                   updated_at = excluded.updated_at",
                params![
                    window_key,
                    chain_id as i64,
                    source,
                    scan_type,
                    subject,
                    from as i64,
                    to as i64,
                    state,
                    coverage,
                    cursor_block.map(|v| v as i64),
                    now,
                ],
            )?;
            conn.execute_batch("COMMIT;")?;
            Ok(WindowClaim {
                window_key,
                state,
                cursor_block: cursor_block.map(|v| v as u64),
                coverage,
            })
        })();
        if result.is_err() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
        result
    }

    pub fn mark_window(
        &self,
        window_key: &str,
        state: &str,
        coverage: &str,
        cursor_block: Option<u64>,
        head_hash: Option<&str>,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(
                state,
                "pending" | "running" | "complete" | "partial" | "failed"
            ),
            "invalid scan window state '{state}'"
        );
        let conn = self.conn();
        conn.execute(
            "UPDATE scan_window SET state = ?1, coverage = ?2, cursor_block = ?3,
                    canonical_head_hash = ?4, last_error = ?5, updated_at = ?6
             WHERE window_key = ?7",
            params![
                state,
                coverage,
                cursor_block.map(|v| v as i64),
                head_hash,
                error,
                v14::now_string(),
                window_key,
            ],
        )?;
        Ok(())
    }

    pub fn retry_failed_windows(&self, chain_id: u64, source: &str) -> anyhow::Result<usize> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE scan_window SET state = 'pending', cursor_block = NULL,
                    last_error = NULL, updated_at = ?3
             WHERE chain_id = ?1 AND source = ?2 AND state IN ('failed', 'partial')",
            params![chain_id as i64, source, v14::now_string()],
        )?;
        Ok(changed)
    }

    /// Reorg invalidation. Network calls must happen before this method.
    pub fn orphan_after_block(&self, chain_id: u64, ancestor: u64) -> anyhow::Result<usize> {
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> anyhow::Result<usize> {
            let now = v14::now_string();
            let orphaned = conn.execute(
                "UPDATE pool_observation SET valid = 0, orphaned_at = ?1
                 WHERE chain_id = ?2 AND valid = 1 AND block_number > ?3",
                params![now, chain_id as i64, ancestor as i64],
            )?;
            conn.execute(
                "UPDATE scan_window SET state = 'pending', cursor_block = NULL,
                        canonical_head_hash = NULL, updated_at = ?1
                 WHERE chain_id = ?2 AND to_block > ?3
                   AND state IN ('complete', 'partial', 'failed', 'running')",
                params![now, chain_id as i64, ancestor as i64],
            )?;
            let revision = self.bump_revision(&conn)?;
            conn.execute(
                "UPDATE pool_identity SET registry_revision = ?1",
                params![revision as i64],
            )?;
            conn.execute(
                "UPDATE pool_status SET registry_revision = ?1",
                params![revision as i64],
            )?;
            Ok(orphaned)
        })();
        if result.is_err() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
        result
    }
}

fn authority_rank(authority: &str) -> i64 {
    match authority {
        "onchain" | "registry" => 3,
        "enrichment" => 2,
        "external" => 1,
        _ => 0,
    }
}

fn refresh_snapshot(
    conn: &Connection,
    obs: &Observation<'_>,
    rank: i64,
    observed_at: &str,
    key_text: &str,
) -> anyhow::Result<()> {
    let (existing_rank, existing_time): (i64, String) = match conn.query_row(
        "SELECT authority_rank, observed_at FROM pool_legacy_snapshot WHERE address = ?1",
        params![obs.legacy.address.as_slice().to_vec()],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
    ) {
        Ok(value) => value,
        Err(rusqlite::Error::QueryReturnedNoRows) => (0, "0".to_string()),
        Err(e) => return Err(e.into()),
    };
    let mut values = legacy_values(obs.legacy);
    values.push(Value::Text(obs.authority.to_string()));
    values.push(Value::Integer(rank));
    values.push(Value::Text(observed_at.to_string()));
    values.push(Value::Text(key_text.to_string()));

    if rank < existing_rank {
        // Lower-authority evidence never rewrites identity or readiness. It
        // may only fill metadata the authoritative observation left unknown.
        conn.execute(
            "UPDATE pool_legacy_snapshot SET
               dex_name = COALESCE(dex_name, ?2),
               token0_symbol = COALESCE(token0_symbol, ?3),
               token1_symbol = COALESCE(token1_symbol, ?4),
               tvl_usd = COALESCE(tvl_usd, ?5),
               volume_usd_24h = COALESCE(volume_usd_24h, ?6),
               volume_usd_30d = COALESCE(volume_usd_30d, ?7),
               observed_at = ?8
             WHERE address = ?1",
            params![
                obs.legacy.address.as_slice().to_vec(),
                obs.legacy.dex_name.as_deref().map(str::to_string),
                obs.legacy.token0_symbol.as_deref().map(str::to_string),
                obs.legacy.token1_symbol.as_deref().map(str::to_string),
                obs.legacy.tvl_usd,
                obs.legacy.volume_usd_24h,
                obs.legacy.volume_usd_30d,
                observed_at,
            ],
        )?;
        return Ok(());
    }

    let accepted = rank > existing_rank
        || (rank == existing_rank
            && observed_at.parse::<u64>().unwrap_or(0)
                >= existing_time.parse::<u64>().unwrap_or(0));
    if !accepted {
        return Ok(());
    }

    conn.execute(
        "INSERT INTO pool_legacy_snapshot
         (address, token0, token1, fee, dex_type, tick_spacing, creation_block,
          pool_id, factory, is_stable, underlying_tokens, balancer_pool_type,
          hook_address, bin_step, maturity_timestamp, dex_name,
          token0_symbol, token1_symbol, tvl_usd, volume_usd_24h, volume_usd_30d,
          derivation_version, authority, authority_rank, observed_at, pool_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
         ON CONFLICT(address) DO UPDATE SET
          token0=excluded.token0, token1=excluded.token1, fee=excluded.fee,
          dex_type=excluded.dex_type, tick_spacing=excluded.tick_spacing,
          creation_block=excluded.creation_block, pool_id=excluded.pool_id,
          factory=excluded.factory, is_stable=excluded.is_stable,
          underlying_tokens=excluded.underlying_tokens,
          balancer_pool_type=excluded.balancer_pool_type,
          hook_address=excluded.hook_address, bin_step=excluded.bin_step,
          maturity_timestamp=excluded.maturity_timestamp, dex_name=excluded.dex_name,
          token0_symbol=excluded.token0_symbol, token1_symbol=excluded.token1_symbol,
          tvl_usd=excluded.tvl_usd, volume_usd_24h=excluded.volume_usd_24h,
          volume_usd_30d=excluded.volume_usd_30d,
          derivation_version=excluded.derivation_version,
          authority=excluded.authority, authority_rank=excluded.authority_rank,
          observed_at=excluded.observed_at, pool_key=excluded.pool_key",
        rusqlite::params_from_iter(values.iter()),
    )?;
    Ok(())
}

fn legacy_values(pool: &PoolInfo) -> Vec<Value> {
    let pool_id = pool.pool_id.map(|id| Value::Blob(id.to_vec()));
    let factory = pool.factory.map(|f| Value::Blob(f.as_slice().to_vec()));
    let is_stable = pool.is_stable.map(|b| Value::Integer(b as i64));
    let underlying_json = pool.underlying_tokens.as_ref().map(|tokens| {
        let hexes: Vec<String> = tokens
            .iter()
            .map(|a| format!("0x{}", hex::encode(a.as_slice())))
            .collect();
        serde_json::to_string(&hexes).unwrap_or_default()
    });
    let balancer_type = pool.balancer_pool_type.map(|v| Value::Integer(v as i64));
    let hook = pool
        .hook_address
        .map(|h| Value::Blob(h.as_slice().to_vec()));
    let bin_step = pool.bin_step.map(|v| Value::Integer(v as i64));
    let maturity = pool.maturity_timestamp.map(|v| Value::Integer(v as i64));
    vec![
        Value::Blob(pool.address.as_slice().to_vec()),
        Value::Blob(pool.token0.as_slice().to_vec()),
        Value::Blob(pool.token1.as_slice().to_vec()),
        Value::Integer(pool.fee as i64),
        Value::Integer(pool.dex_type as i64),
        pool.tick_spacing
            .map(|v| Value::Integer(v as i64))
            .unwrap_or(Value::Null),
        Value::Integer(pool.creation_block as i64),
        pool_id.unwrap_or(Value::Null),
        factory.unwrap_or(Value::Null),
        is_stable.unwrap_or(Value::Null),
        underlying_json.map(Value::Text).unwrap_or(Value::Null),
        balancer_type.unwrap_or(Value::Null),
        hook.unwrap_or(Value::Null),
        bin_step.unwrap_or(Value::Null),
        maturity.unwrap_or(Value::Null),
        pool.dex_name
            .as_deref()
            .map(|v| Value::Text(v.to_string()))
            .unwrap_or(Value::Null),
        pool.token0_symbol
            .as_deref()
            .map(|v| Value::Text(v.to_string()))
            .unwrap_or(Value::Null),
        pool.token1_symbol
            .as_deref()
            .map(|v| Value::Text(v.to_string()))
            .unwrap_or(Value::Null),
        pool.tvl_usd.map(Value::Real).unwrap_or(Value::Null),
        pool.volume_usd_24h.map(Value::Real).unwrap_or(Value::Null),
        pool.volume_usd_30d.map(Value::Real).unwrap_or(Value::Null),
        pool.derivation_version
            .map(|v| Value::Integer(v as i64))
            .unwrap_or(Value::Null),
    ]
}

fn roles_from_pool_info(key: &PoolKey, pool: &PoolInfo) -> Vec<(String, String)> {
    let mut roles = vec![
        ("pool_address".to_string(), hex_string(&pool.address)),
        (
            "legacy_address".to_string(),
            hex_string(&key.legacy_address()),
        ),
    ];
    if let Some(factory) = pool.factory {
        roles.push(("factory".to_string(), hex_string(&factory)));
    }
    if let Some(pool_id) = pool.pool_id {
        roles.push(("pool_id".to_string(), format!("0x{}", hex::encode(pool_id))));
    }
    if let Some(hook) = pool.hook_address {
        roles.push(("hook".to_string(), hex_string(&hook)));
    }
    for token in pool.underlying_tokens.iter().flatten() {
        roles.push(("underlying_tokens".to_string(), hex_string(token)));
    }
    roles
}

fn hex_string(address: &alloy::primitives::Address) -> String {
    format!("0x{}", hex::encode(address.as_slice()))
}
