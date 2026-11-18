//! Calldata builders for non-V2/V3 executor legs.
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::sol_types::SolCall;

pub mod curve {
    alloy::sol! {
        function exchange(int128 i, int128 j, uint256 dx, uint256 min_dy);
        function exchange_underlying(int128 i, int128 j, uint256 dx, uint256 min_dy);
    }
}

pub mod solidly {
    alloy::sol! {
        function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes data);
    }
}

pub mod balancer {
    alloy::sol! {
        struct SingleSwap {
            bytes32 poolId;
            uint8 kind;
            address assetIn;
            address assetOut;
            uint256 amount;
            bytes userData;
        }
        struct FundManagement {
            address sender;
            address recipient;
            bool useInternalBalance;
            bool toInternalBalance;
        }
        function swap(
            SingleSwap singleSwap,
            FundManagement funds,
            uint256 limit,
            uint256 deadline
        ) external payable returns (uint256);
    }
}

pub mod savax {
    alloy::sol! {
        function redeem(uint256 amount);
        function submit() external payable returns (uint256);
    }
}

pub mod lb {
    alloy::sol! {
        /// LFJ pair-family selector for `Path.versions`.
        enum Version { V1, V2, V2_1, V2_2 }

        struct Path {
            uint256[] pairBinSteps;
            Version[] versions;
            address[] tokenPath;
        }

        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            Path path,
            address to,
            uint256 deadline
        ) external returns (uint256 amountOut);
    }
}

pub mod pendle {
    alloy::sol! {
        struct Order {
            uint256 salt;
            uint256 expiry;
            uint256 nonce;
            uint8 orderType;
            address token;
            address YT;
            address maker;
            address receiver;
            uint256 makingAmount;
            uint256 lnImpliedRate;
            uint256 failSafeRate;
            bytes permit;
        }

        struct FillOrderParams {
            Order order;
            bytes signature;
            uint256 makingAmount;
        }

        struct LimitOrderData {
            address limitRouter;
            uint256 epsSkipMarket;
            FillOrderParams[] normalFills;
            FillOrderParams[] flashFills;
            bytes optData;
        }

        function swapExactPtForSy(
            address receiver,
            address market,
            uint256 exactPtIn,
            uint256 minSyOut,
            LimitOrderData limit
        ) external returns (uint256 netSyOut, uint256 netSyFee);

        function swapExactSyForPtSimple(
            address receiver,
            address market,
            uint256 exactSyIn,
            uint256 minPtOut
        ) external returns (uint256 netPtOut, uint256 netSyFee);
    }
}

pub fn curve_exchange(i: i128, j: i128, dx: U256, min_dy: U256, underlying: bool) -> Bytes {
    if underlying {
        curve::exchange_underlyingCall { i, j, dx, min_dy }
            .abi_encode()
            .into()
    } else {
        curve::exchangeCall { i, j, dx, min_dy }.abi_encode().into()
    }
}

pub fn solidly_swap(amount0_out: U256, amount1_out: U256, to: Address) -> Bytes {
    solidly::swapCall {
        amount0Out: amount0_out,
        amount1Out: amount1_out,
        to,
        data: Bytes::new(),
    }
    .abi_encode()
    .into()
}

#[allow(clippy::too_many_arguments)]
pub fn balancer_swap(
    pool_id: [u8; 32],
    token_in: Address,
    token_out: Address,
    amount: U256,
    min_out: U256,
    executor: Address,
    deadline: u64,
) -> Bytes {
    let single = balancer::SingleSwap {
        poolId: B256::from(pool_id),
        kind: 0, // GIVEN_IN
        assetIn: token_in,
        assetOut: token_out,
        amount,
        userData: Bytes::new(),
    };
    let funds = balancer::FundManagement {
        sender: executor,
        recipient: executor,
        useInternalBalance: false,
        toInternalBalance: false,
    };
    balancer::swapCall {
        singleSwap: single,
        funds,
        limit: min_out,
        deadline: U256::from(deadline),
    }
    .abi_encode()
    .into()
}

pub fn savax_redeem(shares: U256) -> Bytes {
    savax::redeemCall { amount: shares }.abi_encode().into()
}

pub fn savax_submit() -> Bytes {
    savax::submitCall {}.abi_encode().into()
}

