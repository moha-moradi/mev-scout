//! Live attribution tagging shared with the explorer.
//!
//! Live detectors answer "what could be made"; these helpers stamp the same
//! sub-tags the explorer attaches to realized `arb_atomic` rows, so the two
//! halves cannot drift. Route conditions are delegated to
//! [`crate::explorer::strategy_tags::arb_strategy_tags`] over a synthesized
//! [`SwapFact`] route.
use alloy::primitives::{Address, U256};

use crate::data::ExecutedLog;
use crate::explorer::classify::is_long_tail_arb;
use crate::explorer::decode::{decode_epoch_reward, decode_gmx_event, decode_rate_cache_update};
use crate::explorer::strategy_tags::{arb_strategy_tags, ArbTagOpts};
use crate::explorer::types::{Amm, LegSource, RateCacheFact, SwapFact};
use crate::paper::sim::pools_of_opp;
use crate::pool::state::{PoolManager, PoolState};
use crate::types::{tags, MevOpportunity, SavaxDirection, SavaxRoute, Strategy};

/// Chain registries and route attributes used to tag live opportunities.
#[derive(Debug, Clone, Default)]
pub struct ArbTagPolicy {
    /// Stable majors + wrapped native (explorer `profit_policy.priority`).
    pub blue_chips: Vec<Address>,
    pub wrapped_native: Address,
    pub savax: Option<Address>,
    /// Pharaoh / Blackhole pools (registry `epoch_venue`).
    pub epoch_venue_pools: Vec<Address>,
    /// GMX V2 market index/long/short tokens.
    pub gmx_market_tokens: Vec<Address>,
    /// GMX V2 EventEmitter allowlist.
    pub gmx_event_emitters: Vec<Address>,
    /// Every current arb exec path borrows through a flash loan.
    pub flash_financed: bool,
    /// Funding-layer provenance from the configured flash provider. Flash
    /// loans are a funding axis, never a separate arbitrage family (A-01).
    pub funding_source: Option<String>,
    /// Flash-funding vault (Balancer). A route pool equal to this address is
    /// reported as a reentrancy risk instead of being silently assumed safe.
    pub funding_vault: Option<Address>,
    /// sAVAX exchange rate (`getPooledAvaxByShares(1e18)`), refreshed per pass.
    pub savax_rate_wad: Option<U256>,
}

impl ArbTagPolicy {
    fn is_epoch_venue(&self, pool: &Address) -> bool {
        self.epoch_venue_pools.contains(pool)
    }

    fn touches_gmx_market(&self, tokens: &[Address]) -> bool {
        tokens
            .iter()
            .any(|t| !t.is_zero() && self.gmx_market_tokens.contains(t))
    }
}

/// Block-wide facts collected from receipt logs, mirroring the explorer's
/// block context for `epoch_transition`, `gmx_adl_arb`, `rate_provider_staleness`
/// and `savax_rate_arb`.
#[derive(Debug, Clone, Default)]
pub struct BlockTagFacts {
    pub epoch_reward_tokens: Vec<Address>,
    pub rate_cache_updates: Vec<RateCacheFact>,
    pub gmx_signal_tx: Option<usize>,
}

/// Collect the tag-relevant signals emitted by one transaction's logs.
pub fn collect_tx_facts(
    logs: &[ExecutedLog],
    tx_index: usize,
    policy: &ArbTagPolicy,
    facts: &mut BlockTagFacts,
) {
    for log in logs {
        let log_data = log.into();
        if let Some(mut update) = decode_rate_cache_update(&log_data) {
            update.tx_index = tx_index as u64;
            facts.rate_cache_updates.push(update);
        }
        if let Some(reward) = decode_epoch_reward(&log_data) {
            if !reward.reward_token.is_zero()
                && !reward.amount.is_zero()
                && !facts.epoch_reward_tokens.contains(&reward.reward_token)
            {
                facts.epoch_reward_tokens.push(reward.reward_token);
            }
        }
        if policy.gmx_event_emitters.contains(&log_data.address)
            && decode_gmx_event(&log_data).is_some()
        {
            facts.gmx_signal_tx = Some(
                facts
                    .gmx_signal_tx
                    .map_or(tx_index, |prev| prev.min(tx_index)),
            );
        }
    }
}

