// SPDX-License-Identifier: MIT
//! The page bridge's location verbs: which content root a page lives in
//! (`codesense.context()`), and where a page may open a file
//! (`codesense.openFile`).
//!
//! Every answer comes from the user's known content roots — the context
//! index's footprint (each synced team's `content/`, each org's private space,
//! all-orgs content) — so a page can never name an arbitrary filesystem path.
//! The page itself never supplies its own location: the trusted host passes the
//! page path it renders.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::context_query::index::IndexRoot;
use crate::context_query::model::Layer;

#[cfg(test)]
mod caller_scope_tests;

/// Where a page or file sits in the footprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentLocation {
    /// `None` for all-orgs content.
    pub org: Option<String>,
    /// `None` for private and all-orgs content.
    pub team: Option<String>,
    pub layer: Layer,
    /// The content root's absolute directory.
    pub content_root: String,
}

/// A file a page may open, resolved inside a team's content root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenTarget {
    /// Absolute path of the file.
    pub path: String,
    pub org: String,
    pub team: String,
    pub content_root: String,
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn location(root: &IndexRoot) -> ContentLocation {
    ContentLocation {
        org: root.org.clone(),
        team: root.team.clone(),
        layer: root.layer,
        content_root: path_str(&root.dir),
    }
}

/// The deepest root `path` sits under. A path with `.` or `..` components is
/// never inside a root. An existing path must also resolve (through symlinks)
/// inside that root's resolved directory, so a link inside a root that points
/// out of it doesn't count; a root reached through a link still matches.
pub(crate) fn root_of<'a>(roots: &'a [IndexRoot], path: &Path) -> Option<&'a IndexRoot> {
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return None;
    }
    let real = std::fs::canonicalize(path).ok();
    let lexical = roots
        .iter()
        .filter(|r| path.starts_with(&r.dir))
        .max_by_key(|r| r.dir.components().count());
    if let Some(root) = lexical {
        let Some(real) = &real else {
            return Some(root);
        };
        let resolved_root = std::fs::canonicalize(&root.dir).unwrap_or_else(|_| root.dir.clone());
        return real.starts_with(&resolved_root).then_some(root);
    }
    let real = real?;
    roots
        .iter()
        .filter_map(|r| {
            std::fs::canonicalize(&r.dir)
                .ok()
                .filter(|d| real.starts_with(d))
                .map(|d| (r, d.components().count()))
        })
        .max_by_key(|(_, depth)| *depth)
        .map(|(r, _)| r)
}

/// The content root the page at `page` lives in, or `None` outside every root.
pub(crate) fn doc_context(roots: &[IndexRoot], page: &str) -> Option<ContentLocation> {
    if page.is_empty() {
        return None;
    }
    root_of(roots, Path::new(page)).map(location)
}

/// A frame's source can be a pane beside content while its host uses a team's
/// content root. Use the document's own location before the host's fallback.
pub(crate) fn frame_source_context(
    roots: &[IndexRoot],
    page: &str,
    content_root: &str,
) -> Option<ContentLocation> {
    doc_context(roots, page)
        .or_else(|| {
            Path::new(page)
                .ancestors()
                .find(|dir| {
                    dir.file_name()
                        .is_some_and(|name| name == crate::context_repo::PANES_DIR)
                })
                .and_then(|dir| dir.parent())
                .and_then(|dir| doc_context(roots, &dir.join("content").to_string_lossy()))
        })
        .or_else(|| doc_context(roots, content_root))
}

/// Join a page-supplied relative path onto `root`, refusing any `..` that
/// would climb out of it and any absolute or prefixed component.
pub(crate) fn join_inside(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut out = root.to_path_buf();
    let mut depth = 0usize;
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) => {
                out.push(s);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => {
                out.pop();
                depth -= 1;
            }
            _ => return Err(format!("refused: {rel} leaves its content root")),
        }
    }
    if depth == 0 {
        return Err("refused: a file path is required".into());
    }
    Ok(out)
}

/// Lexically drop `.`/`..` from an absolute path (no filesystem access).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Resolve what a page asked to open to a file inside a TEAM content root.
///
/// - `team` given: `path` is relative to that team's content root, in the
///   page's own org (an absolute `path` must already sit inside that root).
/// - no `team`, absolute `path`: it must sit inside a known team content root.
/// - no `team`, relative `path`: it resolves against the page's own content
///   root (as a link in the page does).
///
/// The file must exist, and its real (symlink-resolved) location must still be
/// inside the root. Private-space and all-orgs content is not opened this way.
#[cfg(test)]
pub(crate) fn resolve_open_target(
    roots: &[IndexRoot],
    page: &str,
    path: &str,
    team: Option<&str>,
) -> Result<OpenTarget, String> {
    resolve_open_target_for(roots, page, path, team, &Default::default())
}

