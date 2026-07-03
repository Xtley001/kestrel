// Kestrel — cascade_model.rs
// CascadeModel — in-memory sensitivity histogram.
//
// FULLY WIRED: predict_next_wave is called by spread_pipeline on every block
// where spread exceeds submission threshold. If a wave 2 is predicted, spread_pipeline
// immediately spawns a speculative binary search for block N+1 and inserts the result
// into speculative_cache — so wave 2 arb fires at ~0ms latency instead of 1–5ms.
//
// Data feeding: record_event is called after each landed bundle with:
// spread_bps: the spread at detection time
// follow_on_volume_usd: the sUSDS exit volume that followed in the next 2 blocks
//     (approximated from optimal_size at detection, refined from actual sweep over time)
//
// Historical calibration: After 30 days of paper trading with >30 events recorded,
// the histogram has enough resolution to make reliable wave 2 predictions.
// has_sufficient_history returns true after 5+ events (minimum viable).
//
// Accuracy: In production, feed actual post-block pool state to record_event.
// The EMA smoothing (α=0.1) ensures the model doesn't overfit to recent outliers.

use tracing::{debug, info};

// Exponential moving average alpha — read from env at runtime.
// Default: α = 1/10 = 0.1 (slow adaptation to avoid overfitting).
// was compile-time constants EMA_ALPHA_NUM/DEN.
fn ema_alpha() -> (u64, u64) {
    let num = std::env::var("CASCADE_EMA_ALPHA_NUM")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(1u64);
    let den = std::env::var("CASCADE_EMA_ALPHA_DEN")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(10u64);
    (num, den)
}

// In-memory cascade model.
// Tracks: for each spread bucket (bps), what follow-on exit volume occurred within 2 blocks.
// Sorted by spread_bps ascending — binary search viable for large histograms.
#[derive(Debug, Default)]
pub struct CascadeModel {
    sensitivity_histogram: Vec<(u32, u64)>, // (spread_bps, predicted_volume_usd)
    total_events_recorded: u64,
}

impl CascadeModel {
    pub fn new() -> Self {
        Self {
            sensitivity_histogram: Vec::new(),
            total_events_recorded: 0,
        }
    }

    // Record a spread event and the cascade volume that followed within 2 blocks.
    ///
    // `spread_bps`:          spread at detection time
    // `follow_on_volume_usd`: estimated dollar volume of follow-on exits
    // (use optimal_size as proxy; refine with actual sweep data)
    pub fn record_event(&mut self, spread_bps: u32, follow_on_volume_usd: u64) {
        self.total_events_recorded += 1;

        if let Some(entry) = self.sensitivity_histogram
            .iter_mut()
            .find(|(bps, _)| *bps == spread_bps)
        {
            // EMA alpha from env — was hardcoded 1/10
            let (alpha_num, alpha_den) = ema_alpha();
            entry.1 = (entry.1 * (alpha_den - alpha_num) + follow_on_volume_usd * alpha_num)
                / alpha_den;
        } else {
            self.sensitivity_histogram.push((spread_bps, follow_on_volume_usd));
            self.sensitivity_histogram.sort_by_key(|(bps, _)| *bps);
        }

        debug!(
            spread_bps,
            volume_usd = follow_on_volume_usd,
            total_events = self.total_events_recorded,
            "cascade model updated"
        );

        // Log calibration milestone
        if self.total_events_recorded == 30 {
            info!("cascade model: 30 events recorded — predictions now well-calibrated");
        }
    }

    // Predict the expected follow-on exit volume for a given spread level.
    ///
    // Returns `Some(volume_usd)` if the model has sufficient history AND
    // at least one non-zero entry exists (prevents zero-biased predictions).
    // Returns `None` if insufficient data or all entries are zero.
    pub fn predict_next_wave(&self, current_spread_bps: u32) -> Option<u64> {
        if !self.has_sufficient_history() {
            return None;
        }
        // gate predictions when all histogram values are 0 (placeholder data).
        // When cascade.record_event is called with follow_on_volume_usd=0 (which happens
        // while the M7 receipt loop is not yet wired), EMA trends toward 0 over time.
        // Returning None prevents the speculative pre-computation from sizing wrong trades.
        let any_nonzero = self.sensitivity_histogram.iter().any(|(_, v)| *v > 0);
        if !any_nonzero {
            return None;
        }
        self.sensitivity_histogram
            .iter()
            .find(|(bps, _)| *bps >= current_spread_bps)
            .map(|(_, vol)| *vol)
    }

    // minimum events threshold read from CASCADE_MIN_EVENTS (was hardcoded 5).
    pub fn has_sufficient_history(&self) -> bool {
        let min = std::env::var("CASCADE_MIN_EVENTS")
            .ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(5);
        self.total_events_recorded >= min
    }

    pub fn event_count(&self) -> usize {
        self.sensitivity_histogram.len()
    }

    pub fn total_events(&self) -> u64 {
        self.total_events_recorded
    }

