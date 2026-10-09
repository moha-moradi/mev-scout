//! Event topic constants and the Balancer V2 flash-loan decoder.
//!
//! Centralizes all ERC-20, DEX, flash loan, and liquidation event signatures
//! so decoder modules reference a single source of truth.
//!
//! Swap/transfer/liquidation decoding lives in `pool::decoders` (the live
//! `ExecutedLog` path) and `explorer::decode` (the realized-MEV path). The
//! decoder set that used to sit here was reachable only from the removed
//! `chain::{trades,liquidations,transfers,flashloans}` log-range scanners and
//! duplicated those two, so only `decode_balancer_flash` — still called by
//! `explorer::decode` — was kept.

use alloy::primitives::{b256, keccak256, Address, B256, U256};
use alloy::rpc::types::Log;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

// ── ERC-20 ──────────────────────────────────────────────────────────

pub const TRANSFER_TOPIC: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

// ── Uniswap V2 ──────────────────────────────────────────────────────

pub const V2_SWAP_TOPIC: B256 =
    b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

/// `Sync(uint112 reserve0, uint112 reserve1)` — emitted by swap/mint/burn/sync;
/// not emitted by `skim()`.
pub const V2_SYNC_TOPIC: B256 =
    b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

/// `Mint(address indexed sender, uint amount0, uint amount1)`.
pub static V2_MINT_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Mint(address,uint256,uint256)"));

/// `Burn(address indexed sender, uint amount0, uint amount1, address indexed to)`.
pub static V2_BURN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Burn(address,uint256,uint256,address)"));

// ── Uniswap V3 ──────────────────────────────────────────────────────

pub const V3_SWAP_TOPIC: B256 =
    b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");

/// Uniswap V3 Pool `Flash(address sender, address recipient, uint256 amount0,
/// uint256 amount1, uint256 paid0, uint256 paid1)` — Avalanche flash path for
/// plan P2.3 (Uni V4 has no discrete Flash event; flash accounting is unlock-
/// callback only).
pub static UNI_V3_FLASH_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Flash(address,address,uint256,uint256,uint256,uint256)"));

// ── Uniswap V4 ──────────────────────────────────────────────────────

/// Uniswap V4 PoolManager Swap event (verified against v4-core PoolManager):
/// `emit Swap(id, msg.sender, delta.amount0(), delta.amount1(), sqrtPriceX96,
///             liquidity, tick, swapFee)` with `id: PoolId (bytes32)` in
/// topics[1]. NOTE: this is NOT the same signature as the V3 Swap event —
/// the previous string here was identical to V3's, hashing to the V3 topic
/// and silently disabling V4 trade detection.
pub static V4_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)"));

// ── Pancake Infinity CL ──────────────────────────────────────────────

/// Pancake Infinity CLPoolManager Swap event (verified against
/// pancakeswap/infinity-core CLPoolManager.sol + ICLPoolManager.sol):
/// `emit Swap(id, msg.sender, delta.amount0(), delta.amount1(), sqrtPriceX96,
///             liquidity, tick, fee, protocolFee)` with `id: PoolId (bytes32)`
/// in topics[1]. The singleton CLPoolManager emits for every pool; the pool
/// key is the bytes32 PoolId (synthetic pool address = its first 20 bytes,
/// mirroring `discovery/infinity.rs`).
pub static INF_CL_SWAP_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24,uint16)")
});

// ── Balancer V2 ─────────────────────────────────────────────────────

pub static BALANCER_FLASH_LOAN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("FlashLoan(address,address,address,uint256,bytes)"));

/// A decoded flash-loan fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashLoanEvent {
    pub block: u64,
    pub tx_hash: B256,
    pub tx_index: Option<u64>,
    pub log_index: u64,
    pub protocol: String,
    pub initiator: Address,
    pub target: Address,
    pub token: Address,
    pub amount: U256,
    pub fee: Option<U256>,
}

