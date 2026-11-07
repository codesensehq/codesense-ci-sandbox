// SPDX-License-Identifier: MIT
//! The stable `<org>/<team>/<id>` key of a definition kept in a team's (or an
//! org's scratch space's) `config/` dir. Routines and agents both key on it.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::context_repo::scratch_space::{is_scratch_team, SCRATCH_TEAM};
use crate::domain::Principal;

/// The first segment of a rendered personal key. No team org may use it, so a
/// rendered key is a personal key exactly when it starts with it.
pub const PERSONAL_OWNER: &str = "_personal";

/// The org of a definition the app itself ships rather than any team or
/// personal space. The three single-segment reservations (`_personal`,
/// `builtin`, `local`) are refused as bare org slugs by `validate_org_slug`,
/// so a key under `builtin` can never name a team definition. It renders as
/// `builtin/<id>`.
pub const BUILTIN_ORG: &str = "builtin";

/// The `<org>/<team>/<id>` identity of one definition. Ids are only unique
/// within a team, so the org and team are part of the key. SW-CR-1: an org
/// slug is one or two `/`-separated segments, so the rendered key has three
/// or four `/`-separated segments (`<org>/<team>/<id>` or
/// `<owner>/<repo>/<team>/<id>`).
///
/// A personal definition lives in one org's scratch space: its org is that
/// real org and its team is [`SCRATCH_TEAM`]. State behind a key can live in a
/// store shared by every account on the machine, so a personal key also
/// carries its owner's numeric account id, and renders as
/// `_personal/<account>/<owner>/<repo>/_scratch/<id>`. A personal key never
/// resolves for anyone else.
///
/// It serializes as that rendered string, so a client never rebuilds a key
/// from its parts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopedKey {
    pub org: String,
    pub team: String,
    pub id: String,
    /// The owning account of a personal definition; `None` for a team one.
    pub account: Option<String>,
}

impl ScopedKey {
    /// A team definition's key.
    pub fn new(org: impl Into<String>, team: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            org: org.into(),
            team: team.into(),
            id: id.into(),
            account: None,
        }
    }

    /// The key of a definition the app ships: `builtin/<id>`. It has no team
    /// and resolves for everyone.
    pub fn builtin(id: impl Into<String>) -> Self {
        Self::new(BUILTIN_ORG, "", id)
    }

    /// Whether this names a definition the app ships.
    pub fn is_builtin(&self) -> bool {
        self.org == BUILTIN_ORG && self.team.is_empty() && self.account.is_none()
    }

    /// The key of `id` in `org/team` as `principal` sees it: a definition in
    /// an org's scratch space is the principal's own.
    pub fn scoped(
        principal: &Principal,
        org: impl Into<String>,
        team: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        let mut key = Self::new(org, team, id);
        if is_scratch_team(&key.team) {
            key.account = Some(principal.as_str().to_string());
        }
        key
    }

    /// The key of `id` in `principal`'s scratch space for `org`.
    pub fn personal(principal: &Principal, org: impl Into<String>, id: impl Into<String>) -> Self {
        Self::scoped(principal, org, SCRATCH_TEAM, id)
    }

    /// Whether `principal` can resolve this key: any team key, and only their
    /// own personal keys.
    pub fn resolves_for(&self, principal: &Principal) -> bool {
        match &self.account {
            Some(account) => is_scratch_team(&self.team) && account == principal.as_str(),
            None => !is_scratch_team(&self.team),
        }
    }

    pub fn is_personal(&self) -> bool {
        self.account.is_some()
    }

    /// Parse a rendered key. The last two segments are the team and id;
    /// whatever precedes them is the org slug, or a personal key's qualified
    /// org.
    pub fn parse(key: &str) -> Option<Self> {
        if let Some(id) = key
            .strip_prefix(BUILTIN_ORG)
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|id| !id.is_empty() && !id.contains('/'))
        {
            return Some(Self::builtin(id));
        }
        let mut parts = key.rsplitn(3, '/');
        let id = parts.next()?;
        let team = parts.next()?;
        let org = parts.next()?;
        if id.is_empty() || team.is_empty() || org.is_empty() {
            return None;
        }
        let personal = org
            .strip_prefix(PERSONAL_OWNER)
            .and_then(|rest| rest.strip_prefix('/'));
        match personal {
            Some(rest) => {
                let (account, org) = rest.split_once('/')?;
                let valid = !account.is_empty()
                    && account.bytes().all(|b| b.is_ascii_digit())
                    && !org.is_empty()
                    && is_scratch_team(team);
                valid.then(|| Self {
                    account: Some(account.to_string()),
                    ..Self::new(org, team, id)
                })
            }
            None if is_scratch_team(team) => None,
            None => Some(Self::new(org, team, id)),
        }
    }
}

impl Serialize for ScopedKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ScopedKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| serde::de::Error::custom(format!("not a key: {raw:?}")))
    }
}

impl fmt::Display for ScopedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.account {
            Some(account) => write!(
                f,
                "{PERSONAL_OWNER}/{account}/{}/{}/{}",
                self.org, self.team, self.id
            ),
            None if self.is_builtin() => write!(f, "{BUILTIN_ORG}/{}", self.id),
            None => write!(f, "{}/{}/{}", self.org, self.team, self.id),
        }
    }
}

impl std::str::FromStr for ScopedKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| format!("not a key: {s:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_personal_key_names_its_org_and_round_trips() {
        let me = Principal::from(7u64);
        let key = ScopedKey::personal(&me, "acme/ctx", "triage");
        assert_eq!(key.to_string(), "_personal/7/acme/ctx/_scratch/triage");
        assert_eq!(ScopedKey::parse(&key.to_string()), Some(key.clone()));
        assert!(key.is_personal());
        assert!(key.resolves_for(&me));
        assert!(!key.resolves_for(&Principal::from(8u64)));
        assert_ne!(
            key,
            ScopedKey::personal(&me, "other/ctx", "triage"),
            "the same id filed under two orgs is two keys"
        );
    }

    #[test]
    fn a_scratch_team_never_parses_without_its_account() {
        assert_eq!(ScopedKey::parse("acme/ctx/_scratch/triage"), None);
        assert_eq!(ScopedKey::parse("_personal/x/acme/ctx/_scratch/a"), None);
        assert_eq!(ScopedKey::parse("_personal/7/acme/ctx/default/a"), None);
        let team = ScopedKey::parse("acme/ctx/default/a").unwrap();
        assert!(!team.is_personal());
    }

    // SW-CR-1: a single-segment org slug round-trips through parse.
    #[test]
    fn a_single_segment_org_key_round_trips() {
        let key = ScopedKey::new("notes", "backend", "barb");
        assert_eq!(key.to_string(), "notes/backend/barb");
        assert_eq!(ScopedKey::parse(&key.to_string()), Some(key));
    }
}
