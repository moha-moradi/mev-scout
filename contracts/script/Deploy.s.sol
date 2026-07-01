// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "forge-std/Script.sol";
import "forge-std/console.sol";
import {FlashLoanArbitrage} from "../src/executors/FlashLoanArbitrage.sol";
import {SandwichExecutor} from "../src/executors/SandwichExecutor.sol";
import {LiquidationExecutor} from "../src/executors/LiquidationExecutor.sol";
import {JitLiquidityExecutor} from "../src/executors/JitLiquidityExecutor.sol";
import {ExecutorFactory} from "../src/factory/ExecutorFactory.sol";

contract DeployScript is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        vm.startBroadcast(deployerPrivateKey);

        address balancerVault = vm.envAddress("BALANCER_VAULT");
        address aavePool = vm.envAddress("AAVE_POOL");

        FlashLoanArbitrage arb = new FlashLoanArbitrage(balancerVault, aavePool);
        SandwichExecutor sandwich = new SandwichExecutor();
        LiquidationExecutor liq = new LiquidationExecutor();
        JitLiquidityExecutor jit = new JitLiquidityExecutor();

        ExecutorFactory factory = new ExecutorFactory();
        factory.registerExecutor(ExecutorFactory.ExecutorType.FlashLoanArbitrage, address(arb));
        factory.registerExecutor(ExecutorFactory.ExecutorType.Sandwich, address(sandwich));
        factory.registerExecutor(ExecutorFactory.ExecutorType.Liquidation, address(liq));
        factory.registerExecutor(ExecutorFactory.ExecutorType.JitLiquidity, address(jit));

        console.log("Factory deployed at:", address(factory));

        vm.stopBroadcast();
    }
}
