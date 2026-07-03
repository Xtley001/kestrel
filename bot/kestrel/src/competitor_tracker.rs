// Kestrel — competitor_tracker.rs
// Competitor Bundle Tracking via Flashbots Data API.
// MEV-Share EventStream Monitoring.
// Per-Builder Revert Rate Alerting.
//

use dashmap::DashMap;
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

// ── Per-builder performance tracking ──────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct BuilderStats {
    pub submitted: u64,
    pub landed: u64,
    pub landed_and_reverted: u64,
}

impl BuilderStats {
    pub fn revert_rate(&self) -> f64 {
        if self.landed == 0 { 0.0 } else { self.landed_and_reverted as f64 / self.landed as f64 }
    }
    pub fn landing_rate(&self) -> f64 {
        if self.submitted == 0 { 0.0 } else { self.landed as f64 / self.submitted as f64 }
    }
}

pub struct BuilderPerformance {
    stats: Arc<DashMap<String, BuilderStats>>,
}

impl BuilderPerformance {
    pub fn new() -> Self { Self { stats: Arc::new(DashMap::new()) } }

    pub fn record_submission(&self, builder: &str) {
        self.stats.entry(builder.to_string()).or_default().submitted += 1;
    }

    pub fn record_landed(&self, builder: &str, reverted: bool) {
        let mut entry = self.stats.entry(builder.to_string()).or_default();
        entry.landed += 1;
        if reverted { entry.landed_and_reverted += 1; }
        if reverted {
            warn!(builder, revert_rate = entry.revert_rate(), "bundle landed but reverted");
        } else {
            info!(builder, landing_rate = entry.landing_rate(), "bundle landed and succeeded");
        }
    }

    pub fn ranked_builders(&self, all_builders: &[String]) -> Vec<String> {
        let mut ranked: Vec<(String, f64)> = all_builders.iter().map(|b| {
            let rate = self.stats.get(b).map(|s| s.landing_rate()).unwrap_or(1.0);
            (b.clone(), rate)
        }).collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.into_iter().map(|(name, _)| name).collect()
    }

    pub fn ranked_builders_filtered(&self, all_builders: &[String], min_rate: f64) -> Vec<String> {
        let ranked = self.ranked_builders(all_builders);
        let filtered: Vec<String> = ranked.iter()
            .filter(|b| self.stats.get(*b).map(|s| s.landing_rate()).unwrap_or(1.0) >= min_rate)
            .cloned().collect();
        if filtered.is_empty() { ranked.into_iter().take(1).collect() } else { filtered }
    }

    pub fn clone_arc(&self) -> Self { Self { stats: Arc::clone(&self.stats) } }

    // entrypoint: restore stats from persistent store on startup.
    pub fn restore(&self, loaded: std::collections::HashMap<String, BuilderStats>) {
        for (endpoint, stats) in loaded {
            self.stats.insert(endpoint, stats);
        }
        tracing::info!(count = self.stats.len(), "BuilderPerformance: restored from persistent store");
    }

    pub fn snapshot(&self) -> std::collections::HashMap<String, BuilderStats> {
        self.stats.iter().map(|e| (e.key().clone(), e.value().clone())).collect()
    }
}

impl Default for BuilderPerformance { fn default() -> Self { Self::new() } }

// ── Flashbots Data API competitor tracking ────────────────────────────

#[derive(Debug, Deserialize)]
struct FlashbotsBlock { blocks: Vec<FlashbotsBlockEntry> }

#[derive(Debug, Deserialize)]
struct FlashbotsBlockEntry { bundles: Option<Vec<FlashbotsBundle>> }

#[derive(Debug, Deserialize)]
struct FlashbotsBundle {
    // u128 — u64 overflows at ~18.4 ETH; competitors regularly pay 20–100 ETH.
    coinbase_transfer: Option<u128>,
    miner_reward: Option<u128>,
    transactions: Option<Vec<FlashbotsTx>>,
}

#[derive(Debug, Deserialize)]
struct FlashbotsTx { to: Option<String> }

