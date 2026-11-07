// SPDX-License-Identifier: MIT
use super::{DashboardRef, DashboardScope, PaneEntry, PaneScope};
use crate::{content_engine::atomic_io, context_repo::scratch_space, domain::Principal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Installation {
    installed_from: DashboardRef,
    source_version: String,
    source_dir: PathBuf,
}

fn installed_ref(source: &DashboardRef) -> DashboardRef {
    let hash = format!("{:x}", Sha256::digest(source.key().as_bytes()));
    DashboardRef {
        scope: DashboardScope::Private,
        org: source.org.clone(),
        team: None,
        id: format!("installed-{}", &hash[..24]),
        installed_from: Some(Box::new(source.clone())),
    }
}
fn files(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            files(&path, base, out)?;
        } else if path.file_name().is_none_or(|n| n != "install.json") {
            out.push(
                path.strip_prefix(base)
                    .map_err(|e| e.to_string())?
                    .to_owned(),
            );
        }
    }
    Ok(())
}
fn version(dir: &Path) -> Result<String, String> {
    let mut paths = Vec::new();
    files(dir, dir, &mut paths)?;
    paths.sort();
    let mut hash = Sha256::new();
    for path in paths {
        hash.update(path.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(std::fs::read(dir.join(path)).map_err(|e| e.to_string())?);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn choose(
    home: &Path,
    principal: &Principal,
    source: &PaneEntry,
    private_reach: bool,
    private_install: bool,
) -> Result<DashboardRef, String> {
    if source.status == super::PaneStatus::Rejected {
        return Err(source
            .rejected_reason
            .clone()
            .unwrap_or_else(|| "Dashboard rejected".into()));
    }
    if !private_install || !private_reach || source.scope != PaneScope::Team {
        return Ok(source.dashboard_ref.clone());
    }
    let (space, _) = scratch_space::ensure_org_space(home, principal, &source.dashboard_ref.org)?;
    let source_ref = &source.dashboard_ref;
    let dir = scratch_space::ScratchOrgSpace::panes_dir(&space)
        .join("installed")
        .join(
            source_ref
                .team
                .as_deref()
                .ok_or("Team dashboard has no team")?,
        )
        .join(&source_ref.id);
    if !dir.join("install.json").exists() {
        let source_dir = Path::new(&source.panes_dir);
        let mut paths = Vec::new();
        files(source_dir, source_dir, &mut paths)?;
        for path in paths {
            atomic_io::write_tmp_fsync_rename(
                &dir.join(&path),
                &std::fs::read(source_dir.join(&path)).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        let install = Installation {
            installed_from: source_ref.clone(),
            source_version: version(source_dir)?,
            source_dir: source_dir.to_owned(),
        };
        atomic_io::write_tmp_fsync_rename(
            &dir.join("install.json"),
            &serde_json::to_vec_pretty(&install).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(installed_ref(source_ref))
}

pub(crate) fn choose_for(
    home: &Path,
    principal: &Principal,
    source: &PaneEntry,
    private_reach: bool,
    private_install: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<DashboardRef, String> {
    scope.existing(Path::new(&source.path))?;
    if private_install && private_reach && source.scope == PaneScope::Team {
        // Installation copies the whole asset folder, so every copied leaf must
        // be admitted before any destination is created or version is exposed.
        let root = Path::new(&source.panes_dir);
        let mut paths = Vec::new();
        files(root, root, &mut paths).map_err(|_| crate::context_repo::caller_scope::MISSING)?;
        for path in paths {
            scope.require(&root.join(path))?;
        }
    }
    choose(home, principal, source, private_reach, private_install)
}

fn read_installation(dir: &Path) -> Option<Installation> {
    let install: Installation =
        serde_json::from_slice(&std::fs::read(dir.join("install.json")).ok()?).ok()?;
    let team = dir.parent()?;
    let installed = team.parent()?;
    if installed.file_name()? != "installed"
        || installed.parent()?.file_name()? != crate::context_repo::PANES_DIR
        || install.installed_from.scope != DashboardScope::Team
        || install.installed_from.team.as_deref()? != team.file_name()?.to_str()?
        || install.installed_from.id != dir.file_name()?.to_str()?
    {
        return None;
    }
    Some(install)
}

pub(crate) fn reference_at(dir: &Path) -> Option<DashboardRef> {
    read_installation(dir).map(|install| installed_ref(&install.installed_from))
}

pub(crate) fn source_version(dir: &Path) -> Option<String> {
    read_installation(dir).map(|install| install.source_version)
}

pub(crate) fn is_install_dir(dir: &Path) -> bool {
    read_installation(dir).is_some()
}

pub(super) fn discover(
    dir: &Path,
    org: &str,
    view: Option<&crate::context_repo::inheritance::TeamView>,
) -> Vec<PaneEntry> {
    let mut out = Vec::new();
    for team in std::fs::read_dir(dir.join("installed"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for dashboard in std::fs::read_dir(team.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = dashboard.path();
            if view.is_some_and(|v| v.read_path(&path.join("install.json")).is_err()) {
                continue;
            }
            let Some(install) = read_installation(&path) else {
                continue;
            };
            let source = &install.installed_from;
            let manifest = view.map_or_else(
                || super::manifest::load(&path),
                |v| v.private_pane_manifest(&path),
            );
            let entries = super::list_layer_filtered(
                &path,
                PaneScope::Private,
                org,
                None,
                |p| view.is_none_or(|v| v.read_path(p).is_ok()),
                manifest,
                view,
            );
            if let Some(mut entry) = entries.into_iter().find(|p| p.id == source.id) {
                entry.dashboard_ref = installed_ref(source);
                entry.id = entry.dashboard_ref.id.clone();
                entry.panel_id = super::pane_panel_id(PaneScope::Private, org, None, &entry.id);
                entry.install_source_version = Some(install.source_version.clone());
                let source_admitted = view.is_none_or(|view| {
                    let mut paths = Vec::new();
                    files(&install.source_dir, &install.source_dir, &mut paths).is_ok()
                        && paths
                            .iter()
                            .all(|p| view.read_path(&install.source_dir.join(p)).is_ok())
                });
                entry.update_available = source_admitted
                    && version(&install.source_dir).is_ok_and(|v| v != install.source_version);
                out.push(entry);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_install_off_tracks_edits_and_on_pins_html_and_assets() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42u64);
        let team = home.path().join("source");
        std::fs::create_dir_all(team.join("lib")).unwrap();
        std::fs::write(team.join("inbox.html"), "<title>Inbox one</title>").unwrap();
        std::fs::write(team.join("lib/widget.js"), "one").unwrap();
        let source =
            super::super::list_layer(&team, PaneScope::Team, "acme/repo", Some("dev")).remove(0);
        assert_eq!(
            choose(home.path(), &principal, &source, true, false).unwrap(),
            source.dashboard_ref
        );
        assert_eq!(
            choose(home.path(), &principal, &source, false, true).unwrap(),
            source.dashboard_ref
        );
        let installed = choose(home.path(), &principal, &source, true, true).unwrap();
        let space = scratch_space::org_space(home.path(), &principal, "acme/repo").unwrap();
        let scan =
            || super::super::list_layer(&space.panes_dir(), PaneScope::Private, "acme/repo", None);
        let copy = scan().remove(0);
        assert_eq!(copy.dashboard_ref, installed);
        assert_eq!(copy.title, "Inbox one");
        assert!(!copy.update_available);
        assert_eq!(
            copy.install_source_version.as_deref(),
            Some(version(&team).unwrap().as_str())
        );
        std::fs::write(team.join("inbox.html"), "<title>Inbox two</title>").unwrap();
        std::fs::write(team.join("lib/widget.js"), "two").unwrap();
        let copy = scan().remove(0);
        assert!(copy.update_available);
        assert_eq!(copy.title, "Inbox one");
        assert_eq!(
            std::fs::read_to_string(Path::new(&copy.panes_dir).join("lib/widget.js")).unwrap(),
            "one"
        );
        assert_eq!(
            super::super::list_layer(&team, PaneScope::Team, "acme/repo", Some("dev"))[0].title,
            "Inbox two"
        );
        choose(home.path(), &principal, &source, true, true).unwrap();
        assert!(scan()[0].update_available);
    }
}
