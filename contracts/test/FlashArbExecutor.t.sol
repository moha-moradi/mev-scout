// SPDX-License-Identifier: MIT
pragma solidity 0.8.24;

import {FlashArbExecutor} from "../src/FlashArbExecutor.sol";

interface Vm {
    function envOr(string calldata name, string calldata defaultValue) external returns (string memory);
    function envOr(string calldata name, uint256 defaultValue) external returns (uint256);
    function createSelectFork(string calldata urlOrAlias, uint256 blockNumber) external returns (uint256);
    function deal(address account, uint256 newBalance) external;
}

/// Local mocks plus an optional mainnet-fork two-hop. The fork leg runs only
/// when `FORK_RPC_URL` is set, so `forge test` stays green offline.
contract FlashArbExecutorTest {
    Vm internal constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    FlashArbExecutor internal exec;
    MockVault internal vault;
    MockERC20 internal weth;
    MockERC20 internal usdc;
    MockV2Pair internal cheap;
    MockV2Pair internal rich;

    address internal searcher = address(0x5EAE);

    function setUp() public {
        exec = new FlashArbExecutor();
        vault = new MockVault();
        weth = new MockERC20();
        usdc = new MockERC20();
        // WETH is also the unwrap target: teach the mock to pay ETH out.
        weth.setWithdrawer();
        cheap = new MockV2Pair(address(weth), address(usdc));
        rich = new MockV2Pair(address(weth), address(usdc));

        // cheap: 10 WETH / 20_000 USDC (WETH priced ~2000).
        // rich:  10 WETH / 30_000 USDC (WETH priced ~3000).
        // Sell WETH on rich, buy it back on cheap.
        weth.mint(address(cheap), 10 ether);
        usdc.mint(address(cheap), 20_000 ether);
        cheap.sync();
        weth.mint(address(rich), 10 ether);
        usdc.mint(address(rich), 30_000 ether);
        rich.sync();
        // Vault inventory for the flash loan.
        weth.mint(address(vault), 100 ether);
        vm.deal(address(weth), 100 ether);
    }

    function test_abi_version_and_selector() public view {
        assert(exec.ABI_VERSION() == 1);
        bytes4 sel = bytes4(
            keccak256(
                "execute((address,address,uint256,(address,address,address,uint8,uint24)[],address,address))"
            )
        );
        assert(sel == FlashArbExecutor.execute.selector);
    }

    function test_two_hop_repays_and_unwraps_profit() public {
        uint256 before = searcher.balance;
        FlashArbExecutor.Leg[] memory legs = _cycle();

        exec.execute(
            FlashArbExecutor.Plan({
                vault: address(vault),
                borrowToken: address(weth),
                amount: 1 ether,
                legs: legs,
                profitTo: searcher,
                weth: address(weth)
            })
        );

        assert(searcher.balance > before);
        assert(weth.balanceOf(address(exec)) == 0);
        // Vault got the principal back (fee 0).
        assert(weth.balanceOf(address(vault)) == 100 ether);
    }

    function test_second_leg_on_consumed_pool_earns_less() public {
        FlashArbExecutor.Leg[] memory legs = _cycle();
        uint256 before = searcher.balance;
        exec.execute(_plan(legs, 0.05 ether));
        uint256 first = searcher.balance - before;

        uint256 mid = searcher.balance;
        exec.execute(_plan(legs, 0.05 ether));
        uint256 second = searcher.balance - mid;
        // Same pools, same size: the first take moved the price.
        assert(second < first);
    }

    function test_repay_shortfall_reverts() public {
        FlashArbExecutor.Leg[] memory legs = new FlashArbExecutor.Leg[](2);
        // Sell WETH where it is cheap, buy it back where it is rich — output < repay.
        legs[0] = FlashArbExecutor.Leg({
            pool: address(cheap),
            tokenIn: address(weth),
            tokenOut: address(usdc),
            kind: 0,
            fee: 30
        });
        legs[1] = FlashArbExecutor.Leg({
            pool: address(rich),
            tokenIn: address(usdc),
            tokenOut: address(weth),
            kind: 0,
            fee: 30
        });
        (bool ok,) = address(exec).call(abi.encodeCall(FlashArbExecutor.execute, (_plan(legs, 1 ether))));
        assert(!ok);
        assert(searcher.balance == 0);
    }

    /// V3 exact-input: sell WETH at 3000 USDC, buy it back at 2000. Leftover
    /// WETH is unwrapped to the searcher.
    function test_v3_two_hop_repays_and_unwraps_profit() public {
        MockV3Pool sell = new MockV3Pool(address(weth), address(usdc), 3000 ether);
        // 3000e18 * (1e18/2000) / 1e18 = 1.5e18 WETH out.
        MockV3Pool buy = new MockV3Pool(address(weth), address(usdc), 1 ether / 2000);
        usdc.mint(address(sell), 1_000_000 ether);
        weth.mint(address(buy), 100 ether);

        FlashArbExecutor.Leg[] memory legs = new FlashArbExecutor.Leg[](2);
        legs[0] = FlashArbExecutor.Leg({
            pool: address(sell),
            tokenIn: address(weth),
            tokenOut: address(usdc),
            kind: 1,
            fee: 3000
        });
        legs[1] = FlashArbExecutor.Leg({
            pool: address(buy),
            tokenIn: address(usdc),
            tokenOut: address(weth),
            kind: 1,
            fee: 3000
        });

        uint256 before = searcher.balance;
        exec.execute(
            FlashArbExecutor.Plan({
                vault: address(vault),
                borrowToken: address(weth),
                amount: 1 ether,
                legs: legs,
                profitTo: searcher,
                weth: address(weth)
            })
        );
        assert(searcher.balance > before);
        assert(weth.balanceOf(address(exec)) == 0);
        assert(weth.balanceOf(address(vault)) == 100 ether);
    }

    function test_v3_callback_rejects_stranger() public {
        (bool ok,) = address(exec).call(
            abi.encodeCall(
                FlashArbExecutor.uniswapV3SwapCallback, (int256(1), int256(0), abi.encode(address(weth)))
            )
        );
        assert(!ok);
    }

    /// Optional. Set `FORK_RPC_URL` (and optionally `FORK_BLOCK`) to run a
    /// WETH two-hop against Ethereum mainnet state. Skips when unset.
    function test_fork_weth_two_hop() public {
        string memory rpc = vm.envOr("FORK_RPC_URL", string(""));
        if (bytes(rpc).length == 0) return;
        uint256 blockNo = vm.envOr("FORK_BLOCK", uint256(19_000_000));
        vm.createSelectFork(rpc, blockNo);

        address forkVault = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
        address forkWeth = 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;
        address forkUsdc = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;
        address uni = 0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc;
        address sushi = 0x397FF1542f962076d0BFE58eA045FfA2d347ACa0;

        FlashArbExecutor forkExec = new FlashArbExecutor();
        uint256 amount = 1 ether;
        (address buy, address sell, bool profitable) =
            _forkDirection(uni, sushi, forkWeth, forkUsdc, amount);
        if (!profitable) return;

        uint256 before = searcher.balance;
        FlashArbExecutor.Leg[] memory legs = new FlashArbExecutor.Leg[](2);
        legs[0] = FlashArbExecutor.Leg({pool: buy, tokenIn: forkWeth, tokenOut: forkUsdc, kind: 0, fee: 30});
        legs[1] = FlashArbExecutor.Leg({pool: sell, tokenIn: forkUsdc, tokenOut: forkWeth, kind: 0, fee: 30});
        forkExec.execute(
            FlashArbExecutor.Plan({
                vault: forkVault,
                borrowToken: forkWeth,
                amount: amount,
                legs: legs,
                profitTo: searcher,
                weth: forkWeth
            })
        );
        assert(searcher.balance > before);
    }

    function _cycle() internal view returns (FlashArbExecutor.Leg[] memory legs) {
        legs = new FlashArbExecutor.Leg[](2);
        // Sell WETH on the richer pool, buy it back on the cheaper one.
        legs[0] = FlashArbExecutor.Leg({
            pool: address(rich),
            tokenIn: address(weth),
            tokenOut: address(usdc),
            kind: 0,
            fee: 30
        });
        legs[1] = FlashArbExecutor.Leg({
            pool: address(cheap),
            tokenIn: address(usdc),
            tokenOut: address(weth),
            kind: 0,
            fee: 30
        });
    }

    function _plan(FlashArbExecutor.Leg[] memory legs, uint256 amount)
        internal
        view
        returns (FlashArbExecutor.Plan memory)
    {
        return FlashArbExecutor.Plan({
            vault: address(vault),
            borrowToken: address(weth),
            amount: amount,
            legs: legs,
            profitTo: searcher,
            weth: address(weth)
        });
    }

    function _forkDirection(address uni, address sushi, address forkWeth, address forkUsdc, uint256 amount)
        internal
        view
        returns (address buy, address sell, bool profitable)
    {
        uint256 viaUni = _quoteV2(uni, forkWeth, forkUsdc, amount);
        uint256 backSushi = _quoteV2(sushi, forkUsdc, forkWeth, viaUni);
        uint256 viaSushi = _quoteV2(sushi, forkWeth, forkUsdc, amount);
        uint256 backUni = _quoteV2(uni, forkUsdc, forkWeth, viaSushi);
        if (backSushi > amount && backSushi >= backUni) return (uni, sushi, true);
        if (backUni > amount) return (sushi, uni, true);
        return (uni, sushi, false);
    }

    function _quoteV2(address pool, address tokenIn, address tokenOut, uint256 amountIn)
        internal
        view
        returns (uint256)
    {
        (uint112 r0, uint112 r1,) = MockV2Pair(pool).getReserves();
        address token0 = MockV2Pair(pool).token0();
        (uint256 reserveIn, uint256 reserveOut) =
            tokenIn == token0 ? (uint256(r0), uint256(r1)) : (uint256(r1), uint256(r0));
        tokenOut;
        uint256 amountInWithFee = amountIn * 9970;
        return (amountInWithFee * reserveOut) / (reserveIn * 10_000 + amountInWithFee);
    }
}