/// Exact token sequence of a candidate route.
fn route_tokens(opp: &MevOpportunity, pools: &[Address], pm: &PoolManager) -> Option<Vec<Address>> {
    if let Some(tokens) = &opp.route_tokens {
        if tokens.len() == pools.len() + 1 {
            return Some(tokens.clone());
        }
    }
    let mut tokens = Vec::with_capacity(pools.len() + 1);
    tokens.push(opp.token_in);
    let mut current = opp.token_in;
    for addr in pools {
        let pool = pm.get(addr)?;
        current = pool.walk_next_token(current)?;
        tokens.push(current);
    }
    Some(tokens)
}

/// Honest venue identity from the registry's `dex_name` (C-04). A generic
/// `dex_type` such as `UniswapV2` covers many forks, so only an explicit,
/// recognised DEX name is promoted to a venue tag — a missing/unknown name
/// stays untagged rather than making a false venue claim.
fn venue_tag_for_dex_name(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    if n.contains("pharaoh") && n.contains("dlmm") {
        Some(tags::VENUE_PHARAOH_DLMM)
    } else if n.contains("lfj") || n.contains("liquidity book") || n.contains("trader joe") {
        Some(tags::VENUE_LFJ_V2)
    } else if n.contains("pangolin") {
        Some(tags::VENUE_PANGOLIN_V2)
    } else if n.contains("sushi") {
        Some(tags::VENUE_SUSHI_V2)
    } else {
        None
    }
}

fn amm_for(pool: &PoolState) -> Amm {
    use crate::dex_type::DexType;
    let dex = pool.info().dex_type;
    if matches!(dex, DexType::Solidly | DexType::Camelot) {
        return Amm::Solidly;
    }
    match pool {
        PoolState::UniswapV2(_) => Amm::V2,
        PoolState::UniswapV3(_) => Amm::V3,
        PoolState::UniswapV4(_) => Amm::V4,
        PoolState::PancakeInfinity(_) => Amm::Infinity,
        PoolState::Curve(_) => Amm::Curve,
        PoolState::Balancer(_) => Amm::Balancer,
        PoolState::TraderJoeLB(_) => Amm::Lb,
        PoolState::Pendle(_) => Amm::Pendle,
        PoolState::Metric(_) => Amm::Metric,
        PoolState::Fluid(_) => Amm::Fluid,
    }
}

/// Synthesize the explorer's `SwapFact` view of a candidate route.
///
/// Amounts are irrelevant to every tag except the sAVAX divergence probe, so
/// non-sAVAX legs carry a 1-unit placeholder. sAVAX legs are quoted at a
/// 1e18-share probe so the tag measures spot, not price impact.
pub fn synth_route_swaps(
    opp: &MevOpportunity,
    pm: &PoolManager,
    policy: &ArbTagPolicy,
) -> Option<Vec<SwapFact>> {
    let pools = pools_of_opp(opp);
    if pools.is_empty() {
        return None;
    }
    let tokens = route_tokens(opp, &pools, pm)?;
    let mut swaps = Vec::with_capacity(pools.len());
    for (i, pool_addr) in pools.iter().enumerate() {
        let pool = pm.get(pool_addr)?;
        let token_in = *tokens.get(i)?;
        let token_out = *tokens.get(i + 1)?;
        let savax_pair = policy.savax.is_some_and(|savax| {
            (token_in == savax && token_out == policy.wrapped_native)
                || (token_out == savax && token_in == policy.wrapped_native)
        });
        let (amount_in, amount_out) = if savax_pair {
            let probe = 10u128.pow(18);
            let out = pool.quote_dir(probe, token_in, token_out).unwrap_or(0);
            (U256::from(probe), U256::from(out))
        } else {
            (U256::from(1u64), U256::from(1u64))
        };
        swaps.push(SwapFact {
            tx_index: opp.tx_index as u64,
            log_index: 0,
            pool: *pool_addr,
            amm: amm_for(pool),
            token_in,
            token_out,
            amount_in,
            amount_out,
            token_source: LegSource::Registry,
            tick: None,
            owner: None,
        });
    }
    Some(swaps)
}

