//! One-time DEX / token display metadata for the dashboard.
//!
//! The explorer DB stores raw addresses. Human labels are fetched once from
//! keyless public APIs and cached in the scanner cache DB:
//!
//! * `token_symbols` — symbol / name / decimals / icon URL (existing table,
//!   shared with pool discovery);
//! * `pool_dex` — pool → DEX id/name + venue label (DexScreener);
//! * `token_icon` — downloaded small icon bytes as a `data:`-able blob.
//!
//! Nothing here can fail the dashboard: every network/cache error is logged
//! and the raw address stays the fallback label.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use alloy::primitives::Address;
use base64::Engine as _;
use rusqlite::{params, Connection};
use serde_json::Value;

use crate::types::ChainName;
use crate::utils::http_client;

use super::evidence::RouteLeg;
use super::DashboardSnapshot;

const NET_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_ICON_BYTES: usize = 200_000;
/// Per-build network cap (a first all-time run may see hundreds of tokens;
/// the rest resolve on the next build).
const MAX_NETWORK_TOKENS: usize = 100;
const MAX_NETWORK_POOLS: usize = 100;
const MAX_COINGECKO_TOKENS: usize = 20;
const LLAMA_BATCH: usize = 40;
const MAX_EMBEDDED_ICONS: usize = 200;
const FETCH_GAP_MS: u64 = 250;

#[derive(Debug, Clone, Default)]
struct TokenMeta {
    symbol: String,
    name: Option<String>,
    decimals: Option<i64>,
    icon_url: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct PoolMeta {
    dex_id: String,
    dex_name: String,
    label: Option<String>,
    /// Public pool page (DexScreener) for the leg's external link.
    url: Option<String>,
    /// Base/quote token metadata carried by the same DexScreener response.
    tokens: Vec<(Address, TokenMeta)>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EnrichReport {
    pub tokens_cached: usize,
    pub pools_cached: usize,
    pub icons_cached: usize,
    pub network_calls: usize,
}

/// Resolve labels for every route leg in the snapshot and cache the results.
/// Best-effort: the snapshot is left untouched for anything that fails.
pub async fn enrich(
    snapshot: &mut DashboardSnapshot,
    cache_db: &Path,
    chain: ChainName,
) -> EnrichReport {
    let mut report = EnrichReport::default();
    // Receipt-derived insights run before any network work so they apply even
    // when every metadata API is unreachable.
    infer_route_insights(snapshot);
    let (tokens, pools) = collect_targets(snapshot);
    if tokens.is_empty() && pools.is_empty() {
        return report;
    }
    let conn = match open_cache(cache_db) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, path = %cache_db.display(), "metadata cache unavailable");
            return report;
        }
    };
    let mut token_meta = load_tokens(&conn);
    let mut pool_meta = load_pools(&conn);
    let mut icons = load_icons(&conn);

    if let Err(e) = fetch_missing(&conn, chain, &tokens, &pools, &mut token_meta, &mut pool_meta, &mut report).await {
        tracing::warn!(error = %e, "metadata fetch aborted");
    }
    fetch_icons(&conn, &tokens, &token_meta, &mut icons, &mut report).await;
    apply(snapshot, &token_meta, &pool_meta, &icons);
    if report.network_calls > 0 {
        tracing::info!(
            tokens = report.tokens_cached,
            pools = report.pools_cached,
            icons = report.icons_cached,
            calls = report.network_calls,
            "dashboard metadata enriched and cached"
        );
    }
    report
}

fn open_cache(path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    // Cache-table evolution: older builds wrote pool_dex without the link.
    let _ = conn.execute("ALTER TABLE pool_dex ADD COLUMN url TEXT", []);
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS token_symbols (
             address    BLOB PRIMARY KEY,
             symbol     TEXT NOT NULL,
             decimals   INTEGER,
             name       TEXT,
             icon_url   TEXT
         );
         CREATE TABLE IF NOT EXISTS pool_dex (
             pool       BLOB PRIMARY KEY,
             dex_id     TEXT NOT NULL,
             dex_name   TEXT NOT NULL,
             label      TEXT,
             url        TEXT,
             fetched_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS token_icon (
             address    BLOB PRIMARY KEY,
             mime       TEXT NOT NULL,
             data       BLOB NOT NULL,
             fetched_at INTEGER NOT NULL
         );",
    )?;
    Ok(conn)
}

