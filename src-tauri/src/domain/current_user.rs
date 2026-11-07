// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};

use super::Principal;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentUser {
    /// CodeSense-native identity key (SW-FG-1). This is the anchor everything
    /// downstream keys off; `#[serde(transparent)]` renders it as a bare string.
    pub principal: Principal,
    /// GitHub numeric id — now a *connection attribute* of the signed-in GitHub
    /// account (AC-2), not the identity key. Kept so the FE keeps reading the
    /// numeric id unchanged while GitHub is the only sign-in method.
    pub github_user_id: u64,
    pub github_username: String,
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        let u = CurrentUser {
            principal: Principal::from(42u64),
            github_user_id: 42,
            github_username: "octo".into(),
            display_name: "Octo P".into(),
            avatar_url: Some("https://example.com/a.png".into()),
        };
        assert_eq!(u.github_user_id, 42);
        assert_eq!(u.principal.as_str(), "42");
    }

    #[test]
    fn serde_roundtrip() {
        let u = CurrentUser {
            principal: Principal::from(7u64),
            github_user_id: 7,
            github_username: "octo".into(),
            display_name: "Octo".into(),
            avatar_url: None,
        };
        let json = serde_json::to_string(&u).unwrap();
        let back: CurrentUser = serde_json::from_str(&json).unwrap();
        assert_eq!(u, back);
    }
}
