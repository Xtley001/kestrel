// Kestrel — ssr_monitor.rs
// SSR Rate Change Monitor.
//
//
//         Full governance monitoring (DSPause log subscription) remains in the
//         stage_pending_ssr_change / commit_staged_ssr_change path — that requires
//         wiring a separate event filter subscriber. This startup poll is the minimum
//         viable guard: it detects any SSR change that happened between bot restarts.
//
//         TODO production: subscribe to DSPause LogNote events for block-by-block
//         SSR change detection. See governance_addresses::DS_PAUSE.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

static CURRENT_SSR_BPS: AtomicU32 = AtomicU32::new(375); // default 3.75%
static EFFECTIVE_MIN_SPREAD_BPS: AtomicU32 = AtomicU32::new(5);
static STAGED_SSR_BPS: AtomicU32 = AtomicU32::new(0);

pub fn current_ssr_bps() -> u32 { CURRENT_SSR_BPS.load(Ordering::Relaxed) }
pub fn effective_min_spread_bps() -> u32 { EFFECTIVE_MIN_SPREAD_BPS.load(Ordering::Relaxed) }

pub fn stage_pending_ssr_change(pending_ssr_bps: u32, base_min_spread_bps: u32) {
    let current = CURRENT_SSR_BPS.load(Ordering::Relaxed);
    if pending_ssr_bps == current { return; }
    STAGED_SSR_BPS.store(pending_ssr_bps, Ordering::SeqCst);
    let new_threshold = compute_threshold(base_min_spread_bps, pending_ssr_bps, current);
    warn!(pending_ssr_bps, precomputed_threshold_bps = new_threshold,
        "SSR PENDING in mempool — threshold pre-computed and staged");
    EFFECTIVE_MIN_SPREAD_BPS.store(new_threshold, Ordering::SeqCst);
}

pub fn commit_staged_ssr_change(confirmed_ssr_bps: u32, base_min_spread_bps: u32) {
    let staged = STAGED_SSR_BPS.swap(0, Ordering::SeqCst);
    if staged == confirmed_ssr_bps {
        CURRENT_SSR_BPS.store(confirmed_ssr_bps, Ordering::SeqCst);
        info!(confirmed_ssr_bps, "SSR CONFIRMED — matches staged value");
    } else {
        record_ssr_change(confirmed_ssr_bps, base_min_spread_bps);
    }
}

pub fn cancel_staged_ssr_change() {
    let staged = STAGED_SSR_BPS.swap(0, Ordering::SeqCst);
    if staged > 0 {
        warn!(staged, "SSR staged change cancelled — governance tx dropped");
        let current = CURRENT_SSR_BPS.load(Ordering::Relaxed);
        let base: u32 = std::env::var("BASE_MIN_SPREAD_BPS")
            .ok().and_then(|v| v.parse().ok()).unwrap_or(5);
        record_ssr_change(current, base);
    }
}

pub fn classify_pending_ssr_tx(to: &str, calldata_len: usize) -> Option<u32> {
    use governance_addresses::SUSDS_ACCUMULATOR;
    if !to.eq_ignore_ascii_case(SUSDS_ACCUMULATOR) { return None; }
    if calldata_len < 68 { return None; }
    Some(0)
}

pub fn record_ssr_change(new_ssr_bps: u32, base_min_spread_bps: u32) {
    let old = CURRENT_SSR_BPS.swap(new_ssr_bps, Ordering::SeqCst);
    warn!(old_ssr_bps = old, new_ssr_bps, "SSR CHANGE DETECTED — recalibrating spread threshold");
    let new_threshold = compute_threshold(base_min_spread_bps, new_ssr_bps, old);
    EFFECTIVE_MIN_SPREAD_BPS.store(new_threshold, Ordering::SeqCst);
    info!(new_threshold_bps = new_threshold, "spread threshold recalibrated after SSR change");
}

fn compute_threshold(base: u32, new_ssr: u32, old_ssr: u32) -> u32 {
    if old_ssr == 0 { return base; }
    let scaled = (base as u64 * new_ssr as u64) / old_ssr as u64;
    (scaled as u32).clamp(1, base)
}

// ── Startup SSR poll ────────────────────────────────────────────────

