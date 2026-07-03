// Kestrel — gas_budget.rs
// GasBudget — daily limit, priority fee ceiling, opportunity ranking.
//
// NET PROFIT FILTER (NetProfitFilter):
// ─────────────────────────────────────────────
// NetProfitFilter provides a utility method is_profitable for computing
// net profit after gas and flash fees. It is used in tests and tooling.
//
// The live submission gate in spread_pipeline.rs uses a dynamic per-strategy
// floor instead of the global MIN_NET_PROFIT_USD:
//
//   effective_floor = max(config.min_net_profit_usd, gas_cost_usd × config.gas_profit_multiplier)
//
// This scales automatically with live gas prices (mainnet protection) while
// keeping a chain-appropriate hard floor for L2 strategies where gas is negligible.
// Per-strategy values are set in chain_config.rs and env-backed via:
//   ETH_SUSDS_MIN_PROFIT_USD, ETH_SUSDS_GAS_MULTIPLIER, etc.
//
// MIN_NET_PROFIT_USD (env var) is retained as a legacy fallback read by
// NetProfitFilter::from_env — it no longer drives the main submission gate.

use chrono::Utc;
use tracing::warn;

// Net profit filter — computed per trade from live gas price.
// Reads MIN_NET_PROFIT_USD from env (default: $500).
// Gas cost in USD is computed as: gas_units × base_fee_gwei × 1e9 × eth_price_usd / 1e18
pub struct NetProfitFilter {
    // Minimum net profit in USD (after gas + flash fee) before a trade is submitted.
    // Configurable via MIN_NET_PROFIT_USD env var. Default: $500.
    pub min_net_profit_usd: f64,
}

impl NetProfitFilter {
    pub fn from_env() -> Self {
        // MIN_NET_PROFIT_USD default must match .env.example (both 500.0).
        let min = std::env::var("MIN_NET_PROFIT_USD")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(500.0);
        Self { min_net_profit_usd: min }
    }

    // Returns true if net profit exceeds the minimum threshold.
    ///
    // All inputs in USD. This is the gating call before any trade is submitted.
    // gross_profit_usd: raw arbitrage gain before any costs
    // gas_units:        estimated gas units (from revm simulation)
    // base_fee_gwei:    current base fee in gwei (from pending block header)
    // priority_fee_gwei:current priority fee to add on top
    // eth_price_usd:    live ETH price in USD (from price feed)
    // flash_fee_usd:    flash loan fee in USD (0 for Balancer/Morpho; ~0.05% for Aave)
    pub fn is_profitable(
        &self,
        gross_profit_usd: f64,
        gas_units: u64,
        base_fee_gwei: f64,
        priority_fee_gwei: f64,
        eth_price_usd: f64,
        flash_fee_usd: f64,
    ) -> bool {
        let total_fee_gwei  = base_fee_gwei + priority_fee_gwei;
        let gas_eth         = (gas_units as f64) * total_fee_gwei * 1e9 / 1e18;
        let gas_cost_usd    = gas_eth * eth_price_usd;
        let net_profit_usd  = gross_profit_usd - gas_cost_usd - flash_fee_usd;

        if net_profit_usd < self.min_net_profit_usd {
            tracing::debug!(
                gross  = gross_profit_usd,
                gas_usd = gas_cost_usd,
                flash  = flash_fee_usd,
                net    = net_profit_usd,
                min    = self.min_net_profit_usd,
                "trade rejected — net profit below threshold"
            );
            false
        } else {
            tracing::info!(
                gross  = gross_profit_usd,
                gas_usd = gas_cost_usd,
                flash  = flash_fee_usd,
                net    = net_profit_usd,
                "trade accepted — net profit clears threshold"
            );
            true
        }
    }

