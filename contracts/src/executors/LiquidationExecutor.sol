// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC20} from "forge-std/interfaces/IERC20.sol";
import {SafeTransferLib} from "forge-std/SafeTransferLib.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import {IFlashLoanProvider} from "../interfaces/IFlashLoanProvider.sol";
import {IDEXRouter} from "../interfaces/IDEXRouter.sol";
import {IAaveV3Pool} from "../interfaces/IAaveV3Pool.sol";
import {IBalancerVault} from "../interfaces/IBalancerVault.sol";

contract LiquidationExecutor is ReentrancyGuard {
    using SafeTransferLib for address;

    error NotOwner();
    error LiquidationCallFailed();
    error SwapFailed();

    address public owner;

    event LiquidationExecuted(
        address indexed user,
        address indexed debtToken,
        address indexed collateralToken,
        uint256 debtToCover,
        uint256 profit
    );

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    constructor() {
        owner = msg.sender;
    }

    function executeLiquidation(
        address user,
        address debtToken,
        address collateralToken,
        uint256 debtToCover,
        address aavePool,
        IFlashLoanProvider.ProviderType flashLoanProvider,
        uint256 minProfit
    ) external nonReentrant {
        if (flashLoanProvider == IFlashLoanProvider.ProviderType.AaveV3) {
            bytes memory params = abi.encode(
                user, debtToken, collateralToken, debtToCover, aavePool, minProfit, msg.sender
            );
            IAaveV3Pool(aavePool).flashLoanSimple(
                address(this), debtToken, debtToCover, params, 0
            );
        } else if (flashLoanProvider == IFlashLoanProvider.ProviderType.BalancerV2) {
            address balancerVault = IAaveV3Pool(aavePool).ADDRESSES_PROVIDER();
            address[] memory tokens = new address[](1);
            tokens[0] = debtToken;
            uint256[] memory amounts = new uint256[](1);
            amounts[0] = debtToCover;
            bytes memory userData = abi.encode(
                user, debtToken, collateralToken, debtToCover, aavePool, minProfit, msg.sender
            );
            IBalancerVault(balancerVault).flashLoan(address(this), tokens, amounts, userData);
        }
    }

    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external returns (bool) {
        (address user, address debtToken, address collateralToken, uint256 debtToCover,, address aavePool, uint256 minProfit) =
            abi.decode(params, (address, address, address, uint256, uint256, address, uint256));
        _performLiquidation(user, debtToken, collateralToken, debtToCover, aavePool, minProfit, amount, premium);
        return true;
    }

    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external {
        (address user, address debtToken, address collateralToken, uint256 debtToCover,, address aavePool, uint256 minProfit) =
            abi.decode(userData, (address, address, address, uint256, uint256, address, uint256));
        _performLiquidation(user, debtToken, collateralToken, debtToCover, aavePool, minProfit, amounts[0], feeAmounts[0]);
    }

    function _performLiquidation(
        address user,
        address debtToken,
        address collateralToken,
        uint256 debtToCover,
        address aavePool,
        uint256 minProfit,
        uint256 borrowedAmount,
        uint256 fee
    ) internal {
        IERC20(debtToken).approve(aavePool, borrowedAmount);

        IAaveV3Pool(aavePool).liquidationCall(
            collateralToken, debtToken, user, debtToCover, false
        );

        uint256 seized = IERC20(collateralToken).balanceOf(address(this));
        if (seized == 0) revert LiquidationCallFailed();

        address[] memory path = new address[](2);
        path[0] = collateralToken;
        path[1] = debtToken;
        IERC20(collateralToken).approve(debtToken, seized);
        uint256[] memory amounts = IDEXRouter(debtToken).swapExactTokensForTokens(
            seized, 0, path, address(this), block.timestamp
        );
        uint256 swappedBack = amounts[amounts.length - 1];

        uint256 profit = swappedBack - borrowedAmount - fee;
        if (profit < minProfit) revert SwapFailed();

        IERC20(debtToken).approve(msg.sender, borrowedAmount + fee);

        emit LiquidationExecuted(user, debtToken, collateralToken, debtToCover, profit);
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