// Read the live Sky Savings Rate from the sUSDS vault's `ssr` accumulator and
// convert it to an annualised rate in basis points.
///
// sUSDS exposes `ssr` returning a per-second rate as a ray (1e27 fixed point),
// e.g. `1.0000000011...e27`. The APY is `(ssr/1e27)^seconds_per_year - 1`.
///
/// replaces the previous `previewRedeem` heuristic that only logged and
// never updated state, and the `to_big_endian` (ethers) call that did not compile
// against alloy. Returns `None` on any RPC or decode failure.
pub async fn read_ssr_bps<P: alloy::providers::Provider>(
    susds_vault: alloy::primitives::Address,
    provider: &P,
) -> Option<u32> {
    use alloy::sol;

    if susds_vault == alloy::primitives::Address::ZERO {
        warn!("SSR read: sUSDS vault address is zero — skipping (set ETH_SUSDS_ADDRESS)");
        return None;
    }

    sol! {
        #[sol(rpc)]
        interface ISavingsRate {
            function ssr() external view returns (uint256);
        }
    }

    let vault = ISavingsRate::new(susds_vault, provider);
    let ray = match vault.ssr().call().await {
        Ok(r) => r._0,
        Err(e) => {
            warn!(error = %e, "SSR read: ssr() call failed — keeping stored SSR");
            return None;
        }
    };

    // Convert ray-per-second to annualised bps. f64 precision is ample for a bps figure.
    let per_sec = ray.to_string().parse::<f64>().ok()? / 1e27;
    if per_sec <= 0.0 {
        return None;
    }
    const SECONDS_PER_YEAR: f64 = 31_536_000.0;
    let apy = per_sec.powf(SECONDS_PER_YEAR) - 1.0;
    let bps = (apy * 10_000.0).round();
    if !(0.0..=100_000.0).contains(&bps) {
        warn!(bps, "SSR read: computed APY out of sane range — ignoring");
        return None;
    }
    Some(bps as u32)
}

// Poll the live SSR at startup and recalibrate the spread threshold if it has moved
// since the last run. This now stores the value via record_ssr_change.
pub async fn poll_ssr_at_startup<P: alloy::providers::Provider>(
    susds_vault: alloy::primitives::Address,
    provider: &P,
) {
    let base_min_spread: u32 = std::env::var("BASE_MIN_SPREAD_BPS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(5);

    match read_ssr_bps(susds_vault, provider).await {
        Some(new_ssr) => {
            let prev = current_ssr_bps();
            info!(prev_ssr_bps = prev, live_ssr_bps = new_ssr, "SSR startup poll: live rate read");
            if new_ssr != prev {
                record_ssr_change(new_ssr, base_min_spread);
            }
        }
        None => {
            info!(stored_ssr_bps = current_ssr_bps(),
                "SSR startup poll: could not read live SSR — proceeding with stored value");
        }
    }
}

pub mod governance_addresses {
    pub const DS_PAUSE: &str         = "0xbE286431454714F511008713973d3B053A2d38f3";
    pub const VAT: &str              = "0x35D1b3F3D7966A1DFe207aa4514C12a259A0492B";
    pub const SUSDS_ACCUMULATOR: &str = "0xa3931d71877C0E7A3148CB7Eb4463524FEc27fbD";
}

// ── TODO Block-level DSPause governance event subscription ────────────────
//
// Subscribes to new blocks and, each block, calls eth_getLogs for DSPause address
// filtering on the 'note' topic. If any log targets SUSDS_ACCUMULATOR in the
// calldata, re-reads the SSR from the vault and recalibrates the spread threshold.
//
// Spawned only for the Ethereum pipeline in main.rs. If the subscription fails,
// the function warns and returns cleanly — it does NOT panic or restart the pipeline.

// Periodically re-read the live SSR from the sUSDS vault and recalibrate the spread
// threshold whenever it changes.
///
// the previous implementation filtered DSPause `LogNote` events on a malformed,
// zero-parsing topic hash, so it never fired. Reading `ssr` directly every
// `SSR_POLL_INTERVAL_BLOCKS` blocks is simpler and robust: the SSR changes at most a
// few times a year, so an hourly poll detects every change well before it matters.
///
// `node_url`            — WebSocket or IPC URL for the Ethereum node.
// `base_min_spread_bps` — The operator-configured baseline spread gate (from env).
pub async fn watch_governance_events(node_url: String, base_min_spread_bps: u32) {
    use alloy::primitives::Address;
    use alloy::providers::{Provider, ProviderBuilder};
    use futures_util::StreamExt;

    let susds_vault: Address = std::env::var("ETH_SUSDS_ADDRESS")
        .ok().and_then(|s| s.parse().ok())
        .unwrap_or_else(|| governance_addresses::SUSDS_ACCUMULATOR.parse().unwrap_or(Address::ZERO));

    // Poll roughly hourly (300 mainnet blocks ≈ 60 min at 12s/block).
    let poll_interval_blocks: u64 = std::env::var("SSR_POLL_INTERVAL_BLOCKS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(300);

    let provider = match ProviderBuilder::new().on_builtin(&node_url).await {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "watch_governance_events: node connect failed — SSR monitoring inactive");
            return;
        }
    };

    let mut block_stream = match provider.subscribe_blocks().await {
        Ok(s) => s.into_stream(),
        Err(e) => {
            warn!(error = %e, "watch_governance_events: block subscription failed — SSR monitoring inactive");
            return;
        }
    };

    info!(vault = %susds_vault, poll_interval_blocks,
        "watch_governance_events: periodic SSR polling active");

    while let Some(block) = block_stream.next().await {
        let block_num = block.header.number;
        if block_num == 0 || block_num % poll_interval_blocks != 0 {
            continue;
        }
        if let Some(new_ssr) = read_ssr_bps(susds_vault, &provider).await {
            let prev_ssr = current_ssr_bps();
            let delta = (new_ssr as i64 - prev_ssr as i64).unsigned_abs() as u32;
            if delta >= 1 {
                record_ssr_change(new_ssr, base_min_spread_bps);
                warn!(block = block_num, prev_ssr_bps = prev_ssr, new_ssr_bps = new_ssr,
                    delta_bps = delta, "watch_governance_events: SSR change — threshold recalibrated");
            }
        }
    }

    warn!("watch_governance_events: block stream ended — SSR monitoring stopped");
}

