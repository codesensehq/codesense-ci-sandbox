// SPDX-License-Identifier: MIT
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// CodeSense-native identity key.
///
/// This is the value used as the on-disk `users/{id}` path segment, the identity
/// anchor on [`CurrentUser`](super::CurrentUser), and the machine-local roster key.
/// It is intentionally a `String`, not a provider `u64`, so any provider's id shape
/// (GitHub numeric, later UUID / slug) can be an *attribute* under a connection
/// rather than the primary key.
///
/// While GitHub is the only sign-in method the value is the numeric GitHub user id
/// rendered as a decimal string — the same bytes `path_to_user_home` writes to disk
/// today — so existing installs are byte-identical (no filesystem move).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Principal(String);

/// The local guest's id: the identity of someone who started without an account
/// (the no-login first run). Mirrors the frontend's `LOCAL_GUEST_USER_ID`. It is
/// well outside the GitHub id range, so it never names a real account, and it
/// keys the guest's on-disk workspace (`users/999999999999/`) the way a GitHub
/// id keys a signed-in user's.
pub const LOCAL_GUEST_USER_ID: u64 = 999_999_999_999;

impl Principal {
    /// The local guest principal (see [`LOCAL_GUEST_USER_ID`]).
    pub fn local_guest() -> Self {
        Principal::from(LOCAL_GUEST_USER_ID)
    }

    /// Admit an existing numeric namespace without normalizing its bytes.
    pub fn parse_local(value: &str) -> Result<Self, String> {
        if value.starts_with('0')
            || value.is_empty()
            || !value.bytes().all(|b| b.is_ascii_digit())
            || value.parse::<u64>().ok().filter(|id| *id != 0).is_none()
        {
            return Err("Invalid local profile namespace".into());
        }
        Ok(Self(value.into()))
    }

    pub fn numeric_id(&self) -> Result<u64, String> {
        Self::parse_local(&self.0)?;
        self.0
            .parse()
            .map_err(|_| "Invalid local profile namespace".into())
    }

    /// Borrow the underlying identity string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the owned identity string.
    pub fn into_string(self) -> String {
        self.0
    }
}

/// The GitHub bridge: a numeric account id renders to the same decimal string that
/// lands on disk today, so `Principal::from(id)` is disk-identical for existing users.
impl From<u64> for Principal {
    fn from(github_user_id: u64) -> Self {
        Principal(github_user_id.to_string())
    }
}

impl From<String> for Principal {
    fn from(value: String) -> Self {
        Principal(value)
    }
}

impl From<&str> for Principal {
    fn from(value: &str) -> Self {
        Principal(value.to_string())
    }
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Principal {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// So a `Principal` (or `&Principal`) drops straight into `Path::join` with no
/// `.to_string()` at the call site.
impl AsRef<Path> for Principal {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn from_u64_renders_decimal_string() {
        assert_eq!(Principal::from(42u64).as_str(), "42");
        assert_eq!(Principal::from(1234567u64).as_str(), "1234567");
    }

    #[test]
    fn display_renders_inner() {
        assert_eq!(format!("{}", Principal::from(7u64)), "7");
    }

    #[test]
    fn as_ref_path_joins_byte_identical_to_raw_to_string() {
        let id = 1234567u64;
        let base = PathBuf::from("/home/codesense/users");
        let via_principal = base.join(Principal::from(id));
        let via_raw = base.join(id.to_string());
        assert_eq!(via_principal, via_raw);
    }

    #[test]
    fn serde_is_a_bare_string() {
        let p = Principal::from(42u64);
        let json = serde_json::to_string(&p).unwrap();
        // #[serde(transparent)] → the wire form is the bare string, not {"0":"42"}.
        assert_eq!(json, "\"42\"");
        let back: Principal = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn usable_as_map_key() {
        let mut m: HashMap<Principal, u32> = HashMap::new();
        m.insert(Principal::from(1u64), 10);
        m.insert(Principal::from(2u64), 20);
        assert_eq!(m.get(&Principal::from(1u64)), Some(&10));
    }
}