    // Net profit in USD for a given set of costs.
    // Exposed so callers can log / rank without duplicating the formula.
    pub fn net_profit_usd(
        &self,
        gross_profit_usd: f64,
        gas_units: u64,
        base_fee_gwei: f64,
        priority_fee_gwei: f64,
        eth_price_usd: f64,
        flash_fee_usd: f64,
    ) -> f64 {
        let gas_eth      = (gas_units as f64) * (base_fee_gwei + priority_fee_gwei) * 1e9 / 1e18;
        let gas_cost_usd = gas_eth * eth_price_usd;
        gross_profit_usd - gas_cost_usd - flash_fee_usd
    }
}

// Gas budget manager — enforces daily spending limits and priority fee ceiling.
#[derive(Debug, Clone)]
pub struct GasBudget {
    // Daily gas limit in gwei
    pub daily_limit_gwei: u64,
    // Gas spent today in gwei
    pub spent_today_gwei: u64,
    // Hard ceiling on max priority fee — never exceed even in competition
    pub max_priority_fee_gwei: u64,
    // UTC date of the current day (for midnight reset detection)
    pub today: chrono::NaiveDate,
}

// Opportunity to be ranked and filtered by the gas budget system.
#[derive(Debug, Clone)]
pub struct Opportunity {
    pub expected_profit: u128,
    pub estimated_gas: u64,
}