/// Per-leg evidence for venue-specific tags: Curve balances/A, Balancer rate
/// cache, LB bin state, Pendle reserves. Amounts are strings so large u128
/// values survive JSON without precision loss.
fn route_evidence(swaps: &[SwapFact], pm: &PoolManager) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for swap in swaps {
        let Some(state) = pm.get(&swap.pool) else {
            continue;
        };
        let entry = match state {
            PoolState::Curve(curve) => serde_json::json!({
                "venue": "curve",
                "pool": format!("{:#x}", swap.pool),
                "balances": curve.balances.iter().map(u128::to_string).collect::<Vec<_>>(),
                "a": curve.a_coeff,
            }),
            PoolState::Balancer(balancer) => {
                let rates: Vec<serde_json::Value> = balancer
                    .rate_cache
                    .iter()
                    .enumerate()
                    .filter_map(|(i, entry)| {
                        entry.as_ref().map(|e| {
                            serde_json::json!({
                                "token_index": i,
                                "rate_wad": e.rate_wad.to_string(),
                                "block": e.block,
                            })
                        })
                    })
                    .collect();
                serde_json::json!({
                    "venue": "balancer",
                    "pool": format!("{:#x}", swap.pool),
                    "rates": rates,
                })
            }
            PoolState::TraderJoeLB(lb) => serde_json::json!({
                "venue": "lb",
                "pool": format!("{:#x}", swap.pool),
                "bin_step": lb.bin_step,
                "reserve_x": lb.reserve_x.to_string(),
                "reserve_y": lb.reserve_y.to_string(),
            }),
            PoolState::Pendle(pendle) => serde_json::json!({
                "venue": "pendle",
                "pool": format!("{:#x}", swap.pool),
                "total_pt": pendle.total_pt.to_string(),
                "total_sy": pendle.total_sy.to_string(),
            }),
            _ => continue,
        };
        out.push(entry);
    }
    out
}

