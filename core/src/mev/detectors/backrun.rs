//! Backrun detection - finds opportunities that become profitable only after a victim transaction.
//!
//! Implements the state-differential approach: compare opportunities on state S_{i-1} (pre-victim)
//! versus S_i (post-victim) using the net-flip condition: net(pre) <= 0 and net(post) > 0.
//!
//! Key design (per docs/plan_backrun.md):
//! - The pre-image is captured on S_{i-1} by `pre_detect` and consumed by
//!   `post_detect` — never recomputed after the state update
//! - Fresh detector instances per transaction to avoid dedup contamination
//! - Pool-overlap is implied by will_touch ⊇ newly_dirty
//! - Normalized keys across two-hop and multi-hop families
//! - Precedence: backrun claims supersede plain arb claims for same path
//!   (`suppress_superseded_arbs`)

use crate::mev::detectors::{multi_hop::MultiHopArbDetector, two_hop::TwoHopArbDetector};
use crate::pool::state::{check_dedup_key, PoolManager, ScanScope};
use crate::types::{GasConfig, MevOpportunity, Strategy};
use alloy::primitives::{Address, U256};
use std::collections::{HashMap, HashSet};

/// Normalized key for cross-family dedup
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackrunKey {
    pub pool_a: Address,
    pub pool_b: Address,
    pub token_in: Address,
    pub token_out: Address,
}

impl BackrunKey {
    pub fn from_opp(opp: &MevOpportunity) -> Self {
        let (pool_a, pool_b) = if opp.pool_a < opp.pool_b {
            (opp.pool_a, opp.pool_b)
        } else {
            (opp.pool_b, opp.pool_a)
        };
        Self {
            pool_a,
            pool_b,
            token_in: opp.token_in,
            token_out: opp.token_out,
        }
    }

    pub fn to_check_key(&self) -> (Address, Address, Address, Address) {
        (self.pool_a, self.pool_b, self.token_in, self.token_out)
    }
}

/// D2 precedence: a backrun claim supersedes the plain arb claim for the same
/// key in the same block, so P&L is never double-counted.
///
/// Mirrors `explorer::classify`'s superseder (`core/src/explorer/classify.rs`,
/// kind precedence + `events.retain`). Must run **before**
/// `retain_with_rejections`, otherwise superseded rows are mis-recorded as
/// gas/min-profit rejections.
///
/// The key set is derived from the `Strategy::Backrun` rows already present in
/// `opps`; a block with no backrun claim is a no-op.
pub fn suppress_superseded_arbs(opps: &mut Vec<MevOpportunity>) {
    let backrun_keys: HashSet<(Address, Address, Address, Address)> = opps
        .iter()
        .filter(|o| o.strategy == Strategy::Backrun)
        .map(|o| BackrunKey::from_opp(o).to_check_key())
        .collect();
    if backrun_keys.is_empty() {
        return;
    }
    opps.retain(|o| {
        !(matches!(o.strategy, Strategy::TwoHopArb | Strategy::MultiHopArb)
            && backrun_keys.contains(&BackrunKey::from_opp(o).to_check_key()))
    });
}

/// Backrun detector that maintains pre/post detector instances
#[derive(Debug)]
pub struct BackrunDetector {
    block_number: u64,
    pre_two: TwoHopArbDetector,
    pre_multi: MultiHopArbDetector,
    post_two: TwoHopArbDetector,
    post_multi: MultiHopArbDetector,
    /// Cross-family dedup key state (shared across transactions of the block)
    seen: HashMap<(Address, Address, Address, Address), (u128, u128)>,
    /// Pre-image captured on S_{i-1} by `pre_detect`, consumed by `post_detect`.
    pre_image: Vec<MevOpportunity>,
    /// Transaction index `pre_image` was captured for.
    pre_image_tx: Option<usize>,
}

impl BackrunDetector {
    pub fn new(block_number: u64) -> Self {
        Self {
            block_number,
            pre_two: TwoHopArbDetector::new(block_number),
            pre_multi: MultiHopArbDetector::new(block_number),
            post_two: TwoHopArbDetector::new(block_number),
            post_multi: MultiHopArbDetector::new(block_number),
            seen: HashMap::new(),
            pre_image: Vec::new(),
            pre_image_tx: None,
        }
    }

    /// Detect pre-image opportunities on state S_{i-1}.
    ///
    /// Must be called **before** the victim's logs are applied. The result is
    /// retained as the pre-image for the matching `post_detect` call; the
    /// returned copy is informational (tests / logging).
    ///
    /// Detector instances are recreated on every call (fresh per tx, §2.2 of
    /// `docs/plan_backrun.md`): a cross-tx `seen` set would make a pre-existing
    /// gap read as *absent before*, producing false backruns. The only
    /// persistent state is `seen` (emission dedup) and `pre_image`.
    pub fn pre_detect(
        &mut self,
        pool_manager: &PoolManager,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
        scope: &ScanScope,
    ) -> Vec<MevOpportunity> {
        self.pre_two = TwoHopArbDetector::new(self.block_number);
        self.pre_multi = MultiHopArbDetector::new(self.block_number);

        let mut opps = Vec::new();
        opps.extend(self.pre_two.detect(
            pool_manager,
            tx_index,
            timestamp,
            base_fee_per_gas,
            gas_config,
            scope,
        ));
        opps.extend(self.pre_multi.detect(
            pool_manager,
            tx_index,
            timestamp,
            base_fee_per_gas,
            gas_config,
            scope,
        ));

        self.pre_image = opps.clone();
        self.pre_image_tx = Some(tx_index);
        opps
    }