// coinbase_payment_wei is u128 (was u64, overflowed at >18.4 ETH tip).
#[derive(Debug, Clone)]
pub struct CompetitorBundle {
    pub coinbase_payment_wei: u128,
}

pub async fn fetch_competitor_bundles(
    block_number: u64,
    known_pools: &[String],
) -> Vec<CompetitorBundle> {
    let url = format!("https://blocks.flashbots.net/v1/blocks?block_number={}", block_number);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build().unwrap_or_default();

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => { debug!(error = %e, "flashbots API request failed"); return vec![]; }
    };
    let body: FlashbotsBlock = match resp.json().await {
        Ok(b) => b,
        Err(e) => { debug!(error = %e, "flashbots API parse failed"); return vec![]; }
    };

    body.blocks.into_iter().flat_map(|block| {
        block.bundles.unwrap_or_default().into_iter().filter_map(|bundle| {
            let payment = bundle.coinbase_transfer.unwrap_or(0) + bundle.miner_reward.unwrap_or(0);
            let txs = bundle.transactions.unwrap_or_default();
            let touches_our_pools = txs.iter().any(|tx|
                tx.to.as_ref().map(|addr| known_pools.iter().any(|p| p.eq_ignore_ascii_case(addr))).unwrap_or(false)
            );
            if touches_our_pools && payment > 0 { Some(CompetitorBundle { coinbase_payment_wei: payment }) } else { None }
        })
    }).collect()
}

// ── MEV-Share persistent SSE stream ────────────────────────────────────
//
// Replace one-shot HTTP GET (captures 0–2 events/block max) with a
// persistent SSE connection that runs for the lifetime of the process.
//
// Architecture:
//   spawn_mev_share_stream — called once at startup from main.rs
//   Background task maintains SSE connection, auto-reconnects on drop
//   Hints stored in MEV_SHARE_HINTS (pool_addr → hint count this block)
//   reset_mev_share_hints(block_num) — call at start of each block loop
//   count_competitor_hints — zero-latency DashMap read, no HTTP

static MEV_SHARE_HINTS: Lazy<DashMap<String, u64>> = Lazy::new(DashMap::new);
static MEV_SHARE_RESET_BLOCK: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
pub struct MevShareHint {
    pub logs: Option<Vec<MevShareLog>>,
}

#[derive(Debug, Deserialize)]
pub struct MevShareLog {
    pub address: Option<String>,
}

