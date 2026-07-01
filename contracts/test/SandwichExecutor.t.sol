// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "forge-std/Test.sol";
import {SandwichExecutor} from "../src/executors/SandwichExecutor.sol";

contract SandwichExecutorTest is Test {
    SandwichExecutor public sandwich;

    function setUp() public {
        sandwich = new SandwichExecutor();
    }

    function test_owner_is_deployer() public {
        assertEq(sandwich.owner(), address(this));
    }

    function test_withdraw_reverts_for_non_owner() public {
        vm.prank(address(0xbebe));
        vm.expectRevert();
        sandwich.withdraw(address(0), 0);
    }

    function test_execute_sandwich_reverts_with_zero_address_pool() public {
        vm.expectRevert();
        sandwich.executeSandwich(
            address(0xaa), address(0xbb), 1000, address(0), address(0xcc), 1
        );
    }
}