impl GasBudget {
    pub fn from_env() -> Self {
        let daily_limit_eth: f64 = std::env::var("DAILY_GAS_LIMIT_ETH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.5);
        let daily_limit_gwei = (daily_limit_eth * 1e9) as u64;

        // default was 3 in code but .env.example documented 10 — aligned to 10.
        // Verify .env.example and this default always match.
        let max_priority_fee_gwei: u64 = std::env::var("MAX_PRIORITY_FEE_GWEI")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        Self {
            daily_limit_gwei,
            spent_today_gwei: 0,
            max_priority_fee_gwei,
            today: Utc::now().date_naive(),
        }
    }

    // Check if the bot can still submit given the daily budget.
    pub fn can_submit(&self, estimated_gas_gwei: u64) -> bool {
        self.spent_today_gwei + estimated_gas_gwei <= self.daily_limit_gwei
    }

    // Record that gas was spent.
    pub fn record_submission(&mut self, gas_gwei: u64) {
        self.maybe_reset();
        self.spent_today_gwei += gas_gwei;

        if self.spent_today_gwei >= self.daily_limit_gwei {
            warn!(
                spent = self.spent_today_gwei,
                limit = self.daily_limit_gwei,
                "daily gas budget exhausted — halting auto-submission"
            );
        }
    }

    // Reset the daily counter at UTC midnight.
    pub fn maybe_reset(&mut self) {
        let today = Utc::now().date_naive();
        if today > self.today {
            self.spent_today_gwei = 0;
            self.today = today;
        }
    }

    // Rank opportunities by profit/gas ratio and filter by remaining budget.
    pub fn rank_and_filter_opportunities(&self, opps: Vec<Opportunity>) -> Vec<Opportunity> {
        let mut ranked = opps;
        ranked.sort_by(|a, b| {
            let ratio_a = if a.estimated_gas > 0 { a.expected_profit / a.estimated_gas as u128 } else { 0 };
            let ratio_b = if b.estimated_gas > 0 { b.expected_profit / b.estimated_gas as u128 } else { 0 };
            ratio_b.cmp(&ratio_a)
        });
        let mut remaining = self.daily_limit_gwei.saturating_sub(self.spent_today_gwei);
        ranked
            .into_iter()
            .filter(|opp| {
                if opp.estimated_gas <= remaining {
                    remaining -= opp.estimated_gas;
                    true
                } else {
                    false
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_budget(limit_gwei: u64, spent_gwei: u64) -> GasBudget {
        GasBudget {
            daily_limit_gwei: limit_gwei,
            spent_today_gwei: spent_gwei,
            max_priority_fee_gwei: 10, // matches new default
            today: Utc::now().date_naive(),
        }
    }

    fn filter() -> NetProfitFilter {
        NetProfitFilter { min_net_profit_usd: 500.0 }
    }

    // ── NetProfitFilter tests ────────────────────────────────────────────────

    #[test]
    fn profitable_when_gross_far_exceeds_gas() {
        // $5000 gross, ~$50 gas, 0 flash fee → net ~$4950 >> $500 min
        let f = filter();
        assert!(f.is_profitable(5000.0, 300_000, 20.0, 1.0, 3000.0, 0.0));
    }

    #[test]
    fn rejected_when_gas_eats_all_profit() {
        // $600 gross, 400K gas @ 100 gwei base, ETH=$3000 → gas=$120, net=$480 < $500
        let f = filter();
        // gas = 400_000 × 101 gwei × 1e9 / 1e18 × 3000 = 400_000 × 101e-9 × 3000
        //     = 400_000 × 3.03e-4 = $121.2
        assert!(!f.is_profitable(600.0, 400_000, 100.0, 1.0, 3000.0, 0.0));
    }

    #[test]
    fn aave_flash_fee_reduces_net() {
        // $1000 gross, negligible gas, $600 Aave flash fee → net $400 < $500
        let f = filter();
        assert!(!f.is_profitable(1000.0, 100_000, 10.0, 1.0, 3000.0, 600.0));
    }

    #[test]
    fn balancer_zero_flash_fee_passes_easily() {
        // $1000 gross, negligible gas, $0 flash fee → net ~$990 > $500
        let f = filter();
        assert!(f.is_profitable(1000.0, 100_000, 10.0, 1.0, 3000.0, 0.0));
    }

    #[test]
    fn min_net_profit_is_configurable() {
        let f = NetProfitFilter { min_net_profit_usd: 100.0 }; // lower bar
        // $300 gross, tiny gas → net ~$296 > $100
        assert!(f.is_profitable(300.0, 50_000, 10.0, 1.0, 3000.0, 0.0));
    }

    #[test]
    fn net_profit_formula_is_correct() {
        let f = filter();
        // gas = 200_000 × (20+1) gwei = 200_000 × 21e-9 ETH = 4.2e-3 ETH @ $3000 = $12.6
        let net = f.net_profit_usd(1000.0, 200_000, 20.0, 1.0, 3000.0, 50.0);
        let expected = 1000.0 - 12.6 - 50.0;
        assert!((net - expected).abs() < 0.1);
    }

    // ── GasBudget tests ──────────────────────────────────────────────────────

    #[test]
    fn can_submit_when_under_limit() {
        let budget = make_budget(1_000_000, 100_000);
        assert!(budget.can_submit(50_000));
    }

    #[test]
    fn cannot_submit_when_budget_exhausted() {
        let budget = make_budget(1_000_000, 950_001);
        assert!(!budget.can_submit(50_000));
    }

    #[test]
    fn ranking_sorts_by_profit_ratio() {
        let budget = make_budget(10_000_000, 0);
        let opps = vec![
            Opportunity { expected_profit: 1000, estimated_gas: 100 },
            Opportunity { expected_profit: 5000, estimated_gas: 100 }, // best
            Opportunity { expected_profit: 2000, estimated_gas: 200 },
        ];
        let ranked = budget.rank_and_filter_opportunities(opps);
        assert_eq!(ranked[0].expected_profit, 5000);
    }

    #[test]
    fn budget_exhaustion_stops_further_submissions() {
        let budget = make_budget(200, 0);
        let opps = vec![
            Opportunity { expected_profit: 1000, estimated_gas: 100 },
            Opportunity { expected_profit:  800, estimated_gas: 100 },
            Opportunity { expected_profit:  600, estimated_gas: 100 },
        ];
        let filtered = budget.rank_and_filter_opportunities(opps);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn midnight_reset_uses_utc() {
        let mut budget = make_budget(1_000_000, 500_000);
        budget.today = (Utc::now() - chrono::Duration::days(1)).date_naive();
        budget.maybe_reset();
        assert_eq!(budget.spent_today_gwei, 0);
    }
}