/// Stamp every live attribution tag on one opportunity.
pub fn stamp_route_tags(
    opp: &mut MevOpportunity,
    policy: &ArbTagPolicy,
    facts: &BlockTagFacts,
    pm: &PoolManager,
) {
    if opp.strategy == Strategy::Jit {
        return;
    }
    if opp.strategy == Strategy::Backrun {
        opp.add_tag(tags::BACKRUN);
    }
    if policy.flash_financed {
        // Keep the legacy alias for dashboards and add namespaced funding
        // provenance without renaming any persisted tag value.
        opp.add_tag(tags::FLASH_ARB);
        if let Some(source) = policy.funding_source.as_deref() {
            opp.add_tag(tags::funding_tag_for_source(source));
            if opp.funding_source.is_none() {
                opp.funding_source = Some(source.to_string());
            }
        }
    }

    if let Some(swaps) = synth_route_swaps(opp, pm, policy) {
        // A-02: the funding vault must not also be a route pool without an
        // explicit risk signal — the callback re-enters the lending contract.
        if let Some(vault) = policy.funding_vault {
            if swaps.iter().any(|s| s.pool == vault)
                && !opp
                    .risk_flags
                    .iter()
                    .any(|f| f == tags::RISK_FUNDING_REENTRANCY)
            {
                opp.risk_flags
                    .push(tags::RISK_FUNDING_REENTRANCY.to_string());
            }
        }

        // Dynamic FoT: measured bps are allowed through and tagged; static
        // registry tokens are still excluded by `excludes_taxed_route`.
        let fot_bps = swaps
            .iter()
            .flat_map(|s| [s.token_in, s.token_out])
            .filter_map(|t| pm.tax_bps(&t))
            .max()
            .unwrap_or(0);
        if fot_bps > 0 {
            opp.add_tag(tags::FOT_ARB);
        }
        if is_long_tail_arb(&swaps, &policy.blue_chips) {
            opp.add_tag(tags::LONG_TAIL);
        }

        let updates: Vec<&RateCacheFact> = facts.rate_cache_updates.iter().collect();
        let route_has_epoch_venue = swaps.iter().any(|s| policy.is_epoch_venue(&s.pool));
        let gmx_signal = facts
            .gmx_signal_tx
            .is_some_and(|signal_tx| signal_tx <= opp.tx_index)
            && policy.touches_gmx_market(
                &opp.route_tokens
                    .clone()
                    .unwrap_or_else(|| vec![opp.token_in, opp.token_out]),
            );
        let opts = ArbTagOpts {
            wrapped_native: policy.wrapped_native,
            savax: policy.savax,
            savax_exchange_rate_wad: policy.savax_rate_wad,
            epoch_reward_tokens: &facts.epoch_reward_tokens,
            epoch_venue_pools: &policy.epoch_venue_pools,
            gmx_adl_signal: gmx_signal,
            rate_cache_updates: &updates,
        };
        for tag in arb_strategy_tags(&swaps, &opts) {
            opp.add_tag(tag);
        }

        // C-04: stamp honest venue identity from the registry `dex_name` when
        // it names a canonical Avalanche venue. Generic fork types stay
        // untagged rather than being mislabelled as a specific venue.
        for swap in &swaps {
            if let Some(name) = pm.get(&swap.pool).and_then(|p| p.info().dex_name.as_deref()) {
                if let Some(tag) = venue_tag_for_dex_name(name) {
                    opp.add_tag(tag);
                }
            }
        }

        // Evidence accumulate across tag families; start from any details a
        // detector already attached so stamping never destroys provenance.
        let mut details = serde_json::Map::new();

        // A-07/A-08/A-09: two-transaction mechanisms (GMX request + keeper)
        // and yield/rate legs (sAVAX, Pendle PT/YT) are outside core atomic
        // arb. Keep the forensic mechanism tag AND add the disposition tag;
        // core sim/exec/ranking gate on this tag (paper/ledger.rs). Evidence
        // records *why* the row was excluded rather than silently dropping it.
        let mut out_of_core_reasons = Vec::new();
        if opp.has_tag(tags::GMX_ADL_ARB) {
            out_of_core_reasons.push("gmx_two_tx");
        }
        if opp.has_tag(tags::SAVAX_RATE_ARB) {
            out_of_core_reasons.push("savax_rate");
        }
        if swaps.iter().any(|s| s.amm == Amm::Pendle) {
            out_of_core_reasons.push("pendle_yield");
        }
        if !out_of_core_reasons.is_empty() {
            opp.add_tag(tags::OUT_OF_CORE);
            details.insert("out_of_core".into(), serde_json::json!(out_of_core_reasons));
        }

        // Keep the block-signal evidence next to the tags (read-only provenance;
        // tag conditions themselves stay in the explorer helpers above).
        let evidence = route_evidence(&swaps, pm);
        if !evidence.is_empty() {
            details.insert("route_evidence".into(), serde_json::json!(evidence));
        }
        if !facts.epoch_reward_tokens.is_empty() {
            details.insert(
                "epoch_reward_tokens".into(),
                serde_json::json!(facts
                    .epoch_reward_tokens
                    .iter()
                    .map(|a| format!("{a:#x}"))
                    .collect::<Vec<_>>()),
            );
        }
        if let Some(tx) = facts.gmx_signal_tx {
            details.insert("gmx_signal_tx".into(), serde_json::json!(tx));
        }
        if !facts.rate_cache_updates.is_empty() {
            details.insert(
                "rate_cache_updates".into(),
                serde_json::json!(facts.rate_cache_updates.len()),
            );
        }
        if let Some(rate) = policy.savax_rate_wad {
            details.insert("savax_rate_wad".into(), serde_json::json!(rate.to_string()));
        }
        if route_has_epoch_venue {
            details.insert("epoch_venue_route".into(), serde_json::json!(true));
        }
        if fot_bps > 0 {
            details.insert("fot_tax_bps".into(), serde_json::json!(fot_bps));
        }
        if !details.is_empty() {
            // Merge, never replace: a detector may already have stamped route
            // or funding evidence into `details` before tagging ran.
            let merged = opp.details.get_or_insert_with(|| serde_json::json!({}));
            if let Some(obj) = merged.as_object_mut() {
                for (k, v) in details {
                    obj.insert(k, v);
                }
            }
        }
    }

    // sAVAX rows need the virtual leg descriptor for sim/exec, not just the tag.
    if opp.has_tag(tags::SAVAX_RATE_ARB) && opp.savax_route.is_none() {
        opp.savax_route = Some(build_savax_route(opp, policy, pm));
    }
}

