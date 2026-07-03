// Kestrel — chain_config.rs
// Chain configuration system — one ChainConfig per (chain, strategy) pair.
//
// Supported strategies:
//   sUSDS/USDS       — Ethereum mainnet    — Balancer flash, 0% fee
//   sDAI/DAI         — Ethereum mainnet    — Balancer flash, 0% fee
//   sUSDe/USDe       — Arbitrum            — Aave flash, 0.05% fee, cross-venue
//   sUSDS/USDS V4    — Base chain          — Morpho flash, 0% fee, Uniswap V4 hook
//   sUSDS (large)    — Ethereum mainnet    — MakerDAO Flash Mint, 0% fee, $500M+ cap
//   sxDAI/xDAI       — Gnosis chain        — Balancer flash, 0% fee, near-zero gas
//
// All strategies reuse the same KestrelArbitrageur.sol pattern via EIP-1167 clones.

use alloy::primitives::{Address, U256};
use crate::binary_search::FlashProvider;
// FlashProvider::BalancerWithAaveFallback is defined in binary_search.rs (feedback)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chain {
    Ethereum,
    Arbitrum,
    Base,
    Gnosis,
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Chain::Ethereum => write!(f, "ETH"),
            Chain::Arbitrum => write!(f, "ARB"),
            Chain::Base     => write!(f, "BASE"),
            Chain::Gnosis   => write!(f, "GNO"),
        }
    }
}

// Per-(chain, strategy) runtime configuration.
#[derive(Debug, Clone)]
pub struct ChainConfig {
    // Human-readable strategy label for logs and metrics.
    pub label: &'static str,
    pub chain: Chain,
    pub flash_provider: FlashProvider,
    pub yield_vault: Address,
    pub dex_pool: Address,
    // Secondary DEX pool for cross-venue strategies (sUSDe).
    pub secondary_dex_pool: Option<Address>,
    // Strategy variant — determines execution path in spread_pipeline.
    pub strategy: StrategyKind,
    // Minimum trade size — sized to make gas cost negligible.
    pub min_trade_size: U256,
    pub builder_endpoints: Vec<String>,

    // Absolute minimum net profit in USD after gas + flash fees.
    // No trade clears submission below this regardless of gas conditions.
    // Env-backed per strategy (e.g. ETH_SUSDS_MIN_PROFIT_USD).
    pub min_net_profit_usd: f64,

    // Dynamic floor multiplier on the live gas cost estimate.
    // Effective floor = max(min_net_profit_usd, gas_cost_usd × gas_profit_multiplier).
    // Ensures the trade covers gas variance and landing-rate risk:
    // a 5× multiplier means the strategy needs 5× its gas spend to be worth taking.
    // On L2s where gas is near-zero the hard floor dominates; on mainnet
    // the multiplier rises with gas price automatically — no operator intervention needed.
    // Env-backed per strategy (e.g. ETH_SUSDS_GAS_MULTIPLIER).
    pub gas_profit_multiplier: f64,
}

