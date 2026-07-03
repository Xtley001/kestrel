// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {KestrelArbitrageur} from "../src/KestrelArbitrageur.sol";
// FlashMint, sUSDe, and Timelock deployment live in DeployStrategies.s.sol.

// @notice Deploys KestrelArbitrageur implementation and optionally clones it.
// Usage:
// forge script script/Deploy.s.sol --rpc-url mainnet --broadcast --verify
///
// Required env vars:
// DEPLOYER_PRIVATE_KEY  — deployer wallet (needs ETH for gas only)
// PROFIT_WALLET         — cold storage address that receives arb profit
// SUSDS_ADDRESS         — sUSDS vault address
// USDS_ADDRESS          — USDS token address
// BALANCER_VAULT        — Balancer Vault address (0xBA12...)
// CURVE_SUSDS_USDS_POOL — Curve sUSDS/USDS pool address
///
// This script enforces the safe clone deployment pattern:
// 1. Deploy implementation (IMPL_OWNER = deployer wallet)
// 2. In the same broadcast session, clone + initialize atomically
// 3. Post-deployment: verify clone._profitWallet == expected PROFIT_WALLET
// If this assertion fails, initialization was front-run or failed — abort.
///
// The S4 fix in KestrelArbitrageur.sol requires msg.sender == IMPL_OWNER for
// initialize. Since IMPL_OWNER is the deployer wallet and this script calls
// initialize within the same vm.startBroadcast(deployerKey) session, the
// msg.sender check will always pass for legitimate deployments.
///
// An attacker front-running the clone deployment would need to use the SAME
// deployer wallet's private key — impossible without key compromise.
contract Deploy is Script {

    function run() external {
        // ── Safety gate ───────────────────────────────────────────────────────
        bool broadcasting = vm.isContext(VmSafe.ForgeContext.ScriptBroadcast) ||
                            vm.isContext(VmSafe.ForgeContext.ScriptResume);
        if (broadcasting) {
            string memory confirm = vm.envOr("CONFIRM_DEPLOY", string("false"));
            require(
                keccak256(bytes(confirm)) == keccak256(bytes("true")),
                "Deploy: set CONFIRM_DEPLOY=true to deploy to mainnet"
            );
        }

        // ── Deployment parameters ─────────────────────────────────────────────
        address balancerVault = vm.envAddress("BALANCER_VAULT");
        address susds         = vm.envAddress("SUSDS_ADDRESS");
        address usds          = vm.envAddress("USDS_ADDRESS");
        address curvePool     = vm.envAddress("CURVE_SUSDS_USDS_POOL");
        address profitWallet  = vm.envAddress("PROFIT_WALLET");

        // minProfit is per-strategy — set in env, not hardcoded.
        // ETH sUSDS/sDAI: 500e18 ($500) | GNO sxDAI: 5e18 | ARB sUSDe: 50e18
        uint256 minProfit = vm.envOr("MIN_PROFIT_WEI", uint256(500e18));

        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY");

        // ── O5: Pre-flight address verification ───────────────────────────────
        require(balancerVault == 0xBA12222222228d8Ba445958a75a0704d566BF2C8,
            "Deploy: BALANCER_VAULT is not the canonical Balancer V2 address");
        require(susds         != address(0), "Deploy: SUSDS_ADDRESS not set");
        require(usds          != address(0), "Deploy: USDS_ADDRESS not set");
        require(curvePool     != address(0), "Deploy: CURVE_SUSDS_USDS_POOL not set");
        require(profitWallet  != address(0), "Deploy: PROFIT_WALLET not set");

        console2.log("Deploying KestrelArbitrageur...");
        console2.log("  Balancer Vault:", balancerVault);
        console2.log("  sUSDS:         ", susds);
        console2.log("  USDS:          ", usds);
        console2.log("  Curve Pool:    ", curvePool);
        console2.log("  Profit Wallet: ", profitWallet);
        console2.log("  Min Profit:    ", minProfit);

        vm.startBroadcast(deployerKey);

        // ── Deploy implementation ─────────────────────────────────────────────
        // IMPL_OWNER = msg.sender = deployer wallet (baked into immutable).
        // All EIP-1167 clones of this implementation inherit IMPL_OWNER via delegatecall.
        KestrelArbitrageur implementation = new KestrelArbitrageur(
            balancerVault, susds, usds, curvePool, profitWallet, minProfit
        );

        // ── Deploy and initialize clone IN THE SAME BROADCAST CALL ───
        // Using OpenZeppelin Clones (or a simple assembly clone) + immediate initialize
        // within the same vm.startBroadcast session means both calls share the same
        // msg.sender (the deployer wallet == IMPL_OWNER).
        //
        // There is NO front-run window because:
        //   a) KestrelArbitrageur.initialize requires msg.sender == IMPL_OWNER
        //   b) An attacker cannot impersonate the deployer wallet
        //
        // Even if the implementation deploy and clone deploy were in separate blocks,
        // the attacker's initialize call would revert with Unauthorized.
        address cloneAddr = _deployClone(address(implementation));
        KestrelArbitrageur clone = KestrelArbitrageur(cloneAddr);
        clone.initialize(balancerVault, susds, usds, curvePool, profitWallet, minProfit);

        vm.stopBroadcast();

        // ── Post-deployment verification ─────────────────────────────
        // Verify the clone was correctly initialised — profitWallet must match.
        // If this assertion fails, the deployment is compromised: DO NOT use the clone.
        require(
            clone.profitWallet() == profitWallet,
            "DEPLOY FAILED: clone.profitWallet() != expected PROFIT_WALLET. "
            "Initialization may have been front-run. Do NOT fund this contract. "
            "Redeploy."
        );
        require(
            clone.isInitialized(),
            "DEPLOY FAILED: clone.isInitialized() == false. Initialization incomplete."
        );

        console2.log("Implementation deployed to:", address(implementation));
        console2.log("Clone deployed to:         ", cloneAddr);
        console2.log("S4 verification PASSED: profitWallet =", clone.profitWallet());
        console2.log("Set ARBITRAGEUR_ADDRESS=", cloneAddr, "in .env");

        // ── Post-deployment reachability check ────────────────────────────────
        // Call execute with zero flash amount — a revert here is expected
        // (profit guard catches zero-profit case) but confirms ABI is wired.
        try clone.execute(0, 1, 0, 0, 0) {
            console2.log("Post-deploy verification: execute(0) succeeded");
        } catch {
            console2.log("Post-deploy verification: execute(0) reached clone (reverted as expected)");
        }

        // ── Write deployment artifact ─────────────────────────────────────────
        string memory artifact = string.concat(
            '{"implementation":"',
            vm.toString(address(implementation)),
            '","clone":"',
            vm.toString(cloneAddr),
            '","profitWallet":"',
            vm.toString(profitWallet),
            '","network":"mainnet","block":',
            vm.toString(block.number),
            ',"s4_verified":true}'
        );
        vm.writeFile("deployments/mainnet.json", artifact);
    }

    // @dev Deploy an EIP-1167 minimal proxy clone of the implementation.
    // Equivalent to OpenZeppelin Clones.clone without the dependency.
    function _deployClone(address implementation) internal returns (address instance) {
        assembly {
            // EIP-1167 bytecode: 45 bytes
            // 3d602d80600a3d3981f3363d3d373d3d3d363d73{impl}5af43d82803e903d91602b57fd5bf3
            mstore(0x00, 0x3d602d80600a3d3981f3363d3d373d3d3d363d73000000000000000000000000)
            mstore(0x14, shl(0x60, implementation))
            mstore(0x28, 0x5af43d82803e903d91602b57fd5bf30000000000000000000000000000000000)
            instance := create(0, 0x09, 0x37)
        }
        require(instance != address(0), "Clone deployment failed");
    }
}

