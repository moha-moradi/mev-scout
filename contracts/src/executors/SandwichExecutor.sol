// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC20} from "forge-std/interfaces/IERC20.sol";
import {SafeTransferLib} from "forge-std/SafeTransferLib.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {IDEXRouter} from "../interfaces/IDEXRouter.sol";

contract SandwichExecutor is ReentrancyGuard {
    using SafeTransferLib for address;

    error NotOwner();
    error FrontRunFailed();
    error BackRunFailed();
    error NotProfitable();

    address public owner;

    event SandwichExecuted(
        address indexed tokenIn,
        address indexed tokenOut,
        uint256 amountIn,
        uint256 profit
    );

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    constructor() {
        owner = msg.sender;
    }

    function executeSandwich(
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        address pool,
        address router,
        uint256 minProfit
    ) external nonReentrant returns (uint256 profit) {
        uint256 balanceBefore = IERC20(tokenOut).balanceOf(address(this));

        address[] memory path = new address[](2);
        path[0] = tokenOut;
        path[1] = tokenIn;

        IERC20(tokenOut).approve(router, amountIn);
        uint256[] memory amounts = IDEXRouter(router).swapExactTokensForTokens(
            amountIn, 0, path, address(this), block.timestamp
        );
        uint256 bought = amounts[amounts.length - 1];
        if (bought == 0) revert FrontRunFailed();

        path[0] = tokenIn;
        path[1] = tokenOut;
        IERC20(tokenIn).approve(router, bought);
        amounts = IDEXRouter(router).swapExactTokensForTokens(
            bought, 0, path, address(this), block.timestamp
        );
        uint256 received = amounts[amounts.length - 1];
        if (received == 0) revert BackRunFailed();

        uint256 balanceAfter = IERC20(tokenOut).balanceOf(address(this));
        profit = balanceAfter - balanceBefore;
        if (profit < minProfit) revert NotProfitable();

        emit SandwichExecuted(tokenIn, tokenOut, amountIn, profit);
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