impl ChainConfig {
    // Returns true only when all required addresses are non-zero.
    // Strategies with placeholder zero addresses are skipped at startup rather than
    // hammering the RPC with failing calls every block.
    pub fn is_ready(&self) -> bool {
        self.dex_pool != Address::ZERO
            && self.yield_vault != Address::ZERO
            && self.secondary_dex_pool
                .map(|a| a != Address::ZERO)
                .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StrategyKind {
    // Standard: flash → buy yield token on DEX → redeem via vault → repay
    StandardRedemption,
    // Cross-venue: flash → buy on Curve → sell on Uniswap V3 (no cooldown needed)
    CrossVenue,
    // Flash Mint: uses MakerDAO DssFlash for unlimited 0% DAI flash capacity
    // DssFlash is now a peer to Balancer, considered for any ETH Mainnet trade
    FlashMint,
    // NOTE (cleanup L3): UniswapV4Hook and LstRedemption variants removed — the pipeline
    // never implemented them and no active config used them.
}

// ── — sUSDS/USDS on Ethereum mainnet ───────────────────────────────────
pub fn eth_susds_config() -> ChainConfig {
    ChainConfig {
        label: "ETH/sUSDS",
        chain: Chain::Ethereum,
        flash_provider: FlashProvider::Balancer,
        yield_vault: env_addr("ETH_SUSDS_ADDRESS", "0xa3931d71877C0E7A3148CB7Eb4463524FEc27fbD"),
        // Pool address is intentionally NOT hard-coded — set CURVE_SUSDS_USDS_POOL to a
        // verified pool. A zero default makes is_ready() skip the strategy until it is set.
        dex_pool:    env_addr("CURVE_SUSDS_USDS_POOL", "0x0000000000000000000000000000000000000000"),
        secondary_dex_pool: None,
        strategy: StrategyKind::StandardRedemption,
        // min_trade_size from env
        min_trade_size: min_trade_from_env("ETH_SUSDS_MIN_TRADE_USD", 500_000),
        builder_endpoints: mainnet_builder_endpoints(),
        // Mainnet: hard floor $300; 5× gas multiplier auto-scales with gas price.
        // At 30 gwei/450K gas (~$140 gas): effective floor = max($300, $700) = $700.
        // At 150 gwei: effective floor = max($300, $3,500) = $3,500 — auto-protected.
        min_net_profit_usd:    f64_from_env("ETH_SUSDS_MIN_PROFIT_USD",  300.0),
        gas_profit_multiplier: f64_from_env("ETH_SUSDS_GAS_MULTIPLIER",    5.0),
    }
}

// ── — sDAI/DAI on Ethereum mainnet ─────────────────────────────────────
pub fn eth_sdai_config() -> ChainConfig {
    ChainConfig {
        label: "ETH/sDAI",
        chain: Chain::Ethereum,
        flash_provider: FlashProvider::Balancer,
        yield_vault: env_addr("ETH_SDAI_ADDRESS",  "0x83F20F44975D03b1b09e64809B757c47f942BEeA"),
        // Set CURVE_SDAI_DAI_POOL to a verified pool; zero default skips until configured.
        dex_pool:    env_addr("CURVE_SDAI_DAI_POOL","0x0000000000000000000000000000000000000000"),
        secondary_dex_pool: None,
        strategy: StrategyKind::StandardRedemption,
        // min_trade_size from env
        min_trade_size: min_trade_from_env("ETH_SDAI_MIN_TRADE_USD", 500_000),
        builder_endpoints: mainnet_builder_endpoints(),
        // Same profile as ETH/sUSDS — identical gas characteristics.
        min_net_profit_usd:    f64_from_env("ETH_SDAI_MIN_PROFIT_USD",  300.0),
        gas_profit_multiplier: f64_from_env("ETH_SDAI_GAS_MULTIPLIER",    5.0),
    }
}

// ── — sUSDe/USDe cross-venue arb on Arbitrum ───────────────────────────
pub fn arb_susde_config() -> ChainConfig {
    ChainConfig {
        label: "ARB/sUSDe",
        chain: Chain::Arbitrum,
        // aave_fee_bps from env
        flash_provider: FlashProvider::BalancerWithAaveFallback {
            aave_fee_bps: aave_fee_bps_from_env(),
        },
        yield_vault: env_addr("ARB_SUSDE_ADDRESS", "0x211Cc4DD073734dA055fbF44a2b4667d5E5fE5d2"),
        // Set these to verified Arbitrum pools; zero defaults skip until configured.
        dex_pool:    env_addr("ARB_CURVE_SUSDE_POOL", "0x0000000000000000000000000000000000000000"),
        secondary_dex_pool: Some(env_addr("ARB_UNISWAP_SUSDE_POOL", "0x0000000000000000000000000000000000000000")),
        strategy: StrategyKind::CrossVenue,
        // min_trade_size from env
        min_trade_size: min_trade_from_env("ARB_SUSDE_MIN_TRADE_USD", 10_000),
        builder_endpoints: vec![], // ARB is FCFS — submit via sequencer RPC, not builder relay
        min_net_profit_usd:    f64_from_env("ARB_SUSDE_MIN_PROFIT_USD",  50.0),
        gas_profit_multiplier: f64_from_env("ARB_SUSDE_GAS_MULTIPLIER",   3.0),
    }
}

// ── — sUSDS large-scale via MakerDAO Flash Mint ────────────────────────
// NOTE: ETH_FLASHMINT_MIN_TRADE_USD MUST match KestrelFlashMintArbitrageur.MIN_FLASH_SIZE.
// A startup warning is emitted in main.rs if the value differs from $50M.
pub fn eth_flashmint_config() -> ChainConfig {
    ChainConfig {
        label: "ETH/sUSDS-FlashMint",
        chain: Chain::Ethereum,
        flash_provider: FlashProvider::FlashMint,
        yield_vault: env_addr("ETH_SUSDS_ADDRESS", "0xa3931d71877C0E7A3148CB7Eb4463524FEc27fbD"),
        // Pool address is intentionally NOT hard-coded — set CURVE_SUSDS_USDS_POOL to a
        // verified pool. A zero default makes is_ready() skip the strategy until it is set.
        dex_pool:    env_addr("CURVE_SUSDS_USDS_POOL", "0x0000000000000000000000000000000000000000"),
        secondary_dex_pool: None,
        strategy: StrategyKind::FlashMint,
        // min_trade_size from env — WARNING: must match contract MIN_FLASH_SIZE
        min_trade_size: min_trade_from_env("ETH_FLASHMINT_MIN_TRADE_USD", 50_000_000),
        builder_endpoints: mainnet_builder_endpoints(),
        // FlashMint operates at $50M+ sizes. Hard floor of $1,000 reflects the higher
        // coordination cost of a DssFlash borrow and larger revert risk.
        // 5× multiplier handles mainnet gas spikes the same way as standard strategies.
        // Note: ETH_FLASHMINT_MIN_PROFIT_USD is independent of ETH_FLASHMINT_MIN_TRADE_USD.
        min_net_profit_usd:    f64_from_env("ETH_FLASHMINT_MIN_PROFIT_USD", 1000.0),
        gas_profit_multiplier: f64_from_env("ETH_FLASHMINT_GAS_MULTIPLIER",    5.0),
    }
}

// ── — sxDAI/xDAI on Gnosis chain ──────────────────────────────────────
pub fn gnosis_sxdai_config() -> ChainConfig {
    ChainConfig {
        label: "GNO/sxDAI",
        chain: Chain::Gnosis,
        flash_provider: FlashProvider::Balancer,
        yield_vault: env_addr("GNO_SXDAI_ADDRESS", "0xaf204776c7245bF4147c2612BF6e5972Ee483701"),
        // the previous default (0x7f90...F353) is Curve's 3pool on Gnosis
        // (WXDAI/USDC/USDT, 6-dec USDC included) — NOT an sxDAI pool. get_dy against it
        // returns mis-scaled quotes. Defaulted to ZERO so is_ready skips this strategy
        // until GNO_SXDAI_POOL is set to a VERIFIED sDAI/wxDAI pool (check coins/decimals).
        dex_pool:    env_addr("GNO_SXDAI_POOL",    "0x0000000000000000000000000000000000000000"),
        secondary_dex_pool: None,
        strategy: StrategyKind::StandardRedemption,
        // min_trade_size from env
        min_trade_size: min_trade_from_env("GNO_SXDAI_MIN_TRADE_USD", 1_000),
        builder_endpoints: gnosis_builder_endpoints(),
        // Gnosis: gas is negligible (xDAI, sub-cent per tx). Hard floor of $20 dominates.
        // $20 is the minimum return that makes the round-trip worthwhile.
        min_net_profit_usd:    f64_from_env("GNO_SXDAI_MIN_PROFIT_USD",  20.0),
        gas_profit_multiplier: f64_from_env("GNO_SXDAI_GAS_MULTIPLIER",   3.0),
    }
}

// NOTE (cleanup): removed a tail of never-spawned config builders
// (eth_susds_v4, arb_susde_v4, base_susds, gnosis_eure, gnosis_curve_wxdai,
// gnosis_xdai_crossvenue, eth_wsteth_lst, eth_reth_lst) and the eth/arb/base legacy
// aliases. main.rs spawns exactly five strategies; anything else was dead code carrying
// unverified zero-address pools. Re-add behind a feature flag when actually implemented.

// –H15: Read min_trade_size from env, fall back to compile-time default.
// Key is the env var name (e.g. "ETH_SUSDS_MIN_TRADE_USD"), default in USD.
fn min_trade_from_env(key: &str, default_usd: u128) -> U256 {
    let usd = std::env::var(key)
        .ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(default_usd);
    usds(usd)
}

// Read a per-strategy f64 value from env with a default.
// Used for min_net_profit_usd and gas_profit_multiplier.
fn f64_from_env(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(default)
}

// Read Aave flash fee from env. Centralised — same as binary_search.rs.
fn aave_fee_bps_from_env() -> u64 {
    std::env::var("AAVE_FLASH_FEE_BPS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(5)
}

fn mainnet_builder_endpoints() -> Vec<String> {
    use crate::builders::*;
    vec![
        std::env::var("BUILDER_FLASHBOTS").unwrap_or_else(|_| DEFAULT_BUILDER_FLASHBOTS.to_string()),
        std::env::var("BUILDER_TITAN").unwrap_or_else(|_| DEFAULT_BUILDER_TITAN.to_string()),
        std::env::var("BUILDER_BEAVERBUILD").unwrap_or_else(|_| DEFAULT_BUILDER_BEAVERBUILD.to_string()),
        std::env::var("BUILDER_RSYNC").unwrap_or_else(|_| DEFAULT_BUILDER_RSYNC.to_string()),
        std::env::var("BUILDER_BUILDER0X69").unwrap_or_else(|_| DEFAULT_BUILDER_BUILDER0X69.to_string()),
        std::env::var("BUILDER_BLOXROUTE").unwrap_or_else(|_| DEFAULT_BUILDER_BLOXROUTE.to_string()),
    ]
}

fn gnosis_builder_endpoints() -> Vec<String> {
    // Gnosis uses CoW Protocol / direct sequencer
    vec![std::env::var("GNO_BUILDER").unwrap_or_else(|_| "https://rpc.gnosischain.com".to_string())]
}

fn env_addr(key: &str, default: &str) -> Address {
    std::env::var(key).ok().and_then(|s| s.parse().ok())
        .unwrap_or_else(|| default.parse().unwrap_or(Address::ZERO))
}

fn usds(dollars: u128) -> U256 {
    U256::from(dollars) * U256::from(10u128.pow(18))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_active_strategies_configured() {
        // The five strategies main.rs actually spawns.
        let _ = eth_susds_config();
        let _ = eth_sdai_config();
        let _ = arb_susde_config();
        let _ = eth_flashmint_config();
        let _ = gnosis_sxdai_config();
    }

    #[test]
    fn eth_susds_uses_balancer() {
        assert!(matches!(eth_susds_config().flash_provider, FlashProvider::Balancer));
    }

    #[test]
    fn eth_sdai_uses_balancer() {
        assert!(matches!(eth_sdai_config().flash_provider, FlashProvider::Balancer));
    }

    // arb_susde uses BalancerWithAaveFallback, not plain Aave.
    #[test]
    fn arb_susde_uses_balancer_with_aave_fallback() {
        assert!(matches!(
            arb_susde_config().flash_provider,
            FlashProvider::BalancerWithAaveFallback { aave_fee_bps: 5 }
        ));
    }

    #[test]
    fn arb_susde_has_secondary_dex_pool() {
        assert!(arb_susde_config().secondary_dex_pool.is_some());
    }

    #[test]
    fn flashmint_strategy_is_flash_mint_kind() {
        assert!(matches!(eth_flashmint_config().strategy, StrategyKind::FlashMint));
    }

    // FIX : floor now usds(50_000_000) matching contract.
    #[test]
    fn flashmint_floor_is_50m() {
        assert_eq!(eth_flashmint_config().min_trade_size, usds(50_000_000));
    }

    #[test]
    fn gnosis_floor_is_1k() {
        assert_eq!(gnosis_sxdai_config().min_trade_size, usds(1_000));
    }

    #[test]
    fn susds_floor_is_500k() {
        assert_eq!(eth_susds_config().min_trade_size, usds(500_000));
    }

    #[test]
    fn arb_strategy_is_cross_venue() {
        assert!(matches!(arb_susde_config().strategy, StrategyKind::CrossVenue));
    }

    #[test]
    fn gnosis_chain_label_contains_gno() {
        assert!(gnosis_sxdai_config().label.contains("GNO"));
    }

    // ── New tests (spec) ──────────────────────────────────────────────────

    #[test]
    fn min_trade_size_reads_from_env() {
        std::env::set_var("ETH_SUSDS_MIN_TRADE_USD", "250000");
        let config = eth_susds_config();
        assert_eq!(config.min_trade_size, usds(250_000));
        std::env::remove_var("ETH_SUSDS_MIN_TRADE_USD");
    }

    #[test]
    fn flashmint_min_trade_env_overrides_default() {
        std::env::set_var("ETH_FLASHMINT_MIN_TRADE_USD", "100000000");
        let config = eth_flashmint_config();
        assert_eq!(config.min_trade_size, usds(100_000_000));
        std::env::remove_var("ETH_FLASHMINT_MIN_TRADE_USD");
    }

    #[test]
    fn aave_fee_in_arb_config_reads_from_env() {
        std::env::set_var("AAVE_FLASH_FEE_BPS", "9");
        let config = arb_susde_config();
        assert_eq!(config.flash_provider.fallback_fee_bps(), 9);
        std::env::remove_var("AAVE_FLASH_FEE_BPS");
    }
}
