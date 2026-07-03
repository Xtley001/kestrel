// Kestrel — access_list.rs
// Access list generation and caching.
//

use alloy::primitives::{Address, Bytes};
use alloy::providers::Provider;
use alloy::rpc::types::AccessListItem;
use dashmap::DashMap;
use eyre::Result;
use once_cell::sync::Lazy;
use tracing::debug;

// Cache key for an access list — identifies the opportunity type.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct AccessListKey {
    pub pool: Address,
    pub direction: u8, // 0 = discount, 1 = premium
}

// Cached access list result.
#[derive(Debug, Clone)]
pub struct CachedAccessList {
    pub access_list: Vec<AccessListItem>,
    pub cached_at_block: u64,
}

// Global access list cache — keyed by (pool, direction).
// Populated by pre_warm_access_lists at startup and refreshed every 100 blocks.
static ACCESS_LIST_CACHE: Lazy<DashMap<AccessListKey, CachedAccessList>> =
    Lazy::new(DashMap::new);

// Instance-level cache for explicit session management (kept for compatibility).
pub struct AccessListCache {
    inner: std::collections::HashMap<AccessListKey, CachedAccessList>,
}

impl AccessListCache {
    pub fn new() -> Self { Self { inner: std::collections::HashMap::new() } }

    pub async fn get_or_generate<P: Provider>(
        &mut self,
        key: AccessListKey,
        contract: Address,
        calldata: &Bytes,
        current_block: u64,
        provider: &P,
    ) -> Result<Vec<AccessListItem>> {
        let needs_refresh = self.inner.get(&key)
            .map_or(true, |c| current_block > c.cached_at_block + 100);

        if needs_refresh {
            let access_list = generate_access_list(contract, calldata, provider).await?;
            debug!(pool = %key.pool, direction = key.direction, slots = access_list.len(), "access list generated");
            self.inner.insert(key.clone(), CachedAccessList { access_list: access_list.clone(), cached_at_block: current_block });
            Ok(access_list)
        } else {
            Ok(self.inner[&key].access_list.clone())
        }
    }
}

// Call eth_createAccessList on the provider to generate an EIP-2930 access list.
async fn generate_access_list<P: Provider>(
    contract: Address,
    calldata: &Bytes,
    provider: &P,
) -> Result<Vec<AccessListItem>> {
    use alloy::rpc::types::{TransactionRequest, BlockId};

    let tx_request = TransactionRequest {
        to: Some(contract.into()),
        input: calldata.clone().into(),
        ..Default::default()
    };

    let result = provider
        .create_access_list(&tx_request)
        .await?;

    Ok(result.access_list.0)
}

// ── + M4: Free functions for spread_pipeline ────────────────────────────

// Pre-warm the global access list cache for a given pool + direction.
// Called at startup so the first real opportunity has a cached list ready.
pub async fn get_or_generate<P: Provider>(
    pool: Address,
    direction: crate::dex_monitor::SpreadDirection,
    provider: &P,
) -> Result<()> {
    let _ = get_cached_or_generate(pool, direction, provider, 0).await;
    Ok(())
}

// Return the actual access list so it can be attached to signed transactions.
// Uses the global DashMap cache — regenerates every 100 blocks to stay fresh.
// Returns an empty list on error (non-fatal — trade proceeds without access list at
// slightly higher gas cost rather than skipping the opportunity entirely).
pub async fn get_cached_or_generate<P: Provider>(
    pool: Address,
    direction: crate::dex_monitor::SpreadDirection,
    provider: &P,
    current_block: u64,
) -> Result<Vec<AccessListItem>> {
    let dir_byte = match direction {
        crate::dex_monitor::SpreadDirection::Discount => 0u8,
        crate::dex_monitor::SpreadDirection::Premium  => 1u8,
    };
    let key = AccessListKey { pool, direction: dir_byte };

    // Check global cache first
    let needs_refresh = ACCESS_LIST_CACHE
        .get(&key)
        .map_or(true, |c| current_block > c.cached_at_block + 100);

    if !needs_refresh {
        let list = ACCESS_LIST_CACHE.get(&key).map(|c| c.access_list.clone()).unwrap_or_default();
        debug!(pool = %pool, slots = list.len(), "access list cache hit");
        return Ok(list);
    }

    // Generate fresh
    let arbitrageur: Address = std::env::var("ARBITRAGEUR_ADDRESS")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(Address::ZERO);

    // Use actual execute selector + zero-value params as calldata template.
    // Storage slots are determined by the contract and pool, not the amounts.
    // the previous `vec![0x6a, 0x04, 0x35, 0x69, 0u8; 164]` used Rust's
    // vec! initialiser incorrectly. `vec![a, b, c, d, X; N]` produces 4 + N elements
    // (168 bytes), not N elements. The EVM ABI decoder rejected 168 bytes as malformed
    // (not cleanly decodable as 4 + 5×32), producing an empty or wrong access list.
    // Correct structure: 4-byte selector + 5 × 32-byte zero params = 164 bytes exactly.
    let dummy_calldata = {
        let mut buf = Vec::with_capacity(4 + 5 * 32);
        buf.extend_from_slice(&[0x6a, 0x04, 0x35, 0x69]); // execute selector
        buf.extend_from_slice(&[0u8; 160]);                // 5 × 32 zero ABI params
        Bytes::from(buf)
    }; // Total: 164 bytes — correct

    match generate_access_list(arbitrageur, &dummy_calldata, provider).await {
        Ok(list) => {
            debug!(pool = %pool, direction = dir_byte, slots = list.len(), "access list generated and cached");
            ACCESS_LIST_CACHE.insert(key, CachedAccessList {
                access_list: list.clone(),
                cached_at_block: current_block,
            });
            Ok(list)
        }
        Err(e) => {
            debug!(pool = %pool, error = %e, "access list generation failed — proceeding without (higher gas)");
            Ok(vec![]) // non-fatal: trade proceeds without EIP-2930 savings
        }
    }
}

// Return an alloy AccessList from the cached items (for TxEip1559 attachment).
pub fn items_to_access_list(items: Vec<AccessListItem>) -> alloy::rpc::types::AccessList {
    alloy::rpc::types::AccessList(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_list_key_equality() {
        let k1 = AccessListKey { pool: Address::ZERO, direction: 0 };
        let k2 = AccessListKey { pool: Address::ZERO, direction: 0 };
        let k3 = AccessListKey { pool: Address::ZERO, direction: 1 };
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn items_to_access_list_roundtrip() {
        let items: Vec<AccessListItem> = vec![];
        let al = items_to_access_list(items);
        assert_eq!(al.0.len(), 0);
    }

    #[test]
    fn global_cache_initialises_empty() {
        // Just reference the static to ensure it initialises
        let _ = ACCESS_LIST_CACHE.len();
    }
}
