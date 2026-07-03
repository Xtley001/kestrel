// Kestrel — controls.rs
// Shared runtime control state, driven by signed dashboard commands (metrics_ws.rs)
// and read by the trading pipeline (spread_pipeline.rs).
//
// previously the control WebSocket verified command signatures and then did
// nothing ("// Route command to appropriate bot function" was a no-op). These atomics
// give verified commands real effect without threading channels through every pipeline.

use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;

// Master submission switch. Seeded from SUBMISSION_ENABLED at startup; can be
// toggled live from the dashboard. The pipeline checks this every block.
static SUBMISSION_ENABLED: AtomicBool = AtomicBool::new(false);
// Emergency pause. When true, submission is halted regardless of SUBMISSION_ENABLED.
static PAUSED: AtomicBool = AtomicBool::new(false);

// Initialise from environment at startup (call once from main).
pub fn init_from_env() {
    let enabled = std::env::var("SUBMISSION_ENABLED").map(|v| v == "true").unwrap_or(false);
    SUBMISSION_ENABLED.store(enabled, Ordering::SeqCst);
    PAUSED.store(false, Ordering::SeqCst);
    info!(submission_enabled = enabled, "runtime controls initialised");
}

// True only when submission is enabled AND not paused.
pub fn submission_enabled() -> bool {
    SUBMISSION_ENABLED.load(Ordering::Relaxed) && !PAUSED.load(Ordering::Relaxed)
}

pub fn is_paused() -> bool {
    PAUSED.load(Ordering::Relaxed)
}

pub fn set_submission_enabled(enabled: bool) {
    SUBMISSION_ENABLED.store(enabled, Ordering::SeqCst);
    info!(enabled, "runtime control: submission toggled via dashboard");
}

pub fn set_paused(paused: bool) {
    PAUSED.store(paused, Ordering::SeqCst);
    info!(paused, "runtime control: pause toggled via dashboard");
}

// Apply a verified control command. Returns true if the command was recognised.
pub fn apply_command(command: &str, params: &serde_json::Value) -> bool {
    match command {
        "pause"  => { set_paused(true);  true }
        "resume" => { set_paused(false); true }
        "set_submission" => {
            let on = params.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            set_submission_enabled(on);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_blocks_submission() {
        set_submission_enabled(true);
        set_paused(false);
        assert!(submission_enabled());
        set_paused(true);
        assert!(!submission_enabled());
        set_paused(false);
    }

    #[test]
    fn apply_command_recognises_known_commands() {
        assert!(apply_command("pause", &serde_json::Value::Null));
        assert!(apply_command("resume", &serde_json::Value::Null));
        assert!(apply_command("set_submission", &serde_json::json!({"enabled": true})));
        assert!(!apply_command("unknown", &serde_json::Value::Null));
    }
}
