// Kestrel — nonce_manager.rs
// Nonce Management and Stuck Transaction Recovery.
//
// Maintains a local nonce counter with atomic increment — never fetches eth_getTransactionCount
// per-block. If a pending tx is unconfirmed for >2 blocks, sends a replacement at +15% priority
// fee to clear the nonce slot and unblock the pipeline.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

pub struct NonceManager {
    // Local nonce — incremented atomically on each submission.
    current_nonce: Arc<AtomicU64>,
    // Block number at which the last unconfirmed tx was sent. None = no pending tx.
    pending_since_block: Arc<std::sync::Mutex<Option<u64>>>,
    // The nonce of the currently pending tx.
    pending_nonce: Arc<AtomicU64>,
}

impl NonceManager {
    // Initialise from the current on-chain nonce (fetch once at startup).
    pub fn new(initial_nonce: u64) -> Self {
        Self {
            current_nonce: Arc::new(AtomicU64::new(initial_nonce)),
            pending_since_block: Arc::new(std::sync::Mutex::new(None)),
            pending_nonce: Arc::new(AtomicU64::new(0)),
        }
    }

    // Get the next nonce and increment the local counter atomically.
    // Never calls eth_getTransactionCount — uses the local counter exclusively.
    pub fn next_nonce(&self) -> u64 {
        self.current_nonce.fetch_add(1, Ordering::SeqCst)
    }

    // Record that a transaction was submitted at this nonce and block.
    pub fn record_submission(&self, nonce: u64, block: u64) {
        self.pending_nonce.store(nonce, Ordering::SeqCst);
        if let Ok(mut guard) = self.pending_since_block.lock() {
            *guard = Some(block);
        }
    }

    // Call every block. If a pending tx is older than 2 blocks, trigger replacement.
    // Returns Some(stuck_nonce) if a replacement should be sent; None otherwise.
    pub fn maybe_stuck(&self, current_block: u64) -> Option<u64> {
        let guard = self.pending_since_block.lock().ok()?;
        if let Some(since) = *guard {
            if current_block.saturating_sub(since) > 2 {
                let nonce = self.pending_nonce.load(Ordering::SeqCst);
                warn!(
                    nonce,
                    stuck_since = since,
                    current_block,
                    "tx stuck for >2 blocks — replacement needed"
                );
                return Some(nonce);
            }
        }
        None
    }

    // Mark the pending tx as confirmed — clear the pending state.
    pub fn confirm(&self) {
        if let Ok(mut guard) = self.pending_since_block.lock() {
            *guard = None;
            info!("pending tx confirmed — nonce slot cleared");
        }
    }

    pub fn current(&self) -> u64 {
        self.current_nonce.load(Ordering::SeqCst)
    }
}

// Compute replacement gas price: +15% over original, clamped to MAX_PRIORITY_FEE_GWEI.
pub fn replacement_priority_fee_gwei(original_gwei: f64, max_gwei: f64) -> f64 {
    let bumped = original_gwei * 1.15;
    bumped.min(max_gwei)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_increments_atomically() {
        let nm = NonceManager::new(10);
        assert_eq!(nm.next_nonce(), 10);
        assert_eq!(nm.next_nonce(), 11);
        assert_eq!(nm.next_nonce(), 12);
        assert_eq!(nm.current(), 13);
    }

    #[test]
    fn no_stuck_when_no_pending() {
        let nm = NonceManager::new(0);
        assert!(nm.maybe_stuck(100).is_none());
    }

    #[test]
    fn no_stuck_when_pending_is_recent() {
        let nm = NonceManager::new(0);
        nm.record_submission(0, 100);
        assert!(nm.maybe_stuck(101).is_none()); // only 1 block old
    }

    #[test]
    fn stuck_detected_after_2_blocks() {
        let nm = NonceManager::new(5);
        nm.record_submission(5, 100);
        let stuck = nm.maybe_stuck(103); // 3 blocks later
        assert_eq!(stuck, Some(5));
    }

    #[test]
    fn confirm_clears_pending_state() {
        let nm = NonceManager::new(5);
        nm.record_submission(5, 100);
        nm.confirm();
        assert!(nm.maybe_stuck(110).is_none());
    }

    #[test]
    fn replacement_fee_bumped_15_percent() {
        let bumped = replacement_priority_fee_gwei(2.0, 10.0);
        assert!((bumped - 2.3).abs() < 0.01);
    }

    #[test]
    fn replacement_fee_clamped_to_max() {
        let bumped = replacement_priority_fee_gwei(9.0, 10.0);
        assert!(bumped <= 10.0);
    }
}
