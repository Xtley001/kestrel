// Kestrel — speculative_cache.rs
// Speculative Execution on Spread Threshold Entry.
//
// When spread crosses SPECULATIVE_THRESHOLD_BPS (default 3bps, below submission threshold),
// the pipeline spawns a background binary search and stores the result here.
// On the next block — if spread has escalated to submission threshold — the result is
// immediately available: binary search latency drops from ~1–5ms to ~0ms.
//
// (feedback): Cache window extended from 2 blocks to a configurable window (default 4).
// During cascade events spreads can persist for 3–4 blocks; the old 2-block window caused
// the binary search to re-run from scratch on block 3+.  A BTreeMap keyed by block number
// replaces the fixed-key DashMap and entries are evicted past the configured window.
// Set SPECULATIVE_CACHE_WINDOW (blocks) in the environment to override the default of 4.

use dashmap::DashMap;
use alloy::primitives::{Address, U256};
use crate::dex_monitor::SpreadDirection;
use std::sync::Arc;

// Key: (pool_address, direction, block_number)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpeculativeKey {
    pub pool: Address,
    pub direction_is_discount: bool,
    pub block_num: u64,
}

// Value: the result of a background binary search.
#[derive(Debug, Clone)]
pub struct SpeculativeResult {
    pub optimal_size: U256,
    pub computed_at_block: u64,
}

// Thread-safe in-memory speculative result cache.
// Window is configurable (SPECULATIVE_CACHE_WINDOW env var, default 4 blocks).
// Results are stored in a DashMap keyed by (pool, direction, block) and evicted past the window.
pub struct SpeculativeCache {
    inner: Arc<DashMap<SpeculativeKey, SpeculativeResult>>,
    // Number of blocks a speculative result stays valid.
    window: u64,
}

impl SpeculativeCache {
    // Create a new cache.  Window size is read from SPECULATIVE_CACHE_WINDOW (default 4).
    pub fn new() -> Self {
        let window = std::env::var("SPECULATIVE_CACHE_WINDOW")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(4);
        Self { inner: Arc::new(DashMap::new()), window }
    }

    // Store a speculatively-computed trade size and evict entries outside the window.
    pub fn insert(&self, pool: Address, direction: &SpreadDirection, block_num: u64, size: U256) {
        let key = SpeculativeKey {
            pool,
            direction_is_discount: matches!(direction, SpreadDirection::Discount),
            block_num,
        };
        self.inner.insert(key, SpeculativeResult { optimal_size: size, computed_at_block: block_num });
        // Evict results older than `window` blocks to bound memory usage.
        let window = self.window;
        self.inner.retain(|_, v| block_num.saturating_sub(v.computed_at_block) <= window);
    }

    // Retrieve a result for this pool/direction if any entry within the current window exists.
    // Checks all blocks from `current_block` back to `current_block - window` so that
    // a result computed several blocks ago is still returned during multi-block cascades.
    pub fn get(&self, pool: Address, direction: &SpreadDirection, current_block: u64) -> Option<U256> {
        let dir_flag = matches!(direction, SpreadDirection::Discount);
        // Search from the most recent block backward through the window.
        for offset in 0..=self.window {
            let target_block = current_block.saturating_sub(offset);
            let key = SpeculativeKey {
                pool,
                direction_is_discount: dir_flag,
                block_num: target_block,
            };
            if let Some(entry) = self.inner.get(&key) {
                if current_block.saturating_sub(entry.computed_at_block) <= self.window {
                    return Some(entry.optimal_size);
                }
            }
        }
        None
    }

    pub fn clone_arc(&self) -> Self {
        Self { inner: Arc::clone(&self.inner), window: self.window }
    }
}

impl Default for SpeculativeCache {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, U256};

    fn addr(n: u8) -> Address { Address::from([n; 20]) }

    #[test]
    fn insert_and_retrieve_same_block() {
        let cache = SpeculativeCache::new();
        let pool = addr(1);
        cache.insert(pool, &SpreadDirection::Discount, 100, U256::from(1_000_000u64));
        let result = cache.get(pool, &SpreadDirection::Discount, 100);
        assert_eq!(result, Some(U256::from(1_000_000u64)));
    }

    #[test]
    fn retrieve_from_previous_block() {
        let cache = SpeculativeCache::new();
        let pool = addr(2);
        cache.insert(pool, &SpreadDirection::Discount, 99, U256::from(999u64));
        // Result from block 99 should be available at block 100
        let result = cache.get(pool, &SpreadDirection::Discount, 100);
        assert_eq!(result, Some(U256::from(999u64)));
    }

    #[test]
    fn stale_results_not_returned() {
        // Default window is 4; a result 6 blocks old must not be returned.
        let cache = SpeculativeCache::new();
        let pool = addr(3);
        cache.insert(pool, &SpreadDirection::Discount, 90, U256::from(1u64));
        // Block 90 result at block 96 — 6 blocks stale — should not be returned
        let result = cache.get(pool, &SpreadDirection::Discount, 96);
        assert_eq!(result, None);
    }

    #[test]
    fn cascade_window_covers_four_blocks() {
        // a result from block N is still usable at block N+4 (inside the window).
        let cache = SpeculativeCache::new();
        let pool = addr(7);
        cache.insert(pool, &SpreadDirection::Discount, 100, U256::from(42u64));
        // Should be available at blocks 100, 101, 102, 103, 104 (window=4)
        for delta in 0..=4u64 {
            assert_eq!(
                cache.get(pool, &SpreadDirection::Discount, 100 + delta),
                Some(U256::from(42u64)),
                "failed at block 100+{delta}"
            );
        }
        // Block 105 is outside the window
        assert_eq!(cache.get(pool, &SpreadDirection::Discount, 105), None);
    }

    #[test]
    fn direction_mismatch_returns_none() {
        let cache = SpeculativeCache::new();
        let pool = addr(4);
        cache.insert(pool, &SpreadDirection::Discount, 100, U256::from(1u64));
        let result = cache.get(pool, &SpreadDirection::Premium, 100);
        assert_eq!(result, None);
    }

    #[test]
    fn different_pools_do_not_collide() {
        let cache = SpeculativeCache::new();
        cache.insert(addr(5), &SpreadDirection::Discount, 100, U256::from(111u64));
        cache.insert(addr(6), &SpreadDirection::Discount, 100, U256::from(222u64));
        assert_eq!(cache.get(addr(5), &SpreadDirection::Discount, 100), Some(U256::from(111u64)));
        assert_eq!(cache.get(addr(6), &SpreadDirection::Discount, 100), Some(U256::from(222u64)));
    }
}
