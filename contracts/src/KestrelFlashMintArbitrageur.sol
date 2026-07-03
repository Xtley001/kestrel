// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

// MakerDAO Flash Mint Arbitrageur (Zero-Fee, Large-Scale)
//
// DssFlash: 0% fee, unlimited DAI capacity (up to $500M+ per tx).
// DAI ↔ USDS 1:1 convertible via Sky PSM at zero cost.
// Target: rare >$50K events where Balancer $300–500M capacity is exhausted or
//         where the trade size ($50M–$200M) exceeds any single flash provider.
//
// Trade path:
//   1. Flash mint DAI from MakerDAO DssFlash (0% fee, no external liquidity needed)
//   2. Convert DAI → USDS via Sky PSM (1:1, 0 fee)
//   3. Buy sUSDS on Curve sUSDS/USDS at discount
//   4. Redeem sUSDS at canonical protocol rate (IERC4626.redeem)
//   5. Convert USDS → DAI via Sky PSM (1:1, 0 fee)
//   6. Repay DssFlash
//   7. Sweep profit (in DAI or USDS)

import {IERC20} from "@openzeppelin/token/ERC20/IERC20.sol";
import {IERC4626} from "@openzeppelin/interfaces/IERC4626.sol";
import {ICurvePool} from "./interfaces/ICurvePool.sol";

interface IDssFlash {
    // @dev ERC-3156 flashLoan interface used by DssFlash
    function flashLoan(
        address receiver,
        address token,
        uint256 amount,
        bytes calldata data
    ) external returns (bool);

    function max(address token) external view returns (uint256);
}

interface IERC3156FlashBorrower {
    function onFlashLoan(
        address initiator,
        address token,
        uint256 amount,
        uint256 fee,
        bytes calldata data
    ) external returns (bytes32);
}

interface ISkyPSM {
    // @notice Converts DAI to USDS at 1:1 (no fee)
    function daiToUsds(address usr, uint256 daiAmt) external;
    // @notice Converts USDS to DAI at 1:1 (no fee)
    function usdsToDai(address usr, uint256 usdsAmt) external;
}

error Unauthorized();
error InsufficientProfit(uint256 got, uint256 needed);
error ExceedsFlashMintCapacity(uint256 requested, uint256 available);