fn parse_addr(s: &str) -> Option<Address> {
    let raw = s.trim().trim_start_matches("0x");
    if raw.len() != 40 {
        return None;
    }
    let bytes = hex::decode(raw).ok()?;
    let addr = Address::from_slice(&bytes);
    if addr.is_zero() || addr == crate::explorer::profit::NATIVE_MARKER {
        return None;
    }
    Some(addr)
}

fn collect_targets(snapshot: &DashboardSnapshot) -> (HashSet<Address>, HashSet<Address>) {
    let mut tokens = HashSet::new();
    let mut pools = HashSet::new();
    for ev in &snapshot.panels.evidence {
        for leg in &ev.route {
            for t in [leg.token_in.as_deref(), leg.token_out.as_deref()] {
                if let Some(a) = t.and_then(parse_addr) {
                    tokens.insert(a);
                }
            }
            if let Some(p) = leg.pool.as_deref().and_then(parse_addr) {
                pools.insert(p);
            }
        }
    }
    (tokens, pools)
}

fn load_tokens(conn: &Connection) -> HashMap<Address, TokenMeta> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare("SELECT address, symbol, decimals, name, icon_url FROM token_symbols")
    else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    }) else {
        return out;
    };
    for row in rows.flatten() {
        let (addr, symbol, decimals, name, icon_url) = row;
        if addr.len() == 20 {
            out.insert(
                Address::from_slice(&addr),
                TokenMeta {
                    symbol,
                    name,
                    decimals,
                    icon_url,
                },
            );
        }
    }
    out
}

fn load_pools(conn: &Connection) -> HashMap<Address, PoolMeta> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare("SELECT pool, dex_id, dex_name, label, url FROM pool_dex")
    else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    }) else {
        return out;
    };
    for row in rows.flatten() {
        let (pool, dex_id, dex_name, label, url) = row;
        if pool.len() == 20 {
            out.insert(
                Address::from_slice(&pool),
                PoolMeta {
                    dex_id,
                    dex_name,
                    label,
                    url,
                    tokens: Vec::new(),
                },
            );
        }
    }
    out
}

fn load_icons(conn: &Connection) -> HashMap<Address, String> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare("SELECT address, mime, data FROM token_icon") else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    }) else {
        return out;
    };
    for row in rows.flatten() {
        let (addr, mime, data) = row;
        if addr.len() == 20 {
            out.insert(Address::from_slice(&addr), data_uri(&mime, &data));
        }
    }
    out
}

fn data_uri(mime: &str, data: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(data)
    )
}

