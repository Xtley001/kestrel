// Kestrel — rate_cache.rs
// ProtocolRateCache
// Staleness is measured in blocks only — never time-based.
// max_staleness_blocks = 1 (refresh every block).

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use eyre::Result;
use tracing::debug;

// Per-block rate cache for an ERC-4626 yield vault (sUSDS, sDAI).
// Refreshes via previewRedeem(1e18) when current_block > fetched_at_block + max_staleness_blocks.
#[derive(Debug, Clone)]
pub struct ProtocolRateCache {
    // Cached previewRedeem(1e18) value
    pub rate: U256,
    // Block number at which the rate was last fetched
    pub fetched_at_block: u64,
    // Number of blocks before the cache is considered stale — always 1 per 
    pub max_staleness_blocks: u64,
}

impl ProtocolRateCache {
    pub fn new() -> Self {
        Self {
            rate: U256::ZERO,
            fetched_at_block: 0,
            max_staleness_blocks: 1, // refresh every block
        }
    }

    // Return the cached rate if fresh, or fetch a new one via IPC.
    // vault_address: sUSDS contract address (from env var, Section 14)
    pub async fn get_or_refresh<P: Provider>(
        &mut self,
        current_block: u64,
        vault: Address,
        provider: &P,
    ) -> Result<U256> {
        if current_block > self.fetched_at_block + self.max_staleness_blocks {
            self.rate = vault_call_preview_redeem(vault, provider).await?;
            self.fetched_at_block = current_block;
            debug!(
                block = current_block,
                rate = %self.rate,
                "protocol rate cache refreshed"
            );
        }
        Ok(self.rate)
    }

    // Check if the cache is fresh without fetching.
    pub fn is_fresh(&self, current_block: u64) -> bool {
        current_block <= self.fetched_at_block + self.max_staleness_blocks
    }

    // Age of the cache in blocks.
    pub fn age_blocks(&self, current_block: u64) -> u64 {
        current_block.saturating_sub(self.fetched_at_block)
    }
}

impl Default for ProtocolRateCache {
    fn default() -> Self {
        Self::new()
    }
}

// Call vault.previewRedeem(1e18) via IPC.
// Returns the canonical protocol rate with zero slippage.
async fn vault_call_preview_redeem<P: Provider>(
    vault: Address,
    provider: &P,
) -> Result<U256> {
    use alloy::sol;

    sol! {
        #[sol(rpc)]
        interface IERC4626 {
            function previewRedeem(uint256 shares) external view returns (uint256);
        }
    }

    let one_share = U256::from(10u128.pow(18)); // 1e18
    let contract = IERC4626::new(vault, provider);
    let result = contract.previewRedeem(one_share).call().await?;
    Ok(result._0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_cache_does_not_need_refresh() {
        let mut cache = ProtocolRateCache {
            rate: U256::from(1_050_000_000_000_000_000u128), // 1.05e18
            fetched_at_block: 100,
            max_staleness_blocks: 1,
        };
        // Same block — still fresh
        assert!(cache.is_fresh(100));
        // One block ahead — still within staleness window (fetched_at + max_staleness = 101)
        assert!(cache.is_fresh(101));
        // Two blocks ahead — stale
        assert!(!cache.is_fresh(102));
    }

    #[test]
    fn stale_cache_detected_correctly() {
        let cache = ProtocolRateCache {
            rate: U256::from(1_050_000_000_000_000_000u128),
            fetched_at_block: 100,
            max_staleness_blocks: 1,
        };
        // Block 102 — stale (102 > 100 + 1)
        assert!(!cache.is_fresh(102));
    }

    #[test]
    fn age_blocks_computed_correctly() {
        let cache = ProtocolRateCache {
            rate: U256::ZERO,
            fetched_at_block: 100,
            max_staleness_blocks: 1,
        };
        assert_eq!(cache.age_blocks(103), 3);
        assert_eq!(cache.age_blocks(100), 0);
    }

    #[test]
    fn max_staleness_defaults_to_one() {
        let cache = ProtocolRateCache::new();
        assert_eq!(cache.max_staleness_blocks, 1);
    }
}
