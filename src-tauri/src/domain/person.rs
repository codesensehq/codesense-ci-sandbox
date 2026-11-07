// SPDX-License-Identifier: MIT
use std::fmt;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use super::raw_yaml::GITHUB_PROVIDER;

/// SW-FG-4b (AC-3): the roster-plane person key. The domain echo of FG-1's
/// identity-plane [`Principal`](super::Principal), kept a **distinct type** — the
/// roster (`people.yml`) plane must never carry a machine-local Principal — but,
/// like `Principal`, it wraps a plain `String` so no consumer is welded to a
/// provider `u64`. In v1 its value is the GitHub `provider_user_id` rendered to
/// its decimal string (byte-identical to what FG-4 stores on disk today, so there
/// is nothing to migrate); a truly forge-independent minted id waits for the
/// storage FG-13 introduces.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PersonId(String);

impl PersonId {
    /// Borrow the underlying key string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the owned key string.
    pub fn into_string(self) -> String {
        self.0
    }
}

/// The GitHub bridge: a numeric account id renders to the same decimal string that
/// lands on disk today, so `PersonId::from(id)` is disk-identical for existing
/// rosters — mirroring `Principal::from(u64)`. Provider-independent people use
/// a minted value persisted in `people.yml`.
impl From<u64> for PersonId {
    fn from(github_user_id: u64) -> Self {
        PersonId(github_user_id.to_string())
    }
}

impl From<String> for PersonId {
    fn from(value: String) -> Self {
        PersonId(value)
    }
}

impl From<&str> for PersonId {
    fn from(value: &str) -> Self {
        PersonId(value.to_string())
    }
}

impl fmt::Display for PersonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// SW-FG-4b (AC-1): one provider identity on a domain [`Person`]. The domain twin
/// of [`RawIdentity`](super::raw_yaml::RawIdentity) — a **separate** type, so the
/// raw/domain boundary stays intact. A person carries a LIST of these, mirroring
/// `RawPersonEntry`, so one human can hold a GitHub identity AND (once FG-13 lands)
/// a GitLab one without a single→list migration. Both `handle` and
/// `provider_user_id` are optional: a UI-added GitHub person with no handle yet is
/// an identity row with a null handle (AC-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_user_id: Option<String>,
}

/// A validated roster person, keyed by [`PersonId`] and carrying a list of provider
/// [`Identity`] rows. The GitHub-specific accessors
/// ([`github_username`](Person::github_username) /
/// [`github_user_id`](Person::github_user_id)) are **derived** from the GitHub
/// identity rather than stored fields.
///
/// Deserialization reads the identity list (the source of truth); serialization
/// additionally emits a derived `github_user_id` / `github_username` mirror (see
/// the manual [`Serialize`] impl) so the frontend `Person` JSON shape stays
/// compatible while the FE reshape defers to FG-4c.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Person {
    pub id: PersonId,
    #[serde(default)]
    pub identities: Vec<Identity>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

impl Person {
    /// Compose from a pre-validated identity list. Infallible by design: the gate
    /// (a GitHub identity yielding a usable, non-zero numeric id) lives in
    /// [`RawPersonEntry::into_domain`](super::raw_yaml::RawPersonEntry::into_domain),
    /// which is the only production caller — a non-composable row is skipped there
    /// before it ever reaches this constructor.
    pub fn from_identities(
        id: PersonId,
        identities: Vec<Identity>,
        display_name: impl Into<String>,
        avatar_url: Option<String>,
    ) -> Self {
        Self {
            id,
            identities,
            display_name: display_name.into(),
            avatar_url,
        }
    }

    /// The GitHub identity row, if this person carries one (first match wins). The
    /// single accessor the GitHub-derived methods and the serialization mirror read
    /// — mirrors `RawPersonEntry::github_identity`.
    pub fn github_identity(&self) -> Option<&Identity> {
        self.identities
            .iter()
            .find(|i| i.provider == GITHUB_PROVIDER)
    }

    /// The GitHub handle (login), if present. `None` for a handle-less (AC-6) or
    /// (once FG-13 lands) a GitHub-less person.
    pub fn github_username(&self) -> Option<&str> {
        self.github_identity().and_then(|i| i.handle.as_deref())
    }

    /// The GitHub numeric id, parsed from the GitHub identity's `provider_user_id`.
    /// Present for every person the v1 domain composes (the compose gate requires a
    /// usable GitHub id).
    pub fn github_user_id(&self) -> Option<u64> {
        self.github_identity()
            .and_then(|i| i.provider_user_id.as_deref())
            .and_then(|s| s.parse::<u64>().ok())
    }

    /// A non-empty display label: the `display_name` when set, else the GitHub
    /// handle, else the `PersonId` string (the AC-6 handle-less-and-nameless
    /// fallback — a person is always labelable).
    pub fn display_name_or_username(&self) -> &str {
        if !self.display_name.trim().is_empty() {
            &self.display_name
        } else if let Some(handle) = self.github_username() {
            handle
        } else {
            self.id.as_str()
        }
    }
}

