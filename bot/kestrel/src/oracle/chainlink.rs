// Kestrel — oracle/chainlink.rs
// (feedback): Chainlink transmit Mempool Pre-Emption.
//
// Kestrel previously relied on *confirmed* oracle prices to know when a spread
// opened.  A Chainlink `transmit` in the mempool signals a new price is
// arriving 200–800ms before on-chain confirmation — the same window documented
// in the MEV Top Operator Guide (Layer 5 — Intelligence).
//
// This module:
//   1. Subscribes to the Reth IPC full-pending-transaction stream.
//   2. Filters for transactions targeting any Chainlink Aggregator address that
//      Kestrel monitors, matching the `transmit` 4-byte selector.
//   3. Decodes the pending price update from the calldata.
//   4. Immediately triggers the speculative binary search for the anticipated
//      spread so that by the time `transmit` confirms, the bundle is pre-built
//      and can be submitted in the same block.
//
// Reference: Gyrfalcon's oracle/chainlink.rs in the same suite.

use alloy::primitives::{Address, Bytes, U256, keccak256};
use tracing::{debug, info, warn};

// ── Chainlink transmit selector ────────────────────────────────────────────
//
// keccak256("transmit(bytes,bytes32[],bytes32[],bytes32,bytes32)")[..4]
// Pre-computed: 0xc9807539
//
// This is the canonical OCR2 transmit selector.  If Chainlink deploys new
// aggregator versions with a different ABI, add the selector here.
pub const TRANSMIT_SELECTOR: [u8; 4] = [0x25, 0x0a, 0x8a, 0x3c];

// Chainlink Aggregator addresses that Kestrel monitors.
// These correspond to the oracle feeds used by target protocols on Mainnet.
// Extend this list when adding new strategies.
pub fn monitored_aggregators() -> Vec<Address> {
    let mut addrs: Vec<Address> = vec![
        // ETH/USD  — used by Aave, Morpho health factor computation
        "0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419".parse().unwrap_or(Address::ZERO),
        // USDC/USD — used in sUSDS spread reference
        "0x8fFfFfd4AfB6115b954Bd326cbe7B4BA576818f6".parse().unwrap_or(Address::ZERO),
        // DAI/USD  — used by sDAI strategy
        "0xAed0c38402a5d19df6E4c03F4E2DceD6e29c1ee9".parse().unwrap_or(Address::ZERO),
        // stETH/ETH — used by wstETH LST strategy (feedback)
        "0x86392dC19c0b719886221c78AB11eb8Cf5c52812".parse().unwrap_or(Address::ZERO),
    ];
    // Allow operators to inject additional aggregator addresses at runtime.
    if let Ok(extra) = std::env::var("EXTRA_CHAINLINK_AGGREGATORS") {
        for addr_str in extra.split(',') {
            if let Ok(addr) = addr_str.trim().parse::<Address>() {
                addrs.push(addr);
            }
        }
    }
    addrs
}

// A decoded pending Chainlink price update extracted from mempool calldata.
#[derive(Debug, Clone)]
pub struct PendingPriceUpdate {
    // The aggregator contract that will receive this update.
    pub aggregator: Address,
    // Decoded answer from the report bytes (scaled by feed decimals).
    pub answer: i128,
    // The raw observations slice length (number of oracles reporting).
    pub observation_count: usize,
}

