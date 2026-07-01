// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC20} from "forge-std/interfaces/IERC20.sol";
import {SafeTransferLib} from "forge-std/SafeTransferLib.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {IFlashLoanProvider} from "../interfaces/IFlashLoanProvider.sol";
import {IDEXRouter} from "../interfaces/IDEXRouter.sol";
import {IBalancerVault} from "../interfaces/IBalancerVault.sol";
import {IAaveV3Pool} from "../interfaces/IAaveV3Pool.sol";
import {IUniswapV2Pair, IUniswapV2Callee} from "../interfaces/IUniswapV2.sol";
import {IUniswapV3Pool, IUniswapV3SwapCallback} from "../interfaces/IUniswapV3.sol";
import {IWETH} from "../interfaces/IWETH.sol";

contract FlashLoanArbitrage is ReentrancyGuard, IFlashLoanProvider, IUniswapV2Callee, IUniswapV3SwapCallback {
    using SafeTransferLib for address;

    struct SwapStep {
        address tokenIn;
        address tokenOut;
        address pool;
        address router;
        uint24 fee;
    }

    error NotOwner();
    error FlashLoanFailed();
    error SwapFailed();
    error NotProfitable(uint256 profit, uint256 minProfit);
    error InvalidProvider();

    address public owner;
    address public balancerVault;
    address public aavePool;

    event ArbitrageExecuted(
        address indexed tokenIn,
        address indexed tokenOut,
        uint256 inputAmount,
        uint256 profit,
        ProviderType provider
    );

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    constructor(address _balancerVault, address _aavePool) {
        owner = msg.sender;
        balancerVault = _balancerVault;
        aavePool = _aavePool;
    }

    function executeArbitrage(
        ProviderType provider,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minProfit,
        bytes calldata swapPath
    ) external nonReentrant {
        if (provider == ProviderType.BalancerV2) {
            address[] memory tokens = new address[](1);
            tokens[0] = tokenIn;
            uint256[] memory amounts = new uint256[](1);
            amounts[0] = amountIn;
            bytes memory userData = abi.encode(tokenOut, minProfit, swapPath);
            IBalancerVault(balancerVault).flashLoan(address(this), tokens, amounts, userData);
        } else if (provider == ProviderType.AaveV3) {
            bytes memory params = abi.encode(tokenIn, tokenOut, minProfit, swapPath);
            IAaveV3Pool(aavePool).flashLoanSimple(address(this), tokenIn, amountIn, params, 0);
        } else if (provider == ProviderType.UniswapV2) {
            bytes memory data = abi.encode(tokenIn, tokenOut, amountIn, minProfit, swapPath);
            IUniswapV2Pair(swapPath).swap(0, amountIn, address(this), data);
        } else if (provider == ProviderType.UniswapV3) {
            bytes memory data = abi.encode(tokenIn, tokenOut, amountIn, minProfit, swapPath, msg.sender);
            IUniswapV3Pool(swapPath).swap(address(this), true, int256(amountIn), 0, data);
        } else {
            revert InvalidProvider();
        }
    }

    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external {
        if (msg.sender != balancerVault) revert FlashLoanFailed();
        (address tokenOut, uint256 minProfit, bytes memory swapPath) =
            abi.decode(userData, (address, uint256, bytes));
        uint256 amountIn = amounts[0];
        uint256 fee = feeAmounts[0];
        uint256 outputAmount = _executeSwaps(swapPath, amountIn);
        uint256 profit = outputAmount - amountIn - fee;
        if (profit < minProfit) revert NotProfitable(profit, minProfit);
        _approveRepayment(IERC20(tokens[0]), amountIn + fee);
        emit ArbitrageExecuted(tokens[0], tokenOut, amountIn, profit, ProviderType.BalancerV2);
    }

    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external returns (bool) {
        if (msg.sender != aavePool) revert FlashLoanFailed();
        (address tokenIn, address tokenOut, uint256 minProfit, bytes memory swapPath) =
            abi.decode(params, (address, address, uint256, bytes));
        uint256 outputAmount = _executeSwaps(swapPath, amount);
        uint256 profit = outputAmount - amount - premium;
        if (profit < minProfit) revert NotProfitable(profit, minProfit);
        IERC20(tokenIn).approve(aavePool, amount + premium);
        emit ArbitrageExecuted(tokenIn, tokenOut, amount, profit, ProviderType.AaveV3);
        return true;
    }

    function uniswapV2Call(
        address,
        uint256 amount0,
        uint256 amount1,
        bytes calldata data
    ) external {
        (address tokenIn, address tokenOut, uint256 amountIn, uint256 minProfit, bytes memory swapPath) =
            abi.decode(data, (address, address, uint256, uint256, bytes));
        uint256 borrowed = amount0 > 0 ? amount0 : amount1;
        uint256 fee = borrowed / 997; // 0.3% V2 fee simplified
        uint256 outputAmount = _executeSwaps(swapPath, borrowed);
        uint256 profit = outputAmount - amountIn - fee;
        if (profit < minProfit) revert NotProfitable(profit, minProfit);
        _approveRepayment(IERC20(tokenIn), borrowed + fee);
        emit ArbitrageExecuted(tokenIn, tokenOut, amountIn, profit, ProviderType.UniswapV2);
    }

    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external {
        (address tokenIn, address tokenOut, uint256 amountIn, uint256 minProfit, bytes memory swapPath, address caller) =
            abi.decode(data, (address, address, uint256, uint256, bytes, address));
        uint256 owed = uint256(amount0Delta > 0 ? amount0Delta : amount1Delta);
        uint256 outputAmount = _executeSwaps(swapPath, owed);
        uint256 profit = outputAmount - amountIn;
        if (profit < minProfit) revert NotProfitable(profit, minProfit);
        address tokenToRepay = amount0Delta > 0 ? tokenIn : tokenOut;
        _approveRepayment(IERC20(tokenToRepay), owed);
        emit ArbitrageExecuted(tokenIn, tokenOut, amountIn, profit, ProviderType.UniswapV3);
    }

    function _executeSwaps(bytes memory swapPath, uint256 amountIn) internal returns (uint256) {
        SwapStep[] memory steps = abi.decode(swapPath, (SwapStep[]));
        uint256 currentAmount = amountIn;
        for (uint256 i = 0; i < steps.length; i++) {
            SwapStep memory step = steps[i];
            IERC20(step.tokenIn).approve(step.router, currentAmount);
            address[] memory path = new address[](2);
            path[0] = step.tokenIn;
            path[1] = step.tokenOut;
            uint256[] memory amounts = IDEXRouter(step.router).swapExactTokensForTokens(
                currentAmount, 0, path, address(this), block.timestamp
            );
            currentAmount = amounts[amounts.length - 1];
            if (currentAmount == 0) revert SwapFailed();
        }
        return currentAmount;
    }

    function _approveRepayment(IERC20 token, uint256 amount) internal {
        token.approve(msg.sender, amount);
    }

    function withdraw(address token, uint256 amount) external onlyOwner {
        if (token == address(0)) {
            (bool ok,) = payable(owner).call{value: amount}("");
            require(ok, "ETH transfer failed");
        } else {
            IERC20(token).transfer(owner, amount);
        }
    }

    function transferOwnership(address newOwner) external onlyOwner {
        owner = newOwner;
    }

    receive() external payable {}
}
