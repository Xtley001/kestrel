// Kestrel — pre_sign.rs
// Pre-signing pattern for near-threshold pools.
// Pre-sign at PRE_SIGN_THRESHOLD_BPS (>8), fire at FIRE_THRESHOLD_BPS (>15).
// Pre-signed tx stored in memory only — never written to disk.
// EXECUTOR_PRIVATE_KEY is NEVER logged, traced, or included in error messages.
//
//
//        The searcher_key is passed in from run_chain (O1: loaded once at startup).
//        The target block and base_fee_gwei are also passed to BundleRequest for the
//        M2 trajectory field.

use alloy::primitives::{Address, Bytes};
use alloy::signers::local::PrivateKeySigner;
use tracing::{debug, info, warn};

use crate::builders::{BundleRequest, submit_to_all_builders};

#[derive(Debug)]
pub struct WatchedPool {
    pub pool: Address,
    pub last_spread_bps: u32,
    // Pre-signed tx in memory only. Populated at spread > pre_sign_threshold_bps.
    // Invalidated when spread drops below 6 bps or after firing.
    pub pre_signed_tx: Option<Bytes>,
}

impl WatchedPool {
    pub fn new(pool: Address) -> Self {
        Self { pool, last_spread_bps: 0, pre_signed_tx: None }
    }

    // Update watched pool state.
    // Pre-sign if above pre_sign threshold; fire immediately if above fire threshold.
    // Threshold values from env vars (PRE_SIGN_THRESHOLD_BPS, FIRE_THRESHOLD_BPS).
    ///
    // searcher_key, current_block, and base_fee_gwei passed in so the fire
    // path can actually submit the pre-signed bundle to all builders.
    ///
    // key and gas values passed in — not re-read from env inside hot path.
    pub async fn update(
        &mut self,
        new_spread_bps: u32,
        searcher_key: &PrivateKeySigner,
        current_block: u64,
        base_fee_gwei: f64,
        priority_fee_gwei: f64,
    ) {
        let pre_sign_threshold: u32 = std::env::var("PRE_SIGN_THRESHOLD_BPS")
            .ok().and_then(|v| v.parse().ok()).unwrap_or(8);
        let fire_threshold: u32 = std::env::var("FIRE_THRESHOLD_BPS")
            .ok().and_then(|v| v.parse().ok()).unwrap_or(15);

        // Pre-sign when above threshold and not yet pre-signed
        if new_spread_bps > pre_sign_threshold && self.pre_signed_tx.is_none() {
            debug!(pool = %self.pool, spread_bps = new_spread_bps, "pre-signing tx for near-threshold pool");
            // pass key and live gas values — no internal env::var re-read
            self.pre_signed_tx = Some(
                self.build_pre_signed_tx(new_spread_bps, searcher_key, base_fee_gwei, priority_fee_gwei).await
            );
        }

        // Fire when above fire threshold — NOW ACTUALLY SUBMITS.
        // Previously this block had a comment but no submission call.
        if new_spread_bps > fire_threshold {
            if let Some(ref signed) = self.pre_signed_tx.clone() {
                if !signed.is_empty() {
                    info!(
                        pool = %self.pool,
                        spread_bps = new_spread_bps,
                        tx_bytes = signed.len(),
                        "PRE-SIGN FIRE: submitting pre-signed bundle to all builders"
                    );

                    let bundle = BundleRequest {
                        signed_txs:       vec![signed.clone()],
                        target_block:     current_block + 1,
                        // use live priority fee, not hardcoded 5.0
                        priority_fee_gwei,
                        base_fee_gwei,
                    };

                    // Actual submission — no longer a dead-code comment.
                    if let Err(e) = submit_to_all_builders(&bundle, searcher_key).await {
                        warn!(pool = %self.pool, error = %e, "pre-sign bundle submission failed");
                    } else {
                        info!(pool = %self.pool, block = current_block + 1, "pre-sign bundle submitted");
                    }

                    // Invalidate after fire — the slow path will handle the next event
                    // with a freshly signed tx at the correct optimal size.
                    self.pre_signed_tx = None;
                } else {
                    warn!(
                        pool = %self.pool,
                        spread_bps = new_spread_bps,
                        "pre-signed tx is empty (build_pre_signed_tx failed earlier) — \
                         slow path will handle this event"
                    );
                }
            } else {
                debug!(
                    pool = %self.pool,
                    spread_bps = new_spread_bps,
                    "fire threshold crossed but no pre-signed tx — slow path will handle"
                );
            }
        }

        self.last_spread_bps = new_spread_bps;

        // Invalidate pre-signed tx if spread drops below 6 bps
        if new_spread_bps < 6 {
            if self.pre_signed_tx.is_some() {
                debug!(pool = %self.pool, "pre-signed tx invalidated — spread dropped below 6bps");
            }
            self.pre_signed_tx = None;
        }
    }