async fn fetch_missing(
    conn: &Connection,
    chain: ChainName,
    tokens: &HashSet<Address>,
    pools: &HashSet<Address>,
    token_meta: &mut HashMap<Address, TokenMeta>,
    pool_meta: &mut HashMap<Address, PoolMeta>,
    report: &mut EnrichReport,
) -> anyhow::Result<()> {
    let client = http_client(NET_TIMEOUT);

    // 1) Pools via DexScreener: also carries base/quote name+symbol.
    let ds_chain = dexscreener_chain(chain);
    let mut missing_pools: Vec<Address> = pools
        .iter()
        .copied()
        .filter(|p| !pool_meta.contains_key(p))
        .collect();
    missing_pools.sort_unstable_by_key(|a| format!("{a:#x}"));
    for pool in missing_pools.into_iter().take(MAX_NETWORK_POOLS) {
        let url = format!(
            "https://api.dexscreener.com/latest/dex/pairs/{ds_chain}/{pool:#x}"
        );
        report.network_calls += 1;
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(json) => {
                    if let Some(mut fetched) = parse_ds_pool(&json) {
                        for (addr, meta) in fetched.tokens.drain(..) {
                            if !meta.symbol.is_empty() || meta.name.is_some() {
                                upsert_token(conn, addr, &meta);
                            }
                            merge_token(token_meta.entry(addr).or_default(), meta);
                        }
                        upsert_pool(conn, pool, &fetched);
                        pool_meta.insert(pool, fetched);
                        report.pools_cached += 1;
                    }
                }
                Err(e) => tracing::debug!(error = %e, "dexscreener json"),
            },
            Ok(resp) if resp.status().as_u16() == 429 => {
                tracing::warn!("dexscreener rate limited; stopping pool metadata early");
                break;
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "dexscreener request"),
        }
        tokio::time::sleep(Duration::from_millis(FETCH_GAP_MS)).await;
    }

    // 2) Tokens still missing name/icon via GeckoTerminal, then CoinGecko.
    let gt_net = gt_network(chain);
    let mut missing_tokens: Vec<Address> = tokens
        .iter()
        .copied()
        .filter(|a| {
            token_meta
                .get(a)
                .is_none_or(|m| {
                    m.symbol.is_empty() || m.name.is_none() || m.icon_url.is_none()
                })
        })
        .collect();
    missing_tokens.sort_unstable_by_key(|a| format!("{a:#x}"));
    for addr in missing_tokens.into_iter().take(MAX_NETWORK_TOKENS) {
        let url = format!(
            "https://api.geckoterminal.com/api/v2/networks/{gt_net}/tokens/{addr:#x}"
        );
        report.network_calls += 1;
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(json) => {
                    if let Some(fetched) = parse_gt_token(&json) {
                        upsert_token(conn, addr, &fetched);
                        merge_token(token_meta.entry(addr).or_default(), fetched);
                        report.tokens_cached += 1;
                    }
                }
                Err(e) => tracing::debug!(error = %e, "geckoterminal json"),
            },
            Ok(resp) if resp.status().as_u16() == 429 => {
                tracing::warn!("geckoterminal rate limited; stopping token metadata early");
                break;
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "geckoterminal request"),
        }
        tokio::time::sleep(Duration::from_millis(FETCH_GAP_MS)).await;
    }

    // 3) Decimals for humanized amounts: DefiLlama coins batch (symbol too).
    let prefix = crate::cache::llama_chain_prefix(chain);
    let mut need_dec: Vec<Address> = tokens
        .iter()
        .copied()
        .filter(|a| token_meta.get(a).is_none_or(|m| m.decimals.is_none()))
        .collect();
    need_dec.sort_unstable_by_key(|a| format!("{a:#x}"));
    for chunk in need_dec.chunks(LLAMA_BATCH) {
        let keys: Vec<String> = chunk.iter().map(|a| format!("{prefix}:{a:#x}")).collect();
        let url = format!(
            "https://coins.llama.fi/prices/current/{}",
            crate::utils::urlencode(&keys.join(","))
        );
        report.network_calls += 1;
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(json) = resp.json::<Value>().await {
                    for (addr, fetched) in
                        crate::cache::token_meta::parse_llama_coins_response(&json, prefix)
                    {
                        let meta = TokenMeta {
                            symbol: fetched.symbol,
                            name: None,
                            decimals: fetched.decimals.map(i64::from),
                            icon_url: None,
                        };
                        upsert_token(conn, addr, &meta);
                        merge_token(token_meta.entry(addr).or_default(), meta);
                    }
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "llama decimals request"),
        }
        tokio::time::sleep(Duration::from_millis(FETCH_GAP_MS)).await;
    }

    // 4) CoinGecko contract endpoint fills name/icon for what is still missing.
    let platform = crate::cache::coingecko_platform_id(chain);
    let mut still: Vec<Address> = tokens
        .iter()
        .copied()
        .filter(|a| {
            token_meta
                .get(a)
                .is_none_or(|m| m.name.is_none() || m.icon_url.is_none())
        })
        .collect();
    still.sort_unstable_by_key(|a| format!("{a:#x}"));
    for addr in still.into_iter().take(MAX_COINGECKO_TOKENS) {
        let url =
            format!("https://api.coingecko.com/api/v3/coins/{platform}/contract/{addr:#x}");
        report.network_calls += 1;
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(json) = resp.json::<Value>().await {
                    if let Some(c) =
                        crate::cache::token_meta::parse_coingecko_contract_response(&json)
                    {
                        let meta = TokenMeta {
                            symbol: c.symbol,
                            name: c.name,
                            decimals: None,
                            icon_url: c.icon_url,
                        };
                        upsert_token(conn, addr, &meta);
                        merge_token(token_meta.entry(addr).or_default(), meta);
                        report.tokens_cached += 1;
                    }
                }
            }
            Ok(resp) if resp.status().as_u16() == 429 => {
                tracing::warn!("coingecko rate limited; stopping token metadata early");
                break;
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "coingecko request"),
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
    }
    Ok(())
}

