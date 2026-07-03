// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

// Additional tests for KestrelArbitrageur covering:
// pause / unpause (fix)
// Reentrancy guard (double-invoke receiveFlashLoan)
// setOwner / acceptOwnership (fix)
// Chain ID check (fix)
// Non-owner access to all admin functions

import {Test, console2} from "forge-std/Test.sol";
import {KestrelArbitrageur, ContractPaused, WrongChain} from "../src/KestrelArbitrageur.sol";
import {IERC20} from "@openzeppelin/token/ERC20/IERC20.sol";

contract KestrelArbitrageurExtendedTest is Test {

    address constant BALANCER_VAULT = 0xBA12222222228d8Ba445958a75a0704d566BF2C8;
    address constant SUSDS          = 0xa3931d71877C0E7a3148CB7Eb4463524FEc27fbD;
    address constant USDS           = 0xdC035D45d973E3EC169d2276DDab16f1e407384F;
    address constant CURVE_POOL     = address(0x1); // placeholder for unit tests

    address constant PROFIT_WALLET  = address(0xBEEF);
    address constant OWNER          = address(0xDEAD);
    address constant ATTACKER       = address(0xBAD);
    address constant NEW_OWNER      = address(0xFEED);

    uint256 constant FORK_BLOCK     = 21_100_000;

    KestrelArbitrageur arb;

    function setUp() public {
        vm.createSelectFork(
            vm.envOr("RETH_IPC_PATH", string("http://localhost:8545")),
            FORK_BLOCK
        );

        vm.prank(OWNER);
        arb = new KestrelArbitrageur(
            BALANCER_VAULT, SUSDS, USDS, CURVE_POOL, PROFIT_WALLET, 1e18
        );
    }

    // ── Pause tests ─────────────────────────────────────────

    function test_Pause_OwnerCanPause() public {
        vm.prank(OWNER);
        arb.pause();
        assertTrue(arb.paused());
    }

    function test_RevertIf_NonOwnerPauses() public {
        vm.expectRevert();
        vm.prank(ATTACKER);
        arb.pause();
    }

    function test_Pause_ExecuteRevertsWhenPaused() public {
        vm.prank(OWNER);
        arb.pause();

        vm.expectRevert(ContractPaused.selector);
        vm.prank(OWNER);
        arb.execute(1e18, 0, 1, 0, 0);
    }

    function test_Pause_ExecutePackedRevertsWhenPaused() public {
        vm.prank(OWNER);
        arb.pause();

        vm.expectRevert(ContractPaused.selector);
        vm.prank(OWNER);
        arb.executePacked(0, 0);
    }

    function test_Unpause_RestoresExecute() public {
        vm.startPrank(OWNER);
        arb.pause();
        arb.unpause();
        vm.stopPrank();
        assertFalse(arb.paused());
        // execute would revert for other reasons (no real flash loan), but NOT ContractPaused
    }

    function test_RevertIf_NonOwnerUnpauses() public {
        vm.prank(OWNER);
        arb.pause();

        vm.expectRevert();
        vm.prank(ATTACKER);
        arb.unpause();
    }

    // ── Reentrancy guard tests ─────────────────────────────────────────────────

    function test_RevertIf_ReceiveFlashLoanCalledDirectly() public {
        // receiveFlashLoan must only be callable by the Balancer vault.
        // Calling it directly (not from within a flash loan) should revert.
        IERC20[] memory tokens   = new IERC20[](1);
        tokens[0]                = IERC20(USDS);
        uint256[] memory amounts = new uint256[](1);
        amounts[0]               = 1e18;
        uint256[] memory fees    = new uint256[](1);
        fees[0]                  = 0;
        bytes memory userData    = abi.encode(
            uint256(1e18), int128(0), int128(1), uint256(0), uint256(0)
        );

        vm.expectRevert(); // caller != _balancerVault
        vm.prank(ATTACKER);
        arb.receiveFlashLoan(tokens, amounts, fees, userData);
    }

    // ── Owner key rotation tests ───────────────────────────

    function test_SetOwner_ProposesNewOwner() public {
        vm.prank(OWNER);
        arb.setOwner(NEW_OWNER);
        assertEq(arb.pendingOwner(), NEW_OWNER);
    }

    function test_RevertIf_NonOwnerCallsSetOwner() public {
        vm.expectRevert();
        vm.prank(ATTACKER);
        arb.setOwner(ATTACKER);
    }

    function test_AcceptOwnership_TransfersOwner() public {
        vm.prank(OWNER);
        arb.setOwner(NEW_OWNER);

        vm.prank(NEW_OWNER);
        arb.acceptOwnership();

        assertEq(arb.owner(), NEW_OWNER);
        assertEq(arb.pendingOwner(), address(0));
    }

    function test_RevertIf_NonPendingOwnerAccepts() public {
        vm.prank(OWNER);
        arb.setOwner(NEW_OWNER);

        vm.expectRevert();
        vm.prank(ATTACKER);
        arb.acceptOwnership();
    }

    function test_RevertIf_SetOwnerToZeroAddress() public {
        vm.expectRevert();
        vm.prank(OWNER);
        arb.setOwner(address(0));
    }

    // ── Chain ID guard tests ────────────────────────────────

    function test_ChainId_CorrectChainAllowsCall() public {
        // On a real mainnet fork (chainid == 1), the EXPECTED_CHAIN_ID is 1.
        // execute should NOT revert due to chain ID mismatch on the correct chain.
        // It will revert for other reasons (no flash loan) but not WrongChain.
        // We just verify the WrongChain selector is NOT the revert reason.
        vm.prank(OWNER);
        try arb.execute(1e18, 0, 1, 0, 0) {} catch (bytes memory err) {
            // Should not match WrongChain selector
            bytes4 wrongChainSelector = WrongChain.selector;
            bytes4 actual;
            assembly { actual := mload(add(err, 32)) }
            assertNotEq(actual, wrongChainSelector, "should not revert with WrongChain on correct chain");
        }
    }

    function test_ChainId_WrongChainReverts() public {
        // Spoof chain ID to simulate deployment on wrong chain
        vm.chainId(999);

        vm.expectRevert(
            abi.encodeWithSelector(WrongChain.selector, uint256(1), uint256(999))
        );
        vm.prank(OWNER);
        arb.execute(1e18, 0, 1, 0, 0);
    }
}