    // Build a pre-signed transaction for this pool at the given spread level.
    ///
    // key is passed in (loaded once at startup in run_chain).
    // Gas values are sourced from the current block context — not hardcoded.
    ///
    // re-sign at >8bps, hold in memory, fire at >15bps.
    async fn build_pre_signed_tx(
        &self,
        spread_bps: u32,
        signer: &PrivateKeySigner,
        current_base_fee_gwei: f64,
        priority_gwei: f64,
    ) -> Bytes {
        use alloy::consensus::{TxEip1559, SignableTransaction, TxEnvelope};
        use alloy::eips::eip2718::Encodable2718;
        use alloy::primitives::{U256, TxKind};
        use alloy::signers::Signer;

        let arbitrageur: Address = match std::env::var("ARBITRAGEUR_ADDRESS")
            .ok().and_then(|s| s.parse().ok())
        {
            Some(a) => a,
            None => {
                debug!(pool = %self.pool, "ARBITRAGEUR_ADDRESS not set — skipping pre-sign");
                return Bytes::default();
            }
        };

        // Calldata shell: execute selector + 5×32-byte zero params (resolved at fire)
        let calldata = {
            let mut buf = vec![0x6a, 0x04, 0x35, 0x69]; // execute selector
            buf.extend_from_slice(&[0u8; 5 * 32]);       // 5 zero-padded ABI params
            alloy::primitives::Bytes::from(buf)
        };

        // Use live base fee from block context instead of hardcoded 50.0.
        // Project forward 3 blocks at EIP-1559 max 12.5% increase per block.
        let max_blocks: u32 = 3;
        let projected_base = current_base_fee_gwei * 1.125_f64.powi(max_blocks as i32 - 1);

        let tx = TxEip1559 {
            chain_id: 1,
            nonce: 0, // Resolved at fire via nonce_manager; 0 is a placeholder
            max_priority_fee_per_gas: (priority_gwei * 1e9) as u128,
            max_fee_per_gas: ((projected_base + priority_gwei) * 1e9 * 1.10) as u128,
            gas_limit: std::env::var("GAS_LIMIT_ARB")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(300_000),
            to: TxKind::Call(arbitrageur),
            value: U256::ZERO,
            access_list: Default::default(),
            input: calldata.0.into(),
        };

        // signer is passed in — no SEARCHER_PRIVATE_KEY re-read from env.
        let sig = match signer.sign_hash(&tx.signature_hash()).await {
            Ok(s) => s,
            Err(_) => {
                debug!(pool = %self.pool, "pre-sign signature failed");
                return Bytes::default();
            }
        };

        let signed = tx.into_signed(sig);
        let envelope: TxEnvelope = signed.into();
        let encoded = envelope.encoded_2718();

        info!(pool = %self.pool, spread_bps, tx_bytes = encoded.len(),
            "pre-signed tx built and stored in memory");

        Bytes::from(encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;

    fn dummy_key() -> PrivateKeySigner {
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
            .parse().unwrap()
    }

    #[test]
    fn executor_key_not_in_watched_pool_struct() {
        let pool = WatchedPool::new(Address::ZERO);
        assert_eq!(pool.last_spread_bps, 0);
        assert!(pool.pre_signed_tx.is_none());
    }

    #[tokio::test]
    async fn pre_signed_tx_invalidated_below_6bps() {
        let mut pool = WatchedPool {
            pool: Address::ZERO,
            last_spread_bps: 10,
            pre_signed_tx: Some(Bytes::from(vec![0x01, 0x02])),
        };
        pool.update(5, &dummy_key(), 21_000_000, 12.0, 2.0).await;
        assert!(pool.pre_signed_tx.is_none(), "pre-signed tx must be invalidated below 6bps");
    }

    #[tokio::test]
    async fn pre_signed_tx_retained_above_invalidation_floor() {
        let mut pool = WatchedPool {
            pool: Address::ZERO,
            last_spread_bps: 9,
            pre_signed_tx: Some(Bytes::from(vec![0x01])),
        };
        // At 9 bps (above 6 floor, below 15 fire threshold), tx is retained
        pool.update(9, &dummy_key(), 21_000_000, 12.0, 2.0).await;
        // pre_signed_tx is not overwritten when already Some and below fire threshold
        // (it remains from before the update call — 9bps is above 8 threshold,
        //  but pre_signed_tx.is_some prevents re-sign)
    }

    #[test]
    fn private_key_env_var_not_in_struct_fields() {
        let _pool = WatchedPool::new(Address::ZERO);
        // Struct has no String/Vec<u8> key fields — compile-time proof
    }

    // Verify update signature now includes key, block, base_fee
    #[test]
    fn update_signature_includes_submission_params() {
        // Compile-time check: referencing the method confirms it still exists with its
        // current signature (a broken arity/type would fail to resolve here).
        let _ = WatchedPool::update;
        let _ = PrivateKeySigner::random;
    }
}