pub(crate) fn resolve_open_target_for(
    roots: &[IndexRoot],
    page: &str,
    path: &str,
    team: Option<&str>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<OpenTarget, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("refused: a file path is required".into());
    }
    let page_root = if page.is_empty() {
        None
    } else {
        root_of(roots, Path::new(page))
    };
    let (root, file) = match team {
        Some(team) => {
            let org = page_root
                .and_then(|r| r.org.as_deref())
                .ok_or("refused: this page is in no org, so it cannot name a team")?;
            let root = roots
                .iter()
                .find(|r| {
                    r.layer == Layer::Team
                        && r.org.as_deref() == Some(org)
                        && r.team.as_deref() == Some(team)
                })
                .ok_or_else(|| format!("refused: no synced team {team:?} in org {org:?}"))?;
            let file = if Path::new(path).is_absolute() {
                normalize(Path::new(path))
            } else {
                join_inside(&root.dir, path)?
            };
            (root, file)
        }
        None if Path::new(path).is_absolute() => {
            let file = normalize(Path::new(path));
            let root = root_of(roots, &file)
                .ok_or_else(|| format!("refused: {path} is not inside a known content root"))?;
            (root, file)
        }
        None => {
            let root = page_root.ok_or(
                "refused: a relative path needs a page inside a content root; pass {team, path}",
            )?;
            (root, join_inside(&root.dir, path)?)
        }
    };
    if root.layer != Layer::Team {
        return Err(format!(
            "refused: {} is not team content (private and all-orgs files are not opened from a page)",
            path_str(&file)
        ));
    }
    if !file.starts_with(&root.dir) || file == root.dir {
        return Err(format!(
            "refused: {path} is not inside the team's content root"
        ));
    }
    if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        scope.existing(&file)?;
    }
    let real =
        std::fs::canonicalize(&file).map_err(|_| format!("not-found: {}", path_str(&file)))?;
    let real_root = std::fs::canonicalize(&root.dir)
        .map_err(|_| format!("not-found: {}", path_str(&root.dir)))?;
    if !real.starts_with(&real_root) {
        return Err(format!(
            "refused: {path} links outside the team's content root"
        ));
    }
    if !real.is_file() {
        return Err(format!("not-found: {} is not a file", path_str(&file)));
    }
    let (Some(org), Some(team)) = (root.org.clone(), root.team.clone()) else {
        return Err("refused: the content root has no team".into());
    };
    Ok(OpenTarget {
        path: path_str(&file),
        org,
        team,
        content_root: path_str(&root.dir),
    })
}

/// `codesense.context()`: the content root (org, team, layer) the page at
/// `page` lives in; `null` for a page outside every known root.
#[tauri::command]
pub async fn bridge_doc_context(
    app: tauri::AppHandle,
    page: String,
) -> Result<Option<ContentLocation>, String> {
    super::await_blocking("bridge_doc_context", move || {
        let roots = crate::context_query::known_roots(&app);
        Ok(doc_context(&roots, &page))
    })
    .await
}

