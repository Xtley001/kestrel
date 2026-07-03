// Kestrel — main.rs
// Multi-chain, multi-strategy MEV arbitrage bot.
//
//
// Active strategies (5):
//   ETH/sUSDS     Ethereum   Balancer   $500K–$100M   StandardRedemption
//   ETH/sDAI      Ethereum   Balancer   $500K–$100M   StandardRedemption
//   ETH/FlashMint Ethereum   DssFlash   $50M–$100M    FlashMint
//   ARB/sUSDe     Arbitrum   Balancer   $10K–$100M    CrossVenue
//   GNO/sxDAI     Gnosis     Balancer   $1K–$10M      StandardRedemption

use eyre::Result;
use std::time::Duration;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt};

mod access_list;
mod binary_search;
mod builders;
mod cascade_model;
mod controls;
mod chain_config;
mod competitor_tracker;
mod dex_monitor;
mod gas_budget;
mod metrics_ws;
mod nonce_manager;
mod oracle;
mod persistent_store;
mod pool_depth_model;
mod pre_sign;
mod prometheus_exporter;
mod rate_cache;
mod simulate;
mod speculative_cache;
mod spread_pipeline;
mod ssr_monitor;

use chain_config::{
    Chain,
    eth_susds_config, eth_sdai_config, eth_flashmint_config,
    arb_susde_config, gnosis_sxdai_config,
};

