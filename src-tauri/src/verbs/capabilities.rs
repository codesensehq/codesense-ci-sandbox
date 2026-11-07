// SPDX-License-Identifier: MIT
/// The grantable capability vocabulary. Peer capabilities remain reserved.
pub const ALL: &[&str] = &[
    "sessions:read",
    "sessions:send",
    "sessions:stop",
    "launch",
    "inbox:read:own",
    "inbox:read:team",
    "inbox:read:all-teams",
    "inbox:write",
    "records:read:own",
    "records:write:own",
    "records:read:team",
    "records:write:team",
    "context:read",
    "context:write",
    "files:read",
    "files:write",
    "routines:read",
    "routines:run",
    "routines:manage",
    "repo:read",
    "repo:write",
    "machines:read",
    "machines:manage",
    "accounts:manage",
    "dashboards:install:private",
    "dashboards:publish:team",
    "terminal:view",
    "terminal:control",
    "commands:run",
    "admin",
];

// Policy: team-safe keeps private and cross-team reach out of team content.
pub const TEAM_EXCLUDED: &[&str] = &[
    "inbox:read:all-teams",
    "records:read:own",
    "records:write:own",
    "accounts:manage",
    "admin",
    "machines:manage",
    "dashboards:install:private",
    "terminal:control",
];
// Policy: messages retain reads and inbox metadata, without other act capabilities.
pub const MESSAGE_EXCLUDED: &[&str] = &[
    "records:write:team",
    "sessions:send",
    "sessions:stop",
    "launch",
    "routines:run",
    "routines:manage",
    "files:write",
    "repo:write",
    "context:write",
];
// Policy: the right pane has full reach; the context-following dock has team reach.
pub const SLOTS: &[&str] = &["right-pane", "dock", "file-nav"];

pub fn known(capability: &str) -> bool {
    ALL.contains(&capability)
}
pub fn full() -> Vec<String> {
    ALL.iter().map(|s| (*s).to_owned()).collect()
}
pub fn scope_allows(scope: &str, capability: &str) -> bool {
    known(capability)
        && (!matches!(scope, "team" | "message") || !TEAM_EXCLUDED.contains(&capability))
        && (scope != "message" || !MESSAGE_EXCLUDED.contains(&capability))
}
pub fn slot_allows(slot: &str, capability: &str) -> bool {
    known(capability)
        && (slot != "dock" || scope_allows("team", capability))
        && records_allows(slot, "private", capability)
}

/// The private records pair stays inside private panes under team-safe.
pub(crate) fn records_allows(slot: &str, scope: &str, capability: &str) -> bool {
    !matches!(capability, "records:read:own" | "records:write:own")
        || (!matches!(slot, "centre" | "dock" | "message") && !matches!(scope, "team" | "message"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_excludes_reserved_peer_capabilities() {
        assert_eq!(ALL.len(), 30);
        assert!(known("inbox:read:all-teams"));
        assert!(!known("inbox:nuke"));
        assert!(!known("peer:query"));
    }

    #[test]
    fn policy_ceilings_keep_private_and_act_reach_out_of_team_and_message() {
        assert!(!scope_allows("team", "records:read:own"));
        assert!(!scope_allows("team", "terminal:control"));
        assert!(scope_allows("team", "launch"));
        assert!(!scope_allows("message", "launch"));
        assert!(scope_allows("message", "inbox:write"));
        assert!(scope_allows("private", "admin"));
        assert!(!slot_allows("dock", "inbox:read:all-teams"));
        assert!(slot_allows("right-pane", "inbox:read:all-teams"));
    }
}