async fn fetch_icons(
    conn: &Connection,
    tokens: &HashSet<Address>,
    token_meta: &HashMap<Address, TokenMeta>,
    icons: &mut HashMap<Address, String>,
    report: &mut EnrichReport,
) {
    let client = http_client(NET_TIMEOUT);
    let mut targets: Vec<Address> = tokens
        .iter()
        .copied()
        .filter(|a| !icons.contains_key(a))
        .filter(|a| token_meta.get(a).is_some_and(|m| m.icon_url.is_some()))
        .collect();
    targets.sort_unstable_by_key(|a| format!("{a:#x}"));
    for addr in targets.into_iter().take(MAX_EMBEDDED_ICONS) {
        let Some(url) = token_meta.get(&addr).and_then(|m| m.icon_url.as_deref()) else {
            continue;
        };
        let url = thumb_icon_url(url);
        report.network_calls += 1;
        let Ok(resp) = client.get(&url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.starts_with("image/"))
            .unwrap_or("image/png")
            .to_string();
        let Ok(bytes) = resp.bytes().await else {
            continue;
        };
        if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
            continue;
        }
        if let Err(e) = conn.execute(
            "INSERT OR REPLACE INTO token_icon(address, mime, data, fetched_at) VALUES(?1, ?2, ?3, ?4)",
            params![
                addr.as_slice(),
                mime,
                bytes.as_ref(),
                crate::utils::epoch_secs() as i64
            ],
        ) {
            tracing::debug!(error = %e, "token_icon cache write");
            continue;
        }
        icons.insert(addr, data_uri(&mime, &bytes));
        report.icons_cached += 1;
    }
}

fn merge_token(dst: &mut TokenMeta, src: TokenMeta) {
    if !src.symbol.is_empty() {
        dst.symbol = src.symbol;
    }
    if src.name.is_some() {
        dst.name = src.name;
    }
    if src.decimals.is_some() {
        dst.decimals = src.decimals;
    }
    if src.icon_url.is_some() {
        dst.icon_url = src.icon_url;
    }
}

fn upsert_token(conn: &Connection, addr: Address, meta: &TokenMeta) {
    let _ = conn.execute(
        "INSERT INTO token_symbols(address, symbol, decimals, name, icon_url)
         VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(address) DO UPDATE SET
           symbol   = CASE WHEN excluded.symbol <> '' THEN excluded.symbol ELSE token_symbols.symbol END,
           decimals = COALESCE(excluded.decimals, token_symbols.decimals),
           name     = COALESCE(excluded.name, token_symbols.name),
           icon_url = COALESCE(excluded.icon_url, token_symbols.icon_url)",
        params![
            addr.as_slice(),
            &meta.symbol,
            meta.decimals,
            &meta.name,
            &meta.icon_url
        ],
    );
}

fn upsert_pool(conn: &Connection, pool: Address, meta: &PoolMeta) {
    let _ = conn.execute(
        "INSERT OR REPLACE INTO pool_dex(pool, dex_id, dex_name, label, url, fetched_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            pool.as_slice(),
            &meta.dex_id,
            &meta.dex_name,
            &meta.label,
            &meta.url,
            crate::utils::epoch_secs() as i64
        ],
    );
}

fn parse_gt_token(json: &Value) -> Option<TokenMeta> {
    let a = json.get("data")?.get("attributes")?;
    let symbol = a.get("symbol")?.as_str()?.trim();
    if symbol.is_empty() {
        return None;
    }
    let decimals = a
        .get("decimals")
        .and_then(|d| d.as_i64().or_else(|| d.as_u64().map(|v| v as i64)));
    Some(TokenMeta {
        symbol: symbol.to_string(),
        name: a
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        decimals,
        icon_url: a
            .get("image_url")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string),
    })
}

fn parse_ds_pool(json: &Value) -> Option<PoolMeta> {
    let pair = json.get("pairs")?.as_array()?.first()?;
    let dex_id = pair.get("dexId")?.as_str()?.trim();
    if dex_id.is_empty() {
        return None;
    }
    let label = pair
        .get("labels")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut tokens = Vec::new();
    for side in ["baseToken", "quoteToken"] {
        let Some(t) = pair.get(side) else { continue };
        let Some(addr) = t.get("address").and_then(|v| v.as_str()).and_then(parse_addr) else {
            continue;
        };
        let symbol = t
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let name = t
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        if symbol.is_empty() && name.is_none() {
            continue;
        }
        tokens.push((
            addr,
            TokenMeta {
                symbol,
                name,
                decimals: None,
                icon_url: None,
            },
        ));
    }
    Some(PoolMeta {
        dex_id: dex_id.to_ascii_lowercase(),
        dex_name: dex_display_name(dex_id),
        label,
        url: pair
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        tokens,
    })
}