// @title KestrelFlashMintArbitrageur
// @notice Large-scale sUSDS arb using MakerDAO DssFlash (0% fee, $500M+ capacity).
// Unlocks $20M–$200M flash sizes that exceed Balancer's single-vault capacity.
// DAI ↔ USDS via Sky PSM at 1:1 peg — zero conversion cost.
///
// @dev Minimum viable trade: $50M (below this, Balancer handles it more simply).
// Maximum: DssFlash.max(DAI) — typically $500M+.
contract KestrelFlashMintArbitrageur is IERC3156FlashBorrower {

    bytes32 private constant CALLBACK_SUCCESS = keccak256("ERC3156FlashBorrower.onFlashLoan");

    address private immutable DSS_FLASH;
    address private immutable SKY_PSM;
    address private immutable DAI;
    address private immutable USDS;
    address private immutable SUSDS;
    address private immutable CURVE_POOL;
    address private immutable OWNER;
    address private immutable PROFIT_WALLET;

    // $50M absolute floor — below this, use Balancer instead
    uint256 private constant MIN_FLASH_SIZE = 50_000_000e18;
    // $500 minimum profit — never submit a trade that clears less
    uint256 private constant MIN_PROFIT     = 500e18;

    constructor(
        address dssFlash,
        address skyPsm,
        address dai,
        address usds,
        address susds,
        address curvePool,
        address profitWallet
    ) {
        DSS_FLASH    = dssFlash;
        SKY_PSM      = skyPsm;
        DAI          = dai;
        USDS         = usds;
        SUSDS        = susds;
        CURVE_POOL   = curvePool;
        OWNER        = msg.sender;
        PROFIT_WALLET = profitWallet;

        // pattern: pre-approve all token flows
        IERC20(dai).approve(dssFlash, type(uint256).max);   // repay DssFlash
        IERC20(dai).approve(skyPsm,   type(uint256).max);   // DAI → USDS
        IERC20(usds).approve(skyPsm,  type(uint256).max);   // USDS → DAI
        IERC20(usds).approve(curvePool, type(uint256).max); // USDS → Curve
        IERC20(susds).approve(susds,  type(uint256).max);   // sUSDS redeem
    }

    modifier onlyOwner() {
        // OWNER is immutable; immutables are not in storage, so read it in Solidity.
        if (msg.sender != OWNER) revert Unauthorized();
        _;
    }

    // ── Transient reentrancy guard ─────────────────────────────────────
    modifier nonReentrant() {
        assembly { if tload(0) { revert(0, 0) } tstore(0, 1) }
        _;
        assembly { tstore(0, 0) }
    }

    // @notice Execute a large-scale sUSDS arb via MakerDAO Flash Mint.
    // @param flashAmount  DAI to flash-mint (must be >= $50M, <= DssFlash.max(DAI))
    // @param usdsIndex    Curve index for USDS
    // @param susdsIndex   Curve index for sUSDS
    // @param minSusdsOut  Minimum sUSDS from Curve exchange (slippage guard)
    // @param minProfit    Minimum net profit in USDS (bot-computed from simulation)
    function execute(
        uint256 flashAmount,
        int128  usdsIndex,
        int128  susdsIndex,
        uint256 minSusdsOut,
        uint256 minProfit
    ) external onlyOwner nonReentrant {
        require(flashAmount >= MIN_FLASH_SIZE, "below $50M minimum - use Balancer for smaller sizes");

        uint256 capacity = IDssFlash(DSS_FLASH).max(DAI);
        if (flashAmount > capacity) {
            revert ExceedsFlashMintCapacity(flashAmount, capacity);
        }

        bytes memory data = abi.encode(usdsIndex, susdsIndex, minSusdsOut, minProfit);
        IDssFlash(DSS_FLASH).flashLoan(address(this), DAI, flashAmount, data);
    }

    // @notice ERC-3156 flash loan callback from DssFlash.
    // fee is always 0 from MakerDAO DssFlash — this is the key advantage.
    function onFlashLoan(
        address initiator,
        address token,
        uint256 amount,
        uint256 fee,      // Always 0 from DssFlash — confirmed at runtime
        bytes calldata data
    ) external override nonReentrant returns (bytes32) {
        // DSS_FLASH is immutable — compare in Solidity, not via sload.
        if (msg.sender != DSS_FLASH) revert Unauthorized();
        if (initiator != address(this)) revert Unauthorized();
        require(token == DAI, "unexpected token");
        require(fee == 0,     "DssFlash fee must be 0");

        (int128 usdsIndex, int128 susdsIndex, uint256 minSusdsOut, uint256 minProfit) =
            abi.decode(data, (int128, int128, uint256, uint256));

        // Step 1: Convert DAI → USDS via Sky PSM (1:1, zero cost)
        ISkyPSM(SKY_PSM).daiToUsds(address(this), amount);

        // Step 2: Buy sUSDS on Curve (no approve needed — pre-approved).
        // (L6: removed an unused previewRedeem read — the profit guard below is the check.)
        uint256 susdsReceived = ICurvePool(CURVE_POOL).exchange(
            usdsIndex, susdsIndex, amount, minSusdsOut
        );

        // Step 4: Redeem sUSDS at canonical protocol rate
        uint256 usdsReturned = IERC4626(SUSDS).redeem(
            susdsReceived, address(this), address(this)
        );

        // Step 5: Profit guard — net = usdsReturned - amount (DAI repay) - protocol_rate_loss
        uint256 effectiveMinProfit = minProfit > MIN_PROFIT ? minProfit : MIN_PROFIT;
        if (usdsReturned < amount + effectiveMinProfit) {
            revert InsufficientProfit(
                usdsReturned > amount ? usdsReturned - amount : 0,
                effectiveMinProfit
            );
        }

        // Step 6: Convert USDS back to DAI for repayment (1:1 via PSM)
        ISkyPSM(SKY_PSM).usdsToDai(address(this), amount); // exact repay amount

        // Step 7: Repay DssFlash (fee = 0, already approved in constructor)
        // DssFlash pulls repayment automatically — DAI allowance handles it

        // Step 8: Sweep remaining profit (in USDS) to cold wallet
        uint256 profit = IERC20(USDS).balanceOf(address(this));
        if (profit > 0) {
            _transferERC20(USDS, PROFIT_WALLET, profit);
        }

        return CALLBACK_SUCCESS;
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

    // @notice View DssFlash capacity for informational purposes
    function flashMintCapacity() external view returns (uint256) {
        return IDssFlash(DSS_FLASH).max(DAI);
    }
}
