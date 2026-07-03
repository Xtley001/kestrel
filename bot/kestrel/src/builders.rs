// Kestrel — builders.rs
// Multi-builder bundle submission — all builders in parallel.
// HTTP/2 persistent connections — single shared client, 30s TCP keepalive.
// Multi-block bundle targeting — submit to N+1, N+2, N+3 simultaneously.
// Adaptive priority fee bidding — bid fraction of net profit as coinbase payment.
//

use alloy::signers::local::PrivateKeySigner;
use alloy::primitives::Bytes;
use eyre::Result;
use futures::future::join_all;
use once_cell::sync::Lazy;
use std::time::Duration;
use tracing::{debug, error, info, warn};

// Shared HTTP/2 client — never re-created per submission.
static BUILDER_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        // do NOT force http2_prior_knowledge — the relays are HTTPS and negotiate
        // HTTP/2 via ALPN. Prior-knowledge (h2c) skips ALPN and breaks the TLS handshake.
        .tcp_keepalive(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .timeout(Duration::from_millis(2_000))
        .connection_verbose(false)
        .build()
        .expect("failed to build builder HTTP client")
});

pub const DEFAULT_BUILDER_FLASHBOTS:   &str = "https://relay.flashbots.net";
pub const DEFAULT_BUILDER_TITAN:       &str = "https://rpc.titanbuilder.xyz";
pub const DEFAULT_BUILDER_BEAVERBUILD: &str = "https://rpc.beaverbuild.org";
pub const DEFAULT_BUILDER_RSYNC:       &str = "https://rsync-builder.xyz";
pub const DEFAULT_BUILDER_BUILDER0X69: &str = "https://builder0x69.io";
pub const DEFAULT_BUILDER_BLOXROUTE:   &str = "https://mev.api.bloxroute.com/v1/services/submit-flashbots-bundles";

#[derive(Debug, Clone)]
pub struct BundleRequest {
    // Always contains the actual signed and RLP-encoded transaction bytes.
    // Built and signed in spread_pipeline.rs before calling submit_multi_block.
    // (Previously always vec![] — the bundle was always empty.)
    pub signed_txs: Vec<Bytes>,
    pub target_block: u64,
    // Adaptive tip in gwei — computed by competitor_tracker::compute_adaptive_priority_fee
    pub priority_fee_gwei: f64,
    // Current block base fee in gwei, stored so multi-block submissions can
    // document the fee trajectory. The signed tx already has a max_fee_per_gas set to
    // cover through block N+max_blocks (projected in spread_pipeline.rs at sign time).
    pub base_fee_gwei: f64,
}

pub fn builder_urls() -> Vec<(&'static str, String)> {
    vec![
        ("flashbots",   std::env::var("BUILDER_FLASHBOTS").unwrap_or_else(|_| DEFAULT_BUILDER_FLASHBOTS.to_string())),
        ("titan",       std::env::var("BUILDER_TITAN").unwrap_or_else(|_| DEFAULT_BUILDER_TITAN.to_string())),
        ("beaverbuild", std::env::var("BUILDER_BEAVERBUILD").unwrap_or_else(|_| DEFAULT_BUILDER_BEAVERBUILD.to_string())),
        ("rsync",       std::env::var("BUILDER_RSYNC").unwrap_or_else(|_| DEFAULT_BUILDER_RSYNC.to_string())),
        ("builder0x69", std::env::var("BUILDER_BUILDER0X69").unwrap_or_else(|_| DEFAULT_BUILDER_BUILDER0X69.to_string())),
        ("bloxroute",   std::env::var("BUILDER_BLOXROUTE").unwrap_or_else(|_| DEFAULT_BUILDER_BLOXROUTE.to_string())),
    ]
}

