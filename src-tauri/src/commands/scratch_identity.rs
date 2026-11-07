// SPDX-License-Identifier: MIT
//! Who owns a scratch item: the selected local profile, resolved once at the
//! top of a scratch-save path (routines, agents, MCP placements).

use crate::routines::PersonalOwner;

/// The selected local profile, as a scratch item records its owner.
#[derive(Debug, Clone)]
pub(crate) struct ScratchIdentity {
    /// The local profile's numeric namespace.
    pub(crate) profile_id: u64,
    /// The profile's display name (also its handle).
    pub(crate) handle: String,
    pub(crate) display_name: String,
    pub(crate) avatar_url: Option<String>,
}

impl ScratchIdentity {
    /// The owner of an item filed in `org`'s scratch space.
    pub(crate) fn owner_in(self, org: String) -> PersonalOwner {
        PersonalOwner {
            github_user_id: self.profile_id,
            username: self.handle,
            display_name: self.display_name,
            avatar_url: self.avatar_url,
            org,
        }
    }
}

/// Resolve the selected local profile. Errors (no profile selected) carry the
/// message the save path shows the user.
pub(crate) async fn resolve_scratch_identity(
    app: &tauri::AppHandle,
) -> Result<ScratchIdentity, String> {
    let profile = super::local_profile::active(app)?;
    let id = profile.principal.numeric_id()?;
    let name = if profile.display_name.trim().is_empty() {
        format!("Profile {id}")
    } else {
        profile.display_name
    };
    Ok(ScratchIdentity {
        profile_id: id,
        handle: name.clone(),
        display_name: name,
        avatar_url: None,
    })
}
