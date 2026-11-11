//! Forensic explorer dashboard: read-only SQLite → snapshot → HTML/JSON.
//!
//! Never migrates. Adaptive projection so stale schema files still render.

pub mod caps;
pub mod csv;
pub mod evidence;
pub mod reduce;
pub mod render;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::explorer::store::{ExplorerStore, SCHEMA_HEAD};

use caps::StoreCaps;
pub use evidence::OperationEconomics;
use reduce::{reduce, reject_ladder, ReducedPanels, RejectRow, TruncationInfo};

/// Envelope API version embedded in every artifact.
pub const API_VERSION: u32 = 1;

/// Build options for [`build`].
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub db_path: PathBuf,
    pub window: WindowSpec,
    pub max_ops: usize,
    pub max_evidence: usize,
    pub budget_mb: usize,
    pub with_gaps: bool,
    pub gap_block_limit: u64,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            db_path: PathBuf::from("cache/explorer-avalanche.sqlite"),
            window: WindowSpec::all(),
            max_ops: 5_000,
            max_evidence: 200,
            budget_mb: 8,
            with_gaps: false,
            gap_block_limit: 10_000,
        }
    }
}

/// Time window for the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct WindowSpec {
    pub label: String,
    pub since_ts: i64,
    pub until_ts: i64,
}

impl WindowSpec {
    /// Cover everything present (since 0 → now+skew).
    pub fn all() -> Self {
        let now = now_secs();
        Self {
            label: "all".into(),
            since_ts: 0,
            until_ts: now + 3600,
        }
    }

    /// Trailing duration ending at `now`.
    pub fn trailing(label: &str, duration_secs: i64) -> Self {
        let now = now_secs();
        Self {
            label: label.into(),
            since_ts: now.saturating_sub(duration_secs),
            until_ts: now + 60,
        }
    }