contract MockERC20 {
    mapping(address => uint256) public balanceOf;
    bool public withdrawEnabled;
    address public sink;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function setWithdrawer() external {
        withdrawEnabled = true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function withdraw(uint256 amount) external {
        require(withdrawEnabled, "not weth");
        balanceOf[msg.sender] -= amount;
        (bool ok,) = msg.sender.call{value: amount}("");
        require(ok, "withdraw");
    }

    receive() external payable {}
}

contract MockV2Pair {
    address payable public token0;
    address payable public token1;
    uint112 public reserve0;
    uint112 public reserve1;

    constructor(address a, address b) {
        if (a < b) {
            token0 = payable(a);
            token1 = payable(b);
        } else {
            token0 = payable(b);
            token1 = payable(a);
        }
    }

    function sync() external {
        reserve0 = uint112(MockERC20(token0).balanceOf(address(this)));
        reserve1 = uint112(MockERC20(token1).balanceOf(address(this)));
    }

    function getReserves() external view returns (uint112, uint112, uint32) {
        return (reserve0, reserve1, 0);
    }

    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata) external {
        require(amount0Out > 0 || amount1Out > 0, "insufficient output");
        require(amount0Out < reserve0 && amount1Out < reserve1, "liquidity");
        if (amount0Out > 0) MockERC20(token0).transfer(to, amount0Out);
        if (amount1Out > 0) MockERC20(token1).transfer(to, amount1Out);
        uint256 b0 = MockERC20(token0).balanceOf(address(this));
        uint256 b1 = MockERC20(token1).balanceOf(address(this));
        uint256 amount0In = b0 > uint256(reserve0) - amount0Out ? b0 - (uint256(reserve0) - amount0Out) : 0;
        uint256 amount1In = b1 > uint256(reserve1) - amount1Out ? b1 - (uint256(reserve1) - amount1Out) : 0;
        // 0.30% fee, same constant-product check as UniswapV2Pair.
        uint256 bal0Adj = b0 * 1000 - amount0In * 3;
        uint256 bal1Adj = b1 * 1000 - amount1In * 3;
        require(bal0Adj * bal1Adj >= uint256(reserve0) * uint256(reserve1) * 1_000_000, "K");
        reserve0 = uint112(b0);
        reserve1 = uint112(b1);
    }
}

