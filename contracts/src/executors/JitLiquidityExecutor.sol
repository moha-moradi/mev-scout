// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC20} from "forge-std/interfaces/IERC20.sol";
import {SafeTransferLib} from "forge-std/SafeTransferLib.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {IDEXRouter} from "../interfaces/IDEXRouter.sol";
import {IUniswapV3Pool} from "../interfaces/IUniswapV3.sol";
import {IWETH} from "../interfaces/IWETH.sol";

contract JitLiquidityExecutor is ReentrancyGuard {
    using SafeTransferLib for address;

    error NotOwner();
    error MintFailed();
    error BurnFailed();
    error SwapFailed();

    address public owner;

    event JitExecuted(
        address indexed pool,
        int24 tickLower,
        int24 tickUpper,
        uint256 amount0Desired,
        uint256 amount1Desired,
        uint256 profit
    );

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    constructor() {
        owner = msg.sender;
    }

    function executeJit(
        address pool,
        int24 tickLower,
        int24 tickUpper,
        uint256 amount0Desired,
        uint256 amount1Desired,
        address swapRouter
    ) external nonReentrant returns (uint256 profit) {
        (, int24 currentTick,,,,,) = IUniswapV3Pool(pool).slot0();

        IUniswapV3Pool v3Pool = IUniswapV3Pool(pool);
        address token0 = v3Pool.token0();
        address token1 = v3Pool.token1();

        IERC20(token0).approve(pool, amount0Desired);
        IERC20(token1).approve(pool, amount1Desired);

        (uint256 mint0, uint256 mint1) = v3Pool.mint(
            address(this), tickLower, tickUpper, uint128(amount0Desired), ""
        );

        if (mint0 == 0 && mint1 == 0) revert MintFailed();

        (uint256 burn0, uint256 burn1) = v3Pool.burn(tickLower, tickUpper, type(uint128).max);

        if (burn0 == 0 && burn1 == 0) revert BurnFailed();

        v3Pool.collect(address(this), tickLower, tickUpper, type(uint128).max, type(uint128).max);

        uint256 balance0 = IERC20(token0).balanceOf(address(this));
        uint256 balance1 = IERC20(token1).balanceOf(address(this));

        uint256 initial0 = amount0Desired;
        uint256 initial1 = amount1Desired;

        profit = (balance0 > initial0 ? balance0 - initial0 : 0)
               + (balance1 > initial1 ? balance1 - initial1 : 0);

        if (profit > 0 && swapRouter != address(0)) {
            address profitToken = balance0 > initial0 ? token0 : token1;
            uint256 profitAmount = balance0 > initial0 ? balance0 - initial0 : balance1 - initial1;
            IERC20(profitToken).approve(swapRouter, profitAmount);
            address[] memory path = new address[](2);
            path[0] = profitToken;
            path[1] = token0;
            IDEXRouter(swapRouter).swapExactTokensForTokens(
                profitAmount, 0, path, address(this), block.timestamp
            );
        }

        emit JitExecuted(pool, tickLower, tickUpper, amount0Desired, amount1Desired, profit);
    }

    function withdraw(address token, uint256 amount) external onlyOwner {
        if (token == address(0)) {
            (bool ok,) = payable(owner).call{value: amount}("");
            require(ok, "ETH transfer failed");
        } else {
            IERC20(token).transfer(owner, amount);
        }
    }

    receive() external payable {}
}