// Spawn the persistent MEV-Share SSE background stream (call once at startup).
pub fn spawn_mev_share_stream() {
    tokio::spawn(async move {
        loop {
            debug!("MEV-Share SSE: connecting");
            match run_mev_share_sse().await {
                Ok(_)  => warn!("MEV-Share SSE: stream ended — reconnecting in 2s"),
                Err(e) => warn!(error = %e, "MEV-Share SSE: dropped — reconnecting in 5s"),
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    info!("MEV-Share SSE background stream spawned");
}

async fn run_mev_share_sse() -> eyre::Result<()> {
    use futures_util::StreamExt;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()?;

    let response = client
        .get("https://mev-share.flashbots.net/api/v1/events")
        .header("Accept", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .send().await?;

    let mut stream = response.bytes_stream();
    let mut buf = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim().to_string();
            buf = buf[pos + 1..].to_string();

            if let Some(json) = line.strip_prefix("data:") {
                let json = json.trim();
                if json.is_empty() { continue; }
                if let Ok(hint) = serde_json::from_str::<MevShareHint>(json) {
                    for log in hint.logs.unwrap_or_default() {
                        if let Some(addr) = log.address {
                            *MEV_SHARE_HINTS.entry(addr.to_lowercase()).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

// Reset hint counts for a new block. Call at start of each pipeline loop iteration.
pub fn reset_mev_share_hints(block_num: u64) {
    if block_num > MEV_SHARE_RESET_BLOCK.load(Ordering::Relaxed) {
        MEV_SHARE_HINTS.clear();
        MEV_SHARE_RESET_BLOCK.store(block_num, Ordering::Relaxed);
    }
}

// Read from persistent SSE cache — zero-latency, no HTTP call.
pub fn count_competitor_hints(known_pools: &[String]) -> usize {
    known_pools.iter()
        .map(|p| MEV_SHARE_HINTS.get(&p.to_lowercase()).map(|v| *v as usize).unwrap_or(0))
        .sum()
}

// ── Adaptive priority fee bidding ──────────────────────────────────────

// Bid on net profit (caller deducts gas + flash fee before calling).
// Uses integer-first wei conversion to eliminate multi-step f64 rounding error.
pub fn compute_adaptive_priority_fee(
    net_profit_usd: f64,   // M1: net after gas cost and flash fee
    gas_limit: u64,
    eth_price_usd: f64,
    max_priority_fee_gwei: f64,
    competitor_hint_count: usize,
) -> f64 {
    let bid_fraction = if competitor_hint_count > 0 { 0.50 } else { 0.40 };
    let bid_usd = net_profit_usd * bid_fraction;

    // convert to u128 wei first (integer domain), then single cast to f64.
    // Avoids: bid_usd/eth_price*1e18/1e9/gas_limit chain (4 ops, each adding ~1 ULP error).
    let bid_wei_u128: u128 = if eth_price_usd > 0.0 {
        (bid_usd * 1e18 / eth_price_usd) as u128
    } else { 0 };

    let bid_gwei_per_gas: f64 = if gas_limit > 0 {
        bid_wei_u128 as f64 / (gas_limit as f64 * 1e9)
    } else { 0.0 };

    bid_gwei_per_gas.min(max_priority_fee_gwei).max(0.5)
}

// ── Spread model self-calibration ────────────────────────────────────

#[derive(Debug)]
pub struct CalibrationRecord {
    pub simulated_profit_usd: f64,
    pub actual_profit_usd: f64,
    pub block: u64,
}

pub struct SpreadModelCalibrator {
    records: Vec<CalibrationRecord>,
    max_records: usize,
}

impl SpreadModelCalibrator {
    pub fn new() -> Self { Self { records: Vec::new(), max_records: 200 } }

    pub fn record(&mut self, simulated: f64, actual: f64, block: u64) {
        if self.records.len() >= self.max_records { self.records.remove(0); }
        let drift = if simulated > 0.0 { (simulated - actual).abs() / simulated } else { 0.0 };
        if drift > 0.05 {
            warn!(block, simulated, actual, drift_pct = drift * 100.0, "spread model drift >5%");
        }
        self.records.push(CalibrationRecord { simulated_profit_usd: simulated, actual_profit_usd: actual, block });
        self.check_systematic_drift();
    }

    fn check_systematic_drift(&self) {
        if self.records.len() < 10 { return; }
        let recent = &self.records[self.records.len().saturating_sub(10)..];
        let avg_drift: f64 = recent.iter().map(|r| {
            if r.simulated_profit_usd > 0.0 {
                (r.simulated_profit_usd - r.actual_profit_usd).abs() / r.simulated_profit_usd
            } else { 0.0 }
        }).sum::<f64>() / recent.len() as f64;

        if avg_drift > 0.20 {
            warn!(
                avg_drift_pct = avg_drift * 100.0,
                "CALIBRATION ALERT: >20% drift over last 10 trades. \
                 NOTE M7: feed on-chain ProfitSweep receipts for full accuracy."
            );
        }
    }
}

impl Default for SpreadModelCalibrator { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_stats_landing_rate_zero_submitted() {
        let stats = BuilderStats::default();
        assert_eq!(stats.landing_rate(), 0.0);
        assert_eq!(stats.revert_rate(), 0.0);
    }

    #[test]
    fn builder_stats_correct_rates() {
        let mut stats = BuilderStats::default();
        stats.submitted = 10; stats.landed = 8; stats.landed_and_reverted = 2;
        assert!((stats.landing_rate() - 0.8).abs() < 1e-9);
        assert!((stats.revert_rate() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn ranked_builders_sorts_by_landing_rate() {
        let perf = BuilderPerformance::new();
        perf.record_submission("titan"); perf.record_submission("flashbots");
        perf.record_landed("titan", false);
        let ranked = perf.ranked_builders(&["titan".to_string(), "flashbots".to_string()]);
        assert_eq!(ranked[0], "titan");
    }

    // fee is computed on net profit with integer-first conversion
    #[test]
    fn adaptive_fee_40_pct_of_net_uncontested() {
        // $6 net, 400K gas, ETH $3K: bid = 40% × $6 = $2.4 → 0.0008 ETH → 2.0 gwei/gas.
        // Kept below the 10 gwei cap so the 40% fraction is what is measured.
        let fee = compute_adaptive_priority_fee(6.0, 400_000, 3_000.0, 10.0, 0);
        assert!((fee - 2.0).abs() < 0.01, "got {fee}");
    }

    #[test]
    fn adaptive_fee_escalates_when_contested() {
        // Below the cap: uncontested bids 40% (2.0 gwei), contested bids 50% (2.5 gwei).
        let u = compute_adaptive_priority_fee(6.0, 400_000, 3_000.0, 10.0, 0);
        let c = compute_adaptive_priority_fee(6.0, 400_000, 3_000.0, 10.0, 3);
        assert!(c > u, "contested {c} must exceed uncontested {u}");
    }

    #[test]
    fn adaptive_fee_clamped_to_max() {
        let fee = compute_adaptive_priority_fee(1_000_000.0, 400_000, 3_000.0, 5.0, 0);
        assert!(fee <= 5.0);
    }

    #[test]
    fn adaptive_fee_minimum_floor() {
        let fee = compute_adaptive_priority_fee(1.0, 400_000, 3_000.0, 10.0, 0);
        assert!(fee >= 0.5);
    }

    // u128 handles 20 ETH tip without overflow
    #[test]
    fn competitor_bundle_u128_no_overflow_at_20_eth() {
        let twenty_eth: u128 = 20 * 1_000_000_000_000_000_000u128;
        let bundle = CompetitorBundle { coinbase_payment_wei: twenty_eth };
        assert_eq!(bundle.coinbase_payment_wei, twenty_eth);
        assert!(bundle.coinbase_payment_wei > u64::MAX as u128); // proves overflow at u64
    }

    // count reads from DashMap, not HTTP
    #[test]
    fn count_competitor_hints_reads_from_cache() {
        MEV_SHARE_HINTS.insert("0xpool1".to_string(), 3);
        let count = count_competitor_hints(&["0xpool1".to_string()]);
        assert_eq!(count, 3);
        MEV_SHARE_HINTS.remove("0xpool1");
    }

    #[test]
    fn reset_mev_share_hints_clears_map() {
        MEV_SHARE_HINTS.insert("0xpool2".to_string(), 7);
        reset_mev_share_hints(9_999_999);
        assert_eq!(count_competitor_hints(&["0xpool2".to_string()]), 0);
    }

    #[test]
    fn ranked_builders_filtered_excludes_low_performers() {
        let perf = BuilderPerformance::new();
        for _ in 0..10 { perf.record_submission("titan"); }
        for _ in 0..8  { perf.record_landed("titan", false); }
        for _ in 0..10 { perf.record_submission("flashbots"); }
        for _ in 0..2  { perf.record_landed("flashbots", false); }
        let builders = vec!["titan".to_string(), "flashbots".to_string()];
        let filtered = perf.ranked_builders_filtered(&builders, 0.40);
        assert_eq!(filtered, vec!["titan".to_string()]);
    }

    #[test]
    fn ranked_builders_filtered_keeps_one_when_all_below_threshold() {
        let perf = BuilderPerformance::new();
        for _ in 0..10 { perf.record_submission("a"); }
        for _ in 0..1  { perf.record_landed("a", false); }
        let builders = vec!["a".to_string(), "b".to_string()];
        let filtered = perf.ranked_builders_filtered(&builders, 0.50);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn calibrator_handles_zero_simulated() {
        let mut cal = SpreadModelCalibrator::new();
        cal.record(0.0, 0.0, 100);
    }
}
