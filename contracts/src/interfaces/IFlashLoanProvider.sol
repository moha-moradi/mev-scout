// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

interface IFlashLoanProvider {
    enum ProviderType { BalancerV2, AaveV3, UniswapV2, UniswapV3 }
}
