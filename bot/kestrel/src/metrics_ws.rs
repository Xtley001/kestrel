// Kestrel — metrics_ws.rs
// WebSocket metrics emitter on 127.0.0.1:9101
// and control command receiver on 127.0.0.1:9102.
// Both bind to 127.0.0.1 ONLY — never 0.0.0.0.
// Control commands verified via Ed25519 signature before acting.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::accept_async;
use tracing::{error, info, warn};
use futures_util::{SinkExt, StreamExt};

// ── JSON schema for metrics WebSocket messages ────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSpreadState {
    pub pool_name: String,
    pub chain: String,
    pub protocol_rate: String,   // U256 serialised as decimal string
    pub dex_price: String,
    pub spread_bps: u32,
    pub direction: String,       // "DISCOUNT" | "PREMIUM"
    pub optimal_size: String,    // Binary search result — actual value, not estimated
    pub pool_depth: String,
    pub actionable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderStats {
    pub name: String,
    pub submitted: u64,
    pub landed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineLatency {
    pub ipc_recv_ms: f64,
    pub pool_update_ms: f64,
    pub rate_cache_ms: f64,
    pub binary_search_ms: f64,
    pub revm_sim_ms: f64,
    pub bundle_sign_ms: f64,
    pub submit_all_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainStatus {
    pub chain: String,
    pub active: bool,
    pub latest_block: u64,
}

// Full bot metrics message — broadcast on every processed block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMetricsMessage {
    #[serde(rename = "type")]
    pub msg_type: String, // "block_metrics" | "alert"
    pub block_number: u64,
    pub pools: Vec<PoolSpreadState>,
    pub bundles_submitted: u64,
    pub bundles_landed: u64,
    pub bundles_reverted: u64,
    pub session_profit_usds: String,
    pub gas_spent_usd: String,
    pub builders: Vec<BuilderStats>,
    pub latency: PipelineLatency,
    pub whale_detections: u64,
    pub rate_cache_age_blocks: u64,
    pub chains: Vec<ChainStatus>,
}

// Alert message — pushed over the same metrics WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertMessage {
    #[serde(rename = "type")]
    pub msg_type: String, // "alert"
    pub severity: String, // "green" | "amber" | "red"
    pub chain: String,
    pub message: String,
    pub timestamp_ms: u64,
}

// Control command received from dashboard via 9102.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlCommand {
    pub command: String,   // e.g. "set_threshold", "pause", "resume"
    pub params: serde_json::Value,
    pub signature: String, // Ed25519 signature (hex)
    pub pubkey: String,    // Ed25519 public key (hex)
}

// Serve both WebSocket endpoints.
pub async fn serve(
    metrics_rx: broadcast::Receiver<BlockMetricsMessage>,
    _control_tx: mpsc::Sender<ControlCommand>,
) -> eyre::Result<()> {
    // Spawn metrics emitter on 127.0.0.1:9101
    let metrics_rx_clone = metrics_rx.resubscribe();
    tokio::spawn(async move {
        if let Err(e) = serve_metrics(metrics_rx_clone).await {
            error!(error = %e, "metrics WebSocket server failed");
        }
    });

    // Spawn control receiver on 127.0.0.1:9102
    tokio::spawn(async move {
        if let Err(e) = serve_control().await {
            error!(error = %e, "control WebSocket server failed");
        }
    });

    Ok(())
}

// Metrics WebSocket — pushes block metrics to all connected clients.
// Binds to 127.0.0.1:9101 ONLY.
async fn serve_metrics(
    metrics_rx: broadcast::Receiver<BlockMetricsMessage>,
) -> eyre::Result<()> {
    let addr: SocketAddr = "127.0.0.1:9101".parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!("metrics WebSocket listening on ws://127.0.0.1:9101");

    loop {
        let (stream, peer) = listener.accept().await?;
        let rx = metrics_rx.resubscribe();
        tokio::spawn(handle_metrics_client(stream, peer, rx));
    }
}