    /// Parse `all` | `Nh` | `Nd` | `Nw` (same spirit as explorer report windows).
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("all") {
            return Ok(Self::all());
        }
        let (num, unit) = s.split_at(
            s.find(|c: char| !c.is_ascii_digit())
                .ok_or_else(|| anyhow::anyhow!("invalid window '{s}' (expected Nh|Nd|Nw|all)"))?,
        );
        let n: i64 = num
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid window number in '{s}'"))?;
        anyhow::ensure!(n > 0, "window duration must be > 0");
        let secs = match unit {
            "h" | "H" => n.saturating_mul(3600),
            "d" | "D" => n.saturating_mul(86400),
            "w" | "W" => n.saturating_mul(7 * 86400),
            "s" | "S" => n,
            other => anyhow::bail!("invalid window unit '{other}' in '{s}' (use h|d|w)"),
        };
        Ok(Self::trailing(s, secs))
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One op row with optional columns mapped to `None` when absent from schema.
#[derive(Debug, Clone)]
pub struct OpRecord {
    pub id: i64,
    pub block_number: u64,
    pub tx_index: Option<u64>,
    pub tx_hash: String,
    pub ts: u64,
    pub kind: String,
    pub kind_raw: String,
    pub eoa: String,
    pub contract: Option<String>,
    pub confidence: String,
    pub canonical_id: Option<String>,
    pub profit_token: Option<String>,
    pub profit_amount: Option<String>,
    pub profit_usd: Option<f64>,
    pub gas_cost_usd: Option<f64>,
    pub flashloan_fee_usd: Option<f64>,
    pub volume_usd: Option<f64>,
    pub net_profit_usd: Option<f64>,
    pub route_json: Option<String>,
    pub victim_hashes: Option<String>,
    pub details_json: Option<String>,
    pub detector: String,
    pub created_at: u64,
    pub subkind: Option<String>,
    pub excluded_from_net: Option<bool>,
    pub usd_approximate: Option<bool>,
    pub native_unpriced: Option<bool>,
    pub multi_asset_pricing: Option<bool>,
    pub pricing_clamped: Option<bool>,
    pub flash_fee_unpriced: Option<bool>,
}

impl OpRecord {
    pub fn competitor(&self) -> String {
        self.contract
            .as_ref()
            .filter(|c| !c.is_empty())
            .cloned()
            .unwrap_or_else(|| self.eoa.clone())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DbMeta {
    pub path: String,
    pub chain_label: String,
    pub chain_id: Option<u64>,
    pub size_bytes: u64,
    pub user_version: i64,
    pub schema_head: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoveragePanel {
    pub head: Option<u64>,
    pub indexed_to: Option<u64>,
    pub last_indexed_at: Option<u64>,
    pub blocks_in_window: i64,
    pub ops_in_window: i64,
    pub min_block_ts: Option<i64>,
    pub max_block_ts: Option<i64>,
    /// Number endpoints of recorded blocks in the selected time window, not sync state.
    pub min_block_number: Option<u64>,
    pub max_block_number: Option<u64>,
    /// Timestamps belonging to the number endpoints (on-chain time, not runtime).
    pub first_block_ts: Option<i64>,
    pub last_block_ts: Option<i64>,
    pub gap_count: Option<i64>,
    pub first_gap: Option<u64>,
    pub last_gap: Option<u64>,
}

/// P2 index gaps (opt-in via `--with-gaps`).
#[derive(Debug, Clone, Serialize)]
pub struct GapsPanel {
    pub enabled: bool,
    pub scanned_from: Option<u64>,
    pub scanned_to: Option<u64>,
    pub gap_count: i64,
    pub first_gap: Option<u64>,
    pub last_gap: Option<u64>,
    /// Collapsed consecutive runs, capped.
    pub ranges: Vec<GapRange>,
    /// First/last sample of individual gap blocks (deduped, capped).
    pub sample: Vec<u64>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GapRange {
    pub from: u64,
    pub to: u64,
    pub len: u64,
}

impl GapsPanel {
    fn disabled() -> Self {
        Self {
            enabled: false,
            scanned_from: None,
            scanned_to: None,
            gap_count: 0,
            first_gap: None,
            last_gap: None,
            ranges: Vec::new(),
            sample: Vec::new(),
            note: "Pass --with-gaps to sample unclassified blocks near tip (expensive CTE).".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Semantics {
    pub null_usd_means: &'static str,
    pub usd_columns_are: &'static str,
    pub net_excludes: &'static str,
    pub amounts_are: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStateRow {
    pub chain_id: u64,
    pub head: u64,
    pub indexed_to: u64,
    pub last_indexed_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JitOpenRow {
    pub pool: String,
    pub owner: String,
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub opened_block: u64,
    pub liquidity: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OracleFeedRow {
    pub feed: String,
    pub answers: i64,
    pub last_block: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PriceRow {
    pub token: String,
    pub hour: i64,
    pub price_usd: f64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableCount {
    pub table: String,
    pub rows: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectPanel {
    pub recording_empty: bool,
    pub total: i64,
    pub ladder: Vec<RejectRow>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeneratorMeta {
    pub version: String,
    pub generated_at: i64,
}

/// Versioned dashboard artifact (HTML embeds this as JSON).
#[derive(Debug, Clone, Serialize)]
pub struct DashboardSnapshot {
    pub api_version: u32,
    pub generator: GeneratorMeta,
    pub db: DbMeta,
    pub caps: StoreCaps,
    pub window: WindowSpec,
    pub semantics: Semantics,
    pub coverage: CoveragePanel,
    pub gaps: GapsPanel,
    pub panels: ReducedPanels,
    pub rejects: RejectPanel,
    pub jit_open: Vec<JitOpenRow>,
    pub oracle_feeds: Vec<OracleFeedRow>,
    pub prices: Vec<PriceRow>,
    pub storage: Vec<TableCount>,
    pub truncation: TruncationInfo,
}

/// Open read-only, probe caps, load ops, reduce, return snapshot.
pub fn build(opts: &BuildOptions) -> anyhow::Result<DashboardSnapshot> {
    let store = open_with_wal_retry(&opts.db_path)?;
    let caps = StoreCaps::probe(store.connection())?;
    let size_bytes = std::fs::metadata(&opts.db_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let chain_label = chain_label_from_path(&opts.db_path);

    // Single consistent read snapshot.
    let tx = store.connection().unchecked_transaction()?;

    let sync = load_sync_state(&tx)?;
    let ops = load_ops_window(&tx, &caps, opts.window.since_ts, opts.window.until_ts)?;
    let block_rows = load_block_rows(&tx, opts.window.since_ts, opts.window.until_ts)?;
    let first_block = block_rows
        .iter()
        .filter_map(|(n, ts)| n.map(|n| (n, *ts)))
        .min();
    let last_block = block_rows
        .iter()
        .filter_map(|(n, ts)| n.map(|n| (n, *ts)))
        .max();
    let mut sorted_blocks: Vec<i64> = block_rows.iter().map(|(_, ts)| *ts).collect();
    let blocks_in_window = block_rows.len() as i64;
    sorted_blocks.sort_unstable();
    let min_block_ts = sorted_blocks.first().copied();
    let max_block_ts = sorted_blocks.last().copied();

    // `WindowSpec::all` starts at epoch 0 — clamp the *series domain* to observed
    // data so we don't emit ~10⁶ empty minute buckets into the artifact.
    let mut window = opts.window.clone();
    if window.label == "all" || window.since_ts == 0 {
        let op_min = ops.iter().map(|o| o.ts as i64).min();
        let op_max = ops.iter().map(|o| o.ts as i64).max();
        let lo = [min_block_ts, op_min].into_iter().flatten().min();
        let hi = [max_block_ts, op_max].into_iter().flatten().max();
        if let (Some(lo), Some(hi)) = (lo, hi) {
            window.since_ts = lo;
            window.until_ts = hi.max(lo);
        }
    }

    let labels = load_labels(&tx)?;
    let catalogue_tags = load_catalogue_tags();
    let mut panels = reduce(
        &ops,
        &caps,
        &window,
        &sorted_blocks,
        &labels,
        &catalogue_tags,
        opts.max_ops,
        opts.max_evidence,
    );

    // Soft byte budget: drop evidence first, then trim events.
    let budget = opts.budget_mb.saturating_mul(1024 * 1024);
    enforce_budget(&mut panels, budget);

    let reject_counts = load_reject_counts(&tx, opts.window.since_ts, opts.window.until_ts)?;
    let reject_total: i64 = reject_counts.values().sum();
    let rejects = RejectPanel {
        recording_empty: reject_total == 0,
        total: reject_total,
        ladder: reject_ladder(&reject_counts),
        note: if reject_total == 0 {
            "No rejected_candidates rows in window — recording may be off (record_rejections default false), not proof of zero rejects.".into()
        } else {
            "Counts from rejected_candidates in window.".into()
        },
    };

    let jit_open = load_jit_open(&tx)?;
    let oracle_feeds = load_oracle_feeds(&tx)?;
    let prices = load_prices(&tx)?;
    let storage = load_table_counts(&tx, &caps.tables)?;

    let gaps = if opts.with_gaps {
        summarize_gaps(&store, sync.as_ref(), opts.gap_block_limit)?
    } else {
        GapsPanel::disabled()
    };

    // End read txn (commit is a no-op for writes; keeps snapshot consistent).
    tx.commit()?;

    let chain_id = sync.as_ref().map(|s| s.chain_id);
    let coverage = CoveragePanel {
        head: sync.as_ref().map(|s| s.head),
        indexed_to: sync.as_ref().map(|s| s.indexed_to),
        last_indexed_at: sync.as_ref().map(|s| s.last_indexed_at),
        blocks_in_window,
        ops_in_window: ops.len() as i64,
        min_block_ts,
        max_block_ts,
        min_block_number: first_block.map(|(n, _)| n),
        max_block_number: last_block.map(|(n, _)| n),
        first_block_ts: first_block.map(|(_, ts)| ts),
        last_block_ts: last_block.map(|(_, ts)| ts),
        gap_count: if gaps.enabled {
            Some(gaps.gap_count)
        } else {
            None
        },
        first_gap: gaps.first_gap,
        last_gap: gaps.last_gap,
    };

    let truncation = panels.truncation.clone();
    Ok(DashboardSnapshot {
        api_version: API_VERSION,
        generator: GeneratorMeta {
            version: env!("CARGO_PKG_VERSION").to_string(),
            generated_at: now_secs(),
        },
        db: DbMeta {
            path: opts.db_path.display().to_string(),
            chain_label,
            chain_id,
            size_bytes,
            user_version: caps.user_version,
            schema_head: SCHEMA_HEAD,
            status: caps.status_label().to_string(),
        },
        caps,
        window,
        semantics: Semantics {
            null_usd_means: "not priceable — never zero",
            usd_columns_are: "f64 approximations; prefix ≈; never balances",
            net_excludes: "rows with excluded_from_net=1 (when column present)",
            amounts_are: "raw U256 decimal strings; humanize only with known decimals",
        },
        coverage,
        gaps,
        panels,
        rejects,
        jit_open,
        oracle_feeds,
        prices,
        storage,
        truncation,
    })
}

fn open_with_wal_retry(path: &Path) -> anyhow::Result<ExplorerStore> {
    match ExplorerStore::open_read_only(path) {
        Ok(s) => Ok(s),
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("disk I/O") || msg.contains("locked") || msg.contains("busy") {
                std::thread::sleep(std::time::Duration::from_millis(500));
                ExplorerStore::open_read_only(path).map_err(|e2| {
                    anyhow::anyhow!("read-only open retry failed: {e2:#} (first: {msg})")
                })
            } else {
                Err(e)
            }
        }
    }
}

fn chain_label_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    stem.strip_prefix("explorer-")
        .or_else(|| stem.strip_prefix("oracle-"))
        .unwrap_or(stem)
        .to_string()
}

fn load_ops_window(
    conn: &rusqlite::Connection,
    caps: &StoreCaps,
    since: i64,
    until: i64,
) -> anyhow::Result<Vec<OpRecord>> {
    let mut cols = vec![
        "id",
        "block_number",
        "tx_index",
        "tx_hash",
        "ts",
        "kind",
        "eoa",
        "contract",
        "confidence",
        "canonical_id",
        "profit_token",
        "profit_amount",
        "profit_usd",
        "gas_cost_usd",
        "net_profit_usd",
        "route_json",
        "victim_hashes",
        "details_json",
        "detector",
        "created_at",
    ];
    if caps.has_flashloan_fee_usd {
        cols.push("flashloan_fee_usd");
    }
    if caps.has_volume_usd {
        cols.push("volume_usd");
    }
    if caps.has_excluded_from_net {
        cols.push("excluded_from_net");
    }
    if caps.has_subkind {
        cols.push("subkind");
    }
    if caps.has_pricing_flags {
        cols.extend_from_slice(&[
            "usd_approximate",
            "native_unpriced",
            "multi_asset_pricing",
            "pricing_clamped",
            "flash_fee_unpriced",
        ]);
    }
    let sql = format!(
        "SELECT {} FROM mev_ops WHERE ts >= ?1 AND ts <= ?2
         ORDER BY block_number ASC, tx_index ASC, id ASC",
        cols.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let col_index: HashMap<String, usize> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| ((*c).to_string(), i))
        .collect();
    let idx = |name: &str| -> anyhow::Result<usize> {
        col_index
            .get(name)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("missing projected column {name}"))
    };
    // Required base columns always present in projection.
    let i_id = idx("id")?;
    let i_bn = idx("block_number")?;
    let i_th = idx("tx_hash")?;
    let i_ts = idx("ts")?;
    let i_kind = idx("kind")?;
    let i_eoa = idx("eoa")?;
    let i_conf = idx("confidence")?;
    let i_det = idx("detector")?;
    let i_ca = idx("created_at")?;
    let opt = |name: &str| -> Option<usize> { col_index.get(name).copied() };
    let i_txi = opt("tx_index");
    let i_contract = opt("contract");
    let i_cid = opt("canonical_id");
    let i_pt = opt("profit_token");
    let i_pa = opt("profit_amount");
    let i_pusd = opt("profit_usd");
    let i_gas = opt("gas_cost_usd");
    let i_net = opt("net_profit_usd");
    let i_route = opt("route_json");
    let i_victims = opt("victim_hashes");
    let i_details = opt("details_json");
    let i_flash = opt("flashloan_fee_usd");
    let i_vol = opt("volume_usd");
    let i_excl = opt("excluded_from_net");
    let i_sub = opt("subkind");
    let i_approx = opt("usd_approximate");
    let i_native = opt("native_unpriced");
    let i_multi = opt("multi_asset_pricing");
    let i_clamp = opt("pricing_clamped");
    let i_ffu = opt("flash_fee_unpriced");

    let rows = stmt.query_map(rusqlite::params![since, until], |r| {
        let opt_i64 = |i: Option<usize>| -> rusqlite::Result<Option<i64>> {
            match i {
                Some(i) => r.get(i),
                None => Ok(None),
            }
        };
        let opt_f64 = |i: Option<usize>| -> rusqlite::Result<Option<f64>> {
            match i {
                Some(i) => r.get(i),
                None => Ok(None),
            }
        };
        let opt_str = |i: Option<usize>| -> rusqlite::Result<Option<String>> {
            match i {
                Some(i) => r.get(i),
                None => Ok(None),
            }
        };
        let opt_bool = |i: Option<usize>| -> rusqlite::Result<Option<bool>> {
            match i {
                Some(i) => Ok(r.get::<_, Option<i64>>(i)?.map(|x| x != 0)),
                None => Ok(None),
            }
        };
        let kind_raw: String = r.get(i_kind)?;
        let kind = if kind_raw == "unknown" {
            "utility".to_string()
        } else {
            kind_raw.clone()
        };
        Ok(OpRecord {
            id: r.get(i_id)?,
            block_number: r.get::<_, i64>(i_bn)? as u64,
            tx_index: opt_i64(i_txi)?.map(|v| v as u64),
            tx_hash: r.get(i_th)?,
            ts: r.get::<_, i64>(i_ts)? as u64,
            kind,
            kind_raw,
            eoa: r.get(i_eoa)?,
            contract: opt_str(i_contract)?,
            confidence: r.get(i_conf)?,
            canonical_id: opt_str(i_cid)?,
            profit_token: opt_str(i_pt)?,
            profit_amount: opt_str(i_pa)?,
            profit_usd: opt_f64(i_pusd)?,
            gas_cost_usd: opt_f64(i_gas)?,
            flashloan_fee_usd: opt_f64(i_flash)?,
            volume_usd: opt_f64(i_vol)?,
            net_profit_usd: opt_f64(i_net)?,
            route_json: opt_str(i_route)?,
            victim_hashes: opt_str(i_victims)?,
            details_json: opt_str(i_details)?,
            detector: r.get(i_det)?,
            created_at: r.get::<_, i64>(i_ca)? as u64,
            subkind: opt_str(i_sub)?,
            excluded_from_net: opt_bool(i_excl)?,
            usd_approximate: opt_bool(i_approx)?,
            native_unpriced: opt_bool(i_native)?,
            multi_asset_pricing: opt_bool(i_multi)?,
            pricing_clamped: opt_bool(i_clamp)?,
            flash_fee_unpriced: opt_bool(i_ffu)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_block_rows(
    conn: &rusqlite::Connection,
    since: i64,
    until: i64,
) -> anyhow::Result<Vec<(Option<u64>, i64)>> {
    let cols = caps::column_names(conn, "blocks")?;
    if !cols.contains("ts") {
        // Cannot assign rows without chain timestamps to a time window.
        return Ok(Vec::new());
    }
    let number = if cols.contains("block_number") {
        "block_number"
    } else {
        "NULL"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {number}, ts FROM blocks WHERE ts >= ?1 AND ts <= ?2"
    ))?;
    let rows = stmt.query_map(rusqlite::params![since, until], |r| {
        Ok((
            r.get::<_, Option<i64>>(0)?
                .and_then(|n| u64::try_from(n).ok()),
            r.get(1)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_sync_state(conn: &rusqlite::Connection) -> anyhow::Result<Option<SyncStateRow>> {
    let row = conn
        .query_row(
            "SELECT chain_id, head, indexed_to, last_indexed_at FROM sync_state LIMIT 1",
            [],
            |r| {
                Ok(SyncStateRow {
                    chain_id: r.get::<_, i64>(0)? as u64,
                    head: r.get::<_, i64>(1)? as u64,
                    indexed_to: r.get::<_, i64>(2)? as u64,
                    last_indexed_at: r.get::<_, i64>(3)? as u64,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn load_labels(conn: &rusqlite::Connection) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    let Ok(mut stmt) =
        conn.prepare("SELECT address, COALESCE(name, entity, kind, address) FROM labels")
    else {
        return Ok(map);
    };
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (addr, label) = row?;
        map.insert(addr, label);
    }
    Ok(map)
}

fn load_catalogue_tags() -> HashSet<String> {
    let raw = include_str!("../../../data/explorer_tags.json");
    let mut set = HashSet::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(arr) = v.as_array() {
            for item in arr {
                if let Some(s) = item.as_str() {
                    set.insert(s.to_string());
                } else if let Some(s) = item.get("tag").and_then(|t| t.as_str()) {
                    set.insert(s.to_string());
                } else if let Some(s) = item.get("name").and_then(|t| t.as_str()) {
                    set.insert(s.to_string());
                }
            }
        } else if let Some(obj) = v.as_object() {
            for key in obj.keys() {
                set.insert(key.clone());
            }
        }
    }
    set
}

fn load_reject_counts(
    conn: &rusqlite::Connection,
    since: i64,
    until: i64,
) -> anyhow::Result<HashMap<String, i64>> {
    let mut map = HashMap::new();
    let sql = "SELECT r.reject_reason, COUNT(*)
               FROM rejected_candidates r
               LEFT JOIN blocks b ON b.block_number = r.block_number
               WHERE COALESCE(b.ts, r.created_at) >= ?1
                 AND COALESCE(b.ts, r.created_at) <= ?2
               GROUP BY r.reject_reason";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Ok(map);
    };
    let rows = stmt.query_map(rusqlite::params![since, until], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (reason, n) = row?;
        map.insert(reason, n);
    }
    Ok(map)
}

fn load_jit_open(conn: &rusqlite::Connection) -> anyhow::Result<Vec<JitOpenRow>> {
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT pool, owner, tick_lower, tick_upper, opened_block, liquidity
         FROM jit_open_positions ORDER BY opened_block DESC LIMIT 200",
    ) else {
        return Ok(out);
    };
    let rows = stmt.query_map([], |r| {
        Ok(JitOpenRow {
            pool: r.get(0)?,
            owner: r.get(1)?,
            tick_lower: r.get(2)?,
            tick_upper: r.get(3)?,
            opened_block: r.get::<_, i64>(4)? as u64,
            liquidity: r.get::<_, String>(5)?,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_oracle_feeds(conn: &rusqlite::Connection) -> anyhow::Result<Vec<OracleFeedRow>> {
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT feed, COUNT(*), MAX(block_number) FROM oracle_answers GROUP BY feed ORDER BY COUNT(*) DESC LIMIT 50",
    ) else {
        return Ok(out);
    };
    let rows = stmt.query_map([], |r| {
        Ok(OracleFeedRow {
            feed: r.get(0)?,
            answers: r.get(1)?,
            last_block: r.get(2)?,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_prices(conn: &rusqlite::Connection) -> anyhow::Result<Vec<PriceRow>> {
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT token, hour, usd, COALESCE(source, '') FROM prices ORDER BY hour DESC LIMIT 50",
    ) else {
        return Ok(out);
    };
    let rows = stmt.query_map([], |r| {
        Ok(PriceRow {
            token: r.get(0)?,
            hour: r.get(1)?,
            price_usd: r.get(2)?,
            source: r.get(3)?,
        })
    })?;
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_table_counts(
    conn: &rusqlite::Connection,
    tables: &[String],
) -> anyhow::Result<Vec<TableCount>> {
    let mut out = Vec::new();
    for table in tables {
        // table names come from sqlite_master; still quote via identity check
        if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or(0);
        out.push(TableCount {
            table: table.clone(),
            rows: n,
        });
    }
    out.sort_by(|a, b| b.rows.cmp(&a.rows).then(a.table.cmp(&b.table)));
    Ok(out)
}

fn summarize_gaps(
    store: &ExplorerStore,
    sync: Option<&SyncStateRow>,
    limit: u64,
) -> anyhow::Result<GapsPanel> {
    let Some(s) = sync else {
        return Ok(GapsPanel {
            enabled: true,
            scanned_from: None,
            scanned_to: None,
            gap_count: 0,
            first_gap: None,
            last_gap: None,
            ranges: Vec::new(),
            sample: Vec::new(),
            note: "No sync_state row — cannot scan gaps.".into(),
        });
    };
    let to = s.indexed_to;
    let from = to.saturating_sub(limit);
    let gaps = store.unclassified_blocks(from, to)?;
    let ranges = collapse_gap_ranges(&gaps, 50);
    let sample = sample_gaps(&gaps, 20);
    Ok(GapsPanel {
        enabled: true,
        scanned_from: Some(from),
        scanned_to: Some(to),
        gap_count: gaps.len() as i64,
        first_gap: gaps.first().copied(),
        last_gap: gaps.last().copied(),
        ranges,
        sample,
        note: format!(
            "Unclassified blocks in [{from}..{to}] (cap {limit}). \
             Ranges/sample truncated for artifact size; count is exact for the scan window."
        ),
    })
}

fn collapse_gap_ranges(gaps: &[u64], max_ranges: usize) -> Vec<GapRange> {
    let mut out = Vec::new();
    if gaps.is_empty() {
        return out;
    }
    let mut start = gaps[0];
    let mut prev = gaps[0];
    for &b in &gaps[1..] {
        if b == prev + 1 {
            prev = b;
            continue;
        }
        out.push(GapRange {
            from: start,
            to: prev,
            len: prev - start + 1,
        });
        if out.len() >= max_ranges {
            return out;
        }
        start = b;
        prev = b;
    }
    if out.len() < max_ranges {
        out.push(GapRange {
            from: start,
            to: prev,
            len: prev - start + 1,
        });
    }
    out
}

fn sample_gaps(gaps: &[u64], each_end: usize) -> Vec<u64> {
    if gaps.len() <= each_end * 2 {
        return gaps.to_vec();
    }
    let mut out = Vec::with_capacity(each_end * 2);
    out.extend_from_slice(&gaps[..each_end]);
    out.extend_from_slice(&gaps[gaps.len() - each_end..]);
    out
}

fn enforce_budget(panels: &mut ReducedPanels, budget: usize) {
    if budget == 0 {
        return;
    }
    let measure = |p: &ReducedPanels| serde_json::to_vec(p).map(|v| v.len()).unwrap_or(usize::MAX);
    if measure(panels) <= budget {
        return;
    }
    // 1) clear evidence
    panels.evidence.clear();
    panels.truncation.evidence_emitted = 0;
    panels.truncation.hit_budget = true;
    if measure(panels) <= budget {
        return;
    }
    // 2) halve events to floor 50
    while panels.events.len() > 50 && measure(panels) > budget {
        let keep = (panels.events.len() / 2).max(50);
        panels.events.truncate(keep);
        panels.truncation.ops_emitted = panels.events.len();
        panels.truncation.hit_budget = true;
    }
    if measure(panels) <= budget {
        return;
    }
    // 3) halve scatter_points to floor 200
    while panels.scatter_points.len() > 200 && measure(panels) > budget {
        let keep = (panels.scatter_points.len() / 2).max(200);
        panels.scatter_points.truncate(keep);
        panels.truncation.hit_budget = true;
    }
    if measure(panels) <= budget {
        return;
    }
    // 4) clear op_index
    if !panels.op_index.rows.is_empty() {
        panels.op_index = reduce::OpIndex::default();
        panels.truncation.index_emitted = 0;
        panels.truncation.index_complete = false;
        panels.truncation.hit_budget = true;
    }
}

/// Render snapshot as HTML or return JSON/CSV string.
pub fn render_html(snapshot: &DashboardSnapshot) -> anyhow::Result<String> {
    render::to_html(snapshot)
}

pub fn render_json(snapshot: &DashboardSnapshot) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(snapshot)?)
}

/// Report-compatible CSV (`window,section,label,ops,...`).
pub fn render_csv(snapshot: &DashboardSnapshot) -> String {
    csv::render_csv(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::store::ExplorerStore;

    #[test]
    fn window_parse_all_and_day() {
        let w = WindowSpec::parse("all").expect("all");
        assert_eq!(w.label, "all");
        let d = WindowSpec::parse("7d").expect("7d");
        assert!(d.until_ts - d.since_ts >= 7 * 86400 - 1);
    }

    #[test]
    fn build_from_memory_via_temp_file() {
        let dir = std::env::temp_dir().join(format!(
            "mev_dash_build_{}_{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("explorer-avalanche.sqlite");
        {
            let store = ExplorerStore::open(&path).expect("create");
            store.set_sync_state(43114, 100, 100).expect("sync");
            let _ = store;
        }
        let snap = build(&BuildOptions {
            db_path: path.clone(),
            window: WindowSpec::all(),
            ..BuildOptions::default()
        })
        .expect("build");
        assert_eq!(snap.api_version, API_VERSION);
        assert_eq!(snap.db.chain_label, "avalanche");
        assert_eq!(snap.db.user_version, SCHEMA_HEAD);
        assert_eq!(snap.coverage.head, Some(100));
        let html = render_html(&snap).expect("html");
        assert!(html.contains("snapshot"));
        assert!(!html.contains("</script><script>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coverage_empty_db_has_no_endpoints() {
        let dir = std::env::temp_dir().join(format!(
            "mev_dash_empty_{}_{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("explorer-avalanche.sqlite");
        {
            let store = ExplorerStore::open(&path).expect("create");
            store.set_sync_state(43114, 100, 100).expect("sync");
            let _ = store;
        }
        let snap = build(&BuildOptions {
            db_path: path,
            window: WindowSpec::all(),
            ..BuildOptions::default()
        })
        .expect("build");
        assert_eq!(snap.coverage.head, Some(100));
        assert_eq!(snap.coverage.blocks_in_window, 0);
        assert_eq!(snap.coverage.ops_in_window, 0);
        assert_eq!(snap.coverage.min_block_number, None);
        assert_eq!(snap.coverage.max_block_number, None);
        assert_eq!(snap.coverage.min_block_ts, None);
        assert_eq!(snap.coverage.max_block_ts, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Recorded blocks with no MEV rows are still coverage: the panel reports
    /// the block endpoints and timestamps while ops stay zero.
    #[test]
    fn coverage_reports_block_endpoints_for_mev_free_blocks() {
        let dir = std::env::temp_dir().join(format!(
            "mev_dash_cover_{}_{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("explorer-avalanche.sqlite");
        {
            let store = ExplorerStore::open(&path).expect("create");
            store.set_sync_state(43114, 50, 50).expect("sync");
            for (number, ts) in [(40u64, 1_000i64), (42, 1_200), (45, 1_500)] {
                store
                    .connection()
                    .execute(
                        "INSERT INTO blocks(block_number, block_hash, ts, indexed_at)
                         VALUES(?1, ?2, ?3, ?3)",
                        rusqlite::params![number as i64, format!("0xbk{number}"), ts],
                    )
                    .expect("block");
            }
            let _ = store;
        }
        let snap = build(&BuildOptions {
            db_path: path,
            window: WindowSpec {
                label: "cover".into(),
                since_ts: 1_000,
                until_ts: 1_500,
            },
            ..BuildOptions::default()
        })
        .expect("build");
        assert_eq!(snap.coverage.blocks_in_window, 3);
        assert_eq!(snap.coverage.ops_in_window, 0);
        assert_eq!(snap.coverage.min_block_number, Some(40));
        assert_eq!(snap.coverage.max_block_number, Some(45));
        assert_eq!(snap.coverage.first_block_ts, Some(1_000));
        assert_eq!(snap.coverage.last_block_ts, Some(1_500));
        assert_eq!(snap.coverage.min_block_ts, Some(1_000));
        assert_eq!(snap.coverage.max_block_ts, Some(1_500));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reducer_reconciles_with_report_overview_and_by_kind() {
        let dir = std::env::temp_dir().join(format!(
            "mev_dash_recon_{}_{}",
            std::process::id(),
            now_secs()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("explorer-avalanche.sqlite");
        let since = 1_700_000_000i64;
        let (overview_ops, overview_net, overview_gross, by_kind) = {
            let store = ExplorerStore::open(&path).expect("create");
            store.set_sync_state(43114, 10, 10).expect("sync");
            store
                .connection()
                .execute(
                    "INSERT INTO blocks(block_number, block_hash, ts, indexed_at)
                     VALUES(1, '0xbk', ?1, ?1)",
                    [since],
                )
                .expect("block");
            for (i, (kind, gross, net)) in [
                ("arb_atomic", 5.0_f64, 4.5_f64),
                ("jit", 2.0, 1.5),
                ("arb_atomic", 1.0, 0.8),
            ]
            .iter()
            .enumerate()
            {
                store
                    .connection()
                    .execute(
                        "INSERT INTO mev_ops(
                           block_number, tx_index, tx_hash, ts, kind, eoa, confidence,
                           profit_usd, gas_cost_usd, net_profit_usd, volume_usd, flashloan_fee_usd,
                           detector, created_at, subkind, excluded_from_net,
                           usd_approximate, native_unpriced, multi_asset_pricing, pricing_clamped, flash_fee_unpriced
                         ) VALUES(1, ?1, ?2, ?3, ?4, '0xee', 'exact',
                           ?5, 0.1, ?6, 10.0, 0.0, 't', ?3, NULL, 0, 0, 0, 0, 0, 0)",
                        rusqlite::params![
                            i as i64,
                            format!("0xrecon{i}"),
                            since,
                            kind,
                            gross,
                            net
                        ],
                    )
                    .expect("op");
            }
            let overview = store
                .report_window_overview(since as u64, None, None)
                .expect("overview");
            let kinds = store
                .report_by_kind(since as u64, None, None)
                .expect("by_kind");
            (overview.ops, overview.net_usd, overview.gross_usd, kinds)
        };
        let snap = build(&BuildOptions {
            db_path: path,
            window: WindowSpec {
                label: "recon".into(),
                since_ts: since - 10,
                until_ts: since + 10,
            },
            ..BuildOptions::default()
        })
        .expect("build");
        assert_eq!(snap.panels.kpi.ops, overview_ops);
        assert!((snap.panels.kpi.net_usd - overview_net).abs() < 1e-9);
        assert!((snap.panels.kpi.gross_usd - overview_gross).abs() < 1e-9);

        // Roll kind_matrix to kind-only totals and match report_by_kind.
        let mut dash_by_kind: std::collections::BTreeMap<String, (i64, f64, f64)> =
            std::collections::BTreeMap::new();
        for cell in &snap.panels.kind_matrix {
            if cell.ops == 0 && cell.gross_usd == 0.0 && cell.net_usd == 0.0 {
                continue;
            }
            let e = dash_by_kind
                .entry(cell.kind.clone())
                .or_insert((0, 0.0, 0.0));
            e.0 += cell.ops;
            e.1 += cell.gross_usd;
            e.2 += cell.net_usd;
        }
        for row in &by_kind {
            let d = dash_by_kind
                .get(&row.label)
                .unwrap_or_else(|| panic!("missing kind {}", row.label));
            assert_eq!(d.0, row.ops, "ops {}", row.label);
            assert!((d.1 - row.gross_usd).abs() < 1e-9, "gross {}", row.label);
            assert!((d.2 - row.net_usd).abs() < 1e-9, "net {}", row.label);
        }
        let csv = render_csv(&snap);
        assert!(csv.starts_with(
            "window,section,label,ops,volume_usd,gross_usd,gas_usd,flash_fee_usd,net_usd"
        ));
        assert!(csv.contains(",by_kind,arb_atomic,"));
        assert!(csv.contains(",by_kind,jit,"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
