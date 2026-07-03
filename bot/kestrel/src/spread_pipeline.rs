// Kestrel — spread_pipeline.rs
// Per-block spread detection and bundle assembly pipeline.
//

use alloy::consensus::{TxEip1559, SignableTransaction, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::Signer;
use alloy::sol_types::SolCall;
use alloy::transports::BoxTransport;
use dashmap::DashMap;
use eyre::Result;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::binary_search::{
    compute_net_profit, compute_net_profit_premium, direction_to_indices,
    optimal_susds_trade_size, select_flash_provider, usds,
};
use crate::builders::{BundleRequest, submit_multi_block, submit_to_arbitrum_sequencer};
use crate::cascade_model::CascadeModel;
use crate::chain_config::{Chain, ChainConfig, StrategyKind};
use crate::competitor_tracker::{
    BuilderPerformance, SpreadModelCalibrator,
    compute_adaptive_priority_fee, count_competitor_hints,
    fetch_competitor_bundles, reset_mev_share_hints,
};
use crate::dex_monitor::{curve_pool_call_get_dy, get_dex_prices, PoolState, SpreadDirection};
use crate::gas_budget::GasBudget;
use crate::metrics_ws::BlockMetricsMessage;
use crate::nonce_manager::{NonceManager, replacement_priority_fee_gwei};
use crate::persistent_store::{LedgerEntry, ledger_record_submission, load_builder_stats};
use crate::pool_depth_model::PoolDepthRegistry;
use crate::pre_sign::WatchedPool;
use crate::rate_cache::ProtocolRateCache;
use crate::simulate::simulate_arb;
use crate::speculative_cache::SpeculativeCache;
use crate::ssr_monitor::effective_min_spread_bps;

const SPECULATIVE_THRESHOLD_BPS: u64 = 3;
// Halt submission after this many consecutive simulation reverts.
const REVERT_CIRCUIT_BREAKER: u32 = 5;

// Pending cascade event: (block_when_detected, spread_bps, size_usd_at_detection).
// Drained 2 blocks later and fed to the cascade model.
// size_usd_at_detection is our trade size in USD — a directionally-correct proxy for
// follow-on pool volume until receipt-based actual volume is wired (see TODO ).
type PendingCascadeEvent = (u64, u32, u64);

async fn build_provider(
    node_url: &str,
    chain: Chain,
) -> Result<alloy::providers::RootProvider<BoxTransport>> {
    // on_builtin auto-detects ws:// / ws:// / IPC-path and returns a boxed provider
    // (satisfies the default `Provider` bound and supports pubsub over ws/ipc).
    ProviderBuilder::new()
        .on_builtin(node_url)
        .await
        .map_err(|e| eyre::eyre!("[{}] connection failed: {}", chain, e))
}

pub async fn run_chain(
    node_url: String,
    chain: Chain,
    config: ChainConfig,
    metrics_tx: broadcast::Sender<BlockMetricsMessage>,
) -> Result<()> {
    info!(chain = %chain, strategy = config.label, "connecting: {}", node_url);
    let provider = Arc::new(build_provider(&node_url, chain).await?);
    info!(chain = %chain, strategy = config.label, "connected");

    // Pre-warm access list cache at startup
    pre_warm_access_lists(config.dex_pool, Arc::clone(&provider)).await;

    // Poll SSR at startup for Ethereum strategies.
    if chain == Chain::Ethereum {
        let susds_vault: Address = std::env::var("ETH_SUSDS_ADDRESS")
            .ok().and_then(|s| s.parse().ok()).unwrap_or(Address::ZERO);
        crate::ssr_monitor::poll_ssr_at_startup(susds_vault, &*provider).await;
    }

    // Per-pipeline state
    let _pool_state: Arc<DashMap<Address, PoolState>> = Arc::new(DashMap::new());
    let mut rate_cache   = ProtocolRateCache::new();
    let mut gas_budget   = GasBudget::from_env();
    let spec_cache       = SpeculativeCache::new();
    let depth_reg        = Arc::new(PoolDepthRegistry::new());
    let mut cascade      = CascadeModel::new();
    let mut calibrator   = SpreadModelCalibrator::new();

    // Load and RESTORE builder stats immediately after constructing BuilderPerformance.
    // Previously: loaded in main.rs, logged, then discarded — restore was never called.
    // Now: loaded here where BuilderPerformance is constructed, so historical rates take effect.
    let builder_perf = BuilderPerformance::new();
    {
        let loaded = load_builder_stats();
        if !loaded.is_empty() {
            info!(count = loaded.len(), chain = %chain, "restoring builder performance stats");
            builder_perf.restore(loaded);
        }
    }

    // Consecutive revert circuit breaker state
    let mut consecutive_sim_reverts: u32 = 0;
    let mut circuit_open = false;

    // VecDeque for deferred cascade volume recording.
    // When a spread event fires, push (detection_block, spread_bps, size_usd).
    // 2 blocks later: record stored size_usd in cascade model as follow-on proxy.
    // TODO query pool event logs for real follow-on volume after wiring receipts.
    let mut pending_cascade: VecDeque<PendingCascadeEvent> = VecDeque::new();

    // TODO Per-pipeline session metrics for the WS dashboard broadcast.
    // One AtomicU64 per stat — no mutex needed, each pipeline owns its own counters.
    // session_profit_usds_micros: profit in 1e6 USDS units (avoids u64 overflow for
    //   realistic profit values; convert to display string by dividing by 1_000_000).
    let session_profit_usds_micros: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let bundles_submitted_count:    Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

    // FIX : ETH price validated at startup; updated atomically each block.
    let initial_eth_price_cents: u64 = std::env::var("ETH_PRICE_USD")
        .expect("ETH_PRICE_USD must be set (checked by validate_env)")
        .parse::<f64>().expect("ETH_PRICE_USD must be a valid number")
        .mul_add(100.0, 0.0) as u64;
    let eth_price_cents = Arc::new(AtomicU64::new(initial_eth_price_cents));

    if chain == Chain::Ethereum {
        let price_ref  = Arc::clone(&eth_price_cents);
        let oracle_url = node_url.clone();
        tokio::spawn(async move {
            crate::oracle::chainlink::watch_eth_price_updates(oracle_url, price_ref).await;
        });
    }

    // .unwrap_or(Address::ZERO) silently used the zero address when
    // EXECUTOR_ADDRESS was missing or malformed (e.g. missing 0x prefix). The nonce
    // manager would then fetch nonce from address(0) — always 0 on mainnet — causing
    // all transactions after the first to be rejected as nonce replays.
    // Now: panic loudly at startup if the address is missing, malformed, or zero.
    let executor_addr: Address = std::env::var("EXECUTOR_ADDRESS")
        .expect("EXECUTOR_ADDRESS must be set (checked by validate_env)")
        .parse()
        .expect("EXECUTOR_ADDRESS must be a valid 0x-prefixed address");
    assert_ne!(
        executor_addr, Address::ZERO,
        "EXECUTOR_ADDRESS must not be the zero address — check your .env configuration"
    );

    let initial_nonce: u64 = provider
        .get_transaction_count(executor_addr).await
        .unwrap_or(0);
    let nonce_mgr = NonceManager::new(initial_nonce);

    // Load searcher key ONCE at startup — not inside the block loop.
    // load_searcher_key was called on every submission: std::env::var is a syscall,
    // SecretString::parse re-derives the key each call. Negligible at 1-2 calls/min
    // but still an unnecessary pattern. More importantly, this moves the key out of
    // a hot path and makes the "key not found" failure explicit at startup.
    let searcher_key = load_searcher_key()
        .expect("SEARCHER_PRIVATE_KEY must be set and parseable — checked by validate_env()");

    let chain_id: u64 = match chain {
        Chain::Ethereum => 1,
        Chain::Arbitrum => 42161,
        Chain::Gnosis   => 100,
        _               => 1,
    };

    // load MAX_PRIORITY_FEE_GWEI once at startup — not re-read
    // from env every block (unnecessary syscall, and 10 different values possible if
    // env changes). Seed last_priority_fee_gwei to this value so the very first block
    // after (re)start doesn't bid 1.0 gwei regardless of conditions.
    let max_priority_fee_gwei: f64 = std::env::var("MAX_PRIORITY_FEE_GWEI")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
    // Initialise to half the cap rather than a hardcoded 1.0 — more realistic on restart.
    let mut last_priority_fee_gwei: f64 = max_priority_fee_gwei * 0.5;

    // alloy 0.3 — subscribe_blocks is on the Provider trait (pubsub feature),
    // yields a Subscription; use .into_stream to iterate. There is no PubSubExt.
    use futures_util::StreamExt;

    let mut block_stream = provider.subscribe_blocks().await
        .map_err(|e| eyre::eyre!("[{}] block subscription failed: {}", chain, e))?
        .into_stream();

    let mut watched_pool = WatchedPool::new(config.dex_pool);

    while let Some(block) = block_stream.next().await {
        // alloy 0.3 Block — header fields live under `.header` (number is u64).
        let block_num = block.header.number;

        // treat a missing base_fee as a block-processing error —
        // not a default. Post-EIP-1559 blocks always have base_fee_per_gas. A missing
        // value means a malformed block or RPC quirk; defaulting to 20.0 would produce
        // a $0 gas estimate and allow submissions that are mis-priced.
        let base_fee_gwei: f64 = match block.header.base_fee_per_gas {
            Some(f) => f as f64 / 1e9,
            None => {
                warn!(
                    chain = %chain,
                    block = block_num,
                    "block header missing base_fee_per_gas — skipping this block \
                     (post-EIP-1559 blocks must have a base fee)"
                );
                continue;
            }
        };

        // competitor_tracker: Reset MEV-Share hint counts for this block.
        // The persistent SSE stream has been accumulating hints since the last reset.
        reset_mev_share_hints(block_num);

        // Check if circuit breaker is open — skip submission if so.
        if circuit_open {
            warn!(
                chain = %chain, consecutive_reverts = consecutive_sim_reverts,
                "CIRCUIT BREAKER OPEN — submission halted. Set RESET_CIRCUIT_BREAKER=1 \
                 and restart, or wait for manual investigation."
            );
            // Allow reset via env var check each block
            if std::env::var("RESET_CIRCUIT_BREAKER").map(|v| v == "1").unwrap_or(false) {
                warn!(chain = %chain, "Circuit breaker manually reset via RESET_CIRCUIT_BREAKER=1");
                consecutive_sim_reverts = 0;
                circuit_open = false;
            } else {
                // Still process the block for monitoring, but skip submission
            }
        }

        // Re-sync nonce every 5 blocks.
        if block_num % 5 == 0 {
            if let Ok(on_chain_u64) = provider.get_transaction_count(executor_addr).await {
                if on_chain_u64 >= nonce_mgr.current() {
                    nonce_mgr.confirm();
                }
            }
        }

        // Stuck tx replacement.
        if let Some(stuck_nonce) = nonce_mgr.maybe_stuck(block_num) {
            let replacement_fee = replacement_priority_fee_gwei(
                last_priority_fee_gwei,
                gas_budget.max_priority_fee_gwei as f64,
            );
            // A zero-value self-send at a higher tip replaces the stuck nonce.
            let repl = TxEip1559 {
                chain_id,
                nonce: stuck_nonce,
                max_priority_fee_per_gas: (replacement_fee * 1e9) as u128,
                max_fee_per_gas: ((base_fee_gwei * 2.0 + replacement_fee) * 1e9) as u128,
                gas_limit: 21_000,
                to: TxKind::Call(executor_addr),
                value: U256::ZERO,
                access_list: Default::default(),
                input: Bytes::new(),
            };
            if let Ok(sig) = searcher_key.sign_hash(&repl.signature_hash()).await {
                let signed = repl.into_signed(sig);
                let envelope: TxEnvelope = signed.into();
                let _ = provider.send_raw_transaction(&envelope.encoded_2718()).await;
                warn!(chain = %chain, nonce = stuck_nonce, fee_gwei = replacement_fee,
                    "replacement tx submitted for stuck nonce");
                nonce_mgr.confirm();
            }
        }

        // TODO Drain pending cascade events from 2+ blocks ago and record follow-on.
        // size_usd_at_detection is our trade size — directionally correct proxy for follow-on
        // pool volume. Better than zero; TODO will replace with receipt-based actual volume.
        while pending_cascade.front().map(|(b, _, _)| block_num >= b + 2).unwrap_or(false) {
            if let Some((detected_block, detected_spread_bps, size_usd_at_detection)) = pending_cascade.pop_front() {
                cascade.record_event(detected_spread_bps, size_usd_at_detection);
                debug!(
                    chain = %chain,
                    detected_block,
                    detected_spread_bps,
                    size_usd_proxy = size_usd_at_detection,
                    "cascade deferred event recorded (volume=trade_size proxy; M7 will wire receipt-based actual)"
                );
            }
        }

        let eth_price_usd: f64 = eth_price_cents.load(Ordering::Relaxed) as f64 / 100.0;
        // max_priority_fee_gwei is loaded once at startup (B)

        info!(
            chain    = %chain,
            strategy = config.label,
            block    = block_num,
            base_fee_gwei,
            eth_price_usd,
            "new block"
        );

        // Step 1: Protocol rate
        let protocol_rate = match rate_cache
            .get_or_refresh(block_num, config.yield_vault, &*provider).await
        {
            Ok(r)  => r,
            Err(e) => { warn!(chain = %chain, error = %e, "rate cache failed"); continue; }
        };

        // Step 2: DEX spot prices — both directions in parallel
        // Previously: only get_dy(1,0) — always Discount direction price.
        // Now: query both directions, pick the more actionable spread.
        let (discount_price, premium_price) = match get_dex_prices(
            config.dex_pool, &*provider,
        ).await {
            Ok(prices) => prices,
            Err(_)     => continue,
        };

        // both prices are USDS-per-sUSDS (see get_dex_prices). A discount arb
        // only exists when the DEX buy price is BELOW the redemption rate; a premium arb
        // only when the DEX sell price is ABOVE it. Guarding by sign prevents the old
        // false-spread-every-block behaviour where mismatched units always tripped.
        let discount_spread = if discount_price < protocol_rate {
            crate::dex_monitor::spread_bps(protocol_rate, discount_price)
        } else {
            0
        };
        // premium is only submitted when the on-chain premium leg has been
        // deployed and forge-verified by the operator. Default OFF so the bot is
        // bug-free out of the box (discount-only, matching the audited path). Set
        // ENABLE_PREMIUM_DIRECTION=true after deploying the premium-capable contract.
        let premium_enabled = std::env::var("ENABLE_PREMIUM_DIRECTION")
            .map(|v| v == "true").unwrap_or(false);
        let premium_spread = if premium_enabled && premium_price > protocol_rate {
            crate::dex_monitor::spread_bps(protocol_rate, premium_price)
        } else {
            0
        };

        let (direction, spread_bps) = if discount_spread >= premium_spread {
            (SpreadDirection::Discount, discount_spread)
        } else {
            (SpreadDirection::Premium, premium_spread)
        };

        // Both directions zero → no actionable arb this block.
        if discount_spread == 0 && premium_spread == 0 {
            continue;
        }

        // Update PoolDepthRegistry from live pool probes each block.
        {
            let depth_ref = Arc::clone(&depth_reg);
            let pool_addr = config.dex_pool;
            let prov      = Arc::clone(&provider);
            let block     = block_num;
            tokio::spawn(async move {
                if let (Ok(r0), Ok(r1)) = tokio::join!(
                    curve_pool_call_get_dy(pool_addr, 1, 0, usds(10_000_000), &*prov),
                    curve_pool_call_get_dy(pool_addr, 0, 1, usds(10_000_000), &*prov),
                ) {
                    depth_ref.update(pool_addr, r0, r1, 500, block);
                }
            });
        }

        let min_spread_bps = effective_min_spread_bps() as u64;

        // Speculative pre-computation at 3bps
        // direction now passed — previously always Discount (missing param).
        if spread_bps >= SPECULATIVE_THRESHOLD_BPS && spread_bps < min_spread_bps {
            let spec  = spec_cache.clone_arc();
            let pool  = config.dex_pool;
            let rate  = protocol_rate;
            let dir   = direction;
            let prov  = Arc::clone(&provider);
            let floor = config.min_trade_size;
            tokio::spawn(async move {
                if let Ok(Some((size, _))) = optimal_susds_trade_size(
                    pool, rate, 0, U256::ZERO, U256::ZERO, floor,
                    dir, // direction passed
                    &*prov,
                ).await {
                    spec.insert(pool, &dir, block_num, size);
                    debug!(pool = %pool, size = %size, direction = ?dir, "speculative binary search cached");
                }
            });
        }

        if spread_bps < min_spread_bps { continue; }

        // Pool depth pre-screen
        // gas cost stub for depth prescreen read from env (was hardcoded $500).
        let gas_cost_stub_usd = std::env::var("DEPTH_PRESCREEN_GAS_STUB_USD")
            .ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(500);
        let gas_cost_usds_stub = usds(gas_cost_stub_usd);
        if let Some(depth) = depth_reg.get(config.dex_pool) {
            if !depth.quick_viable_check(spread_bps as u32, gas_cost_usds_stub) {
                debug!(chain = %chain, "pool depth pre-screen: not viable");
                continue;
            }
        }

        let balancer_cap = usds(std::env::var("BALANCER_CAPACITY_USD")
            .ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(300_000_000));
        let morpho_cap = usds(std::env::var("MORPHO_CAPACITY_USD")
            .ok().and_then(|v| v.parse::<u128>().ok()).unwrap_or(50_000_000));

        // compute min_net_profit floor from per-strategy config for the search.
        // Previously used the global NetProfitFilter (MIN_NET_PROFIT_USD=$500 for all chains).
        // Now uses config.min_net_profit_usd — each strategy's own hard floor — so the binary
        // search doesn't waste RPC calls finding sizes the gate will immediately reject.
        let min_net_profit_usds = U256::from(
            (config.min_net_profit_usd * 1e18) as u128
        );

        // Check speculative cache first
        // cache re-evaluation now uses direction-aware indices and profit fn.
        // Previously: always called get_dy(1,0,...) and compute_net_profit regardless of direction.
        let optimal_result: Option<(U256, U256)> = match spec_cache
            .get(config.dex_pool, &direction, block_num)
        {
            Some(cached_size) => {
                info!(chain = %chain, size = %cached_size, direction = ?direction, "using speculative cache");
                // use direction-aware indices
                let (i, j) = direction_to_indices(direction);
                match curve_pool_call_get_dy(
                    config.dex_pool, i, j, cached_size, &*provider,
                ).await {
                    Ok(out) => {
                        // direction-aware profit function
                        let net = match direction {
                            SpreadDirection::Discount => compute_net_profit(
                                cached_size, out, protocol_rate, 0, U256::ZERO,
                            ),
                            SpreadDirection::Premium => compute_net_profit_premium(
                                cached_size, out, 0, U256::ZERO,
                            ),
                        };
                        Some((cached_size, net))
                    }
                    Err(_) => None,
                }
            }
            None => {
                // .1: Two-pass binary search.
                // Pass 1: solve with fee=0 to get initial size estimate.
                // If that size needs Aave (fee>0), re-solve with correct fee.
                let initial = optimal_susds_trade_size(
                    config.dex_pool, protocol_rate, 0, U256::ZERO, min_net_profit_usds,
                    config.min_trade_size, direction, &*provider,
                ).await.unwrap_or(None);

                if let Some((initial_size, _)) = initial {
                    let flash_provider_init = select_flash_provider(initial_size, balancer_cap, morpho_cap);
                    let fee_bps_init = flash_provider_init.fee_bps();

                    if fee_bps_init > 0 {
                        // Pass 2: re-solve with the real fee (Aave at 5bps).
                        // The initial search assumed 0% — re-run with correct cost.
                        optimal_susds_trade_size(
                            config.dex_pool, protocol_rate, fee_bps_init, U256::ZERO,
                            min_net_profit_usds, config.min_trade_size, direction, &*provider,
                        ).await.unwrap_or(None)
                    } else {
                        // Balancer or Morpho: 0% fee — pass 1 result is already correct.
                        Some((initial_size, initial.unwrap().1))
                    }
                } else {
                    None
                }
            }
        };

        let Some((size, net_profit_usds)) = optimal_result else { continue };
        let flash_provider = select_flash_provider(size, balancer_cap, morpho_cap);
        let flash_fee_bps  = flash_provider.fee_bps();

        let known_pools = vec![format!("{:?}", config.dex_pool)];
        // count_competitor_hints now reads from persistent SSE DashMap (non-async).
        let hint_count  = count_competitor_hints(&known_pools);

        // Dispatch calldata encoding by StrategyKind.
        let (calldata, arbitrageur) = match config.strategy {
            StrategyKind::StandardRedemption => {
                let cd = encode_standard_calldata(
                    size, direction, config.dex_pool, &*provider, U256::ZERO,
                ).await;
                (cd, arbitrageur_address())
            }
            StrategyKind::FlashMint => {
                let cd = encode_flashmint_calldata(
                    size, direction, config.dex_pool, &*provider, U256::ZERO,
                ).await;
                let addr = std::env::var("ARBITRAGEUR_FLASHMINT_ADDRESS")
                    .ok().and_then(|s| s.parse().ok()).unwrap_or(Address::ZERO);
                (cd, addr)
            }
            StrategyKind::CrossVenue => {
                let secondary = match config.secondary_dex_pool {
                    Some(p) => p,
                    None => {
                        warn!(chain = %chain, "CrossVenue missing secondary_dex_pool — skipping");
                        continue;
                    }
                };
                let cd = encode_crossvenue_calldata(size, config.dex_pool, secondary, &*provider).await;
                let addr = match chain {
                    Chain::Arbitrum => std::env::var("ARB_SUSDE_ARBITRAGEUR")
                        .ok().and_then(|s| s.parse().ok()).unwrap_or(Address::ZERO),
                    _ => arbitrageur_address(),
                };
                (cd, addr)
            }
        };

        if arbitrageur == Address::ZERO {
            warn!(chain = %chain, strategy = config.label,
                "arbitrageur address is zero — check env vars");
            continue;
        }

        let sim = simulate_arb(Arc::clone(&provider), block_num, arbitrageur, calldata.clone()).await;

        match sim {
            Ok(result) if result.success => {
                // Simulation succeeded — reset consecutive revert counter.
                if consecutive_sim_reverts > 0 {
                    info!(chain = %chain, "simulation succeeded after {} reverts — resetting circuit breaker counter", consecutive_sim_reverts);
                }
                consecutive_sim_reverts = 0;

                // Real net profit from binary search.
                let net_profit_usd: f64 = {
                    let n: u128 = net_profit_usds.try_into().unwrap_or(u128::MAX);
                    n as f64 / 1e18
                };
                let size_usd: f64 = {
                    let s: u128 = size.try_into().unwrap_or(u128::MAX);
                    s as f64 / 1e18
                };
                let gross_profit_usd = size_usd * (spread_bps as f64) / 10_000.0;
                let flash_fee_usd    = size_usd * (flash_fee_bps as f64) / 10_000.0;

                // Compute gas cost estimate FIRST, then bid on net-of-gas profit.
                // Previously: bid on gross_profit_usd (before gas/flash fee deduction).
                // At thin spreads, gas is a significant fraction of gross — overbidding.
                let gas_cost_estimate_usd = result.gas_used as f64
                    * base_fee_gwei * 1e-9 * eth_price_usd;

                // true_net_profit_usd properly deducts gas from the binary
                // search result. net_profit_usd excluded gas (gas_cost = U256::ZERO in both
                // search passes — gas wasn't known until after simulation). The previous gate
                // checked net_profit_usd directly, which was pre-gas and too optimistic.
                //
                // Note: flash fee is already deducted inside net_profit_usd by the binary
                // search (compute_net_profit / compute_net_profit_premium both subtract it).
                // Do NOT deduct flash_fee_usd again here — that is Bug B.
                let true_net_profit_usd = net_profit_usd - gas_cost_estimate_usd;

                // Dynamic profit floor: scales automatically with live gas conditions.
                //   effective_floor = max(hard_floor, gas_cost × multiplier)
                // At normal mainnet gas the multiplier term dominates (protects margins).
                // During cheap gas the hard floor takes over (minimum meaningful trade).
                // On L2s gas_cost is near-zero so the hard floor always wins — correct.
                let dynamic_floor = (gas_cost_estimate_usd * config.gas_profit_multiplier)
                    .max(config.min_net_profit_usd);

                // net_after_gas_usd used only for priority fee calculation.
                // Previously subtracted flash_fee_usd again here, double-counting it for
                // Aave trades. Removed — true_net_profit_usd already has flash fee baked in.
                let net_after_gas_usd = true_net_profit_usd.max(0.0);

                let priority_fee_gwei = compute_adaptive_priority_fee(
                    net_after_gas_usd,      // net profit (not gross)
                    result.gas_used,
                    eth_price_usd,
                    max_priority_fee_gwei,
                    hint_count,
                );
                last_priority_fee_gwei = priority_fee_gwei;

                if true_net_profit_usd < dynamic_floor {
                    warn!(
                        chain          = %chain,
                        strategy       = config.label,
                        true_net_usd   = format!("{:.2}", true_net_profit_usd),
                        dynamic_floor  = format!("{:.2}", dynamic_floor),
                        gas_cost_usd   = format!("{:.2}", gas_cost_estimate_usd),
                        multiplier     = config.gas_profit_multiplier,
                        hard_floor_usd = config.min_net_profit_usd,
                        "net profit below dynamic floor — skipping"
                    );
                    continue;
                }

                info!(
                    chain         = %chain,
                    strategy      = config.label,
                    block         = block_num,
                    pool          = %config.dex_pool,
                    direction     = ?direction,
                    spread_bps,
                    size          = %size,
                    true_net_usd  = format!("{:.2}", true_net_profit_usd),
                    dynamic_floor = format!("{:.2}", dynamic_floor),
                    gas_cost_usd  = format!("{:.2}", gas_cost_estimate_usd),
                    tip_gwei      = priority_fee_gwei,
                    flash         = ?flash_provider,
                    flash_fee_usd,
                    hints         = hint_count,
                    "BUNDLE READY"
                );

                calibrator.record(gross_profit_usd, net_profit_usd, block_num);

                // read the live runtime control (env-seeded, dashboard-toggleable)
                // instead of re-reading the env var each block.
                let submission_enabled = crate::controls::submission_enabled();

                let effective_gas_price_gwei = base_fee_gwei + priority_fee_gwei;
                let estimated_gas_gwei = (result.gas_used as f64 * effective_gas_price_gwei) as u64;

                if submission_enabled && !circuit_open && gas_budget.can_submit(estimated_gas_gwei) {
                    // ── Build and sign the actual transaction ──────────────────
                    // Previously: signed_txs: vec![] — bundle was ALWAYS EMPTY.
                    // Builders returned 200 OK but included nothing. Fixed below.

                    // Fetch pre-warmed access list and attach to the transaction.
                    // get_cached_or_generate returns cached items from startup pre-warm.
                    // Saves ~20,000 gas (EIP-2930 warm storage slot reads).
                    let access_list_items = crate::access_list::get_cached_or_generate(
                        config.dex_pool, direction.clone(), &*provider, block_num,
                    ).await.unwrap_or_default();
                    let access_list = crate::access_list::items_to_access_list(access_list_items);

                    // Use projected base fee through N+max_blocks so the tx remains
                    // includable even if base fees rise 12.5%/block until the last target.
                    let max_blocks: u32 = std::env::var("MULTI_BLOCK_TARGET_COUNT")
                        .ok().and_then(|v| v.parse().ok()).unwrap_or(3);
                    let projected_base = base_fee_gwei * 1.125_f64.powi(max_blocks as i32 - 1);
                    let max_fee_per_gas = ((projected_base + priority_fee_gwei) * 1e9 * 1.10) as u128;

                    // Peek nonce without incrementing — only increment after successful sign.
                    let nonce_to_use = nonce_mgr.current();

                    let tx = TxEip1559 {
                        chain_id:                chain_id,
                        nonce:                   nonce_to_use,
                        max_priority_fee_per_gas: (priority_fee_gwei * 1e9) as u128,
                        max_fee_per_gas,
                        gas_limit:               (result.gas_used * 12 / 10) as u128, // 20% buffer
                        to:                      TxKind::Call(arbitrageur),
                        value:                   U256::ZERO,
                        access_list,
                        input:                   calldata.0.clone().into(),
                    };

                    let sign_result = searcher_key.sign_hash(&tx.signature_hash()).await;

                    match sign_result {
                        Ok(sig) => {
                            let signed = tx.into_signed(sig);
                            let envelope: TxEnvelope = signed.into();
                            let signed_bytes = Bytes::from(envelope.encoded_2718());

                            let bundle = BundleRequest {
                                // clone: signed_bytes is reused by the Arbitrum branch below
                                signed_txs:       vec![signed_bytes.clone()],
                                target_block:     block_num + 1,
                                priority_fee_gwei,
                                base_fee_gwei,                           // trajectory field
                            };

                            let min_landing_rate: f64 = std::env::var("MIN_BUILDER_LANDING_RATE")
                                .ok().and_then(|v| v.parse().ok()).unwrap_or(0.40);
                            let all_builders: Vec<String> = crate::builders::builder_urls()
                                .into_iter().map(|(_, url)| url).collect();
                            let ranked = builder_perf.ranked_builders_filtered(
                                &all_builders, min_landing_rate,
                            );

                            // Arbitrum is FCFS — submit directly to the
                            // sequencer via eth_sendRawTransaction. Flashbots relay only
                            // handles Ethereum L1; sending bundles there for ARB silently
                            // drops them. submit_to_arbitrum_sequencer (builders.rs:L217)
                            // existed but was never called — now it is.
                            if chain == Chain::Arbitrum {
                                let arb_rpc = std::env::var("ARB_WS_URL")
                                    .unwrap_or_else(|_| "wss://arb-node.internal:8546".to_string());
                                let _ = submit_to_arbitrum_sequencer(
                                    signed_bytes, &arb_rpc,
                                ).await;
                            } else {
                                let _ = submit_multi_block(
                                    &bundle, block_num, &searcher_key, Some(ranked),
                                ).await;
                            }

                            // Increment nonce only after successful sign+submit.
                            let confirmed_nonce = nonce_mgr.next_nonce();
                            nonce_mgr.record_submission(confirmed_nonce, block_num);
                            gas_budget.record_submission(estimated_gas_gwei);

                            // Record submission in ProfitLedger for P&L tracking.
                            let gas_cost_usd = estimated_gas_gwei as f64 * 1e-9 * eth_price_usd;
                            let _ledger_id = ledger_record_submission(&LedgerEntry {
                                strategy:        config.label.to_string(),
                                chain:           format!("{}", chain),
                                block_submitted: block_num,
                                spread_bps:      spread_bps as u32,
                                size_usd,
                                gross_profit_usd,
                                gas_spent_usd:   gas_cost_usd,
                                flash_fee_usd,
                                // store the fully-net figure (after gas AND flash
                                // fee). net_profit_usd was post-flash/pre-gas; storing
                                // true_net_profit_usd makes PnlSummary::net_pnl correct
                                // without re-subtracting fees.
                                net_profit_usd:  true_net_profit_usd,
                                priority_fee_gwei,
                            });
                            // TODO store ledger_id and update outcome from receipt subscription

                            // TODO Increment session metrics after successful sign+submit.
                            // session_profit_usds_micros stores profit in 1e6 USDS units to keep
                            // values within u64 range for realistic profit sizes.
                            bundles_submitted_count.fetch_add(1, Ordering::Relaxed);
                            session_profit_usds_micros.fetch_add(
                                (net_profit_usd * 1_000_000.0) as u64,
                                Ordering::Relaxed,
                            );

                            // Debounce competitor bundle fetch to once per 5 blocks.
                            // Previously spawned every block: 7,200 API calls/hour.
                            if block_num % 5 == 0 {
                                let pool_label = format!("{:?}", config.dex_pool);
                                tokio::spawn(async move {
                                    let competitors = fetch_competitor_bundles(
                                        block_num, &[pool_label],
                                    ).await;
                                    if !competitors.is_empty() {
                                        info!(count = competitors.len(),
                                            "competitor bundles detected this block");
                                    }
                                });
                            }

                            // TODO Push to pending cascade queue including trade size as
                            // follow-on volume proxy (drained 2 blocks later).
                            // size_usd is f64 (USD, no decimals) — cast to u64 for storage.
                            pending_cascade.push_back((block_num, spread_bps as u32, size_usd as u64));
                        }
                        Err(e) => {
                            // Signing failed — this should be very rare (key is loaded at startup).
                            error!(chain = %chain, error = %e,
                                "transaction signing failed — skipping bundle submission");
                        }
                    }
                }
            }
            Ok(result) => {
                // Simulation reverted — increment circuit breaker counter.
                consecutive_sim_reverts += 1;
                warn!(
                    chain = %chain,
                    block = block_num,
                    gas   = result.gas_used,
                    consecutive = consecutive_sim_reverts,
                    "simulation reverted"
                );
                if consecutive_sim_reverts >= REVERT_CIRCUIT_BREAKER {
                    circuit_open = true;
                    error!(
                        chain = %chain,
                        consecutive = consecutive_sim_reverts,
                        "CIRCUIT BREAKER OPENED: {} consecutive simulation reverts. \
                         Submission halted. Investigate pool state / gas / access list. \
                         Set RESET_CIRCUIT_BREAKER=1 and restart to resume.",
                        REVERT_CIRCUIT_BREAKER
                    );
                }
            }
            Err(e) => {
                warn!(chain = %chain, error = %e, "simulation error");
            }
        }

        // Pass searcher_key + block context to watched_pool.update so
        // the pre-sign fast path can actually submit when the fire threshold is crossed.
        watched_pool.update(spread_bps as u32, &searcher_key, block_num, base_fee_gwei, last_priority_fee_gwei).await;

        // Broadcast block metrics (best-effort)
        let submitted  = bundles_submitted_count.load(Ordering::Relaxed);
        let profit_mic = session_profit_usds_micros.load(Ordering::Relaxed);

        // populate this strategy's live pool state instead of an empty vec, so
        // the dashboard pool monitor shows real spreads. The dashboard merges messages
        // from all pipelines by pool_name.
        let dex_price_for_dir = match direction {
            SpreadDirection::Discount => discount_price,
            SpreadDirection::Premium  => premium_price,
        };
        let pool_depth_str = depth_reg.get(config.dex_pool)
            .map(|d| d.reserve_a.to_string())
            .unwrap_or_else(|| "0".to_string());
        let pools_state = vec![crate::metrics_ws::PoolSpreadState {
            pool_name: config.label.to_string(),
            chain: format!("{}", chain),
            protocol_rate: protocol_rate.to_string(),
            dex_price: dex_price_for_dir.to_string(),
            spread_bps: spread_bps as u32,
            direction: match direction {
                SpreadDirection::Discount => "DISCOUNT".to_string(),
                SpreadDirection::Premium  => "PREMIUM".to_string(),
            },
            optimal_size: size.to_string(),
            pool_depth: pool_depth_str,
            actionable: true,
        }];

        let _ = metrics_tx.send(crate::metrics_ws::BlockMetricsMessage {
            msg_type: "block_metrics".to_string(),
            block_number: block_num,
            pools: pools_state,
            bundles_submitted: submitted,
            bundles_landed: 0,      // TODO: wire from receipt subscription
            bundles_reverted: 0,
            session_profit_usds: format!("{:.6}", profit_mic as f64 / 1_000_000.0),
            gas_spent_usd: format!("{:.4}", gas_budget.spent_today_gwei as f64 * 1e-9),
            builders: vec![],
            latency: crate::metrics_ws::PipelineLatency {
                ipc_recv_ms: 0.0, pool_update_ms: 0.0, rate_cache_ms: 0.0,
                binary_search_ms: 0.0, revm_sim_ms: 0.0,
                bundle_sign_ms: 0.0, submit_all_ms: 0.0,
            },
            whale_detections: 0,
            rate_cache_age_blocks: 0,
            chains: vec![],
        });
    }

    Err(eyre::eyre!("[{}] block stream ended unexpectedly", chain))
}

async fn pre_warm_access_lists<P: Provider>(pool: Address, provider: Arc<P>) {
    info!(pool = %pool, "pre-warming access list cache");
    let _ = crate::access_list::get_cached_or_generate(pool, SpreadDirection::Discount, &*provider, 0).await;
    let _ = crate::access_list::get_cached_or_generate(pool, SpreadDirection::Premium, &*provider, 0).await;
    info!(pool = %pool, "access list cache pre-warmed for both directions");
}

pub async fn encode_standard_calldata<P: Provider>(
    flash_amount: U256, direction: SpreadDirection, curve_pool: Address,
    provider: &P, min_profit: U256,
) -> alloy::primitives::Bytes {
    use alloy::sol;
    sol! {
        interface IArbitrageur {
            function execute(uint256 flashAmount, int128 usdsIndex, int128 susdsIndex,
                             uint256 minSusdsOut, uint256 minProfitOverride) external;
        }
    }
    let (usds_idx, susds_idx): (i128, i128) = match direction {
        SpreadDirection::Discount => (1, 0),
        SpreadDirection::Premium  => (0, 1),
    };
    // minSusdsOut slippage tolerance from env (was hardcoded 999/1000 = 99.9%)
    let min_out_bps: u64 = std::env::var("MIN_SUSDS_OUT_BPS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(9990);
    let get_dy_result = curve_pool_call_get_dy(curve_pool, usds_idx, susds_idx, flash_amount, provider)
        .await.unwrap_or(U256::ZERO);
    let min_susds_out = get_dy_result * U256::from(min_out_bps) / U256::from(10_000u64);
    // min_profit_override from env (was hardcoded 90/100 = 90%)
    let profit_override_bps: u64 = std::env::var("MIN_PROFIT_OVERRIDE_BPS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(9000);
    let min_profit_override = if min_profit > U256::ZERO {
        min_profit * U256::from(profit_override_bps) / U256::from(10_000u64)
    } else { U256::ZERO };
    IArbitrageur::executeCall {
        flashAmount: flash_amount, usdsIndex: usds_idx, susdsIndex: susds_idx,
        minSusdsOut: min_susds_out, minProfitOverride: min_profit_override,
    }.abi_encode().into()
}

pub use encode_standard_calldata as encode_execute_calldata;

async fn encode_flashmint_calldata<P: Provider>(
    flash_amount: U256, direction: SpreadDirection, curve_pool: Address,
    provider: &P, min_profit: U256,
) -> alloy::primitives::Bytes {
    use alloy::sol;
    sol! {
        interface IFlashMintArbitrageur {
            function execute(uint256 flashAmount, int128 usdsIndex, int128 susdsIndex,
                             uint256 minSusdsOut, uint256 minProfit) external;
        }
    }
    let (usds_idx, susds_idx): (i128, i128) = match direction {
        SpreadDirection::Discount => (1, 0),
        SpreadDirection::Premium  => (0, 1),
    };
    // slippage tolerance from env (was hardcoded 999/1000)
    let min_out_bps: u64 = std::env::var("MIN_SUSDS_OUT_BPS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(9990);
    let get_dy_result = curve_pool_call_get_dy(curve_pool, usds_idx, susds_idx, flash_amount, provider)
        .await.unwrap_or(U256::ZERO);
    let min_susds_out = get_dy_result * U256::from(min_out_bps) / U256::from(10_000u64);
    IFlashMintArbitrageur::executeCall {
        flashAmount: flash_amount, usdsIndex: usds_idx, susdsIndex: susds_idx,
        minSusdsOut: min_susds_out, minProfit: min_profit,
    }.abi_encode().into()
}

async fn encode_crossvenue_calldata<P: Provider>(
    flash_amount: U256, curve_pool: Address, _uniswap_pool: Address, provider: &P,
) -> alloy::primitives::Bytes {
    use alloy::sol;
    sol! {
        interface ISusdeArbitrageur {
            function execute(uint256 flashAmount, uint256 minSusdeOut,
                             uint256 minUsdcOut, uint256 minNetProfit) external;
        }
    }
    // CrossVenue slippage from env (was hardcoded 995/1000 = 99.5%)
    let min_out_bps: u64 = std::env::var("MIN_CROSS_VENUE_OUT_BPS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(9950);
    let get_dy_result = curve_pool_call_get_dy(curve_pool, 0, 1, flash_amount, provider)
        .await.unwrap_or(U256::ZERO);
    let min_susde_out = get_dy_result * U256::from(min_out_bps) / U256::from(10_000u64);
    ISusdeArbitrageur::executeCall {
        flashAmount: flash_amount, minSusdeOut: min_susde_out,
        minUsdcOut: U256::ZERO, minNetProfit: usds(50),
    }.abi_encode().into()
}

fn arbitrageur_address() -> Address {
    std::env::var("ARBITRAGEUR_ADDRESS").ok().and_then(|s| s.parse().ok()).unwrap_or(Address::ZERO)
}

// load_searcher_key is now called ONCE before the block loop.
// It is still a standalone function so the stuck-tx replacement path uses it too.
fn load_searcher_key() -> Option<alloy::signers::local::PrivateKeySigner> {
    use secrecy::{SecretString, ExposeSecret};
    let raw: SecretString = std::env::var("SEARCHER_PRIVATE_KEY").ok()?.into();
    raw.expose_secret().parse().ok()
}