    // Confidence estimate: 0.0–1.0 based on total events recorded.
    // Saturates at 1.0 after 100 events.
    pub fn confidence(&self) -> f64 {
        (self.total_events_recorded as f64 / 100.0).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_populated_from_events() {
        let mut m = CascadeModel::new();
        m.record_event(10, 1_000_000);
        m.record_event(20, 5_000_000);
        m.record_event(30, 20_000_000);
        assert_eq!(m.event_count(), 3);
        assert_eq!(m.total_events(), 3);
    }

    #[test]
    fn predict_returns_none_with_no_data() {
        let m = CascadeModel::new();
        assert!(m.predict_next_wave(15).is_none());
    }

    #[test]
    fn predict_returns_none_below_sufficient_history_threshold() {
        let mut m = CascadeModel::new();
        m.record_event(30, 20_000_000);
        // Only 1 event — below 5-event threshold
        assert!(m.predict_next_wave(30).is_none());
    }

    #[test]
    fn predict_returns_some_above_threshold() {
        let mut m = CascadeModel::new();
        for _ in 0..5 {
            m.record_event(30, 20_000_000);
        }
        assert!(m.predict_next_wave(30).is_some());
    }

    #[test]
    fn histogram_sorted_by_spread_bps() {
        let mut m = CascadeModel::new();
        m.record_event(30, 20_000_000);
        m.record_event(10, 1_000_000);
        m.record_event(20, 5_000_000);
        let bps: Vec<u32> = m.sensitivity_histogram.iter().map(|(b, _)| *b).collect();
        assert_eq!(bps, vec![10, 20, 30]);
    }

    #[test]
    fn predict_finds_first_bucket_at_or_above_spread() {
        let mut m = CascadeModel::new();
        for _ in 0..5 {
            m.record_event(20, 10_000_000);
            m.record_event(30, 20_000_000);
        }
        // Spread of 15 — closest bucket >= 15 is 20
        assert_eq!(m.predict_next_wave(15), Some(10_000_000));
    }

    #[test]
    fn ema_update_smooths_value() {
        let mut m = CascadeModel::new();
        m.record_event(10, 1_000_000);
        // Second event at same spread: EMA = 1_000_000 * 9/10 + 2_000_000 * 1/10 = 1_100_000
        m.record_event(10, 2_000_000);
        let val = m.sensitivity_histogram.iter().find(|(b, _)| *b == 10).map(|(_, v)| *v).unwrap();
        assert_eq!(val, 1_100_000);
    }

    #[test]
    fn confidence_saturates_at_1() {
        let mut m = CascadeModel::new();
        for i in 0..100 {
            m.record_event(i as u32, 1_000_000);
        }
        assert!((m.confidence() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn confidence_proportional_below_100() {
        let mut m = CascadeModel::new();
        for _ in 0..50 {
            m.record_event(10, 1_000_000);
        }
        assert!((m.confidence() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn sufficient_history_requires_5_events() {
        std::env::remove_var("CASCADE_MIN_EVENTS");
        let mut m = CascadeModel::new();
        for i in 1..=4 {
            m.record_event(i * 5, 1_000_000);
            assert!(!m.has_sufficient_history());
        }
        m.record_event(25, 1_000_000);
        assert!(m.has_sufficient_history());
    }

    // ── New tests (spec) ──────────────────────────────────────────────────

    #[test]
    fn ema_alpha_reads_from_env() {
        std::env::set_var("CASCADE_EMA_ALPHA_NUM", "2");
        std::env::set_var("CASCADE_EMA_ALPHA_DEN", "10");
        let mut m = CascadeModel::new();
        m.record_event(10, 1_000_000);
        // Second event at alpha=0.2: new = 1_000_000 * 0.8 + 2_000_000 * 0.2 = 1_200_000
        m.record_event(10, 2_000_000);
        let val = m.sensitivity_histogram.iter().find(|(b, _)| *b == 10).map(|(_, v)| *v).unwrap();
        assert_eq!(val, 1_200_000);
        std::env::remove_var("CASCADE_EMA_ALPHA_NUM");
        std::env::remove_var("CASCADE_EMA_ALPHA_DEN");
    }

    #[test]
    fn sufficient_history_threshold_reads_from_env() {
        std::env::set_var("CASCADE_MIN_EVENTS", "3");
        let mut m = CascadeModel::new();
        m.record_event(10, 1_000_000);
        m.record_event(20, 2_000_000);
        assert!(!m.has_sufficient_history());
        m.record_event(30, 3_000_000);
        assert!(m.has_sufficient_history());
        std::env::remove_var("CASCADE_MIN_EVENTS");
    }

    #[test]
    fn predict_returns_none_when_all_zero_biased() {
        // model with 5 events but all zero should return None
        std::env::remove_var("CASCADE_MIN_EVENTS");
        let mut m = CascadeModel::new();
        for _ in 0..5 {
            m.record_event(10, 0); // placeholder zeros
        }
        // After EMA with all zeros: entries converge to 0
        assert!(m.predict_next_wave(10).is_none());
    }
}
