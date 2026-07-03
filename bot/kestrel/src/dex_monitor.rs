// Kestrel — dex_monitor.rs
// DEX price + spread helpers. The pipeline queries prices on demand via get_dex_prices
// and curve_pool_call_get_dy; there is no standing log subscription (see cleanup note).

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::sol;
use eyre::Result;

// Spread direction as specified in Section 2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpreadDirection {
    // DEX price < protocol rate → buy cheap on DEX, redeem at protocol
    Discount,
    // DEX price > protocol rate → mint at protocol, sell on DEX at premium
    Premium,
}

// Live state for a single monitored pool.
#[derive(Debug, Clone, Default)]
pub struct PoolState {
    pub pool: Address,
    pub reserve_0: U256,
    pub reserve_1: U256,
    pub last_exchange_block: u64,
    pub dex_spot_price: U256, // get_dy(usds_idx, susds_idx, 1e18) — updated on each event
    // Last decoded TokenExchange fields (W15 fix — previously stubbed)
    pub last_sold_id: i128,
    pub last_tokens_sold: U256,
    pub last_bought_id: i128,
    pub last_tokens_bought: U256,
}


// Compute the spread between protocol rate and DEX price.
// Returns (direction, magnitude) in raw U256 units.
// Both discount and premium directions are monitored per Section 2.
pub fn compute_spread(protocol_rate: U256, dex_price: U256) -> (SpreadDirection, U256) {
    if dex_price < protocol_rate {
        // Direction 1: DEX is cheap → buy DEX, redeem at protocol
        (SpreadDirection::Discount, protocol_rate - dex_price)
    } else {
        // Direction 2: DEX has premium → mint at protocol, sell on DEX
        (SpreadDirection::Premium, dex_price - protocol_rate)
    }
}

// Compute spread in basis points (bps) for display and thresholding.
pub fn spread_bps(protocol_rate: U256, dex_price: U256) -> u64 {
    let (_, magnitude) = compute_spread(protocol_rate, dex_price);
    if protocol_rate.is_zero() {
        return 0;
    }
    // bps = magnitude * 10000 / protocol_rate
    let bps = magnitude * U256::from(10000u64) / protocol_rate;
    bps.saturating_to::<u64>()
}

sol! {
    #[sol(rpc)]
    interface ICurvePool {
        function get_dy(int128 i, int128 j, uint256 dx) external view returns (uint256);
        event TokenExchange(
            address indexed buyer,
            int128 sold_id,
            uint256 tokens_sold,
            int128 bought_id,
            uint256 tokens_bought
        );
    }
}

// NOTE (cleanup): `run_dex_monitor` and `get_dex_spot_price` were removed — they were
// only reachable from the deleted whale_detector module. The live pipeline reads prices
// via `get_dex_prices` and `curve_pool_call_get_dy` on demand, so the event-subscription
// path (and its alloy PubSubExt import) is no longer needed.

// Query both directions and return both DEX prices **normalised to USDS-per-sUSDS**,
// so they are directly comparable to `protocol_rate` (= previewRedeem(1e18), also
// USDS-per-sUSDS).
///
// (unit bug): the raw quotes are in different units and were previously compared
// directly against `protocol_rate`, producing a false ~900bps spread every block:
// get_dy(1,0,1e18) = sUSDS received per 1 USDS in   → sUSDS-per-USDS (~0.95e18)
// get_dy(0,1,1e18) = USDS received per 1 sUSDS in   → USDS-per-sUSDS  (~1.05e18)
///
// Returns (discount_price, premium_price), both USDS-per-sUSDS:
// discount_price = effective cost to BUY 1 sUSDS on the DEX = 1e36 / get_dy(1,0,1e18)
// premium_price  = proceeds from SELLING 1 sUSDS on the DEX  = get_dy(0,1,1e18)
///
// A discount arb exists when discount_price < protocol_rate (buy cheap, redeem high).
// A premium arb exists when premium_price > protocol_rate (mint low, sell high).
pub async fn get_dex_prices<P: Provider>(
    pool: Address,
    provider: &P,
) -> Result<(U256, U256)> {
    let one = U256::from(10u128.pow(18));
    let contract = ICurvePool::new(pool, provider);
    // Bind the call builders so they outlive the futures inside join! (avoids E0716).
    let call_discount = contract.get_dy(1i128, 0i128, one); // USDS → sUSDS (sUSDS-per-USDS)
    let call_premium  = contract.get_dy(0i128, 1i128, one); // sUSDS → USDS (USDS-per-sUSDS)
    let (discount_result, premium_result) = tokio::join!(call_discount.call(), call_premium.call());
    let susds_per_usds = discount_result?._0;
    let premium_price = premium_result?._0;
    // Invert the buy quote into USDS-per-sUSDS: 1e18 * 1e18 / (sUSDS out per 1 USDS in).
    let discount_price = if susds_per_usds.is_zero() {
        U256::ZERO
    } else {
        one * one / susds_per_usds
    };
    Ok((discount_price, premium_price))
}