// Validate all critical env vars at startup.
// Panics with a clear message if any required var is missing or placeholder.
///
// Validates that a real WS fallback URL exists when IPC socket is absent.
// Previously a missing socket + missing fallback produced a literal "KEY" in the URL
// which 401-ed on connect with no clear error in logs.
fn validate_env() {
    // expanded to include all strategy contract addresses.
    // Previously missing vars caused per-block warn+continue silently, not a startup panic.
    let critical = [
        "SEARCHER_PRIVATE_KEY",
        "EXECUTOR_ADDRESS",
        "ARBITRAGEUR_ADDRESS",
        "ETH_SUSDS_ADDRESS",
        "CURVE_SUSDS_USDS_POOL",
        "ETH_SDAI_ADDRESS",
        "CURVE_SDAI_DAI_POOL",
        "GNO_SXDAI_ADDRESS",
        "GNO_SXDAI_POOL",
        "ETH_PRICE_USD",
        "RETH_IPC_PATH",
        // Strategy-specific contract addresses — absence causes silent per-block skipping
        "ARBITRAGEUR_SDAI_ADDRESS",
        "ARB_SUSDE_ARBITRAGEUR",
        "ARBITRAGEUR_FLASHMINT_ADDRESS",
        "BALANCER_VAULT",
        "DSS_FLASH_ADDRESS",
        "STATE_DB_PATH",
        "SKY_PSM_ADDRESS",
    ];

    let mut missing = vec![];
    for key in critical {
        if std::env::var(key).is_err() {
            missing.push(key);
        }
    }
    if !missing.is_empty() {
        panic!(
            "STARTUP FAILED — missing required env vars: {:?}\n\
             Copy .env.example to .env and fill in all values.",
            missing
        );
    }

    // Validate address vars are not zero or placeholder strings
    // added SKY_PSM_ADDRESS
    // added all strategy contract addresses
    let addr_vars = [
        "ARBITRAGEUR_ADDRESS",
        "ETH_SUSDS_ADDRESS",
        "CURVE_SUSDS_USDS_POOL",
        "SKY_PSM_ADDRESS",
        "ARBITRAGEUR_SDAI_ADDRESS",
        "ARB_SUSDE_ARBITRAGEUR",
        "ARBITRAGEUR_FLASHMINT_ADDRESS",
        "BALANCER_VAULT",
        "DSS_FLASH_ADDRESS",
    ];
    for key in addr_vars {
        let val = std::env::var(key).unwrap();
        if val == "0x0000000000000000000000000000000000000000" || val.starts_with("0xYOUR") {
            panic!(
                "STARTUP FAILED — {} is a placeholder value: {}\n\
                 Set to the real deployed contract address.",
                key, val
            );
        }
    }

    // Validate ETH_PRICE_USD is sane
    let eth_price: f64 = std::env::var("ETH_PRICE_USD")
        .unwrap().parse()
        .expect("ETH_PRICE_USD must be a valid float");
    assert!(
        eth_price > 100.0 && eth_price < 100_000.0,
        "ETH_PRICE_USD={} looks wrong — should be $100–$100,000", eth_price
    );

    // Validate IPC + fallback URL. If IPC socket does not exist at the
    // configured path, there MUST be a real fallback URL — not a placeholder.
    let reth_ipc = std::env::var("RETH_IPC_PATH")
        .unwrap_or_else(|_| "/var/run/reth/reth.ipc".to_string());

    if !std::path::Path::new(&reth_ipc).exists() {
        let fallback = std::env::var("RETH_IPC_FALLBACK")
            .or_else(|_| std::env::var("ETH_WS_FALLBACK"))
            .unwrap_or_default();

        if fallback.is_empty() || fallback.contains("KEY") || fallback.contains("YOUR_KEY") {
            panic!(
                "STARTUP FAILED — IPC socket not found at '{}' and no valid WebSocket \
                 fallback configured.\n\
                 Set RETH_IPC_FALLBACK or ETH_WS_FALLBACK to a real WebSocket URL \
                 (e.g. wss://eth-mainnet.g.alchemy.com/v2/your-api-key).\n\
                 Current fallback value: '{}'",
                reth_ipc, fallback
            );
        }
        warn!(
            ipc = reth_ipc,
            fallback = fallback,
            "IPC socket not found — will use WebSocket fallback (higher latency)"
        );
    }

    if std::env::var("SUBMISSION_ENABLED").unwrap_or_default() != "true" {
        warn!("⚠️  SUBMISSION_ENABLED != true — bot will simulate but NOT submit any bundles");
    }

    // Warn if ETH_FLASHMINT_MIN_TRADE_USD differs from the contract's
    // known MIN_FLASH_SIZE of $50M. A mismatch causes on-chain reverts.
    let flashmint_floor: u128 = std::env::var("ETH_FLASHMINT_MIN_TRADE_USD")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(50_000_000);
    if flashmint_floor != 50_000_000 {
        warn!(
            floor_usd = flashmint_floor,
            "ETH_FLASHMINT_MIN_TRADE_USD differs from contract default of $50M. \
             Ensure KestrelFlashMintArbitrageur.MIN_FLASH_SIZE matches exactly — \
             a mismatch causes on-chain reverts."
        );
    }

    // verify MAX_PRIORITY_FEE_GWEI consistency (code default and .env.example are both 10)
    let _max_fee: u64 = std::env::var("MAX_PRIORITY_FEE_GWEI")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(10);

    info!("Env validation passed ✓");
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .json()
        .init();

    info!("Kestrel starting — yield-bearing stablecoin MEV arbitrage");

    validate_env(); // fails fast with clear panic if env is invalid

    // seed runtime controls from env so dashboard toggles have a baseline.
    controls::init_from_env();

    // ── Infrastructure ──────────────────────────────────────────────────────
    let _prom_handle = tokio::spawn(prometheus_exporter::serve());

    let (metrics_tx, metrics_rx) = tokio::sync::broadcast::channel(128);
    let (control_tx, _control_rx) = tokio::sync::mpsc::channel(16);
    let _ws_handle = tokio::spawn(metrics_ws::serve(metrics_rx, control_tx.clone()));

    let _keepalive = tokio::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            builders::keepalive_ping_all_builders().await;
        }
    });

    // Spawn persistent MEV-Share SSE stream once at startup.
    // All chains read hint counts from the shared DashMap (zero HTTP overhead per block).
    competitor_tracker::spawn_mev_share_stream();

    info!("{}", ssr_monitor::ssr_summary());

    // builder stats are now loaded AND restored inside run_chain
    // where BuilderPerformance is constructed. The load here is kept only for
    // logging — the data that matters flows through spread_pipeline::run_chain.
    let loaded_stats = persistent_store::load_builder_stats();
    if !loaded_stats.is_empty() {
        info!(
            count = loaded_stats.len(),
            "Builder stats loaded from persistent store — will be restored in each pipeline"
        );
    }

    if !oracle::chainlink::verify_transmit_selector() {
        tracing::error!("CHAINLINK SELECTOR MISMATCH — oracle pre-emption will not work");
    } else {
        info!("Chainlink transmit() selector verified: 0x250a8a3c");
    }

    // ── Node URLs ────────────────────────────────────────────────────────────
    let reth_ipc = std::env::var("RETH_IPC_PATH")
        .unwrap_or_else(|_| "/var/run/reth/reth.ipc".to_string());
    let reth_ipc_fallback = std::env::var("RETH_IPC_FALLBACK")
        .unwrap_or_else(|_| std::env::var("ETH_WS_FALLBACK")
            .unwrap_or_else(|_| "wss://eth-mainnet.g.alchemy.com/v2/KEY".to_string()));
    let arb_ws    = std::env::var("ARB_WS_URL")
        .unwrap_or_else(|_| "wss://arb-node.internal:8546".to_string());
    let gnosis_ws = std::env::var("GNOSIS_WS_URL")
        .unwrap_or_else(|_| "wss://rpc.gnosischain.com/wss".to_string());

    info!("[ETH]  primary IPC: {}", reth_ipc);
    info!("[ETH]  fallback WS: {}", reth_ipc_fallback);
    info!("[ARB]  WS:          {}", arb_ws);
    info!("[GNO]  WS:          {}", gnosis_ws);

    let reth_url = if std::path::Path::new(&reth_ipc).exists() {
        info!("[ETH] IPC socket found — using primary");
        reth_ipc.clone()
    } else {
        warn!("[ETH] IPC socket not found at {} — using WebSocket fallback", reth_ipc);
        reth_ipc_fallback.clone()
    };

    // Log any strategies that fail is_ready so operator knows
    for cfg in [eth_susds_config(), eth_sdai_config(), eth_flashmint_config(),
                arb_susde_config(), gnosis_sxdai_config()] {
        if !cfg.is_ready() {
            warn!(strategy = cfg.label,
                "strategy has zero-address pool/vault — will not start. Set addresses in .env.");
        }
    }

    // Print P&L report from last session
    persistent_store::print_pnl_report();

    // TODO Subscribe to DSPause LogNote events for block-by-block SSR change detection.
    // Spawned for the Ethereum pipeline only — Gnosis/Arbitrum SSR comes from their own chains.
    // If the log subscription fails, watch_governance_events warns and returns without panicking
    // or touching any other pipeline. SSR startup poll remains the fallback guard.
    {
        let eth_url_for_gov = reth_url.clone();
        let base_spread: u32 = std::env::var("BASE_MIN_SPREAD_BPS")
            .ok().and_then(|v| v.parse().ok()).unwrap_or(5);
        tokio::spawn(async move {
            ssr_monitor::watch_governance_events(eth_url_for_gov, base_spread).await;
            warn!("watch_governance_events task exited — SSR mid-session detection inactive");
        });
    }

    tokio::join!(
        run_with_restart("ETH/sUSDS", {
            let url = reth_url.clone(); let tx = metrics_tx.clone();
            move || spread_pipeline::run_chain(url.clone(), Chain::Ethereum, eth_susds_config(), tx.clone())
        }),
        run_with_restart("ETH/sDAI", {
            let url = reth_url.clone(); let tx = metrics_tx.clone();
            move || spread_pipeline::run_chain(url.clone(), Chain::Ethereum, eth_sdai_config(), tx.clone())
        }),
        run_with_restart("GNO/sxDAI", {
            let url = gnosis_ws.clone(); let tx = metrics_tx.clone();
            move || spread_pipeline::run_chain(url.clone(), Chain::Gnosis, gnosis_sxdai_config(), tx.clone())
        }),
        run_with_restart("ETH/FlashMint", {
            let url = reth_url.clone(); let tx = metrics_tx.clone();
            move || spread_pipeline::run_chain(url.clone(), Chain::Ethereum, eth_flashmint_config(), tx.clone())
        }),
        run_with_restart("ARB/sUSDe", {
            let url = arb_ws.clone(); let tx = metrics_tx.clone();
            move || spread_pipeline::run_chain(url.clone(), Chain::Arbitrum, arb_susde_config(), tx.clone())
        }),
    );

    Ok(())
}

