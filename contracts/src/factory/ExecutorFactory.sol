// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {FlashLoanArbitrage} from "../executors/FlashLoanArbitrage.sol";
import {SandwichExecutor} from "../executors/SandwichExecutor.sol";
import {LiquidationExecutor} from "../executors/LiquidationExecutor.sol";
import {JitLiquidityExecutor} from "../executors/JitLiquidityExecutor.sol";

contract ExecutorFactory {
    enum ExecutorType { FlashLoanArbitrage, Sandwich, Liquidation, JitLiquidity }

    event ExecutorDeployed(
        ExecutorType indexed kind,
        address indexed executor,
        address indexed deployer
    );

    mapping(ExecutorType => address) public executors;

    function deployExecutor(ExecutorType kind, bytes calldata initParams) external returns (address executor) {
        if (kind == ExecutorType.FlashLoanArbitrage) {
            (address balancerVault, address aavePool) = abi.decode(initParams, (address, address));
            FlashLoanArbitrage arb = new FlashLoanArbitrage(balancerVault, aavePool);
            executor = address(arb);
        } else if (kind == ExecutorType.Sandwich) {
            SandwichExecutor sandwich = new SandwichExecutor();
            executor = address(sandwich);
        } else if (kind == ExecutorType.Liquidation) {
            LiquidationExecutor liq = new LiquidationExecutor();
            executor = address(liq);
        } else if (kind == ExecutorType.JitLiquidity) {
            JitLiquidityExecutor jit = new JitLiquidityExecutor();
            executor = address(jit);
        }

        executors[kind] = executor;
        emit ExecutorDeployed(kind, executor, msg.sender);
    }

    function registerExecutor(ExecutorType kind, address executor) external {
        executors[kind] = executor;
        emit ExecutorDeployed(kind, executor, msg.sender);
    }

    function getExecutor(ExecutorType kind) external view returns (address) {
        return executors[kind];
    }
}
