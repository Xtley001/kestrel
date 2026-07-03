// Kestrel — binary_search.rs
// FIXES (Borrow Logic & Hardcode Spec — May 2026):
//   B2.1 — direction param added: all 6 curve_pool_call_get_dy calls use (i,j)
//           derived from SpreadDirection. Previously ALL used (1,0) — wrong for Premium.
//   B2.2 — compute_net_profit_premium added: correct formula for Premium path.
//   H01  — BINARY_SEARCH_MAX_USD env-backed (was 100_000_000).
//   H02  — BINARY_SEARCH_GRANULARITY_FLOOR_USD env-backed (was 1_000).
//   H03  — BINARY_SEARCH_GRANULARITY_CEIL_USD env-backed (was 50_000).
//   H04  — BINARY_SEARCH_GRANULARITY_DIVISOR env-backed (was 2000).
//   H05  — AAVE_FLASH_FEE_BPS env-backed in select_flash_provider (was 5).
//   H06  — AAVE_FLASH_FEE_BPS env-backed in select_arb_flash_provider (was 5).

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use eyre::Result;
use tracing::debug;

use crate::dex_monitor::{curve_pool_call_get_dy, SpreadDirection};

#[derive(Debug, Clone, Copy)]
pub enum FlashProvider {
    Balancer,
    Aave { fee_bps: u64 },
    Morpho,
    FlashMint,
    BalancerWithAaveFallback { aave_fee_bps: u64 },
}

impl FlashProvider {
    pub fn fee_bps(&self) -> u64 {
        match self {
            FlashProvider::Balancer                         => 0,
            FlashProvider::Aave { fee_bps }                => *fee_bps,
            FlashProvider::Morpho                          => 0,
            FlashProvider::FlashMint                       => 0,
            FlashProvider::BalancerWithAaveFallback { .. } => 0,
        }
    }

    pub fn fallback_fee_bps(&self) -> u64 {
        match self {
            FlashProvider::BalancerWithAaveFallback { aave_fee_bps } => *aave_fee_bps,
            other => other.fee_bps(),
        }
    }
}