pub(crate) fn dex_display_name(id: &str) -> String {
    let key = id.to_ascii_lowercase().replace(['_', '-'], "");
    let known = match key.as_str() {
        "traderjoe" => "Trader Joe",
        "pharaoh" => "Pharaoh",
        "blackhole" => "Blackhole",
        "pangolin" => "Pangolin",
        "uniswap" => "Uniswap",
        "curve" => "Curve",
        "balancer" => "Balancer",
        "sushiswap" => "SushiSwap",
        "solidly" => "Solidly",
        "camelot" => "Camelot",
        "woofi" => "WooFi",
        "glowswap" => "GlowSwap",
        "dexalot" => "Dexalot",
        "pancakeswap" => "PancakeSwap",
        "velodrome" => "Velodrome",
        "aerodrome" => "Aerodrome",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => id.to_string(),
    }
}

fn thumb_icon_url(url: &str) -> String {
    url.replace("/large/", "/thumb/")
        .replace("/standard/", "/thumb/")
}

fn gt_network(chain: ChainName) -> &'static str {
    match chain {
        ChainName::Ethereum => "eth",
        ChainName::Polygon => "polygon_pos",
        ChainName::Bsc => "bsc",
        ChainName::Arbitrum => "arbitrum",
        ChainName::Base => "base",
        ChainName::Optimism => "optimism",
        ChainName::Avalanche => "avax",
    }
}

fn dexscreener_chain(chain: ChainName) -> &'static str {
    match chain {
        ChainName::Ethereum => "ethereum",
        ChainName::Polygon => "polygon",
        ChainName::Bsc => "bsc",
        ChainName::Arbitrum => "arbitrum",
        ChainName::Base => "base",
        ChainName::Optimism => "optimism",
        ChainName::Avalanche => "avalanche",
    }
}

/// Receipt-only inferences that need the full tx bundle (not stored on the op):
/// AMM flash-swap funding and transfer-tax route gaps.
fn infer_route_insights(snapshot: &mut DashboardSnapshot) {
    for ev in &mut snapshot.panels.evidence {
        if ev.kind != "arb_atomic" || ev.tx_bundle.is_none() {
            continue;
        }
        let funding = ev
            .tx_bundle
            .as_ref()
            .and_then(|b| infer_funding_mechanism(b, &ev.details));
        let tax = ev.tx_bundle.as_ref().and_then(infer_transfer_tax);
        if let Some(funding) = funding {
            if ev.details.is_object() {
                ev.details["funding_mechanism"] = Value::String(funding.to_string());
            }
            push_tag(ev, funding);
        }
        if let Some(tax) = tax {
            if ev.details.is_object() {
                ev.details["transfer_tax"] = tax;
            }
            push_tag(ev, "transfer_tax");
        }
    }
}

fn push_tag(ev: &mut super::evidence::OpEvidence, tag: &str) {
    if !ev.tags.iter().any(|t| t == tag) {
        ev.tags.push(tag.to_string());
    }
    if let Some(list) = ev.details.get_mut("tags").and_then(|v| v.as_array_mut()) {
        if !list.iter().any(|v| v.as_str() == Some(tag)) {
            list.push(Value::String(tag.to_string()));
        }
    }
}

struct Xfer {
    log_index: u64,
    token: Address,
    from: Address,
    to: Address,
    amount: u128,
}

fn bundle_transfers(bundle: &Value) -> Vec<Xfer> {
    let Some(arr) = bundle.get("token_transfers").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<Xfer> = arr
        .iter()
        .filter_map(|t| {
            Some(Xfer {
                log_index: t.get("log_index")?.as_u64()?,
                token: parse_addr(t.get("token")?.as_str()?)?,
                from: parse_addr(t.get("from")?.as_str()?)?,
                to: parse_addr(t.get("to")?.as_str()?)?,
                amount: t.get("amount_raw")?.as_str()?.parse().ok()?,
            })
        })
        .collect();
    out.sort_by_key(|t| t.log_index);
    out
}

