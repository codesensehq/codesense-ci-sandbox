// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};

/// SW-FG-3: a provider-neutral binding to a signed-in connection. Names the same
/// `(provider, account)` pair the FG-2 account store is keyed on, so a resolver
/// does `list_accounts(provider)` then matches `account` with no cast and no
/// GitHub assumption. Both fields are `String` — GitHub's account is its
/// `user_id` rendered to string; another provider's stable id.
///
/// Defined as a shared **domain** type (not attachment-private) because it is
/// consumed beyond the attachment record: SW-FG-6 (write routing) and SW-FG-9
/// (context-repo binding) reuse this exact type. `None` at a binding site keeps
/// today's "unbound ⇒ fall back to the active connection" semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRef {
    pub provider: String,
    pub account: String,
}

impl CredentialRef {
    /// The GitHub binding for a numeric `user_id` — the stable key GitHub
    /// accounts are keyed on across the store (`add_github_account`,
    /// `list_github_accounts`). The single constructor every GitHub bind site
    /// and the attachment migrate-on-load lift funnels through, so the
    /// `"github"` literal and the `to_string()` render live in exactly one place.
    pub fn github(user_id: u64) -> Self {
        Self {
            provider: "github".to_string(),
            account: user_id.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_constructor_renders_user_id_to_string() {
        let cref = CredentialRef::github(12_345);
        assert_eq!(cref.provider, "github");
        assert_eq!(cref.account, "12345");
    }

    #[test]
    fn serde_roundtrips_as_provider_account_map() {
        let cref = CredentialRef {
            provider: "github".into(),
            account: "42".into(),
        };
        let yaml = serde_yaml::to_string(&cref).unwrap();
        assert!(yaml.contains("provider: github"), "yaml was: {yaml}");
        assert!(yaml.contains("account: '42'"), "yaml was: {yaml}");
        let back: CredentialRef = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, cref);
    }
}
