// Kestrel — simulate.rs
// Local revm simulation of an arbitrage tx before submission.
//
// uses revm's AlloyDB (not EthersDB — the provider is an alloy provider, which
// does not implement ethers' Middleware, so the old EthersDB path could never compile).
// AlloyDB forks state from the connected node at `block_number`; CacheDB memoises reads.
//
// The revm transact runs on a dedicated rayon pool so it never queues behind other
// blocking work. AlloyDB is constructed with an explicit runtime Handle captured from
// the async caller (AlloyDB::new returns None off a runtime thread; with_handle lets the
// rayon worker drive the provider's async calls on the main runtime).

use alloy::eips::BlockId;
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::RootProvider;
use alloy::transports::BoxTransport;
use eyre::Result;
use once_cell::sync::Lazy;
use rayon::ThreadPool;
use revm::{
    db::{AlloyDB, CacheDB},
    primitives::{ExecutionResult, Output, TransactTo},
    Evm,
};
use std::sync::Arc;
use tracing::debug;

// Dedicated rayon thread pool for revm simulations. Size via REVM_THREAD_POOL_SIZE,
// defaulting to the CPU count.
static SIM_POOL: Lazy<ThreadPool> = Lazy::new(|| {
    let size = std::env::var("REVM_THREAD_POOL_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(num_cpus::get);
    rayon::ThreadPoolBuilder::new()
        .num_threads(size)
        .thread_name(|i| format!("revm-sim-{i}"))
        .build()
        .expect("failed to build revm simulation thread pool")
});

// Result of a local revm simulation.
#[derive(Debug, Clone)]
pub struct SimResult {
    pub gas_used: u64,
    pub output: Option<Bytes>,
    pub revert_data: Option<Bytes>,
    pub success: bool,
}

// The concrete provider type the pipeline builds (WS or IPC → PubSubFrontend).
type SimProvider = RootProvider<BoxTransport>;

// Simulate an arbitrage transaction locally using revm + AlloyDB.
pub async fn simulate_arb(
    provider: Arc<SimProvider>,
    block_number: u64,
    contract: Address,
    calldata: Bytes,
) -> Result<SimResult> {
    let executor_wallet = executor_wallet_address();
    // Capture the main runtime handle so the rayon worker can drive AlloyDB's async reads.
    let handle = tokio::runtime::Handle::current();
    let block_id = BlockId::number(block_number);
    let gas_limit: u64 = std::env::var("GAS_LIMIT_ARB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400_000);

    let (tx, rx) = tokio::sync::oneshot::channel();
    SIM_POOL.spawn(move || {
        let alloy_db = AlloyDB::with_handle((*provider).clone(), block_id, handle);
        let db = CacheDB::new(alloy_db);
        let mut evm = Evm::builder()
            .with_db(db)
            .modify_tx_env(|tx_env| {
                tx_env.caller = executor_wallet.into();
                tx_env.transact_to = TransactTo::Call(contract.into());
                tx_env.data = calldata.0.clone().into();
                tx_env.value = U256::ZERO;
                tx_env.gas_limit = gas_limit;
            })
            .build();
        let result = evm.transact();
        let _ = tx.send(result.map_err(|e| eyre::eyre!("revm transact failed: {e:?}")));
    });

    let exec = rx
        .await
        .map_err(|_| eyre::eyre!("revm sim thread dropped sender"))??;

    let sim = match exec.result {
        ExecutionResult::Success { gas_used, output, .. } => {
            let out = match output {
                Output::Call(bytes) => Some(Bytes::from(bytes.to_vec())),
                Output::Create(bytes, _) => Some(Bytes::from(bytes.to_vec())),
            };
            SimResult { gas_used, output: out, revert_data: None, success: true }
        }
        ExecutionResult::Revert { gas_used, output } => {
            debug!(gas_used, "simulation reverted");
            SimResult {
                gas_used,
                output: None,
                revert_data: Some(Bytes::from(output.to_vec())),
                success: false,
            }
        }
        ExecutionResult::Halt { reason, gas_used } => {
            debug!(?reason, gas_used, "simulation halted");
            SimResult { gas_used, output: None, revert_data: None, success: false }
        }
    };

    Ok(sim)
}

// Read executor wallet address from EXECUTOR_ADDRESS. Panics if missing or zero — the
// zero address must never be a valid simulation caller (checked at startup too).
fn executor_wallet_address() -> [u8; 20] {
    let raw = std::env::var("EXECUTOR_ADDRESS")
        .expect("EXECUTOR_ADDRESS must be set (checked by validate_env)");
    let trimmed = raw.trim_start_matches("0x");
    let bytes: [u8; 20] = hex::decode(trimmed)
        .ok()
        .and_then(|b| b.try_into().ok())
        .expect("EXECUTOR_ADDRESS must be a valid 20-byte hex address");
    assert_ne!(
        bytes, [0u8; 20],
        "EXECUTOR_ADDRESS must not be the zero address — set it to the real executor wallet"
    );
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_result_success_structure() {
        let result = SimResult {
            gas_used: 150_000,
            output: Some(Bytes::from(vec![0x00])),
            revert_data: None,
            success: true,
        };
        assert!(result.success);
        assert_eq!(result.gas_used, 150_000);
        assert!(result.revert_data.is_none());
    }

    #[test]
    fn sim_result_revert_carries_data() {
        let revert = Bytes::from(b"InsufficientProfit".to_vec());
        let result = SimResult {
            gas_used: 30_000,
            output: None,
            revert_data: Some(revert.clone()),
            success: false,
        };
        assert!(!result.success);
        assert!(result.revert_data.is_some());
    }

    // this test previously removed the wrong env var (EXECUTOR_WALLET) and
    // asserted a zero-address return — but the function panics on missing/zero. Now it
    // asserts a valid address parses correctly.
    #[test]
    fn executor_wallet_parses_valid_address() {
        std::env::set_var("EXECUTOR_ADDRESS", "0x000000000000000000000000000000000000dEaD");
        let addr = executor_wallet_address();
        assert_eq!(addr[19], 0xAD);
        std::env::remove_var("EXECUTOR_ADDRESS");
    }
}