/// `codesense.openFile`: validate and resolve a file the page at `page` asked to
/// open (see [`resolve_open_target`]). Opening it is the app's job.
#[tauri::command]
pub async fn bridge_resolve_open_target<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    page: String,
    path: String,
    team: Option<String>,
    frame_id: Option<String>,
) -> Result<OpenTarget, String> {
    let scope =
        crate::context_repo::caller_scope::for_page(&app, Some(&page), frame_id.as_deref())?;
    super::await_blocking("bridge_resolve_open_target", move || {
        let roots = crate::context_query::known_roots(&app);
        resolve_open_target_for(&roots, &page, &path, team.as_deref(), &scope)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        _tmp: tempfile::TempDir,
        roots: Vec<IndexRoot>,
        backend: PathBuf,
        web: PathBuf,
        private: PathBuf,
        outside: PathBuf,
    }

    fn write(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "---\nstatus: todo\n---\n").unwrap();
    }

    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let backend = base.join("orgs/acme/ctx/backend/content");
        let web = base.join("orgs/acme/ctx/web/content");
        let other_org = base.join("orgs/globex/ctx/web/content");
        let private = base.join("scratch/orgs/acme/content");
        let outside = base.join("elsewhere");
        write(&backend.join("e/SW-1.md"));
        write(&web.join("e/WEB-1.md"));
        write(&other_org.join("GX-1.md"));
        write(&private.join("mine.md"));
        write(&outside.join("secret.md"));
        let root = |dir: &Path, org: Option<&str>, team: Option<&str>, layer| IndexRoot {
            dir: dir.to_path_buf(),
            org: org.map(String::from),
            team: team.map(String::from),
            layer,
        };
        let roots = vec![
            root(&backend, Some("acme"), Some("backend"), Layer::Team),
            root(&web, Some("acme"), Some("web"), Layer::Team),
            root(&other_org, Some("globex"), Some("web"), Layer::Team),
            root(&private, Some("acme"), None, Layer::Private),
        ];
        Fx {
            _tmp: tmp,
            roots,
            backend,
            web,
            private,
            outside,
        }
    }

    fn page(fx: &Fx) -> String {
        // A pane anchors at its content root, as the host passes it.
        path_str(&fx.backend.join("board.html"))
    }

    #[test]
    fn context_names_the_root_a_page_lives_in() {
        let fx = fx();
        let ctx = doc_context(&fx.roots, &page(&fx)).unwrap();
        assert_eq!(ctx.org.as_deref(), Some("acme"));
        assert_eq!(ctx.team.as_deref(), Some("backend"));
        assert_eq!(ctx.layer, Layer::Team);
        assert_eq!(ctx.content_root, path_str(&fx.backend));
        let private = doc_context(&fx.roots, &path_str(&fx.private.join("p.html"))).unwrap();
        assert_eq!((private.team, private.layer), (None, Layer::Private));
        assert_eq!(
            doc_context(&fx.roots, &path_str(&fx.outside.join("x.html"))),
            None
        );
        assert_eq!(doc_context(&fx.roots, ""), None);
    }

    #[test]
    fn opens_relative_absolute_and_another_teams_files() {
        let fx = fx();
        let p = page(&fx);
        let own = resolve_open_target(&fx.roots, &p, "e/SW-1.md", None).unwrap();
        assert_eq!(own.path, path_str(&fx.backend.join("e/SW-1.md")));
        assert_eq!((own.org.as_str(), own.team.as_str()), ("acme", "backend"));
        let other = resolve_open_target(&fx.roots, &p, "e/WEB-1.md", Some("web")).unwrap();
        assert_eq!(other.team, "web");
        assert_eq!(other.content_root, path_str(&fx.web));
        let abs = path_str(&fx.web.join("e/WEB-1.md"));
        let by_abs = resolve_open_target(&fx.roots, &p, &abs, None).unwrap();
        assert_eq!(
            (by_abs.path.as_str(), by_abs.team.as_str()),
            (abs.as_str(), "web")
        );
        // A cross-org absolute path inside a known team root opens too.
        let gx = path_str(&fx.roots[2].dir.join("GX-1.md"));
        assert_eq!(
            resolve_open_target(&fx.roots, &p, &gx, None).unwrap().org,
            "globex"
        );
    }

    #[test]
    fn refuses_paths_outside_known_team_roots() {
        let fx = fx();
        let p = page(&fx);
        let err = |path: &str, team: Option<&str>| {
            resolve_open_target(&fx.roots, &p, path, team).unwrap_err()
        };
        assert!(err(&path_str(&fx.outside.join("secret.md")), None).starts_with("refused:"));
        assert!(err("../../../../elsewhere/secret.md", None).starts_with("refused:"));
        assert!(err("e/../../x.md", Some("web")).starts_with("refused:"));
        assert!(err("/etc/hosts", None).starts_with("refused:"));
        assert!(err(&path_str(&fx.private.join("mine.md")), None).starts_with("refused:"));
        assert!(err("x.md", Some("nope")).starts_with("refused: no synced team"));
        // An absolute path claimed for a team must be inside that team.
        assert!(err(&path_str(&fx.web.join("e/WEB-1.md")), Some("backend")).starts_with("refused:"));
        assert!(err("e/missing.md", None).starts_with("not-found:"));
        assert!(err("e", None).starts_with("not-found:"));
        assert!(err("", None).starts_with("refused:"));
        // A relative path from a page outside every root has nothing to resolve against.
        let lost = path_str(&fx.outside.join("p.html"));
        assert!(resolve_open_target(&fx.roots, &lost, "e/SW-1.md", None)
            .unwrap_err()
            .starts_with("refused:"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_link_out_of_the_root() {
        let fx = fx();
        std::os::unix::fs::symlink(fx.outside.join("secret.md"), fx.backend.join("leak.md"))
            .unwrap();
        let err = resolve_open_target(&fx.roots, &page(&fx), "leak.md", None).unwrap_err();
        assert!(err.starts_with("refused:"), "{err}");
    }

    #[test]
    fn root_of_refuses_dot_dot_escapes() {
        let fx = fx();
        let escape = fx.backend.join("../../../../../elsewhere/secret.md");
        assert!(escape.exists());
        assert!(root_of(&fx.roots, &escape).is_none());
        assert_eq!(
            root_of(&fx.roots, &fx.backend.join("e/SW-1.md")).map(|r| r.team.as_deref()),
            Some(Some("backend"))
        );
        // Not written yet: still placed by its lexical root.
        assert!(root_of(&fx.roots, &fx.backend.join("e/new.md")).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn root_of_refuses_a_link_out_of_the_root() {
        let fx = fx();
        std::os::unix::fs::symlink(fx.outside.join("secret.md"), fx.backend.join("leak.md"))
            .unwrap();
        assert!(root_of(&fx.roots, &fx.backend.join("leak.md")).is_none());
    }
}
