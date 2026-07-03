// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

// Test coverage for KestrelTimelock.
// Previously zero test files existed for this contract.
// Covers: 24h delay enforcement, schedule, execute, cancel, unauthorized access.

import {Test, console2} from "forge-std/Test.sol";
import {KestrelTimelock} from "../src/KestrelTimelock.sol";

// @title KestrelTimelockTest
// @notice Tests for the 24-hour admin timelock.
contract KestrelTimelockTest is Test {

    address constant PROPOSER  = address(0xA1);
    address constant EXECUTOR  = address(0xA2);
    address constant ATTACKER  = address(0xBAD);
    address constant TARGET    = address(0xCAFE);

    uint256 constant MIN_DELAY = 24 hours;

    KestrelTimelock timelock;

    function setUp() public {
        address[] memory proposers = new address[](1);
        proposers[0] = PROPOSER;
        address[] memory executors = new address[](1);
        executors[0] = EXECUTOR;

        timelock = new KestrelTimelock(proposers, executors, address(0));
    }

    // ── Test 1: Schedule requires proposer role ────────────────────────────────

    function test_RevertIf_NonProposerSchedules() public {
        vm.expectRevert();
        vm.prank(ATTACKER);
        timelock.schedule(
            TARGET,
            0,
            bytes(""),
            bytes32(0),
            bytes32(uint256(1)),
            MIN_DELAY
        );
    }

    // ── Test 2: Execute before delay reverts ──────────────────────────────────

    function test_RevertIf_ExecuteBeforeDelay() public {
        bytes32 salt = bytes32(uint256(42));
        bytes32 id;

        vm.prank(PROPOSER);
        timelock.schedule(TARGET, 0, bytes(""), bytes32(0), salt, MIN_DELAY);

        // Try to execute immediately — should revert (not ready yet)
        vm.expectRevert();
        vm.prank(EXECUTOR);
        timelock.execute(TARGET, 0, bytes(""), bytes32(0), salt);
    }

    // ── Test 3: Execute after delay succeeds ─────────────────────────────────

    function test_ExecuteAfterDelaySucceeds() public {
        // Schedule a no-op call to TARGET (no code needed for call to succeed)
        bytes32 salt = bytes32(uint256(99));

        vm.prank(PROPOSER);
        timelock.schedule(TARGET, 0, bytes(""), bytes32(0), salt, MIN_DELAY);

        // Warp forward past the delay
        vm.warp(block.timestamp + MIN_DELAY + 1);

        // Execute — should not revert
        vm.prank(EXECUTOR);
        // Note: call to TARGET with empty calldata will succeed (TARGET is an EOA)
        timelock.execute(TARGET, 0, bytes(""), bytes32(0), salt);
    }

    // ── Test 4: Cancel removes scheduled operation ────────────────────────────

    function test_CancelPreventsExecution() public {
        bytes32 salt = bytes32(uint256(77));

        vm.prank(PROPOSER);
        timelock.schedule(TARGET, 0, bytes(""), bytes32(0), salt, MIN_DELAY);

        // Cancel before execution
        bytes32 id = timelock.hashOperation(TARGET, 0, bytes(""), bytes32(0), salt);
        vm.prank(PROPOSER);
        timelock.cancel(id);

        // Warp past delay
        vm.warp(block.timestamp + MIN_DELAY + 1);

        // Attempt to execute cancelled operation — should revert
        vm.expectRevert();
        vm.prank(EXECUTOR);
        timelock.execute(TARGET, 0, bytes(""), bytes32(0), salt);
    }

    // ── Test 5: Delay too short is rejected ───────────────────────────────────

    function test_RevertIf_DelayTooShort() public {
        bytes32 salt = bytes32(uint256(55));

        vm.expectRevert(); // delay < MIN_DELAY
        vm.prank(PROPOSER);
        timelock.schedule(TARGET, 0, bytes(""), bytes32(0), salt, MIN_DELAY - 1);
    }

    // ── Test 6: Non-executor cannot execute ───────────────────────────────────

    function test_RevertIf_NonExecutorExecutes() public {
        bytes32 salt = bytes32(uint256(33));

        vm.prank(PROPOSER);
        timelock.schedule(TARGET, 0, bytes(""), bytes32(0), salt, MIN_DELAY);

        vm.warp(block.timestamp + MIN_DELAY + 1);

        vm.expectRevert();
        vm.prank(ATTACKER);
        timelock.execute(TARGET, 0, bytes(""), bytes32(0), salt);
    }
}