struct SwapLegLite {
    log_index: u64,
    pool: Address,
    amm: String,
    token_in: Address,
    token_out: Address,
    amount_out: u128,
}

fn bundle_swaps(bundle: &Value) -> Vec<SwapLegLite> {
    let Some(arr) = bundle.get("swap_events").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<SwapLegLite> = arr
        .iter()
        .filter_map(|s| {
            Some(SwapLegLite {
                log_index: s.get("log_index")?.as_u64()?,
                pool: parse_addr(s.get("pool")?.as_str()?)?,
                amm: s.get("amm")?.as_str()?.to_string(),
                token_in: parse_addr(s.get("token_in")?.as_str()?)?,
                token_out: parse_addr(s.get("token_out")?.as_str()?)?,
                amount_out: s.get("amount_out_raw")?.as_str()?.parse().ok()?,
            })
        })
        .collect();
    out.sort_by_key(|s| s.log_index);
    out
}

/// A V3/V4-style pool that pays its output before any same-tx input reaches it
/// is funding the cycle (AMM flash swap), not a pre-funded searcher route.
fn infer_funding_mechanism(bundle: &Value, details: &Value) -> Option<&'static str> {
    let flash_loan = details
        .pointer("/arb_meta/flashloan_funded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if flash_loan {
        return Some("flash_loan");
    }
    let transfers = bundle_transfers(bundle);
    let swaps = bundle_swaps(bundle);
    let first = transfers.first()?;
    if first.amount == 0 {
        return None;
    }
    let swap = swaps.iter().find(|s| {
        s.pool == first.from
            && s.token_out == first.token
            && matches!(s.amm.as_str(), "v3" | "v4" | "infinity")
    })?;
    let paid_before = transfers
        .iter()
        .any(|t| t.to == swap.pool && t.token == swap.token_in && t.log_index < first.log_index);
    (!paid_before).then_some("amm_flash_swap_inferred")
}

/// Adjacent route legs that hand a token pool→pool, minus a third-party cut,
/// expose a transfer tax / reflection fee on that token.
fn infer_transfer_tax(bundle: &Value) -> Option<Value> {
    let transfers = bundle_transfers(bundle);
    let swaps = bundle_swaps(bundle);
    for (i, a) in swaps.iter().enumerate() {
        let Some(b) = swaps.get(i + 1) else {
            break;
        };
        if a.token_out != b.token_in || a.amount_out == 0 {
            continue;
        }
        let delivered: u128 = transfers
            .iter()
            .filter(|t| t.from == a.pool && t.to == b.pool && t.token == a.token_out)
            .map(|t| t.amount)
            .sum();
        if delivered == 0 || delivered >= a.amount_out {
            continue;
        }
        let lost = a.amount_out - delivered;
        let third_party: u128 = transfers
            .iter()
            .filter(|t| {
                t.from == a.pool
                    && t.token == a.token_out
                    && t.to != b.pool
                    && t.to != Address::ZERO
            })
            .map(|t| t.amount)
            .sum();
        let tolerance = (lost / 100).max(1);
        if third_party.abs_diff(lost) > tolerance {
            continue;
        }
        // Round to nearest bps so a 1% tax prints as 1.00%, not 0.99%.
        let bps = (lost.saturating_mul(10_000) + a.amount_out / 2) / a.amount_out;
        let recipients: Vec<String> = transfers
            .iter()
            .filter(|t| t.from == a.pool && t.token == a.token_out && t.to != b.pool)
            .map(|t| format!("{:#x}", t.to))
            .collect();
        return Some(serde_json::json!({
            "token": format!("{:#x}", a.token_out),
            "bps": bps,
            "leg_index": i,
            "lost_amount": lost.to_string(),
            "recipients": recipients,
        }));
    }
    None
}

fn apply(
    snapshot: &mut DashboardSnapshot,
    tokens: &HashMap<Address, TokenMeta>,
    pools: &HashMap<Address, PoolMeta>,
    icons: &HashMap<Address, String>,
) {
    for ev in &mut snapshot.panels.evidence {
        for leg in &mut ev.route {
            apply_leg(leg, tokens, pools, icons);
        }
    }
}

