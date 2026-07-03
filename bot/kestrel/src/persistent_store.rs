// Kestrel — persistent_store.rs
// (feedback): Persistent State — SQLite-backed builder statistics and P&L ledger.
//
//
//         ProfitLedger logs every submission (pending), landing outcome (landed/reverted/
//         expired), and confirmed on-chain profit (from PROFIT_WALLET Transfer events).
//         The CLI summary function prints per-strategy and overall P&L for the operator.
//
// Database: SQLite via rusqlite (bundled). Default path: $STATE_DB_PATH or ./kestrel_state.db
// Tables:
//   builder_stats    — per-builder submission/landed/reverted counters (existing)
//   profit_ledger    — per-submission record with outcome and on-chain profit (NEW O4)

use rusqlite::{Connection, params, Result as SqlResult};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};

use crate::competitor_tracker::BuilderStats;

fn open_db() -> SqlResult<Connection> {
    let path = std::env::var("STATE_DB_PATH")
        .unwrap_or_else(|_| "./kestrel_state.db".to_string());
    let conn = Connection::open(&path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    ensure_schema(&conn)?;
    Ok(conn)
}

fn ensure_schema(conn: &Connection) -> SqlResult<()> {
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS builder_stats (
            endpoint             TEXT PRIMARY KEY,
            submitted            INTEGER NOT NULL DEFAULT 0,
            landed               INTEGER NOT NULL DEFAULT 0,
            landed_and_reverted  INTEGER NOT NULL DEFAULT 0,
            updated_at           INTEGER NOT NULL DEFAULT (strftime('%s','now'))
        );

        -- O4: ProfitLedger table
        -- Records every bundle submission and its outcome.
        -- outcome: 'pending' | 'landed' | 'reverted' | 'expired'
        -- on_chain_profit_usd: populated from PROFIT_WALLET Transfer event (M7 wiring needed)
        -- gas_spent_usd: gas_used * gas_price_gwei * 1e-9 * eth_price_usd
        CREATE TABLE IF NOT EXISTS profit_ledger (
            id                   INTEGER PRIMARY KEY AUTOINCREMENT,
            tx_hash              TEXT,
            strategy             TEXT NOT NULL,
            chain                TEXT NOT NULL,
            block_submitted      INTEGER NOT NULL,
            block_landed         INTEGER,
            spread_bps           INTEGER NOT NULL,
            size_usd             REAL NOT NULL,
            gross_profit_usd     REAL NOT NULL,
            gas_spent_usd        REAL NOT NULL,
            flash_fee_usd        REAL NOT NULL,
            net_profit_usd       REAL NOT NULL,
            priority_fee_gwei    REAL NOT NULL,
            outcome              TEXT NOT NULL DEFAULT 'pending',
            on_chain_profit_usd  REAL,
            created_at           INTEGER NOT NULL DEFAULT (strftime('%s','now'))
        );

        CREATE INDEX IF NOT EXISTS profit_ledger_strategy ON profit_ledger(strategy, outcome);
        CREATE INDEX IF NOT EXISTS profit_ledger_block     ON profit_ledger(block_submitted);
    ")?;
    Ok(())
}

// ── Builder stats (existing) ─────────────────────────────────────────────────

pub fn save_builder_stats(stats: &HashMap<String, BuilderStats>) {
    match open_db() {
        Err(e) => { warn!(error = %e, "persistent_store: failed to open DB for save"); }
        Ok(conn) => {
            for (endpoint, s) in stats {
                if let Err(e) = conn.execute(
                    "INSERT OR REPLACE INTO builder_stats
                     (endpoint, submitted, landed, landed_and_reverted, updated_at)
                     VALUES (?1, ?2, ?3, ?4, strftime('%s','now'))",
                    params![endpoint, s.submitted as i64, s.landed as i64, s.landed_and_reverted as i64],
                ) {
                    warn!(error = %e, endpoint, "persistent_store: failed to save builder stats");
                }
            }
            info!(count = stats.len(), "persistent_store: builder stats saved");
        }
    }
}

pub fn load_builder_stats() -> HashMap<String, BuilderStats> {
    let conn = match open_db() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "persistent_store: could not open DB");
            return HashMap::new();
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT endpoint, submitted, landed, landed_and_reverted FROM builder_stats"
    ) {
        Ok(s) => s,
        Err(e) => { warn!(error = %e, "persistent_store: query prepare failed"); return HashMap::new(); }
    };
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, BuilderStats {
            submitted:           row.get::<_, i64>(1)? as u64,
            landed:              row.get::<_, i64>(2)? as u64,
            landed_and_reverted: row.get::<_, i64>(3)? as u64,
        }))
    });
    match rows {
        Err(e) => { warn!(error = %e, "persistent_store: query failed"); HashMap::new() }
        Ok(iter) => {
            let result: HashMap<String, BuilderStats> = iter.filter_map(|r| r.ok()).collect();
            info!(count = result.len(), "persistent_store: loaded builder stats from disk");
            result
        }
    }
}

