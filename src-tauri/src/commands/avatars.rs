// SPDX-License-Identifier: MIT
//! The avatar packs the agents list can draw, for the active team.
//!
//! Faces reach the webview as `data:` URLs of their small variant (or the
//! cleaned SVG), so no file path is opened to the webview and the asset
//! protocol scope stays as it is. The built-in packs are bundled with the
//! webview itself and are not listed here.

use std::collections::BTreeMap;

use serde::Serialize;
use tauri::AppHandle;

use crate::avatars::store::{self, Discovery};
use crate::avatars::{face_ref, PackScope};

use super::await_blocking;

/// The packs found for the active team.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AvatarPacksView {
    pub packs: Vec<AvatarPackView>,
    /// The team's default pack, when it set one.
    pub default_pack: Option<String>,
    /// The default face of every agent that has one, by agent key.
    pub agent_avatars: BTreeMap<String, String>,
    /// Packs and faces skipped, and why.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AvatarPackView {
    pub id: String,
    pub name: String,
    pub scope: PackScope,
    /// A team pack the user also has in their own space (one they shared): it
    /// is listed once, as the team's.
    pub personal_copy: bool,
    pub faces: Vec<AvatarFaceView>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AvatarFaceView {
    pub id: String,
    /// `pack:id`.
    #[serde(rename = "ref")]
    pub reference: String,
    pub label: Option<String>,
    /// A `data:` URL of the image.
    pub src: String,
}

/// The view of what discovery found: every face that can be drawn.
pub(crate) fn view(found: Discovery) -> AvatarPacksView {
    let mut warnings = found.warnings;
    let packs = found
        .packs
        .into_iter()
        .map(|pack| {
            let faces = pack
                .faces
                .iter()
                .filter_map(|face| match store::display_source(face) {
                    Ok(src) => Some(AvatarFaceView {
                        reference: face_ref(&pack.id, &face.id),
                        id: face.id.clone(),
                        label: face.label.clone(),
                        src,
                    }),
                    Err(e) => {
                        warnings.push(format!(
                            "avatars/{}: face `{}`: {e}; skipped",
                            pack.id, face.id
                        ));
                        None
                    }
                })
                .collect();
            AvatarPackView {
                id: pack.id,
                name: pack.name,
                scope: pack.scope,
                personal_copy: pack.personal_copy,
                faces,
            }
        })
        .collect();
    AvatarPacksView {
        packs,
        default_pack: None,
        agent_avatars: BTreeMap::new(),
        warnings,
    }
}

/// Each agent's default face, archived agents included (their sessions may
/// still run). An unreadable agent store reads as no defaults.
fn agent_avatars(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
) -> BTreeMap<String, String> {
    match crate::agents::service::list(home, principal, true) {
        Ok(found) => found
            .into_iter()
            .filter_map(|f| f.agent.avatar.clone().map(|a| (f.key.to_string(), a)))
            .collect(),
        Err(e) => {
            log::warn!(target: "avatars", "agents unreadable while listing faces: {e}");
            BTreeMap::new()
        }
    }
}

/// The packs a context can use: its team's, then the user's own, with the team's default pack and
/// each agent's default face.
fn packs_view(ctx: &super::route_to_context::ActiveContext) -> AvatarPacksView {
    let team = ctx.team_name.clone().unwrap_or_default();
    let roots = store::roots_for_team(&ctx.codesense_home, &ctx.principal(), &ctx.org_slug, &team);
    let found = store::discover(&roots);
    let default_pack = roots
        .iter()
        .find(|r| r.scope == PackScope::Team)
        .and_then(|r| store::read_default_pack(&r.avatars_dir));
    for warning in &found.warnings {
        log::debug!(target: "avatars", "{warning}");
    }
    let mut out = view(found);
    out.default_pack = default_pack;
    out.agent_avatars = agent_avatars(&ctx.codesense_home, &ctx.principal());
    out
}

