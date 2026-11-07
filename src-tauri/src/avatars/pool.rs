// SPDX-License-Identifier: MIT
//! Dealing a face to an agent that has none: the same pick the agents list makes.
//!
//! The agents list (`resolveAgentFace` in `agent-list-view.ts`) deals such an agent a
//! face from the team's default pack, else the built-in funny one: FNV-1a of the agent's
//! key (else its name, else its handle), over UTF-16 code units, modulo the pool size. This
//! is that pick, and `pool-parity.json` is checked by a test on each side.

/// FNV-1a over the UTF-16 code units of `key` (as `charCodeAt` reads them).
pub fn hash_key(key: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in key.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The key the in-app Inbox deals a sender with no session by: its display name, trimmed,
/// else its handle, trimmed, else `?` (`inboxSenderIdentity` in `inbox-sender-identity.ts`).
pub fn inbox_key<'a>(from: &'a str, from_name: &'a str) -> &'a str {
    [from_name.trim(), from.trim()]
        .into_iter()
        .find(|key| !key.is_empty())
        .unwrap_or("?")
}

/// The face `key` is dealt from `pool`, or `None` for an empty pool.
pub fn deal<'a>(pool: &'a [String], key: &str) -> Option<&'a str> {
    if pool.is_empty() {
        return None;
    }
    Some(pool[hash_key(key) as usize % pool.len()].as_str())
}

/// The refs of every face of the shipped pack `id`, in its manifest order.
pub fn builtin_refs(id: &str) -> Vec<String> {
    super::builtin::packs()
        .into_iter()
        .find(|pack| pack.id == id)
        .map(|pack| {
            pack.faces
                .iter()
                .map(|face| super::face_ref(pack.id, &face.id))
                .collect()
        })
        .unwrap_or_default()
}

/// The pack agents are dealt from when the team set none.
pub const FALLBACK_AVATAR_SET: &str = "builtin-funny";

#[cfg(test)]
mod tests {
    use super::*;

    /// One fixture, asserted here and by `avatar-pool-parity.test.ts` against the TS resolver.
    const PARITY: &str = include_str!("pool-parity.json");

    #[test]
    fn the_deal_matches_the_agents_list_for_the_shared_fixture() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(PARITY).unwrap();
        assert!(cases.len() >= 12);
        let pool = builtin_refs(FALLBACK_AVATAR_SET);
        for case in &cases {
            let text = |k: &str| case[k].as_str();
            let key = if case["viaInbox"] == true {
                inbox_key(text("handle").unwrap(), text("displayName").unwrap_or(""))
            } else {
                text("agentKey")
                    .or(text("displayName"))
                    .or(text("handle"))
                    .unwrap()
            };
            assert_eq!(
                deal(&pool, key),
                text("expect"),
                "{}",
                serde_json::to_string(case).unwrap()
            );
        }
    }

    #[test]
    fn an_empty_pool_deals_nothing() {
        assert_eq!(deal(&[], "anyone"), None);
    }
}