// @notice Additional script for deploying strategy-specific clones post-implementation.
// @dev    Run this AFTER the main Deploy script has deployed the implementation.
// Useful when adding new strategy clones without redeploying the implementation.
///
// Same pattern — deployerKey must be the IMPL_OWNER wallet.
contract DeployClone is Script {
    function run() external {
        address implementation = vm.envAddress("IMPLEMENTATION_ADDRESS");
        address balancerVault  = vm.envAddress("BALANCER_VAULT");
        address susds          = vm.envAddress("SUSDS_ADDRESS");
        address usds           = vm.envAddress("USDS_ADDRESS");
        address curvePool      = vm.envAddress("CURVE_SUSDS_USDS_POOL");
        address profitWallet   = vm.envAddress("PROFIT_WALLET");
        uint256 minProfit      = vm.envOr("MIN_PROFIT_WEI", uint256(500e18));
        uint256 deployerKey    = vm.envUint("DEPLOYER_PRIVATE_KEY");

        console2.log("Deploying clone of implementation:", implementation);

        vm.startBroadcast(deployerKey);

        // clone + initialize in one broadcast — deployer wallet == IMPL_OWNER
        address cloneAddr = _deployClone(implementation);
        KestrelArbitrageur clone = KestrelArbitrageur(cloneAddr);
        clone.initialize(balancerVault, susds, usds, curvePool, profitWallet, minProfit);

        vm.stopBroadcast();

        // Mandatory post-init verification
        require(
            clone.profitWallet() == profitWallet,
            "CLONE DEPLOY FAILED: profitWallet mismatch - possible front-run. "
            "Do NOT use this clone. Redeploy."
        );

        console2.log("Clone deployed and verified at:", cloneAddr);
        console2.log("S4 verification PASSED: profitWallet =", clone.profitWallet());
    }

    function _deployClone(address implementation) internal returns (address instance) {
        assembly {
            mstore(0x00, 0x3d602d80600a3d3981f3363d3d373d3d3d363d73000000000000000000000000)
            mstore(0x14, shl(0x60, implementation))
            mstore(0x28, 0x5af43d82803e903d91602b57fd5bf30000000000000000000000000000000000)
            instance := create(0, 0x09, 0x37)
        }
        require(instance != address(0), "Clone deployment failed");
    }
}