// Call get_dy with arbitrary indices and input size (for binary search).
pub async fn curve_pool_call_get_dy<P: Provider>(
    pool: Address,
    i: i128,
    j: i128,
    dx: U256,
    provider: &P,
) -> Result<U256> {
    let contract = ICurvePool::new(pool, provider);
    let result = contract.get_dy(i, j, dx).call().await?;
    Ok(result._0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discount_direction_dex_below_protocol() {
        let protocol_rate = U256::from(1_050_000_000_000_000_000u128); // 1.05e18
        let dex_price = U256::from(1_040_000_000_000_000_000u128);     // 1.04e18 (cheaper)
        let (dir, spread) = compute_spread(protocol_rate, dex_price);
        assert_eq!(dir, SpreadDirection::Discount);
        assert_eq!(spread, U256::from(10_000_000_000_000_000u128)); // 0.01e18
    }

    #[test]
    fn premium_direction_dex_above_protocol() {
        let protocol_rate = U256::from(1_050_000_000_000_000_000u128);
        let dex_price = U256::from(1_060_000_000_000_000_000u128);     // 1.06e18 (premium)
        let (dir, spread) = compute_spread(protocol_rate, dex_price);
        assert_eq!(dir, SpreadDirection::Premium);
        assert_eq!(spread, U256::from(10_000_000_000_000_000u128));
    }

    #[test]
    fn zero_spread_when_prices_equal() {
        let rate = U256::from(1_050_000_000_000_000_000u128);
        let (_, spread) = compute_spread(rate, rate);
        assert_eq!(spread, U256::ZERO);
    }

    #[test]
    fn spread_bps_calculation() {
        let protocol_rate = U256::from(1_050_000_000_000_000_000u128); // 1.05e18
        // 0.1% discount
        let dex_price = U256::from(1_048_950_000_000_000_000u128);
        let bps = spread_bps(protocol_rate, dex_price);
        assert!(bps >= 9 && bps <= 11, "expected ~10 bps, got {}", bps);
    }

    // ── New tests (spec) ──────────────────────────────────────────────────

    #[test]
    fn direction_to_indices_correct() {
        use crate::binary_search::direction_to_indices;
        // index 0 = sUSDS, index 1 = USDS
        assert_eq!(direction_to_indices(SpreadDirection::Discount), (1i128, 0i128));
        assert_eq!(direction_to_indices(SpreadDirection::Premium),  (0i128, 1i128));
    }

    #[test]
    fn spread_bps_symmetric_detection() {
        let protocol_rate = U256::from(1_050_000_000_000_000_000u128);
        let dex_below     = U256::from(1_040_000_000_000_000_000u128);
        let dex_above     = U256::from(1_060_000_000_000_000_000u128);
        assert!(spread_bps(protocol_rate, dex_below) > 0); // Discount
        assert!(spread_bps(protocol_rate, dex_above) > 0); // Premium
    }
}