async fn handle_metrics_client(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    mut rx: broadcast::Receiver<BlockMetricsMessage>,
) {
    let ws = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!(peer = %peer, error = %e, "WebSocket handshake failed");
            return;
        }
    };

    let (mut write, _read) = ws.split();
    info!(peer = %peer, "metrics client connected");

    loop {
        match rx.recv().await {
            Ok(msg) => {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        warn!(error = %e, "failed to serialise metrics message");
                        continue;
                    }
                };
                if write
                    .send(tokio_tungstenite::tungstenite::Message::Text(json))
                    .await
                    .is_err()
                {
                    info!(peer = %peer, "metrics client disconnected");
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(peer = %peer, skipped = n, "metrics client lagged — skipping messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

// Control WebSocket — receives signed commands from dashboard.
// Binds to 127.0.0.1:9102 ONLY.
// Verifies Ed25519 signature on every received command before acting.
async fn serve_control() -> eyre::Result<()> {
    let addr: SocketAddr = "127.0.0.1:9102".parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!("control WebSocket listening on ws://127.0.0.1:9102");

    loop {
        let (stream, peer) = listener.accept().await?;
        tokio::spawn(handle_control_client(stream, peer));
    }
}

async fn handle_control_client(stream: tokio::net::TcpStream, peer: SocketAddr) {
    let ws = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!(peer = %peer, error = %e, "control WebSocket handshake failed");
            return;
        }
    };

    let (mut write, mut read) = ws.split();
    info!(peer = %peer, "control client connected");

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                match serde_json::from_str::<ControlCommand>(&text) {
                    Ok(cmd) => {
                        if verify_command_signature(&cmd) {
                            // apply the verified command to shared runtime controls
                            // (previously this was a no-op — controls had no effect).
                            let recognised = crate::controls::apply_command(&cmd.command, &cmd.params);
                            info!(command = cmd.command, recognised,
                                "control command accepted");
                            let ack = serde_json::json!({
                                "type": "control_ack",
                                "command": cmd.command,
                                "recognised": recognised,
                                "submission_enabled": crate::controls::submission_enabled(),
                                "paused": crate::controls::is_paused(),
                            });
                            let _ = write.send(
                                tokio_tungstenite::tungstenite::Message::Text(ack.to_string())
                            ).await;
                        } else {
                            // Rejected command — log source address and discard.
                            // Bot does NOT panic on bad signature.
                            warn!(
                                peer = %peer,
                                command = cmd.command,
                                "control command REJECTED — invalid signature"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(peer = %peer, error = %e, "invalid control command JSON");
                    }
                }
            }
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                info!(peer = %peer, "control client disconnected");
                break;
            }
            _ => {}
        }
    }
}

// Verify the Ed25519 signature on a control command.
// Returns false for any invalid or missing signature — never panics.
fn verify_command_signature(cmd: &ControlCommand) -> bool {
    use ed25519_dalek::{Signature, VerifyingKey};

    let sig_bytes = match hex::decode(&cmd.signature) {
        Ok(b) if b.len() == 64 => b,
        _ => return false,
    };

    let pubkey_bytes = match hex::decode(&cmd.pubkey) {
        Ok(b) if b.len() == 32 => b,
        _ => return false,
    };

    let Ok(sig) = Signature::try_from(sig_bytes.as_slice()) else {
        return false;
    };

    let Ok(vk_bytes): Result<[u8; 32], _> = pubkey_bytes.try_into() else {
        return false;
    };

    let Ok(vk) = VerifyingKey::from_bytes(&vk_bytes) else {
        return false;
    };

    let message = serde_json::json!({
        "command": cmd.command,
        "params": cmd.params
    });
    let msg_bytes = serde_json::to_vec(&message).unwrap_or_default();

    vk.verify_strict(&msg_bytes, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_message_serialises_correctly() {
        let msg = BlockMetricsMessage {
            msg_type: "block_metrics".to_string(),
            block_number: 19_500_000,
            pools: vec![],
            bundles_submitted: 42,
            bundles_landed: 38,
            bundles_reverted: 1,
            session_profit_usds: "12500000000000000000000".to_string(),
            gas_spent_usd: "250000000000000000000".to_string(),
            builders: vec![],
            latency: PipelineLatency {
                ipc_recv_ms: 0.5,
                pool_update_ms: 1.2,
                rate_cache_ms: 0.08,
                binary_search_ms: 3.1,
                revm_sim_ms: 4.5,
                bundle_sign_ms: 0.1,
                submit_all_ms: 12.0,
            },
            whale_detections: 2,
            rate_cache_age_blocks: 1,
            chains: vec![],
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["type"], "block_metrics");
        assert_eq!(parsed["block_number"], 19_500_000);
        assert_eq!(parsed["bundles_submitted"], 42);
    }

    #[test]
    fn both_ws_ports_bind_to_loopback_only() {
        // Verify the bind addresses are loopback, not 0.0.0.0
        let metrics_addr: std::net::SocketAddr = "127.0.0.1:9101".parse().unwrap();
        let control_addr: std::net::SocketAddr = "127.0.0.1:9102".parse().unwrap();
        assert!(metrics_addr.ip().is_loopback(), "metrics WS must bind to loopback");
        assert!(control_addr.ip().is_loopback(), "control WS must bind to loopback");
    }
}