fn apply_leg(
    leg: &mut RouteLeg,
    tokens: &HashMap<Address, TokenMeta>,
    pools: &HashMap<Address, PoolMeta>,
    icons: &HashMap<Address, String>,
) {
    if let Some(pool) = leg.pool.as_deref().and_then(parse_addr) {
        if let Some(meta) = pools.get(&pool) {
            leg.dex_id = Some(meta.dex_id.clone());
            leg.dex_name = Some(meta.dex_name.clone());
            leg.dex_url = meta.url.clone();
            if meta.label.is_some() {
                leg.pool_label = meta.label.clone();
            }
        }
    }
    let tin = leg.token_in.clone();
    fill_token(
        tin.as_deref(),
        tokens,
        icons,
        &mut leg.token_in_symbol,
        &mut leg.token_in_name,
        &mut leg.token_in_icon,
    );
    let tout = leg.token_out.clone();
    fill_token(
        tout.as_deref(),
        tokens,
        icons,
        &mut leg.token_out_symbol,
        &mut leg.token_out_name,
        &mut leg.token_out_icon,
    );
    if leg.amount_in_human.is_none() {
        leg.amount_in_human = humanize(
            leg.amount_in_net.as_deref().or(leg.amount_in.as_deref()),
            tin.as_deref().and_then(parse_addr).and_then(|a| tokens.get(&a)).and_then(|m| m.decimals),
        );
    }
    if leg.amount_out_human.is_none() {
        leg.amount_out_human = humanize(
            leg.amount_out_net.as_deref().or(leg.amount_out.as_deref()),
            tout.as_deref().and_then(parse_addr).and_then(|a| tokens.get(&a)).and_then(|m| m.decimals),
        );
    }
}

fn humanize(amount: Option<&str>, decimals: Option<i64>) -> Option<String> {
    let d = decimals.filter(|d| (0..=36).contains(d))?;
    let raw = amount?.parse::<f64>().ok()?;
    if !raw.is_finite() {
        return None;
    }
    Some(format!("{:.6}", raw / 10f64.powi(d as i32)))
}

