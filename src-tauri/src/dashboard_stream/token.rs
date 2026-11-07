// SPDX-License-Identifier: MIT
//! SW-DX-3 AC-12 — the scoped, short-lived read-capability token store. Each
//! dashboard render mints a fresh token bound to `{ content_root, expires_at }`
//! for the read-only HTTP routes, so it must NOT be the process-wide
//! `ProxyCapabilityState` secret (scopeless, never expires): a dropped or cloned
//! dashboard's token expires independently.
//!
//! We reuse the random-token + constant-time-compare PRIMITIVES from
//! `http::ProxyCapabilityState` (not a parallel crypto), and add the scope/TTL
//! store on top.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default token lifetime. A dashboard render mints one; it self-expires so a
/// leaked credential is only briefly useful. Short but comfortably longer than a
/// dashboard session.
pub const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(60 * 30);

/// What a valid token grants: the one content root a read route may read.
#[derive(Debug, Clone)]
pub struct ScopedToken {
    pub content_root: String,
}

struct Entry<G> {
    grant: G,
    expires_at: Instant,
}

/// A mint-and-validate store for scoped read capability tokens. Cheap `Mutex<Map>`
/// — mint/validate are rare (once per dashboard render / connect), not hot-path.
pub struct ScopedCapabilityStore<G = ScopedToken> {
    inner: Mutex<HashMap<String, Entry<G>>>,
    ttl: Duration,
}

impl<G: Clone> ScopedCapabilityStore<G> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Mint a fresh 256-bit random token bound to `grant`, valid for the store's
    /// TTL. Returns the credential for an authenticated transport. Opportunistically
    /// evicts already-expired entries so the map can't grow unbounded.
    pub fn mint(&self, grant: G) -> String {
        let token = crate::http::random_hex_secret();
        let expires_at = Instant::now() + self.ttl;
        if let Ok(mut map) = self.inner.lock() {
            let now = Instant::now();
            map.retain(|_, e| e.expires_at > now);
            map.insert(token.clone(), Entry { grant, expires_at });
        }
        token
    }

    /// Validate `presented` in constant time against each live entry, returning
    /// the grant iff a match is found and unexpired. Absent / expired / unknown
    /// → `None` (the read routes map that to 401). Expired matches are evicted.
    pub fn validate(&self, presented: &str) -> Option<G> {
        let mut map = self.inner.lock().ok()?;
        let now = Instant::now();
        map.retain(|_, e| e.expires_at > now);
        // Constant-time compare against each candidate key so a timing side
        // channel can't probe token bytes. The map is tiny (live renders only).
        let mut found: Option<G> = None;
        for (key, entry) in map.iter() {
            if crate::http::constant_time_eq(key.as_bytes(), presented.as_bytes()) {
                found = Some(entry.grant.clone());
            }
        }
        found
    }

    pub fn revoke(&self, token: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(token);
        }
    }

    /// Live (unexpired) token count.
    #[cfg(test)]
    pub fn live_count(&self) -> usize {
        let Ok(mut map) = self.inner.lock() else {
            return 0;
        };
        let now = Instant::now();
        map.retain(|_, e| e.expires_at > now);
        map.len()
    }
}

impl<G: Clone> Default for ScopedCapabilityStore<G> {
    fn default() -> Self {
        Self::new(DEFAULT_TOKEN_TTL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> ScopedToken {
        ScopedToken {
            content_root: "/home/users/1/orgs/acme/api/default/content".to_string(),
        }
    }

    #[test]
    fn mint_then_validate_roundtrips() {
        let store = ScopedCapabilityStore::default();
        let tok = store.mint(grant());
        let got = store
            .validate(&tok)
            .expect("valid token resolves its grant");
        assert_eq!(got.content_root, grant().content_root);
    }

    #[test]
    fn absent_or_wrong_token_rejected() {
        let store = ScopedCapabilityStore::default();
        let _ = store.mint(grant());
        assert!(store.validate("").is_none(), "empty → None (401)");
        assert!(store.validate("deadbeef").is_none(), "unknown → None (401)");
    }

    #[test]
    fn expired_token_rejected_and_evicted() {
        // Zero TTL → every mint is already expired on validate.
        let store = ScopedCapabilityStore::new(Duration::from_millis(0));
        let tok = store.mint(grant());
        std::thread::sleep(Duration::from_millis(5));
        assert!(store.validate(&tok).is_none(), "expired → None (401)");
        assert_eq!(store.live_count(), 0, "expired entry evicted");
    }

    #[test]
    fn distinct_mints_get_distinct_tokens() {
        let store = ScopedCapabilityStore::default();
        let a = store.mint(grant());
        let b = store.mint(grant());
        assert_ne!(a, b, "each render gets a fresh token");
        assert_eq!(store.live_count(), 2);
    }
}