    /// The pre-image captured by the last `pre_detect` call, for assertions.
    pub fn pre_image(&self) -> &[MevOpportunity] {
        &self.pre_image
    }

    /// Detect post-image opportunities on state S_i and emit backrun claims.
    ///
    /// Uses the pre-image retained by `pre_detect` for the same `tx_index`
    /// (S_{i-1}); it is never recomputed here — this call happens after the
    /// victim's logs have already been applied.
    #[allow(clippy::too_many_arguments)]
    pub fn post_detect(
        &mut self,
        pool_manager: &PoolManager,
        tx_index: usize,
        timestamp: u64,
        base_fee_per_gas: u128,
        gas_config: GasConfig,
        scope: &ScanScope,
        _tx_logs: &[crate::data::ExecutedLog],
        txs: &[crate::data::TxData],
    ) -> Vec<MevOpportunity> {
        self.post_two = TwoHopArbDetector::new(self.block_number);
        self.post_multi = MultiHopArbDetector::new(self.block_number);

        let mut post_opps = Vec::new();
        post_opps.extend(self.post_two.detect(
            pool_manager,
            tx_index,
            timestamp,
            base_fee_per_gas,
            gas_config,
            scope,
        ));
        post_opps.extend(self.post_multi.detect(
            pool_manager,
            tx_index,
            timestamp,
            base_fee_per_gas,
            gas_config,
            scope,
        ));

        if post_opps.is_empty() {
            return Vec::new();
        }

        // Pre-image from S_{i-1}, captured before the state update. A mismatch
        // is unreachable: a non-empty post scope (newly_dirty ⊆ will_touch)
        // guarantees the runner called `pre_detect` for this tx. Treating it as
        // absent would mean "not profitable before", so the conservative
        // fallback is an empty index rather than a panic.
        let empty = Vec::new();
        let pre_opps: &Vec<MevOpportunity> = if self.pre_image_tx == Some(tx_index) {
            &self.pre_image
        } else {
            &empty
        };
        let mut pre_index: HashMap<BackrunKey, U256> = HashMap::new();
        for o in pre_opps {
            let k = BackrunKey::from_opp(o);
            pre_index.insert(k, o.expected_profit);
        }

        let dirty: Option<&HashSet<Address>> = match scope {
            ScanScope::Dirty(set) => Some(set),
            ScanScope::Full => None,
        };

        let mut backruns = Vec::new();
        let victim_tx = txs.get(tx_index);

        for mut o in post_opps {
            let k = BackrunKey::from_opp(&o);
            let net1 = o.expected_profit.saturating_sub(U256::from(o.gas_cost_wei));
            let net0 = pre_index
                .get(&k)
                .copied()
                .unwrap_or(U256::ZERO)
                .saturating_sub(U256::from(o.gas_cost_wei));

            // D3: net flip — the opportunity exists primarily because of the
            // victim. net(pre) > 0 means the gap was already executable before
            // it, i.e. a plain arb, not a backrun.
            if net1 > U256::ZERO && net0 <= U256::ZERO {
                let ck = k.to_check_key();
                if check_dedup_key(&mut self.seen, &ck, pool_manager, ck.0, ck.1) {
                    o.strategy = Strategy::Backrun;
                    o.victim_tx_index = Some(tx_index);
                    o.backrun_tx_index = None;
                    if let Some(v) = victim_tx {
                        o.sender = Some(v.from);
                        o.tx_hash = Some(v.hash);
                    }
                    // Canonical id: anchor pool + victim index, matching the
                    // explorer's realized form so T1 matching can land.
                    o.canonical_id = Some(crate::types::opportunity::compute_backrun_canonical_id(
                        anchor_pool(&o, dirty),
                        tx_index,
                    ));
                    backruns.push(o);
                }
            }
        }
        backruns
    }
}

/// First path pool present in the dirty set — the pool whose state change the
/// claim is anchored to (`docs/plan_backrun.md` §2.5).
fn anchor_pool(opp: &MevOpportunity, dirty: Option<&HashSet<Address>>) -> Address {
    let in_scope = |a: &Address| dirty.is_none_or(|d| d.contains(a));
    let candidates: &[Address] = match opp.path.as_deref() {
        Some(p) => p,
        None => std::slice::from_ref(&opp.pool_a),
    };
    candidates
        .iter()
        .chain(std::slice::from_ref(&opp.pool_b))
        .copied()
        .find(in_scope)
        .unwrap_or_else(|| opp.pool_a.min(opp.pool_b))
}