/// SW-FG-4b (AC-5): serialize the identity list PLUS a derived `github_user_id` /
/// `github_username` mirror so the frontend `Person` JSON keeps its existing shape
/// (the FE reshape — identities on the TS twin, provider-scoped lookup — is FG-4c).
/// The mirror is serialize-only: [`Person`]'s derived `Deserialize` reads the
/// identity list and ignores the mirror keys, the same one-way idiom as
/// `Org.org_root`'s `skip_deserializing`.
///
/// `github_username` is `null` for a handle-less person (the FE types it
/// `string | null`). `github_user_id` is **invariantly present in v1**: the sole
/// production constructor is [`RawPersonEntry::into_domain`], whose compose gate
/// requires a usable (non-zero, numeric) GitHub id — so `github_user_id()` is
/// always `Some`, matching the FE's non-null `number`. FG-13 (GitHub-less people)
/// breaks that invariant and MUST widen the FE contract to `number | null` before
/// composing a person with no GitHub identity.
impl Serialize for Person {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Person", 6)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("identities", &self.identities)?;
        state.serialize_field("display_name", &self.display_name)?;
        state.serialize_field("avatar_url", &self.avatar_url)?;
        // Derived FE-compatibility mirror.
        state.serialize_field("github_user_id", &self.github_user_id())?;
        state.serialize_field("github_username", &self.github_username())?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a single-identity GitHub person the way `into_domain` does.
    fn github_person(id: u64, handle: &str, display_name: &str) -> Person {
        Person::from_identities(
            PersonId::from(id),
            vec![Identity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: Some(handle.to_string()),
                provider_user_id: Some(id.to_string()),
            }],
            display_name,
            None,
        )
    }

    #[test]
    fn github_accessors_resolve_the_github_identity() {
        let p = github_person(42, "octocat", "Octo P");
        assert_eq!(p.id, PersonId::from(42u64));
        assert_eq!(p.github_user_id(), Some(42));
        assert_eq!(p.github_username(), Some("octocat"));
        assert_eq!(p.display_name, "Octo P");
        assert_eq!(p.avatar_url, None);
    }

    #[test]
    fn multi_provider_person_keeps_both_identities_github_derives_win() {
        // AC-1: a GitHub + GitLab person carries both identities; the GitHub-derived
        // accessors resolve the GitHub one, ignoring the GitLab row.
        let p = Person::from_identities(
            PersonId::from(2u64),
            vec![
                Identity {
                    provider: GITHUB_PROVIDER.to_string(),
                    handle: Some("bob".into()),
                    provider_user_id: Some("2".into()),
                },
                Identity {
                    provider: "gitlab".into(),
                    handle: Some("bob-gl".into()),
                    provider_user_id: Some("gl-9".into()),
                },
            ],
            "Bob",
            None,
        );
        assert_eq!(p.identities.len(), 2);
        assert_eq!(p.github_user_id(), Some(2));
        assert_eq!(p.github_username(), Some("bob"));
    }

    #[test]
    fn handle_less_github_person_has_id_but_no_username() {
        // AC-6: a handle-less GitHub identity (id present, handle null) exposes the
        // id but no username.
        let p = Person::from_identities(
            PersonId::from(500u64),
            vec![Identity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: None,
                provider_user_id: Some("500".into()),
            }],
            "Grace",
            None,
        );
        assert_eq!(p.github_user_id(), Some(500));
        assert_eq!(p.github_username(), None);
        assert_eq!(p.display_name_or_username(), "Grace");
    }

    #[test]
    fn display_name_or_username_prefers_display_name() {
        let p = github_person(1, "octo", "Octo P");
        assert_eq!(p.display_name_or_username(), "Octo P");
    }

    #[test]
    fn display_name_or_username_falls_back_to_handle_when_name_blank() {
        let p = github_person(1, "octo", "   ");
        assert_eq!(p.display_name_or_username(), "octo");
    }

    #[test]
    fn display_name_or_username_falls_back_to_id_when_name_and_handle_absent() {
        // AC-6: a handle-less, nameless person is still labelable — by the id string.
        let p = Person::from_identities(
            PersonId::from(77u64),
            vec![Identity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: None,
                provider_user_id: Some("77".into()),
            }],
            "",
            None,
        );
        assert_eq!(p.display_name_or_username(), "77");
    }

    #[test]
    fn serialize_emits_github_mirror_for_a_github_person() {
        // AC-5: the FE-facing JSON carries the derived github_user_id (number) and
        // github_username (string) mirror beside the identity list.
        let p = github_person(42, "octo", "Octo P");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["github_user_id"], serde_json::json!(42));
        assert_eq!(v["github_username"], serde_json::json!("octo"));
        assert!(v["identities"].is_array());
    }

    #[test]
    fn serialize_emits_null_username_for_a_handle_less_person() {
        // AC-5/AC-7: a handle-less person serializes github_username as null (the FE
        // login-keying paths must tolerate this) while github_user_id stays a number.
        let p = Person::from_identities(
            PersonId::from(500u64),
            vec![Identity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: None,
                provider_user_id: Some("500".into()),
            }],
            "Grace",
            None,
        );
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["github_user_id"], serde_json::json!(500));
        assert_eq!(v["github_username"], serde_json::Value::Null);
    }

    #[test]
    fn serde_roundtrip_through_identity_list_is_stable() {
        // The mirror is serialize-only; deserialize reads the identity list, so a
        // round-trip reconstructs the same Person (equality is over the real fields).
        let p = Person::from_identities(
            PersonId::from(42u64),
            vec![Identity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: Some("octo".into()),
                provider_user_id: Some("42".into()),
            }],
            "Octo P",
            Some("https://example.com/a.png".into()),
        );
        let json = serde_json::to_string(&p).unwrap();
        let back: Person = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