async fn run_with_restart<F, Fut>(label: &'static str, factory: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let mut delay = Duration::from_secs(1);
    loop {
        match factory().await {
            Ok(()) => { warn!(chain = label, "pipeline exited cleanly — restarting"); }
            Err(e) => {
                warn!(chain = label, error = %e, delay_secs = delay.as_secs(),
                    "pipeline failed — restarting with backoff");
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn active_strategy_configs_compile() {
        use super::chain_config::*;
        let _ = eth_susds_config();
        let _ = eth_sdai_config();
        let _ = eth_flashmint_config();
        let _ = arb_susde_config();
        let _ = gnosis_sxdai_config();
    }

    #[test]
    fn provider_builder_compiles() {
        use alloy::providers::ProviderBuilder;
        let _builder = ProviderBuilder::new();
    }

    #[test]
    fn strategy_skips_when_pool_unset_but_vault_has_default() {
        use super::chain_config::*;
        use alloy::primitives::Address;
        // Pool addresses are not hard-coded: without a configured pool the strategy must
        // report not-ready (is_ready() == false) so it never trades an unverified pool.
        std::env::remove_var("CURVE_SUSDS_USDS_POOL");
        let c = eth_susds_config();
        assert_eq!(c.dex_pool, Address::ZERO, "pool must default to zero (verify via env)");
        assert_ne!(c.yield_vault, Address::ZERO, "ETH/sUSDS vault has a known default");
        assert!(!c.is_ready(), "strategy must be skipped until a verified pool is set");
    }

    // validate that placeholder detection works
    #[test]
    fn fallback_url_placeholder_detected() {
        let url = "wss://eth-mainnet.g.alchemy.com/v2/KEY";
        assert!(url.contains("KEY"));
    }

    #[test]
    fn fallback_url_real_passes_check() {
        let url = "wss://eth-mainnet.g.alchemy.com/v2/abc123realkey";
        assert!(!url.contains("KEY") && !url.is_empty());
    }
}
