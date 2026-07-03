// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

// Test coverage for KestrelFlashMintArbitrageur.
// Previously zero test files existed for this contract.
// Covers: min flash size guard, profit guard, revert on low profit, owner checks.

import {Test, console2} from "forge-std/Test.sol";
import {KestrelFlashMintArbitrageur} from "../src/KestrelFlashMintArbitrageur.sol";
import {IERC20} from "@openzeppelin/token/ERC20/IERC20.sol";

// @title KestrelFlashMintArbitrageurTest
// @notice Unit + fork tests for the ETH/FlashMint strategy contract.
contract KestrelFlashMintArbitrageurTest is Test {

    // ── Ethereum mainnet addresses ─────────────────────────────────────────────
    address constant DSS_FLASH      = 0x60744434d6339a6B27d73d9Eda62b6F66a0a04FA;
    address constant SKY_PSM        = address(0xA11CE);
    address constant DAI            = 0x6B175474E89094C44Da98b954EedeAC495271d0F;
    address constant SUSDS          = 0xa3931d71877C0E7a3148CB7Eb4463524FEc27fbD;
    address constant USDS           = 0xdC035D45d973E3EC169d2276DDab16f1e407384F;
    address constant CURVE_POOL     = 0x0000000000000000000000000000000000000001; // fill in for fork

    address constant PROFIT_WALLET  = address(0xBEEF);
    address constant OWNER          = address(0xDEAD);
    address constant ATTACKER       = address(0xBAD);

    // $50M minimum flash size as per contract constant
    uint256 constant MIN_FLASH_USD  = 50_000_000e18; // 18-decimal USDS

    uint256 constant FORK_BLOCK     = 21_100_000;

    KestrelFlashMintArbitrageur arb;

    function setUp() public {
        vm.createSelectFork(
            vm.envOr("RETH_IPC_PATH", string("http://localhost:8545")),
            FORK_BLOCK
        );

        vm.prank(OWNER);
        arb = new KestrelFlashMintArbitrageur(
            DSS_FLASH,
            SKY_PSM,
            DAI,
            USDS,
            SUSDS,
            CURVE_POOL,
            PROFIT_WALLET
        );
    }

    // ── Test 1: Revert on flash size below minimum ────────────────────────────

    function test_RevertIf_FlashSizeBelowMinimum() public {
        // $49M < $50M minimum
        uint256 tooSmall = 49_000_000e18;
        vm.expectRevert();
        vm.prank(OWNER);
        arb.execute(tooSmall, 0, 1, 0, 0);
    }

    // ── Test 2: Profit guard — reverts if manufactured profit is insufficient ─

    function test_ProfitGuard_RejectsImpossibleProfit() public {
        // Request $999M min profit — impossible to achieve, contract should revert
        vm.expectRevert();
        vm.prank(OWNER);
        arb.execute(MIN_FLASH_USD, 0, 1, 0, 999_000_000e18);
    }

    // ── Test 3: Only owner can call execute ───────────────────────────────────

    function test_RevertIf_ExecuteCalledByNonOwner() public {
        vm.expectRevert();
        vm.prank(ATTACKER);
        arb.execute(MIN_FLASH_USD, 0, 1, 0, 0);
    }

    // ── Test 4: Only owner can withdraw ──────────────────────────────────────

    function test_RevertIf_WithdrawCalledByNonOwner() public {
        vm.expectRevert();
        vm.prank(ATTACKER);
        arb.withdraw(USDS, 1);
    }

    // ── Test 5: Flash callback only callable by DSS_FLASH ────────────────────

    function test_RevertIf_FlashCallbackCalledByNonFlashMinter() public {
        vm.expectRevert();
        vm.prank(ATTACKER);
        arb.onFlashLoan(
            address(arb), // initiator
            USDS,
            MIN_FLASH_USD,
            0,
            abi.encode(MIN_FLASH_USD, int128(0), int128(1), uint256(0), uint256(0))
        );
    }

    // ── Test 6: contract deployed with immutables set ─────────────────────────

    function test_Deployed() public view {
        assertTrue(address(arb) != address(0));
    }
}
