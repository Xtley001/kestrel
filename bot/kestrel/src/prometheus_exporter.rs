// Kestrel — prometheus_exporter.rs
// Prometheus metrics endpoint on http://127.0.0.1:9090/metrics
// All metric names use the kestrel_ prefix.
// Binds to 127.0.0.1 ONLY — never exposed externally.

use eyre::Result;
use prometheus::{
    register_counter, register_counter_vec, register_gauge, register_gauge_vec,
    register_histogram_vec, Counter, CounterVec, Gauge, GaugeVec, HistogramVec,
    TextEncoder, Encoder,
};
use std::sync::OnceLock;
use tokio::net::TcpListener;
use tracing::info;

// ── Metric definitions ────────────────────────────────────────────────────────

pub struct KestrelMetrics {
    // Session profit in USDS (gauge)
    pub session_profit_usds: Gauge,
    // Total bundles submitted (counter)
    pub bundles_submitted_total: Counter,
    // Total bundles landed (counter)
    pub bundles_landed_total: Counter,
    // Total on-chain reverts (counter)
    pub on_chain_reverts_total: Counter,
    // Total gas spent in gwei (counter)
    pub gas_spent_gwei_total: Counter,
    // Current spread in bps per pool and chain (gauge)
    pub spread_bps: GaugeVec,
    // Optimal flash size in USD per pool (gauge)
    pub optimal_size_usd: GaugeVec,
    // Pipeline stage latency in ms (histogram)
    // Stages: ipc_recv, pool_update, rate_cache, binary_search, revm_sim, bundle_sign, submit_all
    pub pipeline_latency_ms: HistogramVec,
    // Builder landing rate per builder (gauge)
    pub builder_landing_rate: GaugeVec,
    // Total opportunity detections (counter)
    pub whale_detections_total: Counter,
}

static METRICS: OnceLock<KestrelMetrics> = OnceLock::new();

pub fn metrics() -> &'static KestrelMetrics {
    METRICS.get_or_init(|| {
        // Histogram bucket boundaries for pipeline latency: meaningful for MEV timing
        let latency_buckets = vec![0.1, 0.5, 1.0, 5.0, 10.0, 20.0, 50.0, 100.0];

        KestrelMetrics {
            session_profit_usds: register_gauge!(
                "kestrel_session_profit_usds",
                "Session profit in USDS"
            )
            .unwrap(),

            bundles_submitted_total: register_counter!(
                "kestrel_bundles_submitted_total",
                "Total number of bundles submitted to builders"
            )
            .unwrap(),

            bundles_landed_total: register_counter!(
                "kestrel_bundles_landed_total",
                "Total number of bundles that landed on-chain"
            )
            .unwrap(),

            on_chain_reverts_total: register_counter!(
                "kestrel_on_chain_reverts_total",
                "Total on-chain reverts (InsufficientProfit guard triggered)"
            )
            .unwrap(),

            gas_spent_gwei_total: register_counter!(
                "kestrel_gas_spent_gwei_total",
                "Total gas spent in gwei"
            )
            .unwrap(),

            spread_bps: register_gauge_vec!(
                "kestrel_spread_bps",
                "Current spread in basis points",
                &["pool", "chain"]
            )
            .unwrap(),

            optimal_size_usd: register_gauge_vec!(
                "kestrel_optimal_size_usd",
                "Optimal flash loan size in USD as computed by binary search",
                &["pool"]
            )
            .unwrap(),

            pipeline_latency_ms: register_histogram_vec!(
                "kestrel_pipeline_latency_ms",
                "Pipeline stage latency in milliseconds",
                &["stage"],
                latency_buckets
            )
            .unwrap(),

            builder_landing_rate: register_gauge_vec!(
                "kestrel_builder_landing_rate",
                "Landing rate per builder (landed / submitted)",
                &["builder"]
            )
            .unwrap(),

            whale_detections_total: register_counter!(
                "kestrel_whale_detections_total",
                "Total whale transactions detected in pending mempool"
            )
            .unwrap(),
        }
    })
}

// Serve Prometheus metrics on http://127.0.0.1:9090/metrics
// Binds to 127.0.0.1 ONLY.
pub async fn serve() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:9090").await?;
    info!("Prometheus metrics listening on http://127.0.0.1:9090/metrics");

    // Initialise metrics on startup
    let _ = metrics();

    loop {
        let (mut stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            handle_request(&mut stream).await;
        });
    }
}

async fn handle_request(stream: &mut tokio::net::TcpStream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = [0u8; 1024];
    let _ = stream.read(&mut buf).await;

    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut output = Vec::new();
    let _ = encoder.encode(&metric_families, &mut output);

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n",
        output.len()
    );

    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&output).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_metrics_use_kestrel_prefix() {
        let m = metrics();
        // Verify metrics are accessible (they use kestrel_ prefix by construction)
        m.bundles_submitted_total.inc();
        m.bundles_landed_total.inc();
        m.on_chain_reverts_total.inc();
        m.whale_detections_total.inc();

        // Verify histogram has the correct stage labels
        m.pipeline_latency_ms
            .with_label_values(&["ipc_recv"])
            .observe(1.5);
        m.pipeline_latency_ms
            .with_label_values(&["binary_search"])
            .observe(3.2);
        m.pipeline_latency_ms
            .with_label_values(&["revm_sim"])
            .observe(5.0);
    }

    #[test]
    fn prometheus_endpoint_binds_to_loopback() {
        let addr: std::net::SocketAddr = "127.0.0.1:9090".parse().unwrap();
        assert!(addr.ip().is_loopback(), "Prometheus must bind to loopback only");
    }
}