/// Decode a Balancer V2 Vault `FlashLoan` event.
///
/// `FlashLoan(address indexed recipient, address indexed sender, address
/// indexed token, uint256 amount, bytes userData)`. Balancer charges no fee,
/// so `fee` is `None`.
pub fn decode_balancer_flash(log: &Log) -> Option<FlashLoanEvent> {
    let topics = log.topics();
    if topics.is_empty() || topics[0] != *BALANCER_FLASH_LOAN_TOPIC {
        return None;
    }
    let caller = if topics.len() > 1 {
        Address::from_slice(&topics[1][12..])
    } else {
        Address::ZERO
    };
    let recipient = if topics.len() > 2 {
        Address::from_slice(&topics[2][12..])
    } else {
        Address::ZERO
    };
    let data = &log.data().data;
    let token = if data.len() >= 20 {
        Address::from_slice(&data[0..20])
    } else {
        Address::ZERO
    };
    let amount = if data.len() >= 52 {
        U256::from_be_slice(&data[20..52])
    } else {
        return None;
    };
    Some(FlashLoanEvent {
        block: log.block_number?,
        tx_hash: log.transaction_hash?,
        tx_index: log.transaction_index,
        log_index: log.log_index?,
        protocol: "balancer_v2".to_string(),
        initiator: caller,
        target: recipient,
        token,
        amount,
        fee: None,
    })
}

// ── Aave V2 ─────────────────────────────────────────────────────────

pub static AAVE_V2_FLASH_LOAN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("FlashLoan(address,address,address,uint256,uint256,uint16)"));

// ── Aave V3 ─────────────────────────────────────────────────────────

pub static AAVE_V3_FLASH_LOAN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("FlashLoan(address,address,address,uint256,uint8,uint256,uint16)"));

pub static AAVE_V3_LIQUIDATION_CALL_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("LiquidationCall(address,address,address,uint256,uint256,address,bool)")
});

// ── Compound V3 ─────────────────────────────────────────────────────

pub static COMPOUND_V3_ABSORB_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Absorb(address,address[],uint256[],uint256)"));

/// Compound V3 Comet `BuyCollateral(address indexed buyer, address indexed asset,
/// uint256 baseAmount, uint256 collateralAmount)` — discount capture after Absorb
/// (§26 / explorer plan P0.3).
pub static COMPOUND_V3_BUY_COLLATERAL_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("BuyCollateral(address,address,uint256,uint256)"));

// ── Compound V2 ─────────────────────────────────────────────────────

/// Compound V2 cToken `LiquidateBorrow(address liquidator, address borrower,
/// uint256 repayAmount, address cTokenCollateral, uint256 seizeTokens)`
/// (`liquidator`/`borrower` indexed).
pub static COMPOUND_V2_LIQUIDATE_BORROW_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("LiquidateBorrow(address,address,uint256,address,uint256)"));

// ── Morpho Blue ─────────────────────────────────────────────────────

/// Morpho Blue `Liquidate(bytes32 id, address caller, address borrower,
/// uint256 repaidAssets, uint256 repaidShares, uint256 seizedAssets,
/// uint256 badDebtAssets, uint256 badDebtShares)` — all three of id/caller/
/// borrower indexed (§24 / explorer plan P0.2).
pub static MORPHO_BLUE_LIQUIDATE_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Liquidate(bytes32,address,address,uint256,uint256,uint256,uint256,uint256)")
});

/// Morpho Blue `FlashLoan(address caller, address token, uint256 assets)` —
/// 0% fee provider (§11 hierarchy / plan P2.3 routing).
pub static MORPHO_BLUE_FLASH_LOAN_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("FlashLoan(address,address,uint256)"));

// ── Silo V2 ─────────────────────────────────────────────────────────

/// Silo V2 PartialLiquidation `LiquidationCall(address liquidator, address silo,
/// address borrower, uint256 repayDebtAssets, uint256 withdrawCollateral,
/// bool receiveSToken)` — liquidator/silo/borrower indexed (§24).
pub static SILO_V2_LIQUIDATION_CALL_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("LiquidationCall(address,address,address,uint256,uint256,bool)"));

// ── Euler V2 ────────────────────────────────────────────────────────

/// Euler V2 EVault `Liquidate(address liquidator, address violator,
/// address collateral, uint256 repayAssets, uint256 yieldBalance)` —
/// liquidator/violator indexed (§24).
pub static EULER_V2_LIQUIDATE_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Liquidate(address,address,address,uint256,uint256)"));