/// One plain path part: not empty, not `.`/`..`, and no `/`, `\\`, `:` or NUL.
fn is_plain_part(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && !part.contains(['/', '\\', '\0', ':'])
}

/// A team key's `(org, team)`. The org is one or more `/`-separated plain parts (`acme`,
/// `acme/widgets`) and the team is one plain part, with exactly one `:` between them. Both become path
/// segments under the user's own org root, so a part that could climb out of it is refused.
fn split_team_key(team_key: &str) -> Result<(&str, &str), String> {
    let (org, team) = team_key
        .split_once(':')
        .ok_or_else(|| format!("team key {team_key:?} is not org:team"))?;
    let org_ok = org.split('/').all(is_plain_part);
    if !org_ok || !is_plain_part(team) {
        return Err(format!(
            "team key {team_key:?} is not an org slug and a team name"
        ));
    }
    Ok((org, team))
}

/// The same packs for any team of the user's, by its key (`org:team`), so the agents list can
/// draw every shown team's agents with that team's faces.
#[tauri::command]
pub async fn avatar_packs_list_for_team(
    app: AppHandle,
    team_key: String,
) -> Result<AvatarPacksView, String> {
    let (org, team) = split_team_key(&team_key)?;
    let (codesense_home, user_id) = super::route_to_context::resolve_ambient(&app).await?;
    let ctx = super::route_to_context::ActiveContext::for_team(
        codesense_home,
        user_id,
        org.to_string(),
        team.to_string(),
    );
    await_blocking("avatar_packs_list_for_team", move || Ok(packs_view(&ctx))).await
}

/// Set the active team's default pack (`None` returns it to the built-in funny
/// pack). A local edit to the team's content, shared like any other.
#[tauri::command]
pub async fn avatar_default_pack_set(app: AppHandle, pack: Option<String>) -> Result<(), String> {
    let ctx = super::route_to_context::derive_active_context(&app).await?;
    await_blocking("avatar_default_pack_set", move || {
        let team = ctx.team_name.clone().unwrap_or_default();
        let dir = store::avatars_dir_for_add(
            &ctx.codesense_home,
            &ctx.principal(),
            &ctx.org_slug,
            &team,
            PackScope::Team,
        )?;
        store::write_default_pack(&dir, pack.as_deref())
    })
    .await?;
    if let Err(e) = tauri::Emitter::emit(&app, crate::avatars::AVATARS_CHANGED, ()) {
        log::debug!(target: "avatars", "avatars:changed emit failed: {e}");
    }
    Ok(())
}