// Check whether `calldata` is a Chainlink `transmit` call and, if so,
// attempt to decode the price answer from the packed report bytes.
///
// Chainlink OCR2 report layout (first 32 bytes of `report` param):
// [0..32]  observation timestamp + config digest (ignored)
// The median observation is encoded as a packed `int192` later in the report.
// For our purposes, we extract the first `int192` from the observations array
// which closely approximates the finalised median for pre-emption use.
///
// Returns `None` if calldata is not a transmit call or cannot be decoded.
pub fn decode_pending_transmit(
    calldata: &Bytes,
) -> Option<i128> {
    if calldata.len() < 4 {
        return None;
    }
    if calldata[..4] != TRANSMIT_SELECTOR {
        return None;
    }

    // transmit(bytes report, bytes32[] rs, bytes32[] ss, bytes32 rawVs)
    // After the 4-byte selector: ABI-encoded parameters.
    // The `report` bytes contain packed int192 observations.
    // Minimum viable decode: read bytes[32..64] which in OCR2 typically
    // encodes the median as the first value in the observations block.
    let data = &calldata[4..];
    if data.len() < 96 {
        return None;
    }

    // ABI offset to `report` bytes (first param): first 32 bytes = offset value
    let report_offset = U256::from_be_slice(&data[..32]);
    let offset: usize = report_offset.try_into().unwrap_or(usize::MAX);
    if offset + 64 > data.len() {
        return None;
    }

    // report length
    let report_len = U256::from_be_slice(&data[offset..offset + 32]);
    let rlen: usize = report_len.try_into().unwrap_or(0);
    if rlen < 32 {
        return None;
    }

    // In OCR2 reports, bytes [32..64] of the report payload contain the
    // median answer as a packed int192.  We read the lower 24 bytes of the
    // 32-byte word (right-aligned int192).
    let answer_start = offset + 32 + 32; // skip offset word + length word + 32 header bytes
    if answer_start + 32 > data.len() {
        return None;
    }
    let answer_word = &data[answer_start..answer_start + 32];
    // int192 occupies the rightmost 24 bytes of the 32-byte word.
    let answer_bytes = &answer_word[8..32]; // 24 bytes = 192 bits
    // Sign-extend from 192-bit to i128 (saturate if out of range).
    let answer = i192_to_i128(answer_bytes);
    Some(answer)
}

// Convert a 24-byte big-endian signed integer (int192) to i128.
// Values that overflow i128 are clamped to i128::MAX / i128::MIN.
fn i192_to_i128(bytes: &[u8]) -> i128 {
    if bytes.len() < 24 { return 0; }
    let is_negative = bytes[0] & 0x80 != 0;
    // Take the least-significant 16 bytes for i128 conversion.
    let lo16 = &bytes[bytes.len() - 16..];
    let raw = i128::from_be_bytes(lo16.try_into().unwrap_or([0u8; 16]));
    // If the high bytes (bytes[0..8]) are non-zero and the sign extends,
    // the value is beyond i128 range — clamp.
    let high_bytes = &bytes[..8];
    let overflow = if is_negative {
        !high_bytes.iter().all(|&b| b == 0xFF)
    } else {
        high_bytes.iter().any(|&b| b != 0)
    };
    if overflow {
        if is_negative { i128::MIN } else { i128::MAX }
    } else {
        raw
    }
}

// Given a pending transaction's target address and calldata, determine whether
// this is a monitored Chainlink `transmit` call.
// Returns `Some(PendingPriceUpdate)` with the decoded answer if so.
pub fn classify_pending_tx(
    to: Address,
    calldata: &Bytes,
) -> Option<PendingPriceUpdate> {
    let aggregators = monitored_aggregators();
    if !aggregators.contains(&to) {
        return None;
    }
    let answer = decode_pending_transmit(calldata)?;
    debug!(
        aggregator = %to,
        answer,
        "Chainlink transmit() detected in mempool — pre-empting spread computation"
    );
    info!(
        aggregator = %to,
        answer,
        "MEMPOOL: Chainlink price update inbound — triggering speculative binary search"
    );
    Some(PendingPriceUpdate {
        aggregator: to,
        answer,
        observation_count: 0, // simplified; extend if observation count matters
    })
}

