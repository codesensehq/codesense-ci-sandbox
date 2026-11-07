// SPDX-License-Identifier: MIT
//! The avatar MCP tools: `avatar_list` (the packs and faces the active team can
//! use, and where to put new ones), `avatar_add` (list an image the agent
//! already put on disk as a face) and `avatar_share` (copy one of the user's own
//! packs into the team). The Tauri commands run the same bodies.
//!
//! Content moves through the filesystem: the agent fetches and crops an image
//! itself; these tools only validate, index and make the small variant. Faces
//! are display only, and nothing here checks who may add to a pack.

use std::path::{Path, PathBuf};

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::avatars::store::{self, AddRequest, PackRoot};
use crate::avatars::{builtin, parse_face_ref, PackScope, AVATARS_DIR};
use crate::context_repo::scratch_space;
use crate::domain::Principal;

/// `{ok:true, data}` or `{ok:false, error:{code, message}}`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum AvatarReply<T> {
    Ok { ok: bool, data: T },
    Refused { ok: bool, error: AvatarRefusal },
}

#[derive(Debug, Serialize)]
pub(crate) struct AvatarRefusal {
    /// `invalid` (an input is wrong; `message` says which and how to fix it).
    pub code: &'static str,
    pub message: String,
}

impl<T> AvatarReply<T> {
    fn ok(data: T) -> Self {
        Self::Ok { ok: true, data }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::Refused {
            ok: false,
            error: AvatarRefusal {
                code: "invalid",
                message: message.into(),
            },
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvatarListParams {
    /// Face references (`pack:id`) to check: each comes back in `checked` as
    /// available on this machine or not, with why.
    #[serde(default)]
    pub check: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvatarAddParams {
    /// The pack (its folder name under `avatars/`): lowercase letters, digits
    /// and dashes, at most 32 characters, not starting `builtin-`. Created on
    /// first add.
    pub pack: String,
    /// The face's id within the pack: lowercase letters, digits and dashes, at
    /// most 31 characters. An existing face with this id is replaced.
    pub id: String,
    /// The image (PNG, WebP or SVG; roughly square; at most 1 MB and 1024px a
    /// side): a path inside the pack folder (used where it is), or any other
    /// absolute path on this machine (copied in as `<id>.<ext>`). A relative
    /// path is read from the pack folder.
    pub file: String,
    /// A short display label, e.g. "Grumpy Cat".
    #[serde(default)]
    pub label: Option<String>,
    /// `team` (default: the active team's content, shared with teammates when
    /// you push it) or `personal` (your private space for the open org: only
    /// you see it).
    #[serde(default)]
    pub scope: Option<String>,
    /// The pack's display name when this add creates the pack (default: from
    /// its folder name).
    #[serde(default, alias = "pack_name")]
    pub pack_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvatarShareParams {
    /// The id of one of your personal packs (its folder under your
    /// `add_to.personal`), as avatar_list lists it with `scope: personal`.
    pub pack: String,
}

/// Where the active team's packs are read from and written to.
#[derive(Debug, Clone)]
pub(crate) struct AvatarPlaces {
    pub team_label: String,
    pub roots: Vec<PackRoot>,
    /// Where a `team` add writes.
    pub team_dir: Option<PathBuf>,
    /// Where a `personal` add writes (the private space is made on first add).
    pub personal_dir: Option<PathBuf>,
}

impl AvatarPlaces {
    pub(crate) fn for_team(home: &Path, principal: &Principal, org: &str, team: &str) -> Self {
        let team_dir = crate::agents::roots::team_content_dir(home, principal, org, team)
            .ok()
            .filter(|_| !scratch_space::is_scratch_team(team))
            .map(|c| c.join(AVATARS_DIR));
        let personal_dir = scratch_space::org_space_at(home, principal, org)
            .ok()
            .map(|space| space.content_dir().join(AVATARS_DIR));
        Self {
            team_label: format!("{org}/{team}"),
            roots: store::roots_for_team(home, principal, org, team),
            team_dir,
            personal_dir,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ListData {
    pub team: String,
    /// The pack agents without a face of their own are dealt from
    /// (`avatars/settings.yaml` in the team's content); `builtin-funny` when unset.
    pub default_pack: String,
    pub packs: Vec<PackView>,
    /// Where new packs go: `avatars/<pack>/` under one of these.
    pub add_to: AddTo,
    /// Packs or faces skipped, and why.
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checked: Vec<RefCheck>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AddTo {
    pub team: Option<String>,
    pub personal: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PackView {
    pub id: String,
    pub name: String,
    pub scope: PackScope,
    /// The pack folder; absent for a built-in pack.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// A team pack you also have in your personal space (one you shared): it
    /// is listed once, as the team's.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub personal_copy: bool,
    pub faces: Vec<FaceView>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FaceView {
    /// What `open_workspace`'s `avatar` takes.
    #[serde(rename = "ref")]
    pub reference: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct RefCheck {
    #[serde(rename = "ref")]
    pub reference: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AddData {
    #[serde(rename = "ref")]
    pub reference: String,
    pub scope: PackScope,
    pub pack_dir: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small_file: Option<String>,
    pub manifest: String,
    pub replaced: bool,
    pub warnings: Vec<String>,
}

fn show(path: &Path) -> String {
    path.display().to_string()
}

/// Every pack the active team can use: its own, the caller's personal ones,
/// then the built-ins.
pub(crate) fn list(places: &AvatarPlaces, params: AvatarListParams) -> AvatarReply<ListData> {
    list_for(places, params, &Default::default())
}

pub(crate) fn list_for(
    places: &AvatarPlaces,
    params: AvatarListParams,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> AvatarReply<ListData> {
    let found = store::discover_for(&places.roots, scope);
    let mut packs: Vec<PackView> = found
        .packs
        .into_iter()
        .map(|p| PackView {
            faces: p
                .faces
                .into_iter()
                .map(|f| FaceView {
                    reference: crate::avatars::face_ref(&p.id, &f.id),
                    id: f.id,
                    label: f.label,
                    file: Some(show(&f.file)),
                })
                .collect(),
            dir: Some(show(&p.dir)),
            personal_copy: p.personal_copy,
            id: p.id,
            name: p.name,
            scope: p.scope,
        })
        .collect();
    packs.extend(builtin::packs().into_iter().map(|p| {
        PackView {
            faces: p
                .faces
                .into_iter()
                .map(|f| FaceView {
                    reference: crate::avatars::face_ref(p.id, &f.id),
                    id: f.id,
                    label: f.label,
                    file: None,
                })
                .collect(),
            id: p.id.to_string(),
            name: p.name,
            scope: PackScope::Builtin,
            dir: None,
            personal_copy: false,
        }
    }));
    let checked = params
        .check
        .iter()
        .map(|reference| check_ref(&packs, reference))
        .collect();
    let default_pack = places
        .team_dir
        .as_deref()
        .filter(|dir| scope.allows(&dir.join("settings.yaml")))
        .and_then(store::read_default_pack)
        .unwrap_or_else(|| "builtin-funny".to_string());
    AvatarReply::ok(ListData {
        team: places.team_label.clone(),
        default_pack,
        packs,
        add_to: AddTo {
            team: places.team_dir.as_deref().map(show),
            personal: places.personal_dir.as_deref().map(show),
        },
        warnings: found.warnings,
        checked,
    })
}

fn check_ref(packs: &[PackView], reference: &str) -> RefCheck {
    let unavailable = |reason: String| RefCheck {
        reference: reference.to_string(),
        available: false,
        reason: Some(reason),
    };
    let Some((pack_id, face_id)) = parse_face_ref(reference) else {
        return unavailable("not a `pack:id` reference".to_string());
    };
    let Some(pack) = packs.iter().find(|p| p.id == pack_id) else {
        return unavailable(format!(
            "no pack `{pack_id}` on this machine for this team: it may be someone's personal pack or not synced yet; it shows as the next fallback face"
        ));
    };
    if pack.faces.iter().any(|f| f.id == face_id) {
        RefCheck {
            reference: reference.to_string(),
            available: true,
            reason: None,
        }
    } else {
        unavailable(format!("pack `{pack_id}` has no usable face `{face_id}`"))
    }
}

/// The scope an add names: `team` unless `personal` is asked for.
pub(crate) fn add_scope(scope: Option<&str>) -> Result<PackScope, String> {
    match scope.map(str::trim) {
        None | Some("") | Some("team") => Ok(PackScope::Team),
        Some("personal") => Ok(PackScope::Personal),
        Some(other) => Err(format!("scope `{other}` must be `team` or `personal`")),
    }
}

/// Add a face to a pack under `avatars_dir` (already resolved for the scope).
pub(crate) fn add(
    scope: PackScope,
    avatars_dir: Result<PathBuf, String>,
    params: AvatarAddParams,
) -> AvatarReply<AddData> {
    add_for(scope, avatars_dir, params, &Default::default(), || {})
}

pub(crate) fn add_for(
    scope: PackScope,
    avatars_dir: Result<PathBuf, String>,
    params: AvatarAddParams,
    caller: &crate::context_repo::caller_scope::CallerScope,
    on_written: impl FnOnce(),
) -> AvatarReply<AddData> {
    let avatars_dir = match avatars_dir {
        Ok(dir) => dir,
        Err(e) => return AvatarReply::invalid(e),
    };
    let request = AddRequest {
        pack: params.pack,
        id: params.id,
        file: PathBuf::from(params.file.trim()),
        label: params.label,
        pack_name: params.pack_name,
    };
    let readable = caller.allows(
        &avatars_dir
            .join(&request.pack)
            .join(crate::avatars::MANIFEST_FILE),
    );
    let result = store::add_face_for(&avatars_dir, &request, caller);
    if result.is_ok() {
        on_written();
    }
    let result = crate::context_repo::caller_scope::readback(readable, result);
    match result {
        Ok(out) => AvatarReply::ok(AddData {
            reference: out.reference,
            scope,
            pack_dir: show(&out.pack_dir),
            file: show(&out.file),
            small_file: out.small_file.as_deref().map(show),
            manifest: show(&out.manifest),
            replaced: out.replaced,
            warnings: out.warnings,
        }),
        Err(e) => AvatarReply::invalid(e),
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ShareData {
    /// The pack's id, unchanged: its faces' `pack:id` refs now resolve to the team's copy.
    pub pack: String,
    /// The team's copy.
    pub pack_dir: String,
}

pub(crate) fn share_for(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    params: AvatarShareParams,
    caller: &crate::context_repo::caller_scope::CallerScope,
    on_written: impl FnOnce(),
) -> AvatarReply<ShareData> {
    let readable = store::avatars_dir_for_add(home, principal, org, team, PackScope::Team)
        .ok()
        .is_some_and(|dir| caller.allows(&dir.join(params.pack.trim())));
    let result = share(home, principal, org, team, params);
    if matches!(&result, AvatarReply::Ok { .. }) {
        on_written();
    }
    crate::context_repo::caller_scope::readback(readable, Ok(result))
        .unwrap_or_else(AvatarReply::invalid)
}

/// Copy the caller's own pack `params.pack` into `org/team`'s content, so every
/// teammate gets its faces. The personal pack stays, for the org's other teams.
/// Refused when the team is not synced here, is the scratch space, or already
/// has a pack with that id.
pub(crate) fn share(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    params: AvatarShareParams,
) -> AvatarReply<ShareData> {
    if team.is_empty() || scratch_space::is_scratch_team(team) {
        return AvatarReply::invalid("open a team to share a pack with it");
    }
    match crate::agents::roots::is_synced_team(home, principal, org, team) {
        Ok(true) => {}
        Ok(false) => {
            return AvatarReply::invalid(format!(
                "team {org}/{team} is not synced on this machine; sync it, then share again"
            ))
        }
        Err(e) => return AvatarReply::invalid(e.to_string()),
    }
    let pack = params.pack.trim().to_string();
    let Some(from) = scratch_space::org_space(home, principal, org)
        .map(|space| space.content_dir().join(AVATARS_DIR))
    else {
        return AvatarReply::invalid(format!("you have no pack `{pack}` to share"));
    };
    let to = match store::avatars_dir_for_add(home, principal, org, team, PackScope::Team) {
        Ok(dir) => dir,
        Err(e) => return AvatarReply::invalid(e),
    };
    match store::share_pack(&from, &to, &pack) {
        Ok(dir) => AvatarReply::ok(ShareData {
            pack,
            pack_dir: show(&dir),
        }),
        Err(e) => AvatarReply::invalid(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64"/></svg>"#;

    fn places(tmp: &Path) -> AvatarPlaces {
        let team_dir = tmp.join("team/avatars");
        let personal_dir = tmp.join("me/avatars");
        AvatarPlaces {
            team_label: "acme/core".into(),
            roots: vec![
                PackRoot {
                    scope: PackScope::Team,
                    avatars_dir: team_dir.clone(),
                },
                PackRoot {
                    scope: PackScope::Personal,
                    avatars_dir: personal_dir.clone(),
                },
            ],
            team_dir: Some(team_dir),
            personal_dir: Some(personal_dir),
        }
    }

    fn add_params(pack: &str, id: &str, file: &Path) -> AvatarAddParams {
        AvatarAddParams {
            pack: pack.into(),
            id: id.into(),
            file: file.display().to_string(),
            label: Some(id.to_uppercase()),
            scope: None,
            pack_name: None,
        }
    }

    fn json<T: Serialize>(reply: &AvatarReply<T>) -> serde_json::Value {
        serde_json::to_value(reply).unwrap()
    }

    #[test]
    fn an_agent_builds_a_pack_of_three_and_lists_it() {
        let tmp = tempfile::tempdir().unwrap();
        let places = places(tmp.path());
        let source = tmp.path().join("in.svg");
        std::fs::write(&source, SVG).unwrap();
        for id in ["one", "two", "three"] {
            let reply = json(&add(
                PackScope::Team,
                Ok(places.team_dir.clone().unwrap()),
                add_params("crew", id, &source),
            ));
            assert_eq!(reply["ok"], true, "{reply}");
            assert_eq!(reply["data"]["ref"], format!("crew:{id}"));
        }
        let reply = json(&list(
            &places,
            AvatarListParams {
                check: vec![
                    "crew:two".into(),
                    "tpb:ricky".into(),
                    "crew:nine".into(),
                    "owl".into(),
                ],
            },
        ));
        assert_eq!(reply["ok"], true);
        let packs = reply["data"]["packs"].as_array().unwrap();
        assert_eq!(packs[0]["id"], "crew");
        assert_eq!(packs[0]["scope"], "team");
        let refs: Vec<_> = packs[0]["faces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["ref"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(refs, ["crew:one", "crew:two", "crew:three"]);
        // The built-ins are listed after the team's packs.
        assert!(packs
            .iter()
            .any(|p| p["id"] == "builtin-funny" && p["scope"] == "builtin"));
        let checked = reply["data"]["checked"].as_array().unwrap();
        assert_eq!(checked[0]["available"], true);
        // A pack that is not on this machine (say, a teammate's personal pack)
        // is reported unavailable, not an error.
        assert_eq!(checked[1]["available"], false);
        assert!(checked[1]["reason"]
            .as_str()
            .unwrap()
            .contains("no pack `tpb`"));
        assert_eq!(checked[2]["available"], false);
        assert_eq!(checked[3]["available"], false);
        assert_eq!(
            reply["data"]["add_to"]["personal"],
            show(&tmp.path().join("me/avatars"))
        );
    }

    #[test]
    fn a_bad_add_is_refused_with_a_message() {
        let tmp = tempfile::tempdir().unwrap();
        let reply = json(&add(
            PackScope::Team,
            Ok(tmp.path().join("avatars")),
            add_params("crew", "x", &tmp.path().join("missing.png")),
        ));
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"]["code"], "invalid");
        let reply = json(&add(
            PackScope::Personal,
            Err("no private space".into()),
            add_params("crew", "x", &tmp.path().join("missing.png")),
        ));
        assert_eq!(reply["error"]["message"], "no private space");
    }

    /// `avatar_share` (and the app's Share with team, which runs the same body)
    /// over a real home: refused for a team not synced here; once shared, the
    /// team's avatar_list holds the pack exactly once, as the team's.
    #[test]
    fn a_shared_pack_is_listed_once_as_the_teams() {
        use crate::agents::resolve::tests::{alice, team_content, ORG, TEAM};
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        team_content(home, &alice());
        let (space, _) = scratch_space::ensure_org_space(home, &alice(), ORG).unwrap();
        let pack = space.content_dir().join(AVATARS_DIR).join("crew");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("a.svg"), SVG).unwrap();
        std::fs::write(pack.join("pack.yaml"), "faces:\n  - {id: a, file: a.svg}\n").unwrap();
        let params = || AvatarShareParams {
            pack: "crew".into(),
        };

        let reply = json(&share(home, &alice(), ORG, "elsewhere", params()));
        assert_eq!(reply["ok"], false);
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not synced on this machine"));

        let reply = json(&share(home, &alice(), ORG, TEAM, params()));
        assert_eq!(reply["ok"], true, "{reply}");
        let listed = json(&list(
            &AvatarPlaces::for_team(home, &alice(), ORG, TEAM),
            AvatarListParams::default(),
        ));
        let crews: Vec<_> = listed["data"]["packs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["id"] == "crew")
            .collect();
        assert_eq!(crews.len(), 1, "{listed}");
        assert_eq!(crews[0]["scope"], "team");
        assert_eq!(crews[0]["personal_copy"], true);
        assert_eq!(listed["data"]["warnings"], serde_json::json!([]));
        assert!(pack.join("pack.yaml").is_file(), "the personal pack stays");

        let again = json(&share(home, &alice(), ORG, TEAM, params()));
        assert!(again["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already has a pack named `crew`"));
    }

    #[test]
    fn scope_defaults_to_team() {
        assert_eq!(add_scope(None), Ok(PackScope::Team));
        assert_eq!(add_scope(Some("personal")), Ok(PackScope::Personal));
        assert!(add_scope(Some("org")).is_err());
    }
}