// ── Oracles / keepers (explorer plan P1.1 / P1.4 / P1.5) ─────────────

/// Chainlink AggregatorV3 `AnswerUpdated(int256 current, uint256 roundId,
/// uint256 updatedAt)` — `current`/`roundId` indexed (P1.4 / §17.8.4).
pub static CHAINLINK_ANSWER_UPDATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("AnswerUpdated(int256,uint256,uint256)"));

/// Balancer ComposableStablePool `TokenRateCacheUpdated(uint256 tokenIndex,
/// uint256 rate)` — mode-A rate-refresh signal for plan P3.2 staleness.
pub static BALANCER_TOKEN_RATE_CACHE_UPDATED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("TokenRateCacheUpdated(uint256,uint256)"));

/// Aave V3 `ReserveDataUpdated(address reserve, uint256 liquidityRate,
/// uint256 stableBorrowRate, uint256 variableBorrowRate, uint256
/// liquidityIndex, uint256 variableBorrowIndex)` — `reserve` indexed
/// (P1.1 interest-accrual attribution).
pub static AAVE_V3_RESERVE_DATA_UPDATED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("ReserveDataUpdated(address,uint256,uint256,uint256,uint256,uint256)")
});

/// Gelato Automate `ExecSuccess(uint256 txFee, address feeToken, address
/// execAddress, bytes execData, bytes32 taskId, bool callSuccess)` — the
/// catalogue's "TaskExecuted" fee fingerprint (P1.5 / §17.8.9 / §21).
pub static GELATO_EXEC_SUCCESS_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("ExecSuccess(uint256,address,address,bytes,bytes32,bool)")
});

/// Chainlink Automation Registry `UpkeepPerformed(uint256 id, bool success,
/// uint96 totalPayment, uint256 gasUsed, uint256 gasOverhead, bytes trigger)`
/// — `id`/`success` indexed (P1.5; DefiLlama fee decoder layout).
pub static CHAINLINK_UPKEEP_PERFORMED_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("UpkeepPerformed(uint256,bool,uint96,uint256,uint256,bytes)")
});

/// Chainlink Automation log-trigger `LogTriggered(uint256 upkeepId,
/// bytes32 triggerConfigId, bytes32 logBlockHash)` — `upkeepId`/
/// `triggerConfigId`/`logBlockHash` indexed (P1.5).
pub static CHAINLINK_LOG_TRIGGERED_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("LogTriggered(uint256,bytes32,bytes32)"));

/// Solidly / Ramses / Pharaoh / Blackhole gauge
/// `NotifyReward(address from, address reward, uint256 amount)` with
/// `from`/`reward` indexed (plan P3.15 epoch-transition fingerprint).
pub static NOTIFY_REWARD_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("NotifyReward(address,address,uint256)"));

/// Keccak of GMX V2 `eventName` strings — matched against the indexed
/// `eventNameHash` topic on EventEmitter logs (plan P3.7). Emitter address
/// is the primary filter; topic0 layout varies across EventLog/EventLog1/2.
pub static GMX_ADL_STATE_UPDATED_HASH: LazyLock<B256> =
    LazyLock::new(|| keccak256("AdlStateUpdated"));
pub static GMX_LIQUIDATE_POSITION_HASH: LazyLock<B256> =
    LazyLock::new(|| keccak256("LiquidatePosition"));
pub static GMX_POSITION_IMPACT_POOL_DISTRIBUTED_HASH: LazyLock<B256> =
    LazyLock::new(|| keccak256("PositionImpactPoolDistributed"));

/// ERC-4337 EntryPoint v0.6/v0.7
/// `UserOperationEvent(bytes32,address,address,uint256,bool,uint256,uint256)`
/// — userOpHash/sender/paymaster indexed (P3.11).
pub static USER_OPERATION_EVENT_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("UserOperationEvent(bytes32,address,address,uint256,bool,uint256,uint256)")
});

// ── Solidly / Velodrome / Aerodrome ─────────────────────────────────