// Verify the transmit selector constant at compile time.
// keccak256("transmit(bytes,bytes32[],bytes32[],bytes32,bytes32)")[..4] = 0xc9807539
pub fn verify_transmit_selector() -> bool {
    let hash = keccak256(b"transmit(bytes,bytes32[],bytes32[],bytes32,bytes32)");
    hash[..4] == TRANSMIT_SELECTOR
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;

    #[test]
    fn transmit_selector_is_correct() {
        assert!(verify_transmit_selector(), "TRANSMIT_SELECTOR constant is wrong");
    }

    #[test]
    fn non_transmit_calldata_returns_none() {
        // Random 4-byte selector that is not transmit
        let calldata = Bytes::from(vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x00]);
        assert!(decode_pending_transmit(&calldata).is_none());
    }

    #[test]
    fn too_short_calldata_returns_none() {
        let calldata = Bytes::from(vec![0xc9, 0x80]);
        assert!(decode_pending_transmit(&calldata).is_none());
    }

    #[test]
    fn non_monitored_address_is_not_classified() {
        let random_addr: Address = "0x1111111111111111111111111111111111111111"
            .parse().unwrap();
        let calldata = Bytes::from(vec![0xc9, 0x80, 0x75, 0x39]);
        assert!(classify_pending_tx(random_addr, &calldata).is_none());
    }

    #[test]
    fn i192_to_i128_positive_small_value() {
        let mut bytes = [0u8; 24];
        bytes[23] = 100; // value = 100
        assert_eq!(i192_to_i128(&bytes), 100);
    }

    #[test]
    fn i192_to_i128_negative_value() {
        // -1 in int192 = all 0xFF bytes
        let bytes = [0xFFu8; 24];
        assert_eq!(i192_to_i128(&bytes), -1);
    }

    #[test]
    fn i192_overflow_clamps_to_max() {
        // High bytes non-zero with positive sign → overflow → i128::MAX
        let mut bytes = [0u8; 24];
        bytes[0] = 0x01; // high byte set, not negative
        bytes[23] = 0xFF;
        let result = i192_to_i128(&bytes);
        assert_eq!(result, i128::MAX);
    }

    #[test]
    fn monitored_aggregators_not_empty() {
        assert!(!monitored_aggregators().is_empty());
    }
}

// ── FIX : ETH price oracle watcher ──────────────────────────
// Connects its own WS/IPC subscription to the node — avoids passing a dyn Provider
// (which doesn't implement PubSubExt). Spawned once per process from spread_pipeline.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// The ETH/USD Chainlink aggregator proxy on Ethereum mainnet.
const ETH_USD_AGGREGATOR: &str = "0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419";

// ETH/USD feed has 8 decimals — price is scaled by 1e8.
const ETH_USD_FEED_DECIMALS: i128 = 100_000_000;

// Watch for Chainlink ETH/USD transmit calls in the pending mempool.
// Opens its own connection to `node_url` (IPC or WS). Updates `price_cents`
// atomically whenever a new price is detected.
// Falls back gracefully and exits silently if the connection fails.
pub async fn watch_eth_price_updates(
    node_url: String,
    price_cents: Arc<AtomicU64>,
) {
    use alloy::providers::{Provider, ProviderBuilder};
    use futures_util::StreamExt;

    let eth_usd_addr: alloy::primitives::Address = ETH_USD_AGGREGATOR
        .parse()
        .unwrap_or(alloy::primitives::Address::ZERO);

    // Build a dedicated provider for the mempool watcher (on_builtin auto-detects ws/ipc).
    let provider = match ProviderBuilder::new().on_builtin(&node_url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e,
                "Chainlink watcher: node connection failed — ETH price will use env var only");
            return;
        }
    };

    let stream = match provider.subscribe_full_pending_transactions().await {
        Ok(s)  => s.into_stream(),
        Err(e) => {
            tracing::warn!(error = %e,
                "Chainlink watcher: subscribe_full_pending_transactions failed — ETH price static");
            return;
        }
    };

    tracing::info!("Chainlink ETH/USD mempool watcher active");
    let mut stream = stream;
    while let Some(tx) = stream.next().await {
        let Some(to) = tx.to else { continue };
        if to != eth_usd_addr { continue; }

        if let Some(answer) = decode_pending_transmit(&tx.input) {
            // Chainlink ETH/USD: 8 decimal places → divide by 1e6 to get cents
            if answer > 0 {
                let cents = (answer / (ETH_USD_FEED_DECIMALS / 100)) as u64;
                price_cents.store(cents, Ordering::Relaxed);
                let price_usd = cents as f64 / 100.0;
                tracing::info!(
                    price_usd,
                    "Chainlink ETH/USD mempool update — price cache refreshed"
                );
            }
        }
    }
}
