// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "@openzeppelin/token/ERC20/IERC20.sol";

// FIX (Audit2-Bug3A): Use ISwapRouter02 instead of IUniswapV3Pool.swap directly.
// Calling pool.swap requires implementing uniswapV3SwapCallback — if missing,
// Uniswap V3 will revert every time. ISwapRouter02.exactInputSingle handles the
// callback internally, so no callback implementation is needed here.
interface ISwapRouter02 {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24  fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata params)
        external returns (uint256 amountOut);
}

interface IAaveFlashLoan {
    function flashLoanSimple(address receiver, address asset, uint256 amount, bytes calldata params, uint16 referralCode) external;
}

interface IAaveSimpleFlashLoanReceiver {
    function executeOperation(address asset, uint256 amount, uint256 premium, address initiator, bytes calldata params) external returns (bool);
}

interface ICurvePool {
    function exchange(int128 i, int128 j, uint256 dx, uint256 min_dy) external returns (uint256);
}

error Unauthorized();
error InsufficientProfit(uint256 got, uint256 needed);

// @title KestrelSusdeArbitrageur
// @notice sUSDe/USDe cross-venue arbitrage on Arbitrum.
///
// FIX (Audit2-Bug3A): Replaced direct IUniswapV3Pool.swap with ISwapRouter02.exactInputSingle.
// The old code called pool.swap which triggers uniswapV3SwapCallback — since the contract
// didn't implement that interface, every swap would revert.
///
// FIX (Audit2-Bug3B): Added USDC → USDe swap step before Aave repayment.
// The old code had USDC after the Uniswap step but tried to repay Aave in USDe (which
// was zero at that point). The contract now converts USDC → USDe for repayment, then
// sweeps remaining USDC profit.
///
// Trade (corrected):
// 1. Flash borrow USDe from Aave V3 (0.05% fee)
// 2. Buy discounted sUSDe on Curve sUSDe/USDe
// 3. Sell sUSDe → USDC on Uniswap V3 via SwapRouter02 (no callback needed)
// 4. Convert USDC → USDe on Uniswap V3 for Aave repayment
// 5. Repay Aave flash loan in USDe
// 6. Sweep remaining USDC as profit
contract KestrelSusdeArbitrageur is IAaveSimpleFlashLoanReceiver {

    address private immutable AAVE_POOL;
    address private immutable SUSDE;
    address private immutable USDE;
    address private immutable USDC;
    address private immutable CURVE_SUSDE_POOL;
    // FIX (Audit2-Bug3A): SWAP_ROUTER replaces raw UNISWAP_SUSDE_POOL reference.
    // ISwapRouter02 on Arbitrum One: 0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45
    address private immutable SWAP_ROUTER;
    address private immutable OWNER;
    address private immutable PROFIT_WALLET;

    uint256 private constant MIN_PROFIT = 50e18; // $50 — Arbitrum gas is cheap

    constructor(
        address aavePool,
        address susde,
        address usde,
        address usdc,
        address curveSusdePool,
        address swapRouter,        // FIX: was uniswapSusdePool (pool address), now router
        address profitWallet
    ) {
        AAVE_POOL        = aavePool;
        SUSDE            = susde;
        USDE             = usde;
        USDC             = usdc;
        CURVE_SUSDE_POOL = curveSusdePool;
        SWAP_ROUTER      = swapRouter;
        OWNER            = msg.sender;
        PROFIT_WALLET    = profitWallet;

        // pattern: pre-approve all token flows at deploy time
        IERC20(usde).approve(curveSusdePool, type(uint256).max); // USDE → Curve
        IERC20(susde).approve(swapRouter,    type(uint256).max); // sUSDe → SwapRouter
        IERC20(usdc).approve(swapRouter,     type(uint256).max); // USDC → SwapRouter (for step 4)
        // FIX (Audit2-Bug3B): Pre-approve Aave for USDe repayment (avoids runtime approve)
        IERC20(usde).approve(aavePool,       type(uint256).max);
    }

    modifier onlyOwner() {
        if (msg.sender != OWNER) revert Unauthorized();
        _;
    }

    // @notice Execute the cross-venue sUSDe arbitrage.
    // @param flashAmount  USDe to borrow from Aave
    // @param minSusdeOut  Minimum sUSDe from Curve (slippage guard)
    // @param minUsdcOut   Minimum USDC from Uniswap (slippage guard)
    // @param minNetProfit Minimum net profit in USDC terms (6 decimals)
    function execute(
        uint256 flashAmount,
        uint256 minSusdeOut,
        uint256 minUsdcOut,
        uint256 minNetProfit
    ) external onlyOwner {
        bytes memory params = abi.encode(minSusdeOut, minUsdcOut, minNetProfit);
        IAaveFlashLoan(AAVE_POOL).flashLoanSimple(
            address(this), USDE, flashAmount, params, 0
        );
    }

    // @notice Aave flash loan callback.
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,       // Aave 0.05% fee in USDe terms
        address initiator,
        bytes calldata params
    ) external override returns (bool) {
        if (msg.sender != AAVE_POOL)    revert Unauthorized();
        if (initiator  != address(this)) revert Unauthorized();

        (uint256 minSusdeOut, uint256 minUsdcOut, uint256 minNetProfit) =
            abi.decode(params, (uint256, uint256, uint256));

        // Step 1: Buy cheap sUSDe on Curve (USDe → sUSDe)
        uint256 susdeReceived = ICurvePool(CURVE_SUSDE_POOL).exchange(
            0, 1, amount, minSusdeOut // i=USDe(0), j=sUSDe(1)
        );

        // Step 2: Sell sUSDe → USDC via Uniswap V3 SwapRouter02
        // FIX (Audit2-Bug3A): Using SwapRouter02.exactInputSingle avoids the need
        // to implement uniswapV3SwapCallback. The router handles the callback.
        uint256 usdcReceived = ISwapRouter02(SWAP_ROUTER).exactInputSingle(
            ISwapRouter02.ExactInputSingleParams({
                tokenIn:           SUSDE,
                tokenOut:          USDC,
                fee:               500,          // 0.05% fee tier on Arbitrum
                recipient:         address(this),
                amountIn:          susdeReceived,
                amountOutMinimum:  minUsdcOut,
                sqrtPriceLimitX96: 0
            })
        );

        // Step 3: FIX (Audit2-Bug3B): Convert USDC → USDe for Aave repayment.
        // After step 2 the contract holds USDC but needs USDe to repay Aave.
        // The old code incorrectly tried to approve & repay in USDe with zero balance.
        uint256 repayAmount = amount + premium;
        // Use the USDC/USDe 0.01% pool on Arbitrum for the back-conversion
        uint256 usdeFromUsdc = ISwapRouter02(SWAP_ROUTER).exactInputSingle(
            ISwapRouter02.ExactInputSingleParams({
                tokenIn:           USDC,
                tokenOut:          USDE,
                fee:               100,          // 0.01% stable pool — lowest cost path
                recipient:         address(this),
                amountIn:          usdcReceived,
                amountOutMinimum:  repayAmount,  // must cover full repay
                sqrtPriceLimitX96: 0
            })
        );

        // Step 4: Profit check (in USDe terms after full round-trip)
        uint256 usdeProfit = usdeFromUsdc > repayAmount ? usdeFromUsdc - repayAmount : 0;
        if (usdeProfit < minNetProfit) {
            revert InsufficientProfit(usdeProfit, minNetProfit);
        }

        // Step 5: Repay Aave — pre-approved in constructor, Aave pulls via transferFrom
        // (No explicit transfer needed; Aave calls transferFrom(this, aavePool, repayAmount))

        // Step 6: Sweep USDe profit to cold wallet
        uint256 profit = IERC20(USDE).balanceOf(address(this));
        if (profit > repayAmount) {
            _transferERC20(USDE, PROFIT_WALLET, profit - repayAmount);
        }

        return true;
    }

    function _transferERC20(address token, address to, uint256 amount) internal {
        assembly {
            mstore(0x00, 0xa9059cbb00000000000000000000000000000000000000000000000000000000)
            mstore(0x04, and(to, 0xffffffffffffffffffffffffffffffffffffffff))
            mstore(0x24, amount)
            if iszero(call(gas(), token, 0, 0x00, 0x44, 0x00, 0x20)) { revert(0, 0) }
        }
    }

    function withdraw(address token, uint256 amount) external onlyOwner {
        _transferERC20(token, PROFIT_WALLET, amount);
    }
}