/// Velodrome V2/Aerodrome pool Swap topic (`Swap(address indexed sender,
/// address indexed to, uint256 amount0In, uint256 amount1In, uint256
/// amount0Out, uint256 amount1Out)` — verified against velodrome-finance/
/// contracts `IPool.sol`). Original Solidly V1 / Camelot pairs instead emit
/// the Uniswap V2 signature, covered by `V2_SWAP_TOPIC`.
pub static SOLIDLY_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(address,address,uint256,uint256,uint256,uint256)"));

// ── Trader Joe LB ───────────────────────────────────────────────────

/// Trader Joe Liquidity Book 2.0 Pair Swap event (also matches the LB 2.2
/// per-side-amounts form — same canonical signature). Verified against
/// lfj-gg/joe-v2 branch v2.0.
pub static TRADER_JOE_LB_SWAP_TOPIC: LazyLock<B256> = LazyLock::new(|| {
    keccak256("Swap(address,address,uint256,bool,uint256,uint256,uint256,uint256)")
});

// Trader Joe Liquidity Book 2.1/2.2 per-side-amounts Swap event (packed
// bytes32 amounts). Verified against lfj-gg/joe-v2 branches main/v2.1/v2.2.
// It lives in `pipeline::scanner::topics::TRADER_JOE_LB_SWAP_LEGACY`, which is
// what `pool/discovery/trader_joe.rs` matches on, so no alias is kept here.

// ── Pendle Finance ──────────────────────────────────────────────────