pub fn reset_builder_stats() {
    match open_db() {
        Err(e) => warn!(error = %e, "persistent_store: reset failed"),
        Ok(conn) => {
            if let Err(e) = conn.execute("DELETE FROM builder_stats", []) {
                warn!(error = %e, "persistent_store: DELETE failed");
            } else {
                info!("persistent_store: builder stats reset");
            }
        }
    }
}

// ── O4: Profit Ledger ─────────────────────────────────────────────────────────

// A submitted bundle entry in the profit ledger.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub strategy: String,
    pub chain: String,
    pub block_submitted: u64,
    pub spread_bps: u32,
    pub size_usd: f64,
    pub gross_profit_usd: f64,
    pub gas_spent_usd: f64,
    pub flash_fee_usd: f64,
    pub net_profit_usd: f64,
    pub priority_fee_gwei: f64,
}

// P&L summary for a strategy or overall.
#[derive(Debug, Default)]
pub struct PnlSummary {
    pub total_submitted: u64,
    pub total_landed: u64,
    pub total_reverted: u64,
    pub total_expired: u64,
    pub total_gross_profit_usd: f64,
    pub total_gas_spent_usd: f64,
    pub total_flash_fee_usd: f64,
    pub total_net_profit_usd: f64,
    pub total_confirmed_onchain_usd: f64,
}

impl PnlSummary {
    pub fn win_rate(&self) -> f64 {
        if self.total_submitted == 0 { 0.0 }
        else { self.total_landed as f64 / self.total_submitted as f64 }
    }
    // net_profit_usd is already fully net (after gas AND flash fee — see
    // spread_pipeline). Do NOT subtract gas/flash again; that double-counted them.
    // gas/flash totals remain as informational columns.
    pub fn net_pnl(&self) -> f64 {
        self.total_net_profit_usd
    }
}

// Record a new bundle submission (outcome = 'pending').
// Returns the row id for later update when the outcome is known.
pub fn ledger_record_submission(entry: &LedgerEntry) -> Option<i64> {
    let conn = match open_db() {
        Ok(c) => c,
        Err(e) => { warn!(error = %e, "ledger: failed to open DB for submission record"); return None; }
    };
    match conn.execute(
        "INSERT INTO profit_ledger
         (strategy, chain, block_submitted, spread_bps, size_usd,
          gross_profit_usd, gas_spent_usd, flash_fee_usd, net_profit_usd,
          priority_fee_gwei, outcome)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending')",
        params![
            entry.strategy, entry.chain,
            entry.block_submitted as i64, entry.spread_bps as i64,
            entry.size_usd, entry.gross_profit_usd, entry.gas_spent_usd,
            entry.flash_fee_usd, entry.net_profit_usd, entry.priority_fee_gwei,
        ],
    ) {
        Ok(_) => Some(conn.last_insert_rowid()),
        Err(e) => { warn!(error = %e, "ledger: INSERT failed"); None }
    }
}

// Update the outcome of a submitted bundle once the result is known.
// outcome: "landed" | "reverted" | "expired"
// on_chain_profit_usd: Some(x) if landed and profit confirmed from receipt.
// TODO wire this from actual on-chain ProfitSweep event receipts.
pub fn ledger_update_outcome(
    row_id: i64,
    tx_hash: &str,
    outcome: &str,
    block_landed: Option<u64>,
    on_chain_profit_usd: Option<f64>,
) {
    let conn = match open_db() {
        Ok(c) => c,
        Err(e) => { warn!(error = %e, "ledger: failed to open DB for outcome update"); return; }
    };
    if let Err(e) = conn.execute(
        "UPDATE profit_ledger
         SET outcome=?1, tx_hash=?2, block_landed=?3, on_chain_profit_usd=?4
         WHERE id=?5",
        params![
            outcome, tx_hash,
            block_landed.map(|b| b as i64),
            on_chain_profit_usd,
            row_id,
        ],
    ) {
        warn!(error = %e, row_id, "ledger: UPDATE outcome failed");
    }
}

