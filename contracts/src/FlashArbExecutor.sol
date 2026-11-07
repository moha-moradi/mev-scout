// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title FlashArbExecutor
/// @notice ABI v2 flash-arb executor. Borrows from Balancer V2 (fee 0), runs
///         V2 `swap` / V3 exact-input pool `swap` (the pool-level equivalent of
///         `exactInputSingle` — no router address, so any V3 fork works),
///         repays the vault, and forwards leftover borrow-token to the
///         immutable `withdrawer`. When the borrow token is WETH, leftover is
///         unwrapped so the withdrawer's native balance delta is the profit.
/// @dev Operator-only `execute`. `minProfit` is in borrow-token units.
interface IERC20 {
    function balanceOf(address) external view returns (uint256);
    function transfer(address, uint256) external returns (bool);
}

interface IWETH is IERC20 {
    function withdraw(uint256) external;
}

interface IBalancerVault {
    function flashLoan(
        address recipient,
        address[] calldata tokens,
        uint256[] calldata amounts,
        bytes calldata userData
    ) external;
}

interface IUniswapV2Pair {
    function token0() external view returns (address);
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32);
    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
}

interface IUniswapV3Pool {
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);
}

contract FlashArbExecutor {
    /// Bump when the `execute` / `Leg` ABI changes. Rust embeds the same number.
    uint8 public constant ABI_VERSION = 2;

    /// Uniswap V3 tick-math limits. +1 / -1 keeps the pool from reverting on the boundary.
    uint160 internal constant MIN_SQRT_RATIO = 4295128739 + 1;
    uint160 internal constant MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342 - 1;

    address public immutable operator;
    address public immutable withdrawer;
    address public immutable weth;

    address internal activeVault;
    address internal expectedV3Pool;
    uint256 internal borrowBalanceBefore;

    error NotOperator();
    error Expired();
    error Reentrant();
    error NotVault();
    error NotPool();
    error RepayFailed();
    error MinProfit();
    error UnknownKind();
    error ZeroAmount();
    error TransferFailed();
    error EthForwardFailed();
    error ZeroAddress();

    struct Leg {
        address pool;
        address tokenIn;
        address tokenOut;
        /// 0 = V2 pair.swap (fee is bps, 30 = 0.30%). 1 = V3 exact-input pool swap.
        uint8 kind;
        uint24 fee;
    }

    struct Plan {
        address vault;
        address borrowToken;
        uint256 amount;
        uint256 minProfit; // borrow-token units, not USD/wei
        uint256 deadline; // unix seconds, checked against block.timestamp
        Leg[] legs;
    }

    constructor(address operator_, address withdrawer_, address weth_) {
        if (operator_ == address(0) || withdrawer_ == address(0) || weth_ == address(0)) {
            revert ZeroAddress();
        }
        operator = operator_;
        withdrawer = withdrawer_;
        weth = weth_;
    }

    receive() external payable {}

    function execute(Plan calldata plan) external {
        if (msg.sender != operator) revert NotOperator();
        if (block.timestamp > plan.deadline) revert Expired();
        if (activeVault != address(0)) revert Reentrant();
        if (plan.amount == 0) revert ZeroAmount();
        if (plan.vault == address(0)) revert ZeroAddress();
        if (plan.legs.length == 0) revert ZeroAmount();

        borrowBalanceBefore = IERC20(plan.borrowToken).balanceOf(address(this));
        activeVault = plan.vault;

        address[] memory tokens = new address[](1);
        tokens[0] = plan.borrowToken;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = plan.amount;

        bytes memory userData = abi.encode(plan.legs, plan.minProfit);
        IBalancerVault(plan.vault).flashLoan(address(this), tokens, amounts, userData);
        activeVault = address(0);
        borrowBalanceBefore = 0;
    }

    function receiveFlashLoan(
        address[] calldata tokens,
        uint256[] calldata amounts,
        uint256[] calldata feeAmounts,
        bytes calldata userData
    ) external {
        if (msg.sender != activeVault) revert NotVault();

        (Leg[] memory legs, uint256 minProfit) = abi.decode(userData, (Leg[], uint256));

        uint256 amount = amounts[0];
        for (uint256 i = 0; i < legs.length; i++) {
            amount = _leg(legs[i], amount);
        }

        uint256 repay = amounts[0] + feeAmounts[0];
        uint256 bal = IERC20(tokens[0]).balanceOf(address(this));
        if (bal < repay) revert RepayFailed();
        _transfer(tokens[0], msg.sender, repay);

        uint256 left = bal - repay;
        uint256 net = left > borrowBalanceBefore ? left - borrowBalanceBefore : 0;
        if (net < minProfit) revert MinProfit();
        if (net == 0) return;

        if (tokens[0] == weth) {
            IWETH(weth).withdraw(net);
            (bool ok,) = withdrawer.call{value: net}("");
            if (!ok) revert EthForwardFailed();
        } else {
            _transfer(tokens[0], withdrawer, net);
        }
    }

    /// Operator-only recovery. Always sends to the immutable withdrawer.
    function sweep(address token) external {
        if (msg.sender != operator) revert NotOperator();
        uint256 bal = IERC20(token).balanceOf(address(this));
        if (bal == 0) return;
        _transfer(token, withdrawer, bal);
    }

    function sweepEth() external {
        if (msg.sender != operator) revert NotOperator();
        uint256 bal = address(this).balance;
        if (bal == 0) return;
        (bool ok,) = withdrawer.call{value: bal}("");
        if (!ok) revert EthForwardFailed();
    }

    /// V3 pools pull the input token from this contract inside the callback.
    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data)
        external
    {
        if (msg.sender != expectedV3Pool) revert NotPool();
        uint256 owed = amount0Delta > 0 ? uint256(amount0Delta) : uint256(amount1Delta);
        address tokenIn = abi.decode(data, (address));
        _transfer(tokenIn, msg.sender, owed);
    }

    function _leg(Leg memory leg, uint256 amountIn) internal returns (uint256 amountOut) {
        if (leg.kind == 0) return _v2(leg, amountIn);
        if (leg.kind == 1) return _v3(leg, amountIn);
        revert UnknownKind();
    }

    function _v2(Leg memory leg, uint256 amountIn) internal returns (uint256 amountOut) {
        _transfer(leg.tokenIn, leg.pool, amountIn);
        IUniswapV2Pair pair = IUniswapV2Pair(leg.pool);
        (uint112 r0, uint112 r1,) = pair.getReserves();
        address token0 = pair.token0();
        (uint256 reserveIn, uint256 reserveOut) = leg.tokenIn == token0
            ? (uint256(r0), uint256(r1))
            : (uint256(r1), uint256(r0));
        uint256 feeBps = leg.fee == 0 ? 30 : uint256(leg.fee);
        amountOut = _amountOut(amountIn, reserveIn, reserveOut, feeBps);
        (uint256 a0, uint256 a1) =
            leg.tokenIn == token0 ? (uint256(0), amountOut) : (amountOut, uint256(0));
        pair.swap(a0, a1, address(this), "");
    }

    function _v3(Leg memory leg, uint256 amountIn) internal returns (uint256 amountOut) {
        bool zeroForOne = leg.tokenIn < leg.tokenOut;
        uint256 beforeOut = IERC20(leg.tokenOut).balanceOf(address(this));
        expectedV3Pool = leg.pool;
        IUniswapV3Pool(leg.pool).swap(
            address(this),
            zeroForOne,
            int256(amountIn),
            zeroForOne ? MIN_SQRT_RATIO : MAX_SQRT_RATIO,
            abi.encode(leg.tokenIn)
        );
        expectedV3Pool = address(0);
        amountOut = IERC20(leg.tokenOut).balanceOf(address(this)) - beforeOut;
    }

    /// Uniswap V2 getAmountOut with a bps fee (30 = 0.30% → 9970/10000).
    function _amountOut(uint256 amountIn, uint256 reserveIn, uint256 reserveOut, uint256 feeBps)
        internal
        pure
        returns (uint256)
    {
        uint256 kept = 10_000 - feeBps;
        uint256 amountInWithFee = amountIn * kept;
        return (amountInWithFee * reserveOut) / (reserveIn * 10_000 + amountInWithFee);
    }

    function _transfer(address token, address to, uint256 amount) internal {
        (bool ok, bytes memory ret) =
            token.call(abi.encodeWithSelector(IERC20.transfer.selector, to, amount));
        if (!ok || (ret.length != 0 && !abi.decode(ret, (bool)))) revert TransferFailed();
    }
}
