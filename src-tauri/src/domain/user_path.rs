// SPDX-License-Identifier: MIT
//! The user id a CodeSense home path belongs to.
//!
//! SW-XS-14: moved out of the retired `domain::staleness` module (view
//! overrides and the stale-epic filter are gone); the user-id derivation is
//! still read by the content engine and the attachment locator.

/// Extract a user ID from a path that contains a `/users/{user_id}/` segment.
///
/// Expected pattern: `.../users/{user_id}/{anything}`. Returns `None` when no
/// `/users/` marker is found, no trailing segment exists, or the user_id
/// segment is empty (e.g. `/users//orgs/foo`). Uses `rfind` to pick the last
/// `/users/` segment, consistent with `parse_context_repo_parts`.
pub(crate) fn extract_user_id(path: &str) -> Option<String> {
    if let Some(location) = crate::context_repo::context_attachments::location_for_known_path(
        std::path::Path::new(path),
    )
    .ok()?
    {
        return Some(location.principal.into_string());
    }
    let marker = "/users/";
    let idx = path.rfind(marker)?;
    let after = &path[idx + marker.len()..];
    let end = after.find('/')?;
    if end == 0 {
        return None;
    }
    Some(after[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_user_id_valid() {
        let path = "/home/.codesense/users/1234567/orgs/codesense/default/repos/org/repo";
        assert_eq!(extract_user_id(path), Some("1234567".to_string()));
    }

    #[test]
    fn extract_user_id_missing() {
        assert_eq!(extract_user_id("/no/users/segment"), None);
    }

    #[test]
    fn extract_user_id_trailing_slash() {
        assert_eq!(extract_user_id("/home/.codesense/users/"), None);
    }

    #[test]
    fn extract_user_id_picks_last_users_segment() {
        let path = "/home/users/shared/.codesense/users/12345/orgs/acme/api";
        assert_eq!(extract_user_id(path), Some("12345".to_string()));
    }

    #[test]
    fn extract_user_id_empty_segment_returns_none() {
        assert_eq!(
            extract_user_id("/home/.codesense/users//orgs/acme/api"),
            None
        );
    }
}