/// Describe the virtual Benqi leg for a tagged sAVAX row.
fn build_savax_route(opp: &MevOpportunity, policy: &ArbTagPolicy, pm: &PoolManager) -> SavaxRoute {
    let savax = policy.savax.unwrap_or(Address::ZERO);
    let wrapped = policy.wrapped_native;
    let route = opp
        .route_tokens
        .clone()
        .unwrap_or_else(|| vec![opp.token_in, opp.token_out]);
    let savax_bought =
        route.first().is_some_and(|t| *t == wrapped) && route.iter().skip(1).any(|t| *t == savax);
    let direction = if savax_bought {
        SavaxDirection::Redeem
    } else {
        SavaxDirection::Mint
    };
    let pool = pools_of_opp(opp).first().copied().unwrap_or(Address::ZERO);
    let rate = policy.savax_rate_wad.unwrap_or(U256::ZERO);
    let borrowed = if opp.input_amount > U256::from(u128::MAX) {
        u128::MAX
    } else {
        opp.input_amount.to::<u128>()
    };
    let shares = match direction {
        SavaxDirection::Redeem => pm
            .get(&pool)
            .and_then(|p| p.quote_dir(borrowed, wrapped, savax))
            .map(U256::from)
            .unwrap_or(U256::ZERO),
        SavaxDirection::Mint => {
            if rate.is_zero() {
                U256::ZERO
            } else {
                opp.input_amount.saturating_mul(U256::from(10u64.pow(18))) / rate
            }
        }
    };
    let min_out_wad = match direction {
        SavaxDirection::Redeem => {
            if rate.is_zero() {
                U256::ZERO
            } else {
                shares.saturating_mul(rate) / U256::from(10u64.pow(18))
            }
        }
        SavaxDirection::Mint => shares,
    };
    SavaxRoute {
        pool,
        direction,
        rate_wad: rate,
        shares,
        min_out_wad,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex_type::DexType;
    use crate::pool::state::{PoolInfo, UniswapV2PoolState};
    use alloy::primitives::address;

    const USDC: Address = address!("4000000000000000000000000000000000000005");
    const WAVAX: Address = address!("b31f66aa3c1e785363f0875a1b74e27b85fd66c7");
    const TOKA: Address = address!("2000000000000000000000000000000000000002");
    const POOL_A: Address = address!("3000000000000000000000000000000000000003");
    const POOL_B: Address = address!("3000000000000000000000000000000000000004");

    fn v2_pool(addr: Address, t0: Address, t1: Address) -> PoolState {
        PoolState::UniswapV2(UniswapV2PoolState {
            info: PoolInfo {
                address: addr,
                token0: t0.min(t1),
                token1: t0.max(t1),
                fee: 30,
                dex_type: DexType::UniswapV2,
                ..Default::default()
            },
            reserve0: 1_000_000,
            reserve1: 1_000_000,
        })
    }

    fn manager() -> PoolManager {
        let mut pm = PoolManager::new();
        pm.add_pool(v2_pool(POOL_A, USDC, TOKA));
        pm.add_pool(v2_pool(POOL_B, TOKA, WAVAX));
        pm
    }

    fn route(_pm: &PoolManager, tokens: Vec<Address>) -> MevOpportunity {
        let mut opp = MevOpportunity::new(1, 0, Strategy::MultiHopArb, POOL_A, 0);
        opp.pool_b = POOL_B;
        opp.token_in = tokens[0];
        opp.token_out = *tokens.last().unwrap_or(&tokens[0]);
        opp.path = Some(vec![POOL_A, POOL_B]);
        opp.route_tokens = Some(tokens);
        opp.input_amount = U256::from(1_000u64);
        opp
    }

    fn policy() -> ArbTagPolicy {
        ArbTagPolicy {
            blue_chips: vec![WAVAX],
            wrapped_native: WAVAX,
            flash_financed: true,
            ..Default::default()
        }
    }

    #[test]
    fn route_tags_cover_flash_long_tail_and_backrun() {
        let pm = manager();
        let mut opp = route(&pm, vec![USDC, TOKA, WAVAX]);
        opp.strategy = Strategy::Backrun;
        stamp_route_tags(&mut opp, &policy(), &BlockTagFacts::default(), &pm);
        assert!(opp.has_tag(tags::FLASH_ARB));
        assert!(opp.has_tag(tags::LONG_TAIL));
        assert!(opp.has_tag(tags::BACKRUN));
    }

    #[test]
    fn funding_source_tag_and_reentrancy_risk_are_stamped() {
        let pm = manager();
        let mut opp = route(&pm, vec![USDC, TOKA, WAVAX]);
        let mut p = policy();
        p.funding_source = Some("balancer_v2_vault".into());
        p.funding_vault = Some(POOL_A);
        stamp_route_tags(&mut opp, &p, &BlockTagFacts::default(), &pm);
        assert!(opp.has_tag(tags::FUNDING_BALANCER_V2));
        assert_eq!(opp.funding_source.as_deref(), Some("balancer_v2_vault"));
        assert!(opp
            .risk_flags
            .iter()
            .any(|f| f == tags::RISK_FUNDING_REENTRANCY));
    }

    #[test]
    fn measured_fot_is_tagged_not_excluded() {
        let pm = {
            let mut pm = manager();
            pm.set_tax_bps(TOKA, 300);
            pm
        };
        let mut opp = route(&pm, vec![USDC, TOKA, WAVAX]);
        stamp_route_tags(&mut opp, &policy(), &BlockTagFacts::default(), &pm);
        assert!(opp.has_tag(tags::FOT_ARB));
        assert_eq!(
            opp.details
                .as_ref()
                .and_then(|d| d.get("fot_tax_bps"))
                .and_then(|v| v.as_u64()),
            Some(300)
        );
    }

    #[test]
    fn epoch_and_gmx_block_facts_stamp_cause_tags() {
        let pm = manager();
        let mut policy = policy();
        policy.epoch_venue_pools = vec![POOL_A, POOL_B];
        policy.gmx_market_tokens = vec![WAVAX];
        let facts = BlockTagFacts {
            epoch_reward_tokens: vec![USDC],
            gmx_signal_tx: Some(0),
            ..Default::default()
        };
        // USDC -> TOKA -> WAVAX touches the reward token and both epoch venues.
        let mut opp = route(&pm, vec![USDC, TOKA, WAVAX]);
        stamp_route_tags(&mut opp, &policy, &facts, &pm);
        assert!(opp.has_tag(tags::EPOCH_TRANSITION));
        assert!(opp.has_tag(tags::GMX_ADL_ARB));
    }

    /// A-07: a GMX signal keeps its forensic tag and adds the disposition tag,
    /// with the reason recorded. Pre-existing detector details are preserved.
    #[test]
    fn out_of_core_keeps_forensic_tag_and_merges_details() {
        let pm = manager();
        let mut policy = policy();
        policy.gmx_market_tokens = vec![WAVAX];
        let facts = BlockTagFacts {
            gmx_signal_tx: Some(0),
            ..Default::default()
        };
        let mut opp = route(&pm, vec![USDC, TOKA, WAVAX]);
        opp.details = Some(serde_json::json!({ "detector_evidence": 42 }));

        stamp_route_tags(&mut opp, &policy, &facts, &pm);

        assert!(opp.has_tag(tags::GMX_ADL_ARB));
        assert!(opp.has_tag(tags::OUT_OF_CORE));
        let d = opp.details.as_ref().expect("details");
        assert_eq!(d["detector_evidence"], 42, "existing evidence must survive");
        assert_eq!(d["out_of_core"][0], "gmx_two_tx");
    }

    /// C-04: venue tags come only from an explicit, recognised `dex_name`.
    #[test]
    fn venue_tag_mapping_is_explicit_and_conservative() {
        assert_eq!(
            venue_tag_for_dex_name("Pangolin"),
            Some(tags::VENUE_PANGOLIN_V2)
        );
        assert_eq!(
            venue_tag_for_dex_name("pangolin-v2"),
            Some(tags::VENUE_PANGOLIN_V2)
        );
        assert_eq!(
            venue_tag_for_dex_name("SushiSwap"),
            Some(tags::VENUE_SUSHI_V2)
        );
        assert_eq!(
            venue_tag_for_dex_name("Pharaoh DLMM"),
            Some(tags::VENUE_PHARAOH_DLMM)
        );
        assert_eq!(venue_tag_for_dex_name("LFJ"), Some(tags::VENUE_LFJ_V2));
        assert_eq!(venue_tag_for_dex_name("QuickSwap"), None);
        assert_eq!(venue_tag_for_dex_name(""), None);
    }

    #[test]
    fn venue_tag_is_stamped_from_named_pool() {
        let mut named = v2_pool(POOL_A, USDC, TOKA);
        if let PoolState::UniswapV2(s) = &mut named {
            s.info.dex_name = Some("SushiSwap".into());
        }
        let mut pm = PoolManager::new();
        pm.add_pool(named);
        pm.add_pool(v2_pool(POOL_B, TOKA, WAVAX));

        let mut opp = route(&pm, vec![USDC, TOKA, WAVAX]);
        stamp_route_tags(&mut opp, &policy(), &BlockTagFacts::default(), &pm);
        assert!(opp.has_tag(tags::VENUE_SUSHI_V2));
    }

    /// A regular core route must not be flagged out-of-core.
    #[test]
    fn core_route_is_not_tagged_out_of_core() {
        let pm = manager();
        let mut opp = route(&pm, vec![USDC, TOKA, WAVAX]);
        stamp_route_tags(&mut opp, &policy(), &BlockTagFacts::default(), &pm);
        assert!(!opp.has_tag(tags::OUT_OF_CORE));
    }

    /// Venue state used by the tags (Curve A/balances, Balancer rate cache) is
    /// persisted as evidence next to `details.tags`.
    #[test]
    fn route_evidence_records_curve_and_balancer_state() {
        use crate::pool::state::{
            BalancerPoolState, BalancerPoolVariant, CurvePoolState, CurvePoolVariant,
            RateCacheEntry,
        };
        use std::collections::HashMap;

        const POOL_CURVE: Address = address!("3000000000000000000000000000000000000011");
        const POOL_BAL: Address = address!("3000000000000000000000000000000000000012");

        let mut pm = manager();
        let mut curve_index = HashMap::new();
        curve_index.insert(USDC, 0usize);
        curve_index.insert(TOKA, 1usize);
        pm.add_pool(PoolState::Curve(CurvePoolState {
            info: PoolInfo {
                address: POOL_CURVE,
                token0: USDC,
                token1: TOKA,
                fee: 4,
                dex_type: DexType::Curve,
                ..Default::default()
            },
            balances: vec![1_000_000, 1_000_000],
            token_index: curve_index,
            a_coeff: 100,
            pool_variant: CurvePoolVariant::Plain,
            gamma: None,
            price_scale: vec![],
            base_pool: None,
        }));
        let mut bal_index = HashMap::new();
        bal_index.insert(TOKA, 0usize);
        bal_index.insert(WAVAX, 1usize);
        pm.add_pool(PoolState::Balancer(BalancerPoolState {
            info: PoolInfo {
                address: POOL_BAL,
                token0: TOKA,
                token1: WAVAX,
                fee: 30,
                dex_type: DexType::Balancer,
                ..Default::default()
            },
            balances: vec![1_000_000, 1_000_000],
            token_index: bal_index,
            pool_id: Some([7u8; 32]),
            weights: vec![],
            pool_variant: BalancerPoolVariant::Weighted,
            amplification: None,
            bpt_index: None,
            scaling_factors: vec![],
            rate_providers: vec![None, None],
            rate_cache: vec![
                Some(RateCacheEntry {
                    rate_wad: 10u128.pow(18),
                    block: 9,
                }),
                None,
            ],
        }));

        let mut opp = MevOpportunity::new(9, 0, Strategy::MultiHopArb, POOL_CURVE, 0);
        opp.pool_b = POOL_BAL;
        opp.token_in = USDC;
        opp.token_out = WAVAX;
        opp.path = Some(vec![POOL_CURVE, POOL_BAL]);
        opp.route_tokens = Some(vec![USDC, TOKA, WAVAX]);
        opp.input_amount = U256::from(1_000u64);

        stamp_route_tags(&mut opp, &policy(), &BlockTagFacts::default(), &pm);
        let evidence = opp
            .details
            .as_ref()
            .and_then(|d| d.get("route_evidence"))
            .and_then(|v| v.as_array())
            .expect("route evidence");
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0]["venue"], "curve");
        assert_eq!(evidence[0]["a"], 100);
        assert_eq!(evidence[1]["venue"], "balancer");
        assert_eq!(evidence[1]["rates"][0]["block"], 9);
    }

    /// Live stamping must be a superset of what the explorer classifier would
    /// emit for the same route/facts: every explorer tag is stamped, so the
    /// two halves cannot drift.
    #[test]
    fn stamp_covers_every_explorer_tag_for_identical_swaps() {
        use crate::explorer::strategy_tags::{arb_strategy_tags, ArbTagOpts};

        let pm = manager();
        let mut policy = policy();
        policy.epoch_venue_pools = vec![POOL_A, POOL_B];
        policy.gmx_market_tokens = vec![WAVAX];
        let facts = BlockTagFacts {
            epoch_reward_tokens: vec![USDC],
            gmx_signal_tx: Some(0),
            ..Default::default()
        };
        let mut opp = route(&pm, vec![USDC, TOKA, WAVAX]);

        let swaps = synth_route_swaps(&opp, &pm, &policy).expect("synth swaps");
        let updates: Vec<&crate::explorer::types::RateCacheFact> =
            facts.rate_cache_updates.iter().collect();
        let expected = arb_strategy_tags(
            &swaps,
            &ArbTagOpts {
                wrapped_native: policy.wrapped_native,
                savax: policy.savax,
                savax_exchange_rate_wad: policy.savax_rate_wad,
                epoch_reward_tokens: &facts.epoch_reward_tokens,
                epoch_venue_pools: &policy.epoch_venue_pools,
                gmx_adl_signal: true,
                rate_cache_updates: &updates,
            },
        );

        stamp_route_tags(&mut opp, &policy, &facts, &pm);
        for tag in expected {
            assert!(
                opp.has_tag(tag),
                "live stamping dropped explorer tag {tag}: {:?}",
                opp.tags
            );
        }
        assert!(opp
            .details
            .as_ref()
            .is_some_and(|d| d.get("route_evidence").is_none()));
    }
}