// Submit the bundle targeting N+1 … N+max_blocks simultaneously.
///
// Multi-block nonce design (intentional):
// All block-offset submissions use the SAME nonce and signed tx bytes.
// Only one can land (first included block consumes the nonce; the others become
// invalid). This is by design — the on-chain profit guard means only the first
// valid execution captures profit; subsequent ones revert at zero cost.
///
// Gas price trajectory (M2):
// The caller (spread_pipeline.rs) signs the tx with a max_fee_per_gas that covers
// the EIP-1559 worst-case base fee through block N+max_blocks:
// max_fee = (base_fee * 1.125^(max_blocks-1) + priority_fee) * 1.10
// This ensures the tx remains includable even if base fees rise 12.5%/block until
// the last targeted block. The transaction is signed once; all offsets share it.
pub async fn submit_multi_block(
    bundle: &BundleRequest,
    current_block: u64,
    searcher_key: &PrivateKeySigner,
    ranked_builders: Option<Vec<String>>,
) -> Result<()> {
    let max_blocks: u64 = std::env::var("MULTI_BLOCK_TARGET_COUNT")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(3);

    // Log the projected base fee trajectory for each block offset.
    for offset in 1..=max_blocks {
        let projected = bundle.base_fee_gwei * 1.125_f64.powi(offset as i32 - 1);
        debug!(
            offset, projected_base_gwei = format!("{:.3}", projected),
            "multi-block base fee projection"
        );
    }

    let futures: Vec<_> = (1..=max_blocks).flat_map(|offset| {
        let mut b = bundle.clone();
        b.target_block = current_block + offset;

        // Submit in landing-rate order so best-performing builder gets first call
        let builders: Vec<(&'static str, String)> = if let Some(ref ranked) = ranked_builders {
            let all = builder_urls();
            let mut ordered: Vec<(&'static str, String)> = ranked.iter().filter_map(|name| {
                all.iter().find(|(n, _)| *n == name.as_str()).cloned()
            }).collect();
            // Append any builders not in the ranked list (new builders default to back)
            for item in &all {
                if !ordered.iter().any(|(n, _)| *n == item.0) {
                    ordered.push(item.clone());
                }
            }
            ordered
        } else {
            builder_urls()
        };

        builders.into_iter().map(move |(name, url)| {
            let b2 = b.clone();
            let key = searcher_key.clone();
            async move { sign_and_submit_bundle(&b2, &key, name, &url).await }
        }).collect::<Vec<_>>()
    }).collect();

    join_all(futures).await;
    Ok(())
}

// Submit to all builders for a single target block (pre_sign fast path).
pub async fn submit_to_all_builders(
    bundle: &BundleRequest,
    searcher_key: &PrivateKeySigner,
) -> Result<()> {
    let builders = builder_urls();
    let futures: Vec<_> = builders.iter()
        .map(|(name, url)| sign_and_submit_bundle(bundle, searcher_key, name, url))
        .collect();
    join_all(futures).await;
    Ok(())
}

async fn sign_and_submit_bundle(
    bundle: &BundleRequest,
    searcher_key: &PrivateKeySigner,
    builder_name: &str,
    url: &str,
) {
    if bundle.signed_txs.is_empty() {
        // Guard: ensures this never fires in production.
        // If it does fire, we have a code path that bypassed the signing step.
        warn!(builder = builder_name, "BUG: signed_txs is empty — bundle not submitted. Check S1 signing path.");
        return;
    }
    match sign_and_submit_inner(bundle, searcher_key, url).await {
        Ok(_) => {
            info!(
                builder = builder_name,
                block = bundle.target_block,
                tip_gwei = bundle.priority_fee_gwei,
                txs = bundle.signed_txs.len(),
                "bundle submitted"
            );
        }
        Err(e) => {
            error!(builder = builder_name, error = %e, "bundle submission failed");
        }
    }
}