fn fill_token(
    addr: Option<&str>,
    tokens: &HashMap<Address, TokenMeta>,
    icons: &HashMap<Address, String>,
    symbol: &mut Option<String>,
    name: &mut Option<String>,
    icon: &mut Option<String>,
) {
    let Some(a) = addr.and_then(parse_addr) else {
        return;
    };
    if let Some(meta) = tokens.get(&a) {
        if !meta.symbol.is_empty() {
            *symbol = Some(meta.symbol.clone());
        }
        if meta.name.is_some() {
            *name = meta.name.clone();
        }
        if icon.is_none() {
            *icon = meta.icon_url.clone();
        }
    }
    if let Some(data) = icons.get(&a) {
        *icon = Some(data.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gt_token_fixture_parses() {
        let v = json!({"data":{"attributes":{
            "name":"Bitcoin","symbol":"BTC.b","decimals":8,
            "image_url":"https://img/x.png"}}});
        let m = parse_gt_token(&v).expect("token");
        assert_eq!(m.symbol, "BTC.b");
        assert_eq!(m.name.as_deref(), Some("Bitcoin"));
        assert_eq!(m.decimals, Some(8));
        assert!(m.icon_url.is_some());
    }

    #[test]
    fn dexscreener_pool_fixture_parses() {
        let v = json!({"pairs":[{"dexId":"traderjoe","labels":["v2.2"]}]});
        let p = parse_ds_pool(&v).expect("pool");
        assert_eq!(p.dex_id, "traderjoe");
        assert_eq!(p.dex_name, "Trader Joe");
        assert_eq!(p.label.as_deref(), Some("v2.2"));
    }

    #[test]
    fn dex_names_and_thumb() {
        assert_eq!(dex_display_name("pharaoh"), "Pharaoh");
        assert_eq!(dex_display_name("some_dex"), "Somedex");
        assert_eq!(
            thumb_icon_url("https://img/coins/1/large/a.png"),
            "https://img/coins/1/thumb/a.png"
        );
    }

    /// Receipt bundle of tx 0x5e8147…f212 (op 84): a V3 leg pays BTC.b before
    /// its WAVAX input arrives, and HEFE loses 1% to a fee wallet between the
    /// Pharaoh and LFJ legs.
    fn op84_bundle() -> Value {
        let btc = "0x152b9d0fdc40c096757f570a51e494bd4b943e50";
        let hefe = "0x18e3605b13f10016901eac609b9e188cf7c18973";
        let wavax = "0xb31f66aa3c1e785363f0875a1b74e27b85fd66c7";
        let p1 = "0x9d4e156065c395b0819df9a22803ac18816ad56a";
        let p2 = "0xe11e871d312cc9bae9eb24c7bdc2031f8453bf44";
        let p3 = "0x8fef4fe4970a5d6bfa7c65871a2ebfd0f42aa822";
        json!({
            "token_transfers": [
                {"log_index":0,"token":btc,"from":p3,"to":p1,"amount_raw":"80056"},
                {"log_index":1,"token":hefe,"from":p1,"to":"0x3cf34808f8590934b98fc5dc075d2267ab0627c5","amount_raw":"1570945013639596078874"},
                {"log_index":2,"token":hefe,"from":p1,"to":p2,"amount_raw":"155523556350320011808608"},
                {"log_index":5,"token":wavax,"from":p2,"to":"0x777e0fc6d730cc42b853dc66e9fa1d8700000000","amount_raw":"7499212146699552016"},
                {"log_index":8,"token":wavax,"from":"0x777e0fc6d730cc42b853dc66e9fa1d8700000000","to":p3,"amount_raw":"7233408547818569728"}
            ],
            "swap_events": [
                {"log_index":4,"pool":p1,"amm":"v2","token_in":btc,"token_out":hefe,"amount_out_raw":"157094501363959607887482"},
                {"log_index":7,"pool":p2,"amm":"v2","token_in":hefe,"token_out":wavax,"amount_out_raw":"7499212146699552016"},
                {"log_index":10,"pool":p3,"amm":"v3","token_in":wavax,"token_out":btc,"amount_out_raw":"80056"}
            ]
        })
    }

    #[test]
    fn op84_infers_amm_flash_swap_and_one_percent_tax() {
        let bundle = op84_bundle();
        let details = json!({"arb_meta":{"flashloan_funded":false}});
        assert_eq!(
            infer_funding_mechanism(&bundle, &details),
            Some("amm_flash_swap_inferred")
        );
        let tax = infer_transfer_tax(&bundle).expect("tax");
        assert_eq!(tax["bps"], 100);
        assert_eq!(tax["leg_index"], 0);
        assert_eq!(
            tax["token"],
            "0x18e3605b13f10016901eac609b9e188cf7c18973"
        );
    }

    #[test]
    fn leg_metadata_applies() {
        let pool = "0x73f7212838692e560f5b26fdb05cf0aa6cf56f33";
        let tok = "0x152b9d0fdc40c096757f570a51e494bd4b943e50";
        let mut leg = RouteLeg {
            pool: Some(pool.into()),
            token_in: Some(tok.into()),
            token_out: Some(tok.into()),
            amm: Some("v2".into()),
            ..RouteLeg::default()
        };
        let mut tokens = HashMap::new();
        tokens.insert(
            parse_addr(tok).expect("addr"),
            TokenMeta {
                symbol: "BTC.b".into(),
                name: Some("Bitcoin".into()),
                decimals: Some(8),
                icon_url: Some("https://img/x.png".into()),
            },
        );
        let mut pools = HashMap::new();
        pools.insert(
            parse_addr(pool).expect("addr"),
            PoolMeta {
                dex_id: "pharaoh".into(),
                dex_name: "Pharaoh".into(),
                label: Some("v2".into()),
                url: Some("https://dexscreener.com/avalanche/0x73".into()),
                tokens: Vec::new(),
            },
        );
        let mut icons = HashMap::new();
        icons.insert(parse_addr(tok).expect("addr"), "data:image/png;base64,AA==".into());
        apply_leg(&mut leg, &tokens, &pools, &icons);
        assert_eq!(leg.dex_name.as_deref(), Some("Pharaoh"));
        assert_eq!(leg.pool_label.as_deref(), Some("v2"));
        assert_eq!(leg.token_in_symbol.as_deref(), Some("BTC.b"));
        assert_eq!(leg.token_in_name.as_deref(), Some("Bitcoin"));
        assert_eq!(leg.token_in_icon.as_deref(), Some("data:image/png;base64,AA=="));
    }
}