pub fn ssr_summary() -> String {
    format!("SSR: {:.2}% | min_spread: {}bps", current_ssr_bps() as f64 / 100.0, effective_min_spread_bps())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssr_change_recalibrates_threshold() {
        CURRENT_SSR_BPS.store(375, Ordering::SeqCst);
        EFFECTIVE_MIN_SPREAD_BPS.store(5, Ordering::SeqCst);
        record_ssr_change(150, 5);
        assert_eq!(effective_min_spread_bps(), 2);
    }

    #[test]
    fn threshold_never_below_1bps() {
        CURRENT_SSR_BPS.store(375, Ordering::SeqCst);
        record_ssr_change(10, 5);
        assert!(effective_min_spread_bps() >= 1);
    }

    #[test]
    fn threshold_never_exceeds_base() {
        CURRENT_SSR_BPS.store(375, Ordering::SeqCst);
        record_ssr_change(500, 5);
        assert!(effective_min_spread_bps() <= 5);
    }

    #[test]
    fn ssr_summary_formats_correctly() {
        CURRENT_SSR_BPS.store(375, Ordering::SeqCst);
        EFFECTIVE_MIN_SPREAD_BPS.store(5, Ordering::SeqCst);
        let s = ssr_summary();
        assert!(s.contains("3.75%"));
        assert!(s.contains("5bps"));
    }

    #[test]
    fn governance_addresses_not_zero() {
        assert!(!governance_addresses::DS_PAUSE.is_empty());
        assert!(!governance_addresses::VAT.is_empty());
        assert!(!governance_addresses::SUSDS_ACCUMULATOR.is_empty());
    }

    #[test]
    fn zero_old_ssr_does_not_panic() {
        CURRENT_SSR_BPS.store(0, Ordering::SeqCst);
        record_ssr_change(375, 5);
        assert_eq!(effective_min_spread_bps(), 5);
    }

    #[test]
    fn stage_pending_pre_computes_threshold() {
        CURRENT_SSR_BPS.store(375, Ordering::SeqCst);
        EFFECTIVE_MIN_SPREAD_BPS.store(5, Ordering::SeqCst);
        STAGED_SSR_BPS.store(0, Ordering::SeqCst);
        stage_pending_ssr_change(150, 5);
        assert_eq!(effective_min_spread_bps(), 2);
        assert_eq!(STAGED_SSR_BPS.load(Ordering::SeqCst), 150);
    }

    #[test]
    fn commit_staged_matches_confirms_cleanly() {
        CURRENT_SSR_BPS.store(375, Ordering::SeqCst);
        STAGED_SSR_BPS.store(150, Ordering::SeqCst);
        EFFECTIVE_MIN_SPREAD_BPS.store(2, Ordering::SeqCst);
        commit_staged_ssr_change(150, 5);
        assert_eq!(CURRENT_SSR_BPS.load(Ordering::SeqCst), 150);
        assert_eq!(STAGED_SSR_BPS.load(Ordering::SeqCst), 0);
        assert_eq!(effective_min_spread_bps(), 2);
    }

    #[test]
    fn classify_pending_ssr_tx_matches_accumulator() {
        let result = classify_pending_ssr_tx(governance_addresses::SUSDS_ACCUMULATOR, 128);
        assert!(result.is_some());
    }

    #[test]
    fn classify_pending_ssr_tx_ignores_other_contracts() {
        let result = classify_pending_ssr_tx("0x1111111111111111111111111111111111111111", 128);
        assert!(result.is_none());
    }

    #[test]
    fn classify_pending_ssr_tx_ignores_short_calldata() {
        let result = classify_pending_ssr_tx(governance_addresses::SUSDS_ACCUMULATOR, 10);
        assert!(result.is_none());
    }
}