/// Copy one of the user's own packs into the active team's content, so every
/// teammate gets its faces (any agent already wearing one keeps it: the
/// `pack:id` is the same, and now resolves to the team's copy). The personal
/// pack stays, for the org's other teams. Refused when the team is not synced
/// here or already has a pack with that id. A local edit to the team's
/// content, shared like any other.
#[tauri::command]
pub async fn avatar_pack_share_with_team(app: AppHandle, pack: String) -> Result<(), String> {
    let ctx = super::route_to_context::derive_active_context(&app).await?;
    let reply = await_blocking("avatar_pack_share_with_team", move || {
        let team = ctx.team_name.clone().unwrap_or_default();
        Ok(crate::mcp::avatar_tools::share(
            &ctx.codesense_home,
            &ctx.principal(),
            &ctx.org_slug,
            &team,
            crate::mcp::avatar_tools::AvatarShareParams { pack },
        ))
    })
    .await?;
    if let crate::mcp::avatar_tools::AvatarReply::Refused { error, .. } = reply {
        return Err(error.message);
    }
    if let Err(e) = tauri::Emitter::emit(&app, crate::avatars::AVATARS_CHANGED, ()) {
        log::debug!(target: "avatars", "avatars:changed emit failed: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avatars::store::PackRoot;

    #[test]
    fn faces_reach_the_webview_as_data_urls() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("avatars/crew");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><script>x()</script><rect width="4" height="4"/></svg>"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("pack.yaml"),
            "name: Crew\nfaces:\n  - {id: a, file: a.svg, label: A}\n  - {id: b, file: b.png}\n",
        )
        .unwrap();
        let found = store::discover(&[PackRoot {
            scope: PackScope::Team,
            avatars_dir: tmp.path().join("avatars"),
        }]);
        let out = view(found);
        assert_eq!(out.packs.len(), 1);
        let face = &out.packs[0].faces[0];
        assert_eq!(face.reference, "crew:a");
        assert!(face.src.starts_with("data:image/svg+xml;base64,"));
        let svg = String::from_utf8(
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                face.src.trim_start_matches("data:image/svg+xml;base64,"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(!svg.contains("script"));
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["packs"][0]["faces"][0]["ref"], "crew:a");
        assert_eq!(json["packs"][0]["scope"], "team");
    }
}

#[cfg(test)]
mod split_team_key_tests {
    use super::split_team_key;

    #[test]
    fn splits_an_owner_name_org_and_its_team() {
        assert_eq!(split_team_key("acme:ops"), Ok(("acme", "ops")));
        assert_eq!(
            split_team_key("acme/widgets:core"),
            Ok(("acme/widgets", "core"))
        );
        assert_eq!(
            split_team_key("codesense/docs:getting-started"),
            Ok(("codesense/docs", "getting-started"))
        );
    }

    #[test]
    fn refuses_a_key_without_one_separator_or_with_an_empty_or_traversing_part() {
        for bad in [
            "acme",
            ":ops",
            "acme:",
            "acme:..",
            "../x:ops",
            "acme/..:ops",
            "acme/./w:ops",
            "/acme:ops",
            "acme/:ops",
            "acme//w:ops",
            "acme:a/b",
            "acme:a:b",
            "acme:a\\b",
            "acme\\w:ops",
            "acme:.",
            "acme:a\0",
        ] {
            assert!(split_team_key(bad).is_err(), "{bad:?} should be refused");
        }
    }
}

#[cfg(test)]
mod packs_for_team_tests {
    use super::packs_view;
    use crate::commands::route_to_context::ActiveContext;

    fn write_pack(team_dir: &std::path::Path, id: &str) {
        write_pack_in(&team_dir.join("content/avatars"), id);
    }

    fn write_pack_in(avatars_dir: &std::path::Path, id: &str) {
        let dir = avatars_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><rect width="4" height="4"/></svg>"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("pack.yaml"),
            format!("name: {id}\nfaces:\n  - {{id: a, file: a.svg}}\n"),
        )
        .unwrap();
    }

    #[test]
    fn an_owner_name_org_lists_each_teams_own_packs() {
        let tmp = tempfile::tempdir().unwrap();
        let org = tmp.path().join("users/41/orgs/acme/widgets");
        write_pack(&org.join("core"), "core-crew");
        write_pack(&org.join("ops"), "ops-crew");
        let ids_for = |team_key: &str| -> Vec<String> {
            let (org_slug, team) = super::split_team_key(team_key).unwrap();
            let ctx = ActiveContext::for_team(
                tmp.path().to_path_buf(),
                41,
                org_slug.to_string(),
                team.to_string(),
            );
            packs_view(&ctx).packs.into_iter().map(|p| p.id).collect()
        };
        assert_eq!(ids_for("acme/widgets:core"), ["core-crew"]);
        assert_eq!(ids_for("acme/widgets:ops"), ["ops-crew"]);

        // The user's own packs in the org come with every team's.
        let ctx = ActiveContext::for_team(
            tmp.path().to_path_buf(),
            41,
            "acme/widgets".to_string(),
            "core".to_string(),
        );
        let space = crate::context_repo::scratch_space::org_space_at(
            tmp.path(),
            &ctx.principal(),
            "acme/widgets",
        )
        .unwrap();
        write_pack_in(&space.content_dir().join("avatars"), "mine");
        assert_eq!(ids_for("acme/widgets:core"), ["core-crew", "mine"]);
        assert_eq!(ids_for("acme/widgets:ops"), ["ops-crew", "mine"]);
    }
}
