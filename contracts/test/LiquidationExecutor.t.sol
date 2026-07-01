// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "forge-std/Test.sol";
import {LiquidationExecutor} from "../src/executors/LiquidationExecutor.sol";

contract LiquidationExecutorTest is Test {
    LiquidationExecutor public liq;

    function setUp() public {
        liq = new LiquidationExecutor();
    }

    function test_owner_is_deployer() public {
        assertEq(liq.owner(), address(this));
    }

    function test_withdraw_reverts_for_non_owner() public {
        vm.prank(address(0xbebe));
        vm.expectRevert();
        liq.withdraw(address(0), 0);
    }
}
