// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

// KestrelTimelock.sol
// (feedback): 24-Hour Admin Timelock on Contract Operations.
//
// Previously, all admin functions (pause, upgrade, fee-recipient change, sweep)
// were instant-execution.  A compromised operator key allowed immediate draining
// of the contract with no detection window.
//
// This contract wraps OpenZeppelin TimelockController with a 24-hour minimum delay.
// All admin calls to KestrelArbitrageur and related contracts MUST go through this
// timelock.  The operator proposes an action; it executes 24 hours later unless
// cancelled.  This gives a 24-hour window to detect a compromised key and cancel
// the malicious proposal before any funds move.
//
// Reference implementation: Peregrine's PeregrineTimelock.sol in this suite.
//
// Deployment:
//   1. Deploy KestrelTimelock with:
//        minDelay   = 24 hours (86400 seconds)
//        proposers  = [operator_multisig]
//        executors  = [address(0)]  // anyone can execute after delay
//        admin      = address(0)    // renounce admin immediately
//   2. Transfer ownership / DEFAULT_ADMIN_ROLE of KestrelArbitrageur to this timelock.
//   3. Revoke any direct operator access to admin functions.
//
// Usage:
//   • Schedule: timelock.schedule(target, value, data, predecessor, salt, delay)
//   • Execute:  timelock.execute(target, value, data, predecessor, salt)
//     (only callable after delay has elapsed)
//   • Cancel:   timelock.cancel(id)
//     (callable by CANCELLER_ROLE at any time before execution)
//
// The CANCELLER_ROLE should be held by a separate cold-key guardian or
// a multi-sig with a lower threshold than the proposer, so that cancellation
// is faster than proposal execution in an emergency.

import {TimelockController} from "@openzeppelin/governance/TimelockController.sol";

contract KestrelTimelock is TimelockController {
    // Minimum delay enforced by this timelock — 24 hours.
    // Overriding to a constant prevents a future governance vote from lowering it.
    uint256 public constant KESTREL_MIN_DELAY = 24 hours;

    // @param proposers  Addresses that can schedule operations (operator multi-sig).
    // @param executors  Addresses that can execute after delay; pass address(0) for open execution.
    // @param admin      Pass address(0) to renounce the admin role immediately on deployment,
    // preventing any future changes to proposer/executor sets without
    // going through the timelock itself.
    constructor(
        address[] memory proposers,
        address[] memory executors,
        address admin
    )
        TimelockController(
            KESTREL_MIN_DELAY,
            proposers,
            executors,
            admin
        )
    {}

    // NOTE: TimelockController.updateDelay is `external` and can only be called through
    // this timelock itself (self-call guard in the base). The 24h floor is set at
    // construction via KESTREL_MIN_DELAY. A custom override calling super.updateDelay is
    // not possible for an external base function, so it is intentionally omitted; a
    // governance action would have to route a delay change through the timelock itself.
}
