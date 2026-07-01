// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "forge-std/Test.sol";
import {JitLiquidityExecutor} from "../src/executors/JitLiquidityExecutor.sol";

contract JitLiquidityExecutorTest is Test {
    JitLiquidityExecutor public jit;

    function setUp() public {
        jit = new JitLiquidityExecutor();
    }

    function test_owner_is_deployer() public {
        assertEq(jit.owner(), address(this));
    }

    function test_withdraw_reverts_for_non_owner() public {
        vm.prank(address(0xbebe));
        vm.expectRevert();
        jit.withdraw(address(0), 0);
    }
}
