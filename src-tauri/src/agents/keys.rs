// SPDX-License-Identifier: MIT
//! The stable identity of one agent definition.

use crate::context_repo::scoped_key::ScopedKey;

/// The `<org>/<team>/<id>` identity of one agent; see [`ScopedKey`]. It
/// renders exactly like a routine key, so a personal agent's key is
/// `_personal/<account>/<owner>/<repo>/_scratch/<id>` and never resolves for
/// anyone else.
pub type AgentKey = ScopedKey;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Principal;

    #[test]
    fn keys_render_parse_and_resolve_like_routine_keys() {
        let team = AgentKey::new("acme/api", "backend", "triage");
        assert_eq!(team.to_string(), "acme/api/backend/triage");
        assert_eq!(
            AgentKey::parse("acme/api/backend/triage"),
            Some(team.clone())
        );

        let alice = Principal::from(1u64);
        let bob = Principal::from(2u64);
        let mine = AgentKey::personal(&alice, "acme/api", "triage");
        assert_eq!(mine.to_string(), "_personal/1/acme/api/_scratch/triage");
        assert_eq!(AgentKey::parse(&mine.to_string()), Some(mine.clone()));
        assert!(mine.resolves_for(&alice));
        assert!(!mine.resolves_for(&bob));
        assert!(team.resolves_for(&bob));

        assert_eq!(
            AgentKey::parse("_personal/x/acme/api/_scratch/triage"),
            None
        );
        assert_eq!(AgentKey::parse("backend/triage"), None);
        let json = serde_json::to_string(&mine).unwrap();
        assert_eq!(json, "\"_personal/1/acme/api/_scratch/triage\"");
        assert_eq!(serde_json::from_str::<AgentKey>(&json).unwrap(), mine);
    }
}