// Return aggregated P&L summary for a given strategy (or all strategies if None).
pub fn ledger_pnl_summary(strategy: Option<&str>) -> PnlSummary {
    let conn = match open_db() {
        Ok(c) => c,
        Err(e) => { warn!(error = %e, "ledger: failed to open DB for PnL query"); return PnlSummary::default(); }
    };

    let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = match strategy {
        Some(s) => (
            "SELECT outcome,
                    COUNT(*) as cnt,
                    SUM(gross_profit_usd),
                    SUM(gas_spent_usd),
                    SUM(flash_fee_usd),
                    SUM(net_profit_usd),
                    COALESCE(SUM(on_chain_profit_usd),0)
             FROM profit_ledger WHERE strategy=?1 GROUP BY outcome".to_string(),
            vec![Box::new(s.to_string())],
        ),
        None => (
            "SELECT outcome,
                    COUNT(*) as cnt,
                    SUM(gross_profit_usd),
                    SUM(gas_spent_usd),
                    SUM(flash_fee_usd),
                    SUM(net_profit_usd),
                    COALESCE(SUM(on_chain_profit_usd),0)
             FROM profit_ledger GROUP BY outcome".to_string(),
            vec![],
        ),
    };

    let mut summary = PnlSummary::default();

    let result = conn.prepare(&sql).and_then(|mut stmt| {
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        stmt.query_map(params_refs.as_slice(), |row| {
            // propagate column read errors instead of silently returning 0.0.
            // unwrap_or(0.0) masked schema mismatches and row corruption — the PnL report
            // would show $0 revenue on all strategies with no indication of data loss.
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, f64>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, f64>(6)?,
            ))
        }).map(|rows| rows.filter_map(|r| {
            r.map_err(|e| {
                tracing::warn!(error = %e, "ledger: skipping unreadable row (schema mismatch or corruption)");
            }).ok()
        }).collect::<Vec<_>>())
    });

    match result {
        Err(e) => { warn!(error = %e, "ledger: PnL query failed"); }
        Ok(rows) => {
            for (outcome, cnt, gross, gas, flash, net, onchain) in rows {
                summary.total_submitted += cnt;
                summary.total_gross_profit_usd += gross;
                summary.total_gas_spent_usd += gas;
                summary.total_flash_fee_usd += flash;
                summary.total_net_profit_usd += net;
                summary.total_confirmed_onchain_usd += onchain;
                match outcome.as_str() {
                    "landed"   => summary.total_landed   = cnt,
                    "reverted" => summary.total_reverted = cnt,
                    "expired"  => summary.total_expired  = cnt,
                    _ => {}
                }
            }
        }
    }
    summary
}