/// Constant-price V3 pool. Pulls `tokenIn` via the executor callback, then pays
/// `amountIn * outPerIn / 1e18` of the other token.
contract MockV3Pool {
    address public token0;
    address public token1;
    uint256 public outPerIn;

    constructor(address a, address b, uint256 rate) {
        if (a < b) {
            token0 = a;
            token1 = b;
        } else {
            token0 = b;
            token1 = a;
        }
        outPerIn = rate;
    }

    function swap(address recipient, bool, int256 amountSpecified, uint160, bytes calldata data)
        external
        returns (int256 amount0, int256 amount1)
    {
        require(amountSpecified > 0, "exact in");
        uint256 amountIn = uint256(amountSpecified);
        address tokenIn = abi.decode(data, (address));
        address tokenOut = tokenIn == token0 ? token1 : token0;
        int256 d0 = tokenIn == token0 ? int256(amountIn) : int256(0);
        int256 d1 = tokenIn == token1 ? int256(amountIn) : int256(0);
        FlashArbExecutor(payable(msg.sender)).uniswapV3SwapCallback(d0, d1, data);
        uint256 amountOut = amountIn * outPerIn / 1e18;
        require(amountOut > 0, "zero out");
        MockERC20(payable(tokenOut)).transfer(recipient, amountOut);
        if (tokenIn == token0) return (int256(amountIn), -int256(amountOut));
        return (-int256(amountOut), int256(amountIn));
    }
}

contract MockVault {
    function flashLoan(
        address recipient,
        address[] calldata tokens,
        uint256[] calldata amounts,
        bytes calldata userData
    ) external {
        MockERC20(payable(tokens[0])).transfer(recipient, amounts[0]);
        uint256[] memory fees = new uint256[](amounts.length);
        FlashArbExecutor(payable(recipient)).receiveFlashLoan(tokens, amounts, fees, userData);
    }
}
