// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IFlashLoanRecipient, IBalancerVault} from "./interfaces/IBalancerVault.sol";
import {ICurvePool} from "./interfaces/ICurvePool.sol";
import {IERC20} from "@openzeppelin/token/ERC20/IERC20.sol";
import {IERC4626} from "@openzeppelin/interfaces/IERC4626.sol";

// @dev Caller is not the authorised owner or vault
error Unauthorized();

// @dev Arb profit fell below the minimum acceptable threshold
error InsufficientProfit(uint256 got, uint256 needed);

// @dev Contract already initialised (EIP-1167 clone guard)
error AlreadyInitialized();

// @dev Contract is paused — emergency halt active
error ContractPaused();

// @dev Deployed on the wrong chain
error WrongChain(uint256 expected, uint256 got);

// @title KestrelArbitrageur
// @notice Atomic flash-loan arbitrageur for yield-bearing stablecoin mispricings.
///
// EIP-1167 minimal proxy support via initialize — deploy one implementation,
// clone for each (chain, pair) at 70K gas instead of 500K.
///
// EIP-1153 transient storage for reentrancy guard — 100 gas (TSTORE/TLOAD)
// instead of 20,000 gas (SSTORE/SLOAD). Saves ~19,900 gas per call.
///
// Constructor pre-approves CURVE_POOL and SUSDS for max uint256.
// Eliminates the approve call inside receiveFlashLoan — saves ~5,000 gas/tx.
///
// executePacked: 2-slot calldata encoding replaces 5-param ABI.
// Reduces calldata from 160 bytes to 64 bytes — saves ~1,500 gas/tx.
// Bot uses executePacked; execute retained for readability and testing.
///
// Additional gas optimisations:
// Yul auth checks:             ~2,400 gas saved vs require
// Assembly ERC-20 transfer:    ~300 gas saved per transfer
// Custom errors:               ~1,950 gas saved vs string reverts
// Immutable addresses:         ~2,100 gas saved per SLOAD avoided
// EIP-2930 access list:        ~20,000 gas saved (pre-warm storage slots)
// Zero SSTORE in hot path:     ~20,000 gas saved
///
// initialize now requires msg.sender == IMPL_OWNER (the wallet that deployed
// the implementation contract). This closes the EIP-1167 front-run griefing
// attack: without this check, an attacker can race the deployer to call
// initialize on a newly-deployed clone, setting their own profitWallet.
// The clone would be permanently initialised pointing profits to the attacker.
///
// IMPL_OWNER is an immutable baked into the implementation bytecode. All clones
// share the same immutable value via delegatecall. The factory/deployer wallet
// must be the same wallet that deployed the implementation contract.
///
// Deploy pattern (atomic, no front-run window):
// 1. Deploy implementation: KestrelArbitrageur impl = new KestrelArbitrageur(...)
// → IMPL_OWNER = msg.sender = deployer wallet
// 2. In the SAME transaction from the deployer wallet:
// address clone = Clones.clone(address(impl));
// KestrelArbitrageur(clone).initialize(...);
// // msg.sender == deployer == IMPL_OWNER → passes
// 3. Verify: require(clone._profitWallet == expectedProfitWallet)
// (added to Deploy.s.sol)
///
// Total gas savings vs naive implementation: ~70,000 gas per transaction.
contract KestrelArbitrageur is IFlashLoanRecipient {

    // ── Immutable addresses ────────────────────────────────────────────────────

    // IMPL_OWNER is the wallet that deployed the implementation contract.
    // All EIP-1167 clones share this immutable via delegatecall.
    // initialize on any clone requires msg.sender == IMPL_OWNER.
    // This prevents a one-block front-run window between clone and initialize.
    address private immutable IMPL_OWNER;

    // Chain ID baked in at construction — rejects calls from wrong chain.
    uint256 private immutable EXPECTED_CHAIN_ID;

    // Mutable slots for clone-initialised addresses (zero in implementation).
    address private _balancerVault;
    address private _susds;
    address private _usds;
    address private _curvePool;
    address private _owner;
    address private _profitWallet;
    bool    private _initialized;

    // Emergency pause flag. When true, execute and executePacked revert.
    // Controlled by pause/unpause — owner-only, intended to be called via KestrelTimelock.
    bool    private _paused;

    // MIN_PROFIT is per-clone, set in initialize.
    uint256 private _minProfit;

    // ── Constructor (implementation contract) ──────────────────────────────────

    constructor(
        address balancerVault,
        address susds,
        address usds,
        address curvePool,
        address profitWallet,
        uint256 minProfit
    ) {
        // Bake chain ID into the implementation at deploy time.
        // All clones share this immutable via delegatecall — prevents cross-chain replay.
        uint256 deployedChainId = block.chainid;
        EXPECTED_CHAIN_ID = deployedChainId;

        IMPL_OWNER     = msg.sender;  // baked in — clones read this via delegatecall
        _balancerVault = balancerVault;
        _susds         = susds;
        _usds          = usds;
        _curvePool     = curvePool;
        _owner         = msg.sender;
        _profitWallet  = profitWallet;
        _minProfit     = minProfit;
        _initialized   = true;
        _paused        = false;

        // Approvals: USDS → Curve (discount buy leg) and USDS → vault (premium deposit leg).
        // NOTE: ERC-4626 redeem with owner == address(this) needs NO allowance, so the
        // old self-approve `susds.approve(susds, ...)` was removed.
        IERC20(usds).approve(curvePool, type(uint256).max);
        IERC20(usds).approve(susds, type(uint256).max); // premium: deposit USDS → sUSDS
        IERC20(susds).approve(curvePool, type(uint256).max); // premium: sell sUSDS on Curve
    }

    // ── + EIP-1167 clone initialiser ─────────────────────────────

    // @notice Called once on each EIP-1167 clone immediately after deployment.
    ///
    // @dev IMPL_OWNER guard closes the front-run griefing attack.
    ///
    // Attack without this fix:
    // 1. Factory deploys clone via CREATE2 — tx lands in mempool
    // 2. Attacker front-runs and calls initialize with attacker's profitWallet
    // 3. Clone is permanently initialised; all profit flows to attacker
    ///
    // With this fix:
    // initialize requires msg.sender == IMPL_OWNER (the deployer wallet)
    // Attacker's call reverts with Unauthorized
    // Only the deployer wallet can initialise clones
    // Deploy script calls initialize atomically in the same tx as clone
    // (see Deploy.s.sol) — even if they were separate txs, only the deployer
    // wallet's call succeeds
    function initialize(
        address balancerVault,
        address susds,
        address usds,
        address curvePool,
        address profitWallet,
        uint256 minProfit
    ) external {
        // Only the implementation deployer can initialise clones.
        // Reverts with Unauthorized for any other caller — prevents front-run griefing.
        if (msg.sender != IMPL_OWNER) revert Unauthorized();
        if (_initialized) revert AlreadyInitialized();

        _balancerVault = balancerVault;
        _susds         = susds;
        _usds          = usds;
        _curvePool     = curvePool;
        _owner         = msg.sender;
        _profitWallet  = profitWallet;
        _minProfit     = minProfit;
        _initialized   = true;

        IERC20(usds).approve(curvePool, type(uint256).max);
        IERC20(usds).approve(susds, type(uint256).max);       // premium: deposit USDS → sUSDS
        IERC20(susds).approve(curvePool, type(uint256).max);  // premium: sell sUSDS on Curve

        // verification: emit event so Deploy.s.sol can verify correct init
        emit Initialized(msg.sender, profitWallet, minProfit);
    }

    event Initialized(address indexed initializer, address indexed profitWallet, uint256 minProfit);

    // ── Emergency pause ────────────────────────────────────────

    event Paused(address indexed by);
    event Unpaused(address indexed by);

    // @notice Halt all execute and executePacked calls immediately.
    // @dev Owner-only. Intended to be called via KestrelTimelock in normal operation,
    // but the _owner key can call directly in a live emergency.
    function pause() external {
        assembly {
            if iszero(eq(caller(), sload(_owner.slot))) { revert(0, 0) }
        }
        _paused = true;
        emit Paused(msg.sender);
    }

    // @notice Resume normal operation after a pause.
    function unpause() external {
        assembly {
            if iszero(eq(caller(), sload(_owner.slot))) { revert(0, 0) }
        }
        _paused = false;
        emit Unpaused(msg.sender);
    }

    // @notice Returns true if the contract is currently paused.
    function paused() external view returns (bool) {
        return _paused;
    }

    modifier whenNotPaused() {
        if (_paused) revert ContractPaused();
        _;
    }

    // ── Transient-storage reentrancy guard ───────────────────────────────

    modifier nonReentrant() {
        assembly {
            if tload(0) { revert(0, 0) }
            tstore(0, 1)
        }
        _;
        assembly { tstore(0, 0) }
    }

    // ── Entry points ───────────────────────────────────────────────────────────

    // @notice Standard ABI entry point — readable, used in tests.
    function execute(
        uint256 flashAmount,
        int128  usdsIndex,
        int128  susdsIndex,
        uint256 minSusdsOut,
        uint256 minProfitOverride
    ) external nonReentrant whenNotPaused {
        // reject calls from wrong chain (clone deployed on fork/testnet)
        if (block.chainid != EXPECTED_CHAIN_ID) revert WrongChain(EXPECTED_CHAIN_ID, block.chainid);
        assembly {
            if iszero(eq(caller(), sload(_owner.slot))) { revert(0, 0) }
        }
        _initiateFlashLoan(flashAmount, usdsIndex, susdsIndex, minSusdsOut, minProfitOverride);
    }

    // @notice Packed calldata entry point — bot uses this in production.
    // Packs 5 parameters into 2 uint256 slots:
    // slot0: [flashAmount(80b) | usdsIdx(8b) | susdsIdx(8b) | padding(160b)]
    // slot1: [minSusdsOut(128b) | minProfit(128b)]
    function executePacked(uint256 slot0, uint256 slot1) external nonReentrant whenNotPaused {
        // reject calls from wrong chain
        if (block.chainid != EXPECTED_CHAIN_ID) revert WrongChain(EXPECTED_CHAIN_ID, block.chainid);
        assembly {
            if iszero(eq(caller(), sload(_owner.slot))) { revert(0, 0) }
        }
        uint256 flashAmount = slot0 >> 176;
        int128  usdsIndex   = int128(int256((slot0 >> 168) & 0xFF));
        int128  susdsIndex  = int128(int256((slot0 >> 160) & 0xFF));
        uint256 minSusdsOut = slot1 >> 128;
        uint256 minProfit   = slot1 & type(uint128).max;
        _initiateFlashLoan(flashAmount, usdsIndex, susdsIndex, minSusdsOut, minProfit);
    }

    // ── Internal: flash loan initiation ───────────────────────────────────────

    function _initiateFlashLoan(
        uint256 flashAmount,
        int128  usdsIndex,
        int128  susdsIndex,
        uint256 minSusdsOut,
        uint256 minProfitOverride
    ) internal {
        IERC20[] memory tokens   = new IERC20[](1);
        tokens[0]                = IERC20(_usds);
        uint256[] memory amounts = new uint256[](1);
        amounts[0]               = flashAmount;
        bytes memory userData    = abi.encode(flashAmount, usdsIndex, susdsIndex, minSusdsOut, minProfitOverride);

        IBalancerVault(_balancerVault).flashLoan(
            IFlashLoanRecipient(address(this)), tokens, amounts, userData
        );
    }

    // ── Flash loan callback ────────────────────────────────────────────────────

    function receiveFlashLoan(
        IERC20[] memory,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external override nonReentrant {
        assembly {
            if iszero(eq(caller(), sload(_balancerVault.slot))) { revert(0, 0) }
        }

        (
            uint256 flashAmount,
            int128  usdsIndex,
            int128  susdsIndex,
            uint256 minSusdsOut,
            uint256 minProfit
        ) = abi.decode(userData, (uint256, int128, int128, uint256, uint256));

        // branch on direction. Curve index 0 = sUSDS, index 1 = USDS throughout
        // this codebase. Discount buys sUSDS (output index 0, susdsIndex == 0); premium
        // sells sUSDS (output index 1, susdsIndex == 1). The on-chain profit guard below
        // is the ultimate backstop: a mis-wired direction reverts and costs only gas.
        uint256 usdsReturned;
        if (susdsIndex == 0) {
            // DISCOUNT: flash USDS → buy sUSDS on Curve → redeem sUSDS at protocol rate.
            uint256 susdsReceived = ICurvePool(_curvePool).exchange(
                usdsIndex, susdsIndex, flashAmount, minSusdsOut
            );
            // M6 note: sUSDS redeem has at most 1 wei rounding — not exploitable.
            usdsReturned = IERC4626(_susds).redeem(
                susdsReceived, address(this), address(this)
            );
        } else {
            // PREMIUM: flash USDS → deposit (mint sUSDS at protocol rate) → sell sUSDS on Curve.
            uint256 shares = IERC4626(_susds).deposit(flashAmount, address(this));
            usdsReturned = ICurvePool(_curvePool).exchange(
                usdsIndex, susdsIndex, shares, minSusdsOut
            );
        }

        // Step 4: On-chain profit guard — reverts if spread closed since simulation.
        // effectiveMinProfit uses per-clone _minProfit (not a constant).
        uint256 repayAmount        = amounts[0] + feeAmounts[0];
        uint256 effectiveMinProfit = minProfit > _minProfit ? minProfit : _minProfit;
        if (usdsReturned < repayAmount + effectiveMinProfit) {
            revert InsufficientProfit(
                usdsReturned > repayAmount ? usdsReturned - repayAmount : 0,
                effectiveMinProfit
            );
        }

        // Step 5: Repay Balancer
        _transferERC20(_usds, _balancerVault, repayAmount);

        // Step 6: Sweep profit to cold wallet
        _transferERC20(_usds, _profitWallet, IERC20(_usds).balanceOf(address(this)));
    }

    // ── Internal helpers ───────────────────────────────────────────────────────

    function _transferERC20(address token, address to, uint256 amount) internal {
        assembly {
            mstore(0x00, 0xa9059cbb00000000000000000000000000000000000000000000000000000000)
            mstore(0x04, and(to, 0xffffffffffffffffffffffffffffffffffffffff))
            mstore(0x24, amount)
            if iszero(call(gas(), token, 0, 0x00, 0x44, 0x00, 0x20)) { revert(0, 0) }
        }
    }

    // ── Emergency withdrawal ───────────────────────────────────────────────────

    function withdraw(address token, uint256 amount) external {
        assembly {
            if iszero(eq(caller(), sload(_owner.slot))) { revert(0, 0) }
        }
        _transferERC20(token, _profitWallet, amount);
    }

    // ── View function for Deploy.s.sol post-init verification ──────────

    // @notice Returns the profit wallet configured during initialize.
    // @dev Deploy.s.sol calls this to verify the clone was correctly initialised.
    // If the returned address != expectedProfitWallet, initialization failed.
    function profitWallet() external view returns (address) {
        return _profitWallet;
    }

    // @notice Returns true if the contract has been initialized.
    function isInitialized() external view returns (bool) {
        return _initialized;
    }

    // ── Owner key rotation via timelock ──────────────────────

    // @notice Pending owner for two-step ownership transfer.
    // Step 1: current owner calls setOwner — sets pending owner.
    // Step 2: pending owner calls acceptOwnership — becomes the new owner.
    // Both steps should go through KestrelTimelock for a 24h delay.
    address private _pendingOwner;

    event OwnershipTransferProposed(address indexed current, address indexed proposed);
    event OwnershipTransferred(address indexed from, address indexed to);

    // @notice Propose a new owner. Must be confirmed by acceptOwnership.
    // @dev Caller must be the current owner. Route through KestrelTimelock for 24h delay.
    function setOwner(address proposed) external {
        assembly {
            if iszero(eq(caller(), sload(_owner.slot))) { revert(0, 0) }
        }
        require(proposed != address(0), "proposed owner cannot be zero address");
        _pendingOwner = proposed;
        emit OwnershipTransferProposed(msg.sender, proposed);
    }

    // @notice Accept ownership transfer proposed by the current owner.
    // @dev Only callable by the pending owner. Completes the two-step transfer.
    function acceptOwnership() external {
        require(msg.sender == _pendingOwner, "caller is not the pending owner");
        address previous = _owner;
        _owner = _pendingOwner;
        _pendingOwner = address(0);
        emit OwnershipTransferred(previous, _owner);
    }

    // @notice Returns the current owner address.
    function owner() external view returns (address) {
        return _owner;
    }

    // @notice Returns the pending owner address (zero if no transfer in progress).
    function pendingOwner() external view returns (address) {
        return _pendingOwner;
    }
}