// Print a human-readable P&L report to stdout (for operator CLI use).
pub fn print_pnl_report() {
    let overall = ledger_pnl_summary(None);
    info!(
        "═══════════════ KESTREL P&L REPORT ═══════════════\n\
         Submitted : {:>8} | Landed: {:>6} | Reverted: {:>5} | Expired: {:>5}\n\
         Win rate  : {:>7.1}%\n\
         Gross P&L : ${:>12.2}\n\
         Gas spent : ${:>12.2}\n\
         Flash fees: ${:>12.2}\n\
         Net P&L   : ${:>12.2}  (sim-based; wire on-chain receipts for M7 accuracy)\n\
         Confirmed : ${:>12.2}  (on-chain — 0 until M7 receipt wiring complete)\n\
         ════════════════════════════════════════════════════",
        overall.total_submitted, overall.total_landed, overall.total_reverted, overall.total_expired,
        overall.win_rate() * 100.0,
        overall.total_gross_profit_usd,
        overall.total_gas_spent_usd,
        overall.total_flash_fee_usd,
        overall.net_pnl(),
        overall.total_confirmed_onchain_usd,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests mutate the global STATE_DB_PATH env var and hit a shared DB, so they
    // must not run concurrently. A mutex serialises them and each gets a unique file.
    static DB_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static DB_TEST_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn with_temp_db<F: FnOnce()>(f: F) {
        let _guard = DB_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let seq = DB_TEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = std::env::temp_dir()
            .join(format!("kestrel_test_{}_{}.db", std::process::id(), seq))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_file(&path);
        std::env::set_var("STATE_DB_PATH", &path);
        f();
        let _ = std::fs::remove_file(&path);
        std::env::remove_var("STATE_DB_PATH");
    }

    fn sample_entry(strategy: &str) -> LedgerEntry {
        LedgerEntry {
            strategy: strategy.to_string(), chain: "ethereum".to_string(),
            block_submitted: 20_000_000, spread_bps: 8, size_usd: 5_000_000.0,
            gross_profit_usd: 4_000.0, gas_spent_usd: 36.0, flash_fee_usd: 0.0,
            net_profit_usd: 3_964.0, priority_fee_gwei: 2.5,
        }
    }

    #[test]
    fn builder_stats_save_load_roundtrip() {
        with_temp_db(|| {
            let mut stats = HashMap::new();
            stats.insert("https://relay.flashbots.net".to_string(), BuilderStats {
                submitted: 100, landed: 80, landed_and_reverted: 5,
            });
            save_builder_stats(&stats);
            let loaded = load_builder_stats();
            assert_eq!(loaded.len(), 1);
            let fb = loaded.get("https://relay.flashbots.net").unwrap();
            assert_eq!(fb.submitted, 100);
            assert_eq!(fb.landed, 80);
            assert_eq!(fb.landed_and_reverted, 5);
        });
    }

    #[test]
    fn builder_stats_overwrite_existing() {
        with_temp_db(|| {
            let mut stats = HashMap::new();
            stats.insert("titan".to_string(), BuilderStats { submitted: 10, landed: 8, landed_and_reverted: 0 });
            save_builder_stats(&stats);
            stats.insert("titan".to_string(), BuilderStats { submitted: 20, landed: 18, landed_and_reverted: 1 });
            save_builder_stats(&stats);
            let loaded = load_builder_stats();
            let t = loaded.get("titan").unwrap();
            assert_eq!(t.submitted, 20);
            assert_eq!(t.landed_and_reverted, 1);
        });
    }

    #[test]
    fn reset_builder_stats_clears_rows() {
        with_temp_db(|| {
            let mut stats = HashMap::new();
            stats.insert("x".to_string(), BuilderStats { submitted: 5, landed: 4, landed_and_reverted: 0 });
            save_builder_stats(&stats);
            reset_builder_stats();
            assert!(load_builder_stats().is_empty());
        });
    }

    // O4 tests

    #[test]
    fn ledger_submission_inserts_pending_row() {
        with_temp_db(|| {
            let id = ledger_record_submission(&sample_entry("eth_susds"));
            assert!(id.is_some());
        });
    }

    #[test]
    fn ledger_update_outcome_landed() {
        with_temp_db(|| {
            let id = ledger_record_submission(&sample_entry("eth_susds")).unwrap();
            ledger_update_outcome(id, "0xabc", "landed", Some(20_000_001), Some(3_800.0));
            let summary = ledger_pnl_summary(Some("eth_susds"));
            assert_eq!(summary.total_landed, 1);
            assert!((summary.total_confirmed_onchain_usd - 3_800.0).abs() < 0.01);
        });
    }

    #[test]
    fn ledger_pnl_summary_aggregates_correctly() {
        with_temp_db(|| {
            let id1 = ledger_record_submission(&sample_entry("eth_susds")).unwrap();
            let id2 = ledger_record_submission(&sample_entry("eth_susds")).unwrap();
            ledger_update_outcome(id1, "0x1", "landed",   Some(1), Some(3_900.0));
            ledger_update_outcome(id2, "0x2", "reverted", None,    None);
            let summary = ledger_pnl_summary(Some("eth_susds"));
            assert_eq!(summary.total_landed,   1);
            assert_eq!(summary.total_reverted, 1);
            assert_eq!(summary.total_submitted, 2);
            assert!((summary.win_rate() - 0.5).abs() < 0.01);
        });
    }

    #[test]
    fn ledger_pnl_all_strategies() {
        with_temp_db(|| {
            let id1 = ledger_record_submission(&sample_entry("eth_susds")).unwrap();
            let id2 = ledger_record_submission(&sample_entry("eth_sdai")).unwrap();
            ledger_update_outcome(id1, "0x1", "landed",  Some(1), None);
            ledger_update_outcome(id2, "0x2", "expired", None,    None);
            let summary = ledger_pnl_summary(None);
            assert_eq!(summary.total_submitted, 2);
            assert_eq!(summary.total_landed,    1);
            assert_eq!(summary.total_expired,   1);
        });
    }

    #[test]
    fn win_rate_zero_when_no_submissions() {
        let s = PnlSummary::default();
        assert_eq!(s.win_rate(), 0.0);
    }

    #[test]
    fn net_pnl_is_the_stored_fully_net_total() {
        // net_profit_usd is already net of gas + flash; net_pnl returns it as-is.
        let s = PnlSummary {
            total_net_profit_usd: 3_839.0,
            total_gas_spent_usd: 36.0,
            total_flash_fee_usd: 125.0,
            ..Default::default()
        };
        assert!((s.net_pnl() - 3_839.0).abs() < 0.01);
    }
}
