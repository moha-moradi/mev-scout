// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

interface IExecutorFactory {
    enum ExecutorType { FlashLoanArbitrage, Sandwich, Liquidation, JitLiquidity }

    function deployExecutor(ExecutorType kind, bytes calldata initParams) external returns (address);

    function registerExecutor(ExecutorType kind, address executor) external;

    function getExecutor(ExecutorType kind) external view returns (address);
}