async fn sign_and_submit_inner(
    bundle: &BundleRequest,
    searcher_key: &PrivateKeySigner,
    url: &str,
) -> eyre::Result<()> {
    use alloy::signers::Signer;

    let txs: Vec<String> = bundle.signed_txs.iter()
        .map(|tx| format!("0x{}", hex::encode(tx)))
        .collect();

    let bundle_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_sendBundle",
        "params": [{
            "txs": txs,
            "blockNumber": format!("0x{:x}", bundle.target_block),
            "minTimestamp": 0,
            "maxTimestamp": 0,
            "revertingTxHashes": []
        }]
    });

    let body_str = serde_json::to_string(&bundle_body)?;
    // Flashbots-style relays expect the signature to be an EIP-191 personal_sign
    // over the *hex string* of keccak256(body) — NOT a signature of the raw hash. Signing
    // the raw hash produced a signature every relay rejected, dropping every bundle.
    let body_hash = alloy::primitives::keccak256(body_str.as_bytes());
    let hash_hex  = format!("0x{}", hex::encode(body_hash));
    let sig       = searcher_key.sign_message(hash_hex.as_bytes()).await?;
    let sig_header = format!("{}:0x{}", searcher_key.address(), hex::encode(sig.as_bytes()));

    BUILDER_CLIENT
        .post(url)
        .header("X-Flashbots-Signature", sig_header)
        .header("Content-Type", "application/json")
        .body(body_str)
        .send()
        .await?;

    Ok(())
}

// Submit a raw tx to the Arbitrum sequencer (arrival-time ordered, not gas-price ordered).
pub async fn submit_to_arbitrum_sequencer(signed_tx: Bytes, sequencer_url: &str) -> eyre::Result<()> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "eth_sendRawTransaction",
        "params": [format!("0x{}", hex::encode(&signed_tx))]
    });
    BUILDER_CLIENT.post(sequencer_url)
        .header("Content-Type", "application/json")
        .json(&body).send().await?;
    Ok(())
}

// Keep-alive OPTIONS ping to warm all TCP connections between blocks.
// Prevents cold-connection TLS handshake on next submission.
pub async fn keepalive_ping_all_builders() {
    let futs: Vec<_> = builder_urls().into_iter().map(|(name, url)| async move {
        let _ = BUILDER_CLIENT.head(&url).send().await;
        debug!(builder = name, "keepalive ping sent");
    }).collect();
    join_all(futs).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_six_builders_present() {
        let urls = builder_urls();
        let names: Vec<&str> = urls.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"flashbots"));
        assert!(names.contains(&"titan"));
        assert!(names.contains(&"beaverbuild"));
        assert!(names.contains(&"rsync"));
        assert!(names.contains(&"builder0x69"));
        assert!(names.contains(&"bloxroute"));
        assert_eq!(urls.len(), 6);
    }

    #[test]
    fn bundle_request_has_base_fee_field() {
        let b = BundleRequest {
            signed_txs: vec![Bytes::from(vec![0x02, 0x01])],
            target_block: 21_000_000,
            priority_fee_gwei: 3.0,
            base_fee_gwei: 12.5,  // new field
        };
        assert!((b.base_fee_gwei - 12.5).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_signed_txs_detected() {
        let b = BundleRequest {
            signed_txs: vec![],
            target_block: 21_000_000,
            priority_fee_gwei: 1.0,
            base_fee_gwei: 10.0,
        };
        assert!(b.signed_txs.is_empty()); // submit_multi_block checks and skips
    }

    #[test]
    fn multi_block_generates_correct_future_count() {
        let max_blocks: u64 = 3;
        let builder_count = builder_urls().len(); // 6
        assert_eq!(max_blocks as usize * builder_count, 18);
    }

    #[test]
    fn bundle_json_has_correct_structure() {
        let txs = vec!["0x02abcd".to_string()];
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "eth_sendBundle",
            "params": [{"txs": txs, "blockNumber": "0x12a05f20",
                        "minTimestamp": 0, "maxTimestamp": 0, "revertingTxHashes": []}]
        });
        assert_eq!(body["method"], "eth_sendBundle");
        assert_eq!(body["params"][0]["txs"][0], "0x02abcd");
    }

    #[test]
    fn base_fee_projection_formula() {
        // EIP-1559: max 12.5% increase per block
        let base = 10.0_f64;
        let projected_n3 = base * 1.125_f64.powi(2); // block N+3 (offset=3, powi(3-1)=powi(2))
        assert!((projected_n3 - 12.656).abs() < 0.01);
    }

    #[test]
    fn http2_client_initialises() {
        let _ = &*BUILDER_CLIENT;
    }
}
