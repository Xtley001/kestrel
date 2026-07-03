// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Script, console2} from "forge-std/Script.sol";
import {VmSafe} from "forge-std/Vm.sol";
import {KestrelFlashMintArbitrageur} from "../src/KestrelFlashMintArbitrageur.sol";
import {KestrelSusdeArbitrageur} from "../src/KestrelSusdeArbitrageur.sol";
import {KestrelTimelock} from "../src/KestrelTimelock.sol";
import {KestrelArbitrageur} from "../src/KestrelArbitrageur.sol";

// @notice Deploy scripts for the FlashMint, sUSDe, and Timelock contracts, plus a helper
// to hand contract ownership to the timelock. Each guards mainnet broadcasts behind
// CONFIRM_DEPLOY=true.

abstract contract DeployBase is Script {
    function _confirm() internal view {
        bool broadcasting = vm.isContext(VmSafe.ForgeContext.ScriptBroadcast) ||
            vm.isContext(VmSafe.ForgeContext.ScriptResume);
        if (broadcasting) {
            require(
                keccak256(bytes(vm.envOr("CONFIRM_DEPLOY", string("false")))) ==
                    keccak256(bytes("true")),
                "set CONFIRM_DEPLOY=true to broadcast"
            );
        }
    }
}

// @notice Deploy the MakerDAO flash-mint arbitrageur.
// Env: DSS_FLASH_ADDRESS, SKY_PSM_ADDRESS, DAI_ADDRESS, USDS_ADDRESS, SUSDS_ADDRESS,
// CURVE_SUSDS_USDS_POOL, PROFIT_WALLET, DEPLOYER_PRIVATE_KEY.
contract DeployFlashMint is DeployBase {
    function run() external {
        _confirm();
        address dssFlash   = vm.envAddress("DSS_FLASH_ADDRESS");
        address skyPsm     = vm.envAddress("SKY_PSM_ADDRESS");
        address dai        = vm.envAddress("DAI_ADDRESS");
        address usds       = vm.envAddress("USDS_ADDRESS");
        address susds      = vm.envAddress("SUSDS_ADDRESS");
        address curvePool  = vm.envAddress("CURVE_SUSDS_USDS_POOL");
        address profit     = vm.envAddress("PROFIT_WALLET");
        uint256 key        = vm.envUint("DEPLOYER_PRIVATE_KEY");

        require(dssFlash != address(0) && skyPsm != address(0), "DeployFlashMint: zero core addr");

        vm.startBroadcast(key);
        KestrelFlashMintArbitrageur fm = new KestrelFlashMintArbitrageur(
            dssFlash, skyPsm, dai, usds, susds, curvePool, profit
        );
        vm.stopBroadcast();

        console2.log("KestrelFlashMintArbitrageur:", address(fm));
        console2.log("Set ARBITRAGEUR_FLASHMINT_ADDRESS=", address(fm));
    }
}

// @notice Deploy the sUSDe cross-venue arbitrageur (Arbitrum).
// Env: AAVE_POOL_ADDRESS, SUSDE_ADDRESS, USDE_ADDRESS, USDC_ADDRESS,
// ARB_CURVE_SUSDE_POOL, SWAP_ROUTER_ADDRESS, PROFIT_WALLET, DEPLOYER_PRIVATE_KEY.
contract DeploySusde is DeployBase {
    function run() external {
        _confirm();
        address aavePool   = vm.envAddress("AAVE_POOL_ADDRESS");
        address susde      = vm.envAddress("SUSDE_ADDRESS");
        address usde       = vm.envAddress("USDE_ADDRESS");
        address usdc       = vm.envAddress("USDC_ADDRESS");
        address curvePool  = vm.envAddress("ARB_CURVE_SUSDE_POOL");
        address router     = vm.envAddress("SWAP_ROUTER_ADDRESS");
        address profit     = vm.envAddress("PROFIT_WALLET");
        uint256 key        = vm.envUint("DEPLOYER_PRIVATE_KEY");

        require(aavePool != address(0) && susde != address(0), "DeploySusde: zero core addr");

        vm.startBroadcast(key);
        KestrelSusdeArbitrageur su = new KestrelSusdeArbitrageur(
            aavePool, susde, usde, usdc, curvePool, router, profit
        );
        vm.stopBroadcast();

        console2.log("KestrelSusdeArbitrageur:", address(su));
        console2.log("Set ARB_SUSDE_ARBITRAGEUR=", address(su));
    }
}

// @notice Deploy the 24h admin timelock.
// Env: TIMELOCK_PROPOSER (operator multisig), TIMELOCK_CANCELLER (guardian),
// DEPLOYER_PRIVATE_KEY. Executors are open (address(0)); admin is renounced.
contract DeployTimelock is DeployBase {
    function run() external {
        _confirm();
        address proposer  = vm.envAddress("TIMELOCK_PROPOSER");
        uint256 key       = vm.envUint("DEPLOYER_PRIVATE_KEY");
        require(proposer != address(0), "DeployTimelock: TIMELOCK_PROPOSER not set");

        address[] memory proposers = new address[](1);
        proposers[0] = proposer;
        address[] memory executors = new address[](1);
        executors[0] = address(0); // open execution after delay

        vm.startBroadcast(key);
        // admin = address(0): renounce admin so proposer/executor sets can only change
        // through the timelock itself.
        KestrelTimelock timelock = new KestrelTimelock(proposers, executors, address(0));
        vm.stopBroadcast();

        console2.log("KestrelTimelock:", address(timelock));
        console2.log("Set KESTREL_TIMELOCK_ADDRESS=", address(timelock));
        console2.log("Next: propose setOwner(timelock) on each arbitrageur, then");
        console2.log("schedule acceptOwnership() through the timelock (see TransferOwnership).");
    }
}

// @notice Step 1 of the two-step ownership handover: current owner proposes the timelock
// as the new owner of a KestrelArbitrageur. Run once per arbitrageur clone.
// Env: ARBITRAGEUR_ADDRESS, KESTREL_TIMELOCK_ADDRESS, OWNER_PRIVATE_KEY.
// Step 2 (timelock.schedule → execute of acceptOwnership) is performed via the
// operator's timelock tooling, not this script, so the 24h delay is enforced.
contract ProposeTimelockOwner is DeployBase {
    function run() external {
        _confirm();
        address arb      = vm.envAddress("ARBITRAGEUR_ADDRESS");
        address timelock = vm.envAddress("KESTREL_TIMELOCK_ADDRESS");
        uint256 key      = vm.envUint("OWNER_PRIVATE_KEY");
        require(arb != address(0) && timelock != address(0), "ProposeTimelockOwner: zero addr");

        vm.startBroadcast(key);
        KestrelArbitrageur(arb).setOwner(timelock);
        vm.stopBroadcast();

        console2.log("Proposed timelock as new owner of:", arb);
        console2.log("Now schedule KestrelArbitrageur.acceptOwnership() through the timelock.");
    }
}
