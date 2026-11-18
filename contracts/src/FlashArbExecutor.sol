// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

/// @title FlashArbExecutor
/// @notice ABI v3 flash-arb executor. Borrows from Balancer V2 (fee 0), runs
///         V2 `swap`, V3 exact-input pool `swap`, or a generic
///         `(target, spender, calldata, minOut)` call for Curve/Balancer/LB/
///         Pendle/sAVAX, repays the vault, and forwards leftover borrow-token
///         to the immutable `withdrawer`. When the borrow token is WETH,
///         leftover is unwrapped so the withdrawer's native balance delta is
///         the profit.
/// @dev Operator-only `execute`. `minProfit` is in borrow-token units.
interface IERC20 {
    function balanceOf(address) external view returns (uint256);
    function transfer(address, uint256) external returns (bool);
    function approve(address, uint256) external returns (bool);
}

interface IWETH is IERC20 {
    function deposit() external payable;
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
    uint8 public constant ABI_VERSION = 3;

    /// Uniswap V3 tick-math limits. +1 / -1 keeps the pool from reverting on the boundary.
    uint160 internal constant MIN_SQRT_RATIO = 4295128739 + 1;
    uint160 internal constant MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342 - 1;

    address public immutable operator;
    address public immutable withdrawer;
    address public immutable weth;

    address internal activeVault;
    address internal expectedV3Pool;
    uint256 internal borrowBalanceBefore;

    /// Optional operator-managed allowlist for generic call targets.
    /// Left disabled for local revm/anvil simulation.
    bool public restrictTargets;
    mapping(address => bool) public allowedTargets;

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
    error CallFailed();
    error MinOut();
    error TargetNotAllowed();
    error TooManyLegs();
    error DataTooLarge();

    struct Leg {
        /// Pool, Balancer Vault, router, or sAVAX contract.
        address target;
        address tokenIn;
        /// address(0) means native ETH.
        address tokenOut;
        /// Approved spender for kind 2; address(0) skips approval.
        address spender;
        /// 0 V2 exact-in, 1 V3 exact-in, 2 call, 3 wrapAll,
        /// 4 unwrap, 5 callWithValue, 6 callWithTransfer.
        uint8 kind;
        uint24 fee;
        /// Required tokenOut (or native) balance delta for generic kinds.
        uint256 minOut;
        /// Calldata for kinds 2/5/6.
        bytes data;
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

    function version() external pure returns (uint8) {
        return ABI_VERSION;
    }

    function setTargetAllowed(address target, bool allowed) external {
        if (msg.sender != operator) revert NotOperator();
        allowedTargets[target] = allowed;
    }

    function setRestrictTargets(bool restrict) external {
        if (msg.sender != operator) revert NotOperator();
        restrictTargets = restrict;
    }

    function execute(Plan calldata plan) external {
        if (msg.sender != operator) revert NotOperator();
        if (block.timestamp > plan.deadline) revert Expired();
        if (activeVault != address(0)) revert Reentrant();
        if (plan.amount == 0) revert ZeroAmount();
        if (plan.vault == address(0)) revert ZeroAddress();
        if (plan.legs.length == 0) revert ZeroAmount();
        if (plan.legs.length > 8) revert TooManyLegs();

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
        if (legs.length > 8) revert TooManyLegs();
        for (uint256 i = 0; i < legs.length; i++) {
            if (legs[i].data.length > 512) revert DataTooLarge();
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
        if (leg.kind == 2) return _call(leg, amountIn);
        if (leg.kind == 3) return _wrapAll();
        if (leg.kind == 4) return _unwrap(amountIn);
        if (leg.kind == 5) return _callWithValue(leg, amountIn);
        if (leg.kind == 6) return _callWithTransfer(leg, amountIn);
        revert UnknownKind();
    }

    function _checkTarget(address target) internal view {
        if (target == address(0)) revert ZeroAddress();
        if (restrictTargets && !allowedTargets[target]) revert TargetNotAllowed();
    }

    function _balanceOf(address token) internal view returns (uint256) {
        return token == address(0)
            ? address(this).balance
            : IERC20(token).balanceOf(address(this));
    }

    function _snapshotAndCheck(Leg memory leg, uint256 before_) internal view returns (uint256 amountOut) {
        uint256 after_ = _balanceOf(leg.tokenOut);
        if (after_ < before_) revert CallFailed();
        amountOut = after_ - before_;
        if (amountOut < leg.minOut) revert MinOut();
    }

    /// Generic call with an optional ERC-20 approval. The target pulls tokenIn.
    function _call(Leg memory leg, uint256 amountIn) internal returns (uint256 amountOut) {
        _checkTarget(leg.target);
        if (leg.spender != address(0)) {
            _forceApprove(leg.tokenIn, leg.spender, amountIn);
        }
        uint256 before_ = _balanceOf(leg.tokenOut);
        (bool ok,) = leg.target.call(leg.data);
        if (!ok) revert CallFailed();
        return _snapshotAndCheck(leg, before_);
    }

    /// V2-style pools that expect the input token transferred before `swap`.
    function _callWithTransfer(Leg memory leg, uint256 amountIn) internal returns (uint256 amountOut) {
        _checkTarget(leg.target);
        _transfer(leg.tokenIn, leg.target, amountIn);
        uint256 before_ = _balanceOf(leg.tokenOut);
        (bool ok,) = leg.target.call(leg.data);
        if (!ok) revert CallFailed();
        return _snapshotAndCheck(leg, before_);
    }

    /// Payable call (sAVAX `submit()` and similar).
    function _callWithValue(Leg memory leg, uint256 amountIn) internal returns (uint256 amountOut) {
        _checkTarget(leg.target);
        uint256 before_ = _balanceOf(leg.tokenOut);
        (bool ok,) = leg.target.call{value: amountIn}(leg.data);
        if (!ok) revert CallFailed();
        return _snapshotAndCheck(leg, before_);
    }

    function _wrapAll() internal returns (uint256 amountOut) {
        amountOut = address(this).balance;
        if (amountOut == 0) revert ZeroAmount();
        IWETH(weth).deposit{value: amountOut}();
    }

    function _unwrap(uint256 amountIn) internal returns (uint256 amountOut) {
        if (amountIn == 0) revert ZeroAmount();
        IWETH(weth).withdraw(amountIn);
        return amountIn;
    }

    function _forceApprove(address token, address spender, uint256 amount) internal {
        (bool ok,) = token.call(abi.encodeWithSelector(IERC20.approve.selector, spender, 0));
        if (!ok) revert TransferFailed();
        (ok,) = token.call(abi.encodeWithSelector(IERC20.approve.selector, spender, amount));
        if (!ok) revert TransferFailed();
    }

    function _v2(Leg memory leg, uint256 amountIn) internal returns (uint256 amountOut) {
        _transfer(leg.tokenIn, leg.target, amountIn);
        IUniswapV2Pair pair = IUniswapV2Pair(leg.target);
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
        expectedV3Pool = leg.target;
        IUniswapV3Pool(leg.target).swap(
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
