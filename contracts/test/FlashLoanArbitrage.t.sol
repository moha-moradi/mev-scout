// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "forge-std/Test.sol";
import {FlashLoanArbitrage} from "../src/executors/FlashLoanArbitrage.sol";
import {IFlashLoanProvider} from "../src/interfaces/IFlashLoanProvider.sol";

contract FlashLoanArbitrageTest is Test {
    FlashLoanArbitrage public arb;
    address constant BALANCER_VAULT = address(0x1);
    address constant AAVE_POOL = address(0x2);

    function setUp() public {
        arb = new FlashLoanArbitrage(BALANCER_VAULT, AAVE_POOL);
    }

    function test_owner_is_deployer() public {
        assertEq(arb.owner(), address(this));
    }

    function test_transfer_ownership() public {
        address newOwner = address(0xdead);
        arb.transferOwnership(newOwner);
        assertEq(arb.owner(), newOwner);
    }

    function test_only_owner_can_withdraw() public {
        vm.prank(address(0xbebe));
        vm.expectRevert();
        arb.withdraw(address(0), 0);
    }

    function test_revert_on_invalid_provider() public {
        IFlashLoanProvider.ProviderType invalid = IFlashLoanProvider.ProviderType(99);
        vm.expectRevert();
        arb.executeArbitrage(invalid, address(0), address(0), 0, 0, "");
    }
}