/// LFJ LB router exact-in swap over one pair.
///
/// `version` must match the pair family (see `ChainConfig.lb_factory_versions`);
/// the V2.1 and V2.2 routers accept the same ABI but not each other's pairs.
#[allow(clippy::too_many_arguments)]
pub fn lb_swap_exact_tokens_for_tokens(
    amount_in: U256,
    min_out: U256,
    token_in: Address,
    token_out: Address,
    bin_step: u32,
    to: Address,
    deadline: u64,
    version: lb::Version,
) -> Bytes {
    lb::swapExactTokensForTokensCall {
        amountIn: amount_in,
        amountOutMin: min_out,
        path: lb::Path {
            pairBinSteps: vec![U256::from(bin_step)],
            versions: vec![version],
            tokenPath: vec![token_in, token_out],
        },
        to,
        deadline: U256::from(deadline),
    }
    .abi_encode()
    .into()
}

/// Pendle exact PT → SY through the router (empty limit-order data).
pub fn pendle_swap_exact_pt_for_sy(
    receiver: Address,
    market: Address,
    exact_pt_in: U256,
    min_sy_out: U256,
) -> Bytes {
    pendle::swapExactPtForSyCall {
        receiver,
        market,
        exactPtIn: exact_pt_in,
        minSyOut: min_sy_out,
        limit: empty_limit_order_data(),
    }
    .abi_encode()
    .into()
}

/// Pendle exact SY → PT through the router's on-chain approximation.
pub fn pendle_swap_exact_sy_for_pt(
    receiver: Address,
    market: Address,
    exact_sy_in: U256,
    min_pt_out: U256,
) -> Bytes {
    pendle::swapExactSyForPtSimpleCall {
        receiver,
        market,
        exactSyIn: exact_sy_in,
        minPtOut: min_pt_out,
    }
    .abi_encode()
    .into()
}

fn empty_limit_order_data() -> pendle::LimitOrderData {
    pendle::LimitOrderData {
        limitRouter: Address::ZERO,
        epsSkipMarket: U256::ZERO,
        normalFills: vec![],
        flashFills: vec![],
        optData: Bytes::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    /// Selectors pinned against on-chain signature registries:
    /// 4byte `0x2a443fae` (LBRouter Path), `0x3346d3a3` (swapExactPtForSy with
    /// the deployed `LimitOrderData` shape), and the simple SY→PT entrypoint.
    #[test]
    fn router_selectors_match_published_abis() {
        assert_eq!(
            lb::swapExactTokensForTokensCall::SELECTOR,
            [0x2a, 0x44, 0x3f, 0xae]
        );
        assert_eq!(
            pendle::swapExactPtForSyCall::SELECTOR,
            [0x33, 0x46, 0xd3, 0xa3]
        );
        assert_eq!(
            pendle::swapExactSyForPtSimpleCall::SELECTOR,
            [0x40, 0xa1, 0x69, 0xf9]
        );
    }

    #[test]
    fn lb_path_round_trips_pair_family() {
        let t0 = address!("0x0000000000000000000000000000000000000001");
        let t1 = address!("0x0000000000000000000000000000000000000002");
        let to = address!("0x0000000000000000000000000000000000000003");
        let data = lb_swap_exact_tokens_for_tokens(
            U256::from(1_000u64),
            U256::from(990u64),
            t0,
            t1,
            20,
            to,
            12,
            lb::Version::V2_1,
        );
        let decoded =
            lb::swapExactTokensForTokensCall::abi_decode(&data).expect("decode lb call");
        assert_eq!(decoded.path.pairBinSteps, vec![U256::from(20u64)]);
        assert_eq!(decoded.path.tokenPath, vec![t0, t1]);
        assert_eq!(decoded.path.versions.len(), 1);
        assert!(matches!(decoded.path.versions[0], lb::Version::V2_1));
    }

    #[test]
    fn pendle_empty_limit_data_decodes() {
        let market = address!("0x0000000000000000000000000000000000000004");
        let to = address!("0x0000000000000000000000000000000000000005");
        let data = pendle_swap_exact_pt_for_sy(to, market, U256::from(5u64), U256::from(4u64));
        let decoded = pendle::swapExactPtForSyCall::abi_decode(&data).expect("decode pendle call");
        assert!(decoded.limit.normalFills.is_empty());
        assert!(decoded.limit.flashFills.is_empty());
        assert_eq!(decoded.market, market);
    }
}