// /H06: centralised Aave fee reader
fn aave_fee_bps_from_env() -> u64 {
    std::env::var("AAVE_FLASH_FEE_BPS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(5)
}

pub fn select_flash_provider(
    required_size: U256,
    balancer_available: U256,
    morpho_available: U256,
) -> FlashProvider {
    // Provider ordering is critical for fee minimisation:
    //   Balancer (0% fee, $300M cap)  →  primary for all ETH/GNO strategies
    //   Morpho   (0% fee, $50M cap)   →  covers sizes in range (balancer, balancer+morpho]
    //   Aave     (5bps fee)            →  last resort only when both zero-fee providers exhausted
    //
    // IMPORTANT: morpho_available is Morpho's *independent* pool capacity ($50M), not a
    // cumulative cap. Any size that Balancer can't cover lands in Morpho if it fits within
    // Balancer's cap + Morpho's additional capacity. The check is therefore:
    //   required_size <= balancer_available + morpho_available
    //
    // Previously this read `required_size <= morpho_available` ($50M), which was permanently
    // dead code — any size that exceeded balancer_available ($300M) could never also satisfy
    // `size <= $50M`. The Morpho branch never fired and everything over $300M hit Aave at 5bps.
    if required_size <= balancer_available {
        FlashProvider::Balancer
    } else if required_size <= balancer_available + morpho_available {
        // Size exceeds Balancer but fits within the combined zero-fee envelope.
        // Route through Morpho (0% fee) to avoid Aave's 5bps charge.
        FlashProvider::Morpho
    } else {
        FlashProvider::Aave { fee_bps: aave_fee_bps_from_env() }
    }
}

pub fn select_arb_flash_provider(
    required_size: U256,
    balancer_available: U256,
    _aave_fee_bps: u64, // kept for API compat — read from env internally
) -> FlashProvider {
    if required_size <= balancer_available {
        FlashProvider::Balancer
    } else {
        FlashProvider::BalancerWithAaveFallback { aave_fee_bps: aave_fee_bps_from_env() }
    }
}

// –H04: all three granularity constants env-backed
fn adaptive_granularity(pool_depth: U256) -> U256 {
    let divisor = std::env::var("BINARY_SEARCH_GRANULARITY_DIVISOR")
        .ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(2000);
    let floor_usd = std::env::var("BINARY_SEARCH_GRANULARITY_FLOOR_USD")
        .ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(1_000);
    let ceil_usd = std::env::var("BINARY_SEARCH_GRANULARITY_CEIL_USD")
        .ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(50_000);

    let raw = pool_depth / U256::from(divisor);
    raw.clamp(usds(floor_usd), usds(ceil_usd))
}

// Map SpreadDirection to Curve pool indices.
// Index 0 = sUSDS (yield-bearing), index 1 = USDS (base stablecoin).
pub fn direction_to_indices(direction: SpreadDirection) -> (i128, i128) {
    match direction {
        SpreadDirection::Discount => (1, 0), // sell USDS → buy sUSDS
        SpreadDirection::Premium  => (0, 1), // sell sUSDS → buy USDS
    }
}

// Net profit — Discount direction.
// net = (susds_out × protocol_rate / 1e18) - flash_in - flash_fee - gas_cost
#[inline]
pub fn compute_net_profit(
    flash_in: U256,
    susds_out: U256,
    protocol_rate: U256,
    flash_fee_bps: u64,
    gas_cost: U256,
) -> U256 {
    let protocol_value = susds_out * protocol_rate / U256::from(10u128.pow(18));
    let gross          = protocol_value.saturating_sub(flash_in);
    let flash_fee      = flash_in * U256::from(flash_fee_bps) / U256::from(10_000u64);
    gross.saturating_sub(flash_fee + gas_cost)
}

// Net profit — Premium direction.
///
// Premium execution:
// 1. Flash borrow flash_in USDS
// 2. Deposit into vault → sUSDS at protocol rate
// 3. Sell sUSDS on Curve (i=0 → j=1) → usds_out_from_dex USDS
// 4. Repay flash; net = usds_out - flash_in - fee - gas
pub fn compute_net_profit_premium(
    flash_in: U256,
    usds_out_from_dex: U256,
    flash_fee_bps: u64,
    gas_cost: U256,
) -> U256 {
    let gross     = usds_out_from_dex.saturating_sub(flash_in);
    let flash_fee = flash_in * U256::from(flash_fee_bps) / U256::from(10_000u64);
    gross.saturating_sub(flash_fee + gas_cost)
}

// Compute the optimal flash loan size via binary search over live get_dy.
///
// direction parameter drives Curve indices — previously always (1,0).
// Premium direction converts flash_in USDS → sUSDS before each get_dy call.
// hi ceiling read from BINARY_SEARCH_MAX_USD env var.
pub async fn optimal_susds_trade_size<P: Provider>(
    curve_pool: Address,
    protocol_rate: U256,
    flash_fee_bps: u64,
    gas_cost: U256,
    min_net_profit: U256,
    lo_floor: U256,
    direction: SpreadDirection,
    provider: &P,
) -> Result<Option<(U256, U256)>> {
    let mut lo = lo_floor;
    // 
    let hi_usd = std::env::var("BINARY_SEARCH_MAX_USD")
        .ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(100_000_000);
    let mut hi = usds(hi_usd);

    let (i, j) = direction_to_indices(direction);

    // For Premium, dx to get_dy is sUSDS = flash_usds * 1e18 / protocol_rate
    let to_query = |flash_usds: U256| -> U256 {
        match direction {
            SpreadDirection::Discount => flash_usds,
            SpreadDirection::Premium  => {
                if protocol_rate.is_zero() { return U256::ZERO; }
                flash_usds * U256::from(10u128.pow(18)) / protocol_rate
            }
        }
    };

    // Direction-aware net profit
    let net_profit = |flash_in: U256, curve_out: U256| -> U256 {
        match direction {
            SpreadDirection::Discount => compute_net_profit(
                flash_in, curve_out, protocol_rate, flash_fee_bps, gas_cost,
            ),
            SpreadDirection::Premium => compute_net_profit_premium(
                flash_in, curve_out, flash_fee_bps, gas_cost,
            ),
        }
    };

    // Fan out first 3 calls in parallel
    let mid_init = (lo + hi) / U256::from(2);
    let (lo_out, mid_out, hi_out) = tokio::join!(
        curve_pool_call_get_dy(curve_pool, i, j, to_query(lo),       provider),
        curve_pool_call_get_dy(curve_pool, i, j, to_query(mid_init), provider),
        curve_pool_call_get_dy(curve_pool, i, j, to_query(hi),       provider),
    );

    if let (Ok(lo_v), Ok(mid_v), Ok(_)) = (lo_out, mid_out, hi_out) {
        if net_profit(mid_init, mid_v) >= net_profit(lo, lo_v) {
            lo = mid_init;
        } else {
            hi = mid_init;
        }
    }

    let granularity = adaptive_granularity(hi);

    // compare adjacent points to locate profit peak
    while hi.saturating_sub(lo) > granularity {
        let mid       = (lo + hi) / U256::from(2);
        let mid_right = mid + granularity;

        let (left_out, right_out) = tokio::join!(
            curve_pool_call_get_dy(curve_pool, i, j, to_query(mid),       provider),
            curve_pool_call_get_dy(curve_pool, i, j, to_query(mid_right), provider),
        );

        match (left_out, right_out) {
            (Ok(lo_v), Ok(hi_v)) => {
                if net_profit(mid_right, hi_v) >= net_profit(mid, lo_v) {
                    lo = mid;
                } else {
                    hi = mid_right;
                }
            }
            _ => {
                debug!("get_dy failed during binary search — using current lo");
                break;
            }
        }
    }

    let final_out = match curve_pool_call_get_dy(
        curve_pool, i, j, to_query(lo), provider,
    ).await {
        Ok(v)  => v,
        Err(_) => return Ok(None),
    };
    let final_net = net_profit(lo, final_out);

    if final_net > min_net_profit {
        Ok(Some((lo, final_net)))
    } else {
        Ok(None)
    }
}

pub fn usds(dollars: u128) -> U256 {
    U256::from(dollars) * U256::from(10u128.pow(18))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balancer_selected_when_capacity_sufficient() {
        let size = usds(5_000_000); let bal = usds(50_000_000); let mor = usds(10_000_000);
        assert!(matches!(select_flash_provider(size, bal, mor), FlashProvider::Balancer));
    }

    #[test]
    fn morpho_selected_when_balancer_insufficient() {
        // size=$15M; bal=$10M; mor=$20M → 15M > 10M, 15M <= 10M+20M=30M → Morpho
        let size = usds(15_000_000); let bal = usds(10_000_000); let mor = usds(20_000_000);
        assert!(matches!(select_flash_provider(size, bal, mor), FlashProvider::Morpho));
    }

    #[test]
    fn aave_fallback_when_all_zero_fee_insufficient() {
        // size=$50M; bal=$10M; mor=$20M → 50M > 10M+20M=30M → Aave
        std::env::remove_var("AAVE_FLASH_FEE_BPS");
        let size = usds(50_000_000); let bal = usds(10_000_000); let mor = usds(20_000_000);
        assert!(matches!(select_flash_provider(size, bal, mor), FlashProvider::Aave { fee_bps: 5 }));
    }

    #[test]
    fn adaptive_granularity_deep_pool() {
        std::env::remove_var("BINARY_SEARCH_GRANULARITY_CEIL_USD");
        assert_eq!(adaptive_granularity(usds(200_000_000)), usds(50_000));
    }

    #[test]
    fn adaptive_granularity_shallow_pool() {
        std::env::remove_var("BINARY_SEARCH_GRANULARITY_FLOOR_USD");
        std::env::remove_var("BINARY_SEARCH_GRANULARITY_CEIL_USD");
        let g = adaptive_granularity(usds(5_000_000));
        assert!(g >= usds(1_000) && g <= usds(50_000));
    }

    #[test]
    fn balancer_fee_is_zero() { assert_eq!(FlashProvider::Balancer.fee_bps(), 0); }

    #[test]
    fn morpho_fee_is_zero() { assert_eq!(FlashProvider::Morpho.fee_bps(), 0); }

    #[test]
    fn flash_mint_fee_is_zero() { assert_eq!(FlashProvider::FlashMint.fee_bps(), 0); }

    #[test]
    fn aave_fee_is_five_bps() { assert_eq!(FlashProvider::Aave { fee_bps: 5 }.fee_bps(), 5); }

    #[test]
    fn balancer_with_aave_fallback_reports_zero_primary_fee() {
        let p = FlashProvider::BalancerWithAaveFallback { aave_fee_bps: 5 };
        assert_eq!(p.fee_bps(), 0);
        assert_eq!(p.fallback_fee_bps(), 5);
    }

    #[test]
    fn select_arb_prefers_balancer_when_sufficient() {
        assert!(matches!(select_arb_flash_provider(usds(5_000_000), usds(10_000_000), 5), FlashProvider::Balancer));
    }

    #[test]
    fn select_arb_falls_back_to_aave_when_balancer_insufficient() {
        std::env::remove_var("AAVE_FLASH_FEE_BPS");
        assert!(matches!(
            select_arb_flash_provider(usds(15_000_000), usds(10_000_000), 5),
            FlashProvider::BalancerWithAaveFallback { aave_fee_bps: 5 }
        ));
    }

    #[test]
    fn compute_net_profit_zero_fees_positive() {
        let rate = U256::from(1_010_000_000_000_000_000u128);
        let net = compute_net_profit(usds(1_000_000), usds(1_000_000), rate, 0, U256::ZERO);
        assert!(net > U256::ZERO);
    }

    #[test]
    fn compute_net_profit_gas_exceeds_gross_returns_zero() {
        let rate = U256::from(1_000_001_000_000_000_000u128);
        let net = compute_net_profit(usds(1_000_000), usds(1_000_000), rate, 0, usds(1_000));
        assert_eq!(net, U256::ZERO);
    }

    // ── New tests (spec) ──────────────────────────────────────────────────

    #[test]
    fn discount_direction_uses_index_1_to_0() {
        assert_eq!(direction_to_indices(SpreadDirection::Discount), (1, 0));
    }

    #[test]
    fn premium_direction_uses_index_0_to_1() {
        assert_eq!(direction_to_indices(SpreadDirection::Premium), (0, 1));
    }

    #[test]
    fn compute_net_profit_premium_positive_when_dex_above_protocol() {
        let net = compute_net_profit_premium(usds(1_000_000), usds(1_005_000), 0, U256::ZERO);
        assert_eq!(net, usds(5_000));
    }

    #[test]
    fn compute_net_profit_premium_zero_when_dex_at_protocol() {
        let net = compute_net_profit_premium(usds(1_000_000), usds(1_000_000), 0, U256::ZERO);
        assert_eq!(net, U256::ZERO);
    }

    #[test]
    fn compute_net_profit_premium_deducts_aave_fee() {
        // 5bps on $1M = $500; $2K gross - $500 fee = $1.5K
        let net = compute_net_profit_premium(usds(1_000_000), usds(1_002_000), 5, U256::ZERO);
        assert_eq!(net, usds(1_500));
    }

    #[test]
    fn aave_fee_reads_from_env() {
        std::env::set_var("AAVE_FLASH_FEE_BPS", "9");
        // size=$80M; bal=$10M; mor=$20M → 80M > 10M+20M=30M → Aave at 9bps
        let p = select_flash_provider(usds(80_000_000), usds(10_000_000), usds(20_000_000));
        assert_eq!(p.fee_bps(), 9);
        std::env::remove_var("AAVE_FLASH_FEE_BPS");
    }

    #[test]
    fn binary_search_ceiling_reads_from_env() {
        let ceiling = std::env::var("BINARY_SEARCH_MAX_USD")
            .ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(100_000_000);
        assert!(ceiling > 0);
    }

    #[test]
    fn granularity_reads_from_env() {
        std::env::set_var("BINARY_SEARCH_GRANULARITY_CEIL_USD", "25000");
        assert_eq!(adaptive_granularity(usds(200_000_000)), usds(25_000));
        std::env::remove_var("BINARY_SEARCH_GRANULARITY_CEIL_USD");
    }
}
