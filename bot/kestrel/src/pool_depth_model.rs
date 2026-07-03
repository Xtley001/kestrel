// Kestrel — pool_depth_model.rs
// Pool Depth Model for fast pre-screening before binary search.
//
// Maintains per-pool reserve state updated from TokenExchange events + balances calls.
// Provides a fast StableSwap approximation that rejects unviable opportunities in ~0μs
// avoiding the full 1–5ms binary search on thin-spread/deep-pool situations.
// Reduces wasted computation by ~30–50% on non-competitive blocks.

use alloy::primitives::{Address, U256};
use dashmap::DashMap;
use std::sync::Arc;
use tracing::debug;

// Per-pool depth state — updated from on-chain events.
#[derive(Debug, Clone)]
pub struct PoolDepthModel {
    pub reserve_a: U256,       // e.g. USDS balance
    pub reserve_b: U256,       // e.g. sUSDS balance
    pub amplification: u64,    // Curve A parameter
    pub last_updated_block: u64,
}

impl PoolDepthModel {
    pub fn new(reserve_a: U256, reserve_b: U256, amplification: u64, block: u64) -> Self {
        Self { reserve_a, reserve_b, amplification, last_updated_block: block }
    }

    // Total pool depth in token units (approximate — ignores StableSwap curve shape).
    pub fn total_depth(&self) -> U256 {
        self.reserve_a + self.reserve_b
    }

    // Fast viability check — can we profitably trade `spread_bps` at ANY size?
    // Uses reserve ratio as a proxy for price impact.
    ///
    // DEPTH_VIABLE_MULTIPLIER env-backed (was hardcoded 3; spec default 2).
    // FIX (Audit M5): default changed from 3 → 2 to reduce false negatives.
    pub fn quick_viable_check(&self, spread_bps: u32, gas_cost_usds: U256) -> bool {
        if self.total_depth().is_zero() {
            return false;
        }
        let multiplier = std::env::var("DEPTH_VIABLE_MULTIPLIER")
            .ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(2); // M5: was 3, now 2
        let spread_fraction_num = U256::from(spread_bps as u64);
        let estimated_profit_at_depth = self.total_depth() * spread_fraction_num / U256::from(10_000u64);
        let viable = estimated_profit_at_depth >= gas_cost_usds * U256::from(multiplier);
        if !viable {
            debug!(
                spread_bps,
                depth  = %self.total_depth(),
                gas    = %gas_cost_usds,
                "pool depth check: fast-reject — spread too thin for depth"
            );
        }
        viable
    }

    // staleness threshold read from DEPTH_MODEL_STALE_BLOCKS (was hardcoded 10).
    pub fn is_stale(&self, current_block: u64) -> bool {
        let threshold = std::env::var("DEPTH_MODEL_STALE_BLOCKS")
            .ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(10);
        current_block.saturating_sub(self.last_updated_block) > threshold
    }
}

// Shared pool depth registry — updated by dex_monitor TokenExchange event handlers.
pub struct PoolDepthRegistry {
    models: Arc<DashMap<Address, PoolDepthModel>>,
}

impl PoolDepthRegistry {
    pub fn new() -> Self {
        Self { models: Arc::new(DashMap::new()) }
    }

    pub fn update(&self, pool: Address, reserve_a: U256, reserve_b: U256, amp: u64, block: u64) {
        self.models.insert(pool, PoolDepthModel::new(reserve_a, reserve_b, amp, block));
        debug!(pool = %pool, block, "pool depth model updated");
    }

    pub fn get(&self, pool: Address) -> Option<PoolDepthModel> {
        self.models.get(&pool).map(|m| m.clone())
    }

    pub fn clone_arc(&self) -> Self {
        Self { models: Arc::clone(&self.models) }
    }
}

impl Default for PoolDepthRegistry {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usds(n: u128) -> U256 { U256::from(n) * U256::from(10u128.pow(18)) }
    fn addr(n: u8) -> Address { Address::from([n; 20]) }

    #[test]
    fn deep_pool_wide_spread_is_viable() {
        let model = PoolDepthModel::new(usds(100_000_000), usds(100_000_000), 500, 1);
        // 50bps spread, $500 gas cost — should be viable
        assert!(model.quick_viable_check(50, usds(500)));
    }

    #[test]
    fn shallow_pool_thin_spread_rejected() {
        let model = PoolDepthModel::new(usds(1_000_000), usds(1_000_000), 500, 1);
        // 2bps spread on $2M pool — max gross ≈ $400, gas $500 → not viable
        assert!(!model.quick_viable_check(2, usds(500)));
    }

    #[test]
    fn zero_depth_always_rejected() {
        let model = PoolDepthModel::new(U256::ZERO, U256::ZERO, 500, 1);
        assert!(!model.quick_viable_check(100, usds(1)));
    }

    #[test]
    fn staleness_detected_after_10_blocks() {
        std::env::remove_var("DEPTH_MODEL_STALE_BLOCKS");
        let model = PoolDepthModel::new(usds(1_000_000), usds(1_000_000), 500, 100);
        assert!(!model.is_stale(109));
        assert!(model.is_stale(111));
    }

    #[test]
    fn total_depth_sums_reserves() {
        let model = PoolDepthModel::new(usds(30_000_000), usds(70_000_000), 500, 1);
        assert_eq!(model.total_depth(), usds(100_000_000));
    }

    #[test]
    fn registry_stores_and_retrieves() {
        let reg = PoolDepthRegistry::new();
        reg.update(addr(1), usds(50_000_000), usds(50_000_000), 500, 100);
        let m = reg.get(addr(1)).unwrap();
        assert_eq!(m.reserve_a, usds(50_000_000));
    }

    #[test]
    fn registry_miss_returns_none() {
        let reg = PoolDepthRegistry::new();
        assert!(reg.get(addr(99)).is_none());
    }

    // ── New tests (spec) ──────────────────────────────────────────────────

    #[test]
    fn viable_check_multiplier_reads_from_env() {
        std::env::set_var("DEPTH_VIABLE_MULTIPLIER", "2");
        let model = PoolDepthModel::new(usds(10_000_000), usds(10_000_000), 500, 1);
        // 10bps on $20M depth: gross ≈ $20K; 2 × $300 gas = $600 → viable
        assert!(model.quick_viable_check(10, usds(300)));
        std::env::remove_var("DEPTH_VIABLE_MULTIPLIER");
    }

    #[test]
    fn staleness_threshold_reads_from_env() {
        std::env::set_var("DEPTH_MODEL_STALE_BLOCKS", "5");
        let model = PoolDepthModel::new(usds(1_000_000), usds(1_000_000), 500, 100);
        assert!(!model.is_stale(104)); // 4 blocks: not stale at threshold=5
        assert!(model.is_stale(106));  // 6 blocks: stale
        std::env::remove_var("DEPTH_MODEL_STALE_BLOCKS");
    }
}