/// Pendle V2 market Swap event (caller, receiver indexed).
pub static PENDLE_MARKET_SWAP_TOPIC: LazyLock<B256> =
    LazyLock::new(|| keccak256("Swap(address,address,int256,int256,uint256,uint256)"));

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::keccak256;

    /// One pin per event kind. Aliases of the same kind (events vs scanner vs
    /// pool decoders) must match; different kinds must not collide.
    #[test]
    fn topic_manifest_is_pinned_and_distinct() {
        use crate::explorer::decode::{
            ONEINCH_SWAPPED_TOPIC, PARASWAP_SWAPPED_TOPIC, PARASWAP_SWAPPED_V3_TOPIC,
            ZRX_FILL_TOPIC,
        };
        use crate::pipeline::scanner::topics;
        use crate::pool::decoders::{
            self, LB_DEPOSITED_TO_BINS_TOPIC, LB_WITHDRAWN_FROM_BINS_TOPIC,
        };

        let kinds: std::cell::RefCell<Vec<(&str, B256)>> = std::cell::RefCell::new(Vec::new());
        let pin_lit = |name: &'static str, topic: B256, expected: B256| {
            assert_eq!(topic, expected, "{name}");
            kinds.borrow_mut().push((name, topic));
        };
        let pin_sig = |name: &'static str, topic: B256, sig: &'static str| {
            assert_eq!(topic, keccak256(sig), "{name}");
            kinds.borrow_mut().push((name, topic));
        };

        pin_lit(
            "erc20 transfer",
            TRANSFER_TOPIC,
            b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"),
        );
        pin_lit(
            "v2 swap",
            V2_SWAP_TOPIC,
            b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"),
        );
        pin_sig(
            "v3 swap",
            V3_SWAP_TOPIC,
            "Swap(address,address,int256,int256,uint160,uint128,int24)",
        );
        assert_eq!(
            decoders::V3_SWAP_TOPIC,
            V3_SWAP_TOPIC,
            "decoder v3 swap alias"
        );
        assert_eq!(*topics::V3_SWAP, V3_SWAP_TOPIC, "scanner v3 swap alias");
        pin_sig(
            "v3 mint",
            decoders::V3_MINT_TOPIC,
            "Mint(address,address,int24,int24,uint128,uint256,uint256)",
        );
        pin_sig(
            "v3 burn",
            decoders::V3_BURN_TOPIC,
            "Burn(address,int24,int24,uint128,uint256,uint256)",
        );
        pin_lit(
            "trader joe lb",
            *TRADER_JOE_LB_SWAP_TOPIC,
            b256!("c528cda9e500228b16ce84fadae290d9a49aecb17483110004c5af0a07f6fd73"),
        );
        assert_eq!(*topics::TRADER_JOE_LB_SWAP, *TRADER_JOE_LB_SWAP_TOPIC);
        pin_lit(
            "trader joe lb legacy",
            *topics::TRADER_JOE_LB_SWAP_LEGACY,
            b256!("ad7d6f97abf51ce18e17a38f4d70e975be9c0708474987bb3e26ad21bd93ca70"),
        );
        pin_sig(
            "pendle market",
            *topics::PENDLE_MARKET_SWAP,
            "Swap(address,address,int256,int256,uint256,uint256)",
        );
        assert_eq!(*PENDLE_MARKET_SWAP_TOPIC, *topics::PENDLE_MARKET_SWAP);
        pin_lit(
            "pendle pt/sy",
            *topics::PENDLE_SWAP_PT_AND_SY,
            b256!("3f5e2944826baeaed8eb77f0f74e6088a154a0fc1317f062fd984585607b4739"),
        );
        pin_lit(
            "pendle yt/sy",
            *topics::PENDLE_SWAP_YT_AND_SY,
            b256!("05499aba408f669fb848399c146fad5bd604d50b15566bdc19e81c40922fab8d"),
        );
        pin_lit(
            "pendle pt/token",
            *topics::PENDLE_SWAP_PT_AND_TOKEN,
            b256!("d3c1d9b397236779b29ee5b5b150c1110fc8221b6b6ec0be49c9f4860ceb2036"),
        );
        pin_lit(
            "pendle yt/token",
            *topics::PENDLE_SWAP_YT_AND_TOKEN,
            b256!("a3a2846538c60e47775faa60c6ae79b67dee6d97bb70e386ebbaf4c3a38e8b81"),
        );
        pin_sig(
            "1inch swapped",
            ONEINCH_SWAPPED_TOPIC,
            "Swapped(address,address,address,address,uint256,uint256)",
        );
        pin_sig(
            "paraswap swapped",
            PARASWAP_SWAPPED_TOPIC,
            "Swapped(address,address,address,address,uint256,uint256,uint256,string)",
        );
        pin_sig(
            "paraswap swapped v3",
            PARASWAP_SWAPPED_V3_TOPIC,
            "SwappedV3(bytes16,address,uint256,address,address,address,address,uint256,uint256,uint256)",
        );
        pin_sig(
            "0x fill",
            ZRX_FILL_TOPIC,
            "Fill(address,address,bytes,bytes,bytes,bytes,bytes32,address,address,uint256,uint256,uint256,uint256,uint256)",
        );
        pin_sig(
            "lb deposit",
            *LB_DEPOSITED_TO_BINS_TOPIC,
            "DepositedToBins(address,address,uint256[],bytes32[])",
        );
        pin_sig(
            "lb withdraw",
            *LB_WITHDRAWN_FROM_BINS_TOPIC,
            "WithdrawnFromBins(address,address,uint256[],bytes32[])",
        );

        let kinds = kinds.into_inner();
        for i in 0..kinds.len() {
            for j in (i + 1)..kinds.len() {
                assert_ne!(
                    kinds[i].1, kinds[j].1,
                    "{} collides with {}",
                    kinds[i].0, kinds[j].0
                );
            }
        }
    }

    /// Solidly topic must be the verified Velodrome V2 signature and must
    /// NOT collide with the V2 topic (original Solidly V1 pairs share V2's).
    #[test]
    fn solidly_swap_topic_is_verified() {
        assert_eq!(
            *SOLIDLY_SWAP_TOPIC,
            b256!("b3e2773606abfd36b5bd91394b3a54d1398336c65005baf7bf7a05efeffaf75b")
        );
        assert_ne!(*SOLIDLY_SWAP_TOPIC, V2_SWAP_TOPIC);
    }

    /// Regression guard: the V4 Swap topic must NOT equal the V3 topic.
    /// The old string `Swap(address,address,int256,int256,uint160,uint128,int24)`
    /// hashed to exactly V3_SWAP, silently disabling V4 trade detection.
    #[test]
    fn v4_swap_topic_differs_from_v3_and_is_verified() {
        assert_ne!(*V4_SWAP_TOPIC, V3_SWAP_TOPIC);
        assert_eq!(
            *V4_SWAP_TOPIC,
            b256!("40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f")
        );
    }
}
