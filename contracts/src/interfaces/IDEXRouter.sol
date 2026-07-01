// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

interface IDEXRouter {
    function swapExactTokensForTokens(
        uint amountIn,
        uint amountOutMin,
        address[] calldata path,
        address to,
        uint deadline
    ) external returns (uint[] memory amounts);

    function exactInput(
        bytes calldata path,
        address to,
        uint deadline
    ) external returns (uint amountOut);
}
