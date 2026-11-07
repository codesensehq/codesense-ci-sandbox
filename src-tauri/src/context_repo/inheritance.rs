// SPDX-License-Identifier: MIT
//! The inherited view of team content. Ancestors export only declared paths.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crate::agents::content_reader::{ContentReader, FsContentReader, Node};
use crate::content_engine::validate_path_segment;
use crate::domain::parse_org_yml;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;

pub(crate) fn content_owner(path: &Path) -> Option<(PathBuf, String)> {
    let path = normalize(path);
    for content in path.ancestors() {
        if content.file_name()?.to_str()? != super::TEAM_CONTENT_DIR {
            continue;
        }
        let team_dir = content.parent()?;
        let org = team_dir.parent()?;
        let team = team_dir.file_name()?.to_str()?;
        if org.join("teams.yml").is_file() && validate_path_segment(team).is_none() {
            return Some((org.to_path_buf(), team.to_string()));
        }
    }
    None
}

pub(crate) fn real_content_owner(path: &Path) -> Option<(PathBuf, String)> {
    content_owner(&real_path(path).ok()?)
}

pub(crate) fn org_slug_of(root: &Path) -> Option<String> {
    Some(
        parse_org_yml(&std::fs::read(root.join("org.yml")).ok()?)
            .ok()?
            .slug,
    )
}

pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(c.as_os_str()),
        }
    }
    out
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
fn shareable(path: &str) -> bool {
    safe_relative(path) && (path.starts_with("content/") || path.starts_with("panes/"))
}
fn missing() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "not found")
}

#[cfg(test)]
thread_local! {
    /// Canonicalize calls made by this thread (PBF-267 syscall-budget test).
    pub(crate) static CANONICALIZE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn real_path(path: &Path) -> io::Result<PathBuf> {
    let mut at = path.to_path_buf();
    let mut missing_parts = Vec::new();
    loop {
        #[cfg(test)]
        CANONICALIZE_CALLS.with(|c| c.set(c.get() + 1));
        match at.canonicalize() {
            Ok(mut real) => {
                for part in missing_parts.into_iter().rev() {
                    real.push(part);
                }
                return Ok(real);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                missing_parts.push(at.file_name().ok_or_else(missing)?.to_owned());
                if !at.pop() {
                    return Err(missing());
                }
            }
            Err(_) => return Err(missing()),
        }
    }
}

/// Per-query memo for scoped index filtering (SV-PBF-267 PR 1). It lives for one
/// query and is then dropped, so there is nothing to invalidate when
/// `sharing.yml`, `teams.yml`, `org.yml` or a root symlink changes between queries.
///
/// It replaces the per-row `real_path` + ancestor scan of [`TeamView::resolve_read`]
/// with one canonicalize per distinct root dir and one ancestor lookup per
/// distinct directory. The decision logic is the same code
/// ([`TeamView::resolve_real`]).
pub(crate) struct ScopeMemo {
    /// Rows are known to be files, so the ancestor search may start at the parent.
    rows_are_files: bool,
    real_dirs: HashMap<PathBuf, Option<PathBuf>>,
    source_dirs: HashMap<PathBuf, Option<PathBuf>>,
    slugs: HashMap<PathBuf, Option<String>>,
    views: HashMap<ViewKey, Arc<TeamView>>,
}
impl ScopeMemo {
    fn with_rows_are_files(rows_are_files: bool) -> Self {
        Self {
            rows_are_files,
            real_dirs: HashMap::new(),
            source_dirs: HashMap::new(),
            slugs: HashMap::new(),
            views: HashMap::new(),
        }
    }
    /// For a query over index records, which are always regular files.
    pub(crate) fn for_index_rows() -> Self {
        Self::with_rows_are_files(true)
    }
    /// For one arbitrary path: the ancestor search starts at the path itself.
    fn for_path() -> Self {
        Self::with_rows_are_files(false)
    }
    /// `real_path(dir)`, computed once per distinct `dir`.
    pub(crate) fn real_dir(&mut self, dir: &Path) -> Option<&Path> {
        if !self.real_dirs.contains_key(dir) {
            self.real_dirs
                .insert(dir.to_path_buf(), Result::ok(real_path(dir)));
        }
        self.real_dirs[dir].as_deref()
    }
    /// The nearest directory at or above `dir` holding `teams.yml` or `org.yml`.
    fn source_dir(&mut self, dir: &Path) -> Option<PathBuf> {
        if let Some(hit) = self.source_dirs.get(dir) {
            return hit.clone();
        }
        let found = if dir.join("teams.yml").is_file() || dir.join("org.yml").is_file() {
            Some(dir.to_path_buf())
        } else {
            dir.parent().and_then(|p| self.source_dir(p))
        };
        self.source_dirs.insert(dir.to_path_buf(), found.clone());
        found
    }
    fn slug(&mut self, org_root: &Path) -> Option<String> {
        self.slugs
            .entry(org_root.to_path_buf())
            .or_insert_with(|| org_slug_of(org_root))
            .clone()
    }
    fn view(&mut self, root: &Path, slug: &str, team: &str) -> Arc<TeamView> {
        self.views
            .entry((root.to_path_buf(), slug.to_string(), team.to_string()))
            .or_insert_with(|| team_view(root, slug, team))
            .clone()
    }
    /// Memoised [`unknown_caller_admits`] for a row whose real path is `real`.
    pub(crate) fn unknown_caller_admits_real(&mut self, real: &Path) -> bool {
        let start = if self.rows_are_files {
            real.parent()
        } else {
            Some(real)
        };
        let Some(root) = start.and_then(|d| self.source_dir(d)) else {
            return true;
        };
        let Some(slug) = self.slug(&root) else {
            return false;
        };
        let view = self.view(&root, &slug, "__unknown_caller__");
        view.resolve_real(self, real.to_path_buf(), None)
            .map_or(Admit::Denied, |read| read.admission)
            != Admit::Denied
    }
}

/// An unplaced session can still read private space, but cannot assume any team.
pub(crate) fn unknown_caller_admits(path: &Path) -> bool {
    let Ok(real) = real_path(path) else {
        return false;
    };
    let Some(root) = real
        .ancestors()
        .find(|p| p.join("org.yml").is_file() || p.join("teams.yml").is_file())
    else {
        return true;
    };
    let Some(slug) = org_slug_of(root) else {
        return false;
    };
    team_view(root, &slug, "__unknown_caller__").admits_path(&real) != Admit::Denied
}

fn profile_namespace(path: &Path) -> Option<(PathBuf, crate::domain::Principal)> {
    let profile = path.ancestors().find(|p| {
        p.parent()
            .is_some_and(|p| p.file_name().is_some_and(|n| n == "users"))
    })?;
    let home = profile.parent()?.parent()?.to_path_buf();
    let principal = crate::domain::Principal::parse_local(profile.file_name()?.to_str()?).ok()?;
    Some((home, principal))
}
pub(crate) fn view_for_content(path: &Path) -> Option<Arc<TeamView>> {
    if let Some((root, team)) = content_owner(path) {
        return Some(team_view(&root, &org_slug_of(&root)?, &team));
    }
    let (home, principal) = profile_namespace(path)?;
    let content = path
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == super::TEAM_CONTENT_DIR))?;
    let space = content.parent()?;
    let slug = format!(
        "{}/{}",
        space.parent()?.file_name()?.to_str()?,
        space.file_name()?.to_str()?
    );
    if super::scratch_space::scratch_org_root(&home, &principal, &slug)
        .ok()?
        .as_path()
        != space
    {
        return None;
    }
    let root = super::path_to_org_root(&home, &principal, &slug).ok()?;
    Some(team_view(&root, &slug, super::scratch_space::SCRATCH_TEAM))
}

type Stamp = Option<(SystemTime, u64)>;
fn stamp(path: &Path) -> Stamp {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.modified().ok()?, m.len())))
}

/// What a parent team with no `config/sharing.yml` says to its child teams and
/// to whoever nests a team under it. It names the team, never its files, so it
/// reads the same whatever the parent holds.
pub(crate) fn shares_nothing_warning(team: &str) -> String {
    format!(
        "{team} shares nothing with its child teams: it has no config/sharing.yml. \
         To share its content, add share: ['content/**'] there."
    )
}

/// The warning for nesting a team under `parent` in the org at `root`: some
/// when `parent` has no `config/sharing.yml`. Reads only; never writes the file.
pub(crate) fn nesting_warning(root: &Path, parent: &str) -> Option<String> {
    if validate_path_segment(parent).is_some() {
        return None;
    }
    let declaration = root.join(parent).join("config/sharing.yml");
    matches!(std::fs::metadata(declaration), Err(e) if e.kind() == io::ErrorKind::NotFound)
        .then(|| shares_nothing_warning(parent))
}

#[derive(Debug)]
struct ShareSet {
    include: GlobSet,
    exclude: GlobSet,
    diagnostic: Option<String>,
    /// The ancestor has no `config/sharing.yml` (an explicit `share: []` is not this).
    undeclared: bool,
}
impl ShareSet {
    /// `team`'s sharing declaration at `path`. A missing file shares nothing and
    /// says so; an explicit `share: []` shares nothing silently.
    fn load(path: &Path, team: &str) -> Self {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Declaration {
            share: Vec<String>,
        }
        let compile = || -> Result<Option<(GlobSet, GlobSet)>, String> {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.to_string()),
            };
            let doc: Declaration = serde_yaml::from_str(&text).map_err(|e| e.to_string())?;
            let mut include = GlobSetBuilder::new();
            let mut exclude = GlobSetBuilder::new();
            for pattern in doc.share {
                let (negative, pattern) = pattern
                    .strip_prefix('!')
                    .map_or((false, pattern.as_str()), |s| (true, s));
                if !shareable(pattern) {
                    return Err("only content/** and panes/** may be shared".into());
                }
                let glob = GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| e.to_string())?;
                if negative {
                    exclude.add(glob);
                } else {
                    include.add(glob);
                }
            }
            Ok(Some((
                include.build().map_err(|e| e.to_string())?,
                exclude.build().map_err(|e| e.to_string())?,
            )))
        };
        match compile() {
            Ok(Some((include, exclude))) => Self {
                include,
                exclude,
                diagnostic: None,
                undeclared: false,
            },
            Ok(None) => Self {
                include: GlobSet::empty(),
                exclude: GlobSet::empty(),
                diagnostic: Some(shares_nothing_warning(team)),
                undeclared: true,
            },
            Err(why) => {
                let diagnostic = format!("{}: {why}; nothing shared", path.display());
                log::warn!(target: "inheritance", "{diagnostic}");
                Self {
                    include: GlobSet::empty(),
                    exclude: GlobSet::empty(),
                    diagnostic: Some(diagnostic),
                    undeclared: false,
                }
            }
        }
    }
    fn admits(&self, path: &str) -> bool {
        shareable(path) && self.include.is_match(path) && !self.exclude.is_match(path)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InheritedLayer {
    team: String,
    viewer: String,
    slug: String,
    dir: PathBuf,
    gates: Vec<Arc<ShareSet>>,
}
impl InheritedLayer {
    pub(crate) fn team(&self) -> &str {
        &self.team
    }
    pub(crate) fn admits(&self, relative: &str) -> bool {
        shareable(relative) && self.gates.iter().all(|g| g.admits(relative))
    }
    pub(crate) fn content(&self) -> ContentRoot {
        ContentRoot {
            root: self.dir.join("content"),
            inherited: Some(self.clone()),
        }
    }
    pub(crate) fn exists(&self, relative: &str) -> bool {
        self.file_path(relative).is_some_and(|p| p.is_file())
    }
    pub(crate) fn open(&self, relative: &str) -> io::Result<Box<dyn io::Read>> {
        FsContentReader
            .open(&self.file_path(relative).ok_or_else(missing)?)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    missing()
                } else {
                    e
                }
            })
    }
    pub(crate) fn file_path(&self, relative: &str) -> Option<PathBuf> {
        self.read_path(relative)?;
        let path = self.dir.join(relative);
        if path.symlink_metadata().is_ok_and(|m| !m.is_file()) {
            return None;
        }
        no_symlinks(&self.dir, &path).then_some(path)
    }
    pub(crate) fn read_path(&self, relative: &str) -> Option<PathBuf> {
        if !self.admits(relative) {
            return None;
        }
        team_view(self.dir.parent()?, &self.slug, &self.viewer)
            .read_path(&self.dir.join(relative))
            .ok()
    }
    pub(crate) fn walk(&self, sub: &str) -> Vec<(String, PathBuf)> {
        if !safe_relative(sub) {
            return Vec::new();
        }
        walkdir::WalkDir::new(self.dir.join(sub))
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
            .filter_map(|e| {
                let rel = e
                    .path()
                    .strip_prefix(&self.dir)
                    .ok()?
                    .to_str()?
                    .replace('\\', "/");
                self.read_path(&rel)
                    .filter(|path| path.is_file())
                    .map(|_| (rel, e.path().to_path_buf()))
            })
            .collect()
    }
}
fn no_symlinks(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let mut at = root.to_path_buf();
    if at.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return false;
    }
    for c in rel.components() {
        at.push(c);
        if at.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            return false;
        }
    }
    true
}

/// A content directory with its inherited gate retained through resolution.
#[derive(Debug, Clone)]
pub(crate) struct ContentRoot {
    root: PathBuf,
    inherited: Option<InheritedLayer>,
}
impl ContentRoot {
    pub(crate) fn own(root: PathBuf) -> Self {
        Self {
            root,
            inherited: None,
        }
    }
    pub(crate) fn safety_boundary(&self) -> PathBuf {
        self.inherited
            .as_ref()
            .and_then(|l| l.dir.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone())
    }
    /// Match a dropped-event scope without treating the directory as permission to read it.
    pub(crate) fn overlaps(&self, scope: &Path) -> bool {
        crate::content_engine::dispatcher::paths_overlap(&self.root, scope)
    }
    /// Physical event identity only; reads still require the inherited content gates.
    pub(crate) fn event_root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn own_path(&self) -> Option<&Path> {
        self.inherited.is_none().then_some(self.root.as_path())
    }
    pub(crate) fn admits(&self, logical: &str) -> bool {
        safe_relative(logical)
            && self.inherited.as_ref().map_or_else(
                || no_symlinks(&self.root, &self.root.join(logical)),
                |l| l.admits(&format!("content/{logical}")),
            )
    }
    pub(crate) fn file_path(&self, logical: &str) -> Option<PathBuf> {
        if let Some(layer) = &self.inherited {
            return layer.file_path(&format!("content/{logical}"));
        }
        self.admits(logical).then(|| self.root.join(logical))
    }
    pub(crate) fn relative_directory(&self, path: &Path) -> Option<PathBuf> {
        let path = normalize(path);
        let real_root = real_path(&self.root).ok();
        let rel = path
            .strip_prefix(&self.root)
            .ok()
            .or_else(|| path.strip_prefix(real_root.as_ref()?).ok())?;
        self.visible(rel.to_str()?).then(|| rel.to_path_buf())
    }
    pub(crate) fn relative_for_read(&self, path: &Path) -> Option<PathBuf> {
        let path = normalize(path);
        let real_root = real_path(&self.root).ok();
        let rel = path
            .strip_prefix(&self.root)
            .ok()
            .or_else(|| path.strip_prefix(real_root.as_ref()?).ok())?;
        let logical = rel.to_str()?;
        (safe_relative(logical)
            && self
                .inherited
                .as_ref()
                .is_none_or(|l| l.admits(&format!("content/{logical}"))))
        .then(|| rel.to_path_buf())
    }
    pub(crate) fn relative(&self, path: &Path) -> Option<PathBuf> {
        let path = normalize(path);
        let rel = path.strip_prefix(&self.root).ok()?;
        self.admits(rel.to_str()?).then(|| rel.to_path_buf())
    }
    pub(crate) fn exists(&self, logical: &str) -> bool {
        self.file_path(logical).is_some_and(|p| p.is_file())
    }
    pub(crate) fn open(
        &self,
        reader: &dyn ContentReader,
        logical: &str,
    ) -> io::Result<Box<dyn io::Read>> {
        reader
            .open(&self.file_path(logical).ok_or_else(missing)?)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    missing()
                } else {
                    e
                }
            })
    }
    fn visible(&self, logical: &str) -> bool {
        if self.inherited.is_none() {
            return logical.is_empty() || safe_relative(logical);
        }
        if self
            .inherited
            .as_ref()
            .is_some_and(|l| l.read_path(&format!("content/{logical}")).is_some())
            && self.root.join(logical).is_file()
        {
            return true;
        }
        let prefix = if logical.is_empty() {
            "content".to_string()
        } else {
            format!("content/{logical}")
        };
        self.inherited
            .as_ref()
            .is_some_and(|l| !l.walk(&prefix).is_empty())
    }
    pub(crate) fn node(&self, reader: &dyn ContentReader, logical: &str) -> io::Result<Node> {
        if !self.visible(logical) {
            return Err(missing());
        }
        reader.node(&self.root.join(logical))
    }
    pub(crate) fn list(
        &self,
        reader: &dyn ContentReader,
        logical: &str,
    ) -> io::Result<Vec<std::ffi::OsString>> {
        if self.inherited.is_none() {
            return reader.list(&self.root.join(logical));
        }
        if !self.visible(logical) {
            return Err(missing());
        }
        Ok(reader
            .list(&self.root.join(logical))?
            .into_iter()
            .filter(|name| {
                let rel = if logical.is_empty() {
                    name.to_string_lossy().into_owned()
                } else {
                    format!("{logical}/{}", name.to_string_lossy())
                };
                self.visible(&rel)
            })
            .collect())
    }
    pub(crate) fn ignore_rules(
        &self,
    ) -> Option<crate::agents::manifest_resolve::context::IgnoreRules> {
        crate::agents::manifest_resolve::context::IgnoreRules::for_root(&self.root)
    }
    pub(crate) fn excludes(&self) -> Arc<crate::content_engine::TeamExcludes> {
        crate::install::projection_excludes::load_team_excludes_cached(&self.root)
    }
    pub(crate) fn config_applies_to(&self, logical: &str) -> bool {
        self.inherited.is_none() || self.exists(logical)
    }
    pub(crate) fn merge_marks(&self) -> crate::install::merge_marks::MergeMarks {
        crate::install::merge_marks::load_merge_marks_from(&super::team_config_dir_beside_content(
            &self.root,
        ))
    }
    pub(crate) fn projections(
        &self,
    ) -> Vec<crate::install::projection_manifest::RawProjectionEntry> {
        crate::install::projection_manifest::load_specs(&self.root)
            .into_iter()
            .filter(|s| self.visible(&s.source_subdir))
            .collect()
    }
    pub(crate) fn projections_admitted(
        &self,
        admits: impl Fn(&Path) -> bool,
    ) -> Vec<crate::install::projection_manifest::RawProjectionEntry> {
        self.projections()
            .into_iter()
            .filter(|spec| admits(&self.root.join(&spec.source_subdir)))
            .collect()
    }

    pub(crate) fn rebased(&self, canonical: &Path, checkout: &Path) -> Self {
        let mut out = self.clone();
        if let Ok(rel) = self.root.strip_prefix(canonical) {
            out.root = checkout.join(rel);
        }
        if let Some(layer) = &mut out.inherited {
            if let Some(relative) = layer
                .dir
                .parent()
                .and_then(|root| root.strip_prefix(canonical).ok())
            {
                let root = checkout.join(relative);
                let view = team_view(&root, &layer.slug, &layer.viewer);
                if let Some(inherited) = view.inherited.iter().find(|l| l.team == layer.team) {
                    return inherited.content();
                }
                layer.dir = root.join(&layer.team);
                layer.gates = vec![Arc::new(ShareSet {
                    include: GlobSet::empty(),
                    exclude: GlobSet::empty(),
                    diagnostic: None,
                    undeclared: false,
                })];
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Admit {
    Own,
    Inherited { team: String },
    Private,
    OtherTeam,
    Denied,
}
struct AdmittedRead {
    admission: Admit,
    path: PathBuf,
    implicit_companion: bool,
}
#[derive(Debug)]
pub(crate) struct TeamView {
    pub(crate) inherited: Vec<InheritedLayer>,
    root: PathBuf,
    team: String,
    source_org: Option<String>,
    org: String,
    reader_known: bool,
    pub(crate) diagnostics: Vec<String>,
    /// Ancestors with no `config/sharing.yml`, nearest first.
    undeclared: Vec<String>,
}
impl TeamView {
    /// What a path no tier holds adds when an ancestor shares nothing for want
    /// of a `config/sharing.yml`. The same for every missing path, whether or
    /// not that ancestor holds it, so a denied path still reads as a missing one.
    pub(crate) fn missing_path_hint(&self) -> Option<String> {
        (!self.undeclared.is_empty()).then(|| {
            self.undeclared
                .iter()
                .map(|team| format!("parent team {team} shares nothing (no config/sharing.yml)"))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
    pub(crate) fn org(&self) -> &str {
        &self.org
    }
    pub(crate) fn reader_team(&self) -> &str {
        &self.team
    }
    pub(crate) fn pane_manifest_at(
        &self,
        dir: &Path,
        owner: Option<&str>,
    ) -> Option<Result<crate::panes::manifest::PaneManifest, String>> {
        self.load_pane_manifest(dir, owner)
    }
    pub(crate) fn own_content(&self) -> ContentRoot {
        ContentRoot::own(self.root.join(&self.team).join("content"))
    }
    /// R3 companions are scoped to their owning panes folder and admitted HTML.
    pub(crate) fn pane_manifest(
        &self,
        owner: &str,
    ) -> Option<Result<crate::panes::manifest::PaneManifest, String>> {
        let dir = self.root.join(owner).join("panes");
        self.load_pane_manifest(&dir, Some(owner))
    }
    pub(crate) fn private_pane_manifest(
        &self,
        dir: &Path,
    ) -> Option<Result<crate::panes::manifest::PaneManifest, String>> {
        self.load_pane_manifest(dir, None)
    }
    fn load_pane_manifest(
        &self,
        dir: &Path,
        owner: Option<&str>,
    ) -> Option<Result<crate::panes::manifest::PaneManifest, String>> {
        let read = self
            .resolve_read(&dir.join(crate::panes::manifest::MANIFEST_FILE), owner)
            .ok()?;
        let bytes = std::fs::read(&read.path).ok()?;
        Some(crate::panes::manifest::parse(&bytes).map(|mut manifest| {
            manifest.panes.retain(|p| {
                crate::panes::pane_id_of_file(&p.html) == Some(p.id.as_str())
                    && self
                        .resolve_read(&dir.join(&p.html), None)
                        .is_ok_and(|html| !read.implicit_companion || (html.path.parent() == read.path.parent() && matches!(html.admission, Admit::Inherited { team } if owner == Some(team.as_str()))))
                    // Architecture: restriction follows the resolved manifest owner,
                    // not a same-named team in another org. resolve_read has already
                    // checked the real owner before reading the manifest bytes.
                    && match &read.admission {
                        Admit::Inherited { team } => self.inherited.iter()
                            .find(|layer| &layer.team == team)
                            .is_none_or(|layer| layer.admits(&format!("panes/{}", p.html))),
                        _ => true,
                    }
            });
            manifest
        }))
    }
    /// This view's own team folder. A path only, for scoping watchers; never read through it.
    pub(crate) fn own_team_dir(&self) -> PathBuf {
        self.root.join(&self.team)
    }
    /// The path a pane file of `owner` has or had, for watcher recovery of a deleted file.
    /// The view's own team always; an inherited owner only when its sharing admits that pane
    /// (a glob check, so it works for a deleted file). A path only; never read through it.
    pub(crate) fn pane_recovery_path(&self, owner: &str, id: &str) -> Option<PathBuf> {
        if validate_path_segment(owner).is_some() || !crate::panes::is_valid_pane_id(id) {
            return None;
        }
        let relative = format!("panes/{id}.html");
        let admitted = owner == self.team
            || self
                .inherited
                .iter()
                .any(|layer| layer.team == owner && layer.admits(&relative));
        admitted.then(|| self.root.join(owner).join(relative))
    }
    pub(crate) fn pane_file(&self, owner: &str, id: &str) -> Option<PathBuf> {
        if validate_path_segment(owner).is_some() || !crate::panes::is_valid_pane_id(id) {
            return None;
        }
        let path = self
            .root
            .join(owner)
            .join("panes")
            .join(format!("{id}.html"));
        self.read_path(&path).ok()?;
        Some(path)
    }
    pub(crate) fn pane_content_root(&self, owner: &str) -> Option<PathBuf> {
        if validate_path_segment(owner).is_some() {
            return None;
        }
        let authority = if !self.reader_known
            || owner == self.team
            || self.inherited.iter().any(|l| l.team == owner)
        {
            self.team.as_str()
        } else {
            owner
        };
        Some(self.root.join(authority).join("content"))
    }
    pub(crate) fn team_names(&self) -> Vec<String> {
        std::iter::once(self.team.clone())
            .chain(self.inherited.iter().map(|l| l.team.clone()))
            .collect()
    }
    pub(crate) fn admits_path(&self, path: &Path) -> Admit {
        self.resolve_read(path, None)
            .map_or(Admit::Denied, |read| read.admission)
    }
    /// [`Self::admits_path`] for a canonical path, through a per-query memo.
    pub(crate) fn admits_real(&self, memo: &mut ScopeMemo, real: PathBuf) -> Admit {
        self.resolve_real(memo, real, None)
            .map_or(Admit::Denied, |read| read.admission)
    }
    pub(crate) fn read_path(&self, path: &Path) -> io::Result<PathBuf> {
        self.resolve_read(path, None).map(|read| read.path)
    }
    pub(crate) fn read_path_strict(&self, path: &Path) -> io::Result<PathBuf> {
        let resolved = self.read_path(path)?;
        let lexical = normalize(path);
        let expected = lexical
            .strip_prefix(&self.root)
            .ok()
            .map(|rel| real_path(&self.root).map(|r| r.join(rel)))
            .transpose()?
            .or_else(|| {
                let (home, principal) = profile_namespace(&lexical)?;
                let profile = super::path_to_user_home(&home, &principal);
                let relative = lexical.strip_prefix(&profile).ok()?;
                Some(real_path(&profile).ok()?.join(relative))
            });
        if expected.as_ref() == Some(&resolved) {
            Ok(resolved)
        } else {
            Err(missing())
        }
    }
    /// Architecture: structural directories in a selected checkout are visible
    /// only when that checkout and the caller both admit a descendant.
    pub(crate) fn visible_directory(&self, path: &Path) -> bool {
        let Ok(path) = real_path(path) else {
            return false;
        };
        if !path.is_dir() {
            return false;
        }
        let Ok(caller_root) = real_path(&self.root) else {
            return false;
        };
        let Ok(Some(root)) = self.source_root_for(&path, &caller_root) else {
            return false;
        };
        let Ok(relative) = path.strip_prefix(&root) else {
            return false;
        };
        let mut parts = relative.components();
        let Some(Component::Normal(owner)) = parts.next() else {
            return false;
        };
        let Some(owner) = owner.to_str() else {
            return false;
        };
        if !self.inherited.iter().any(|layer| layer.team == owner) {
            return false;
        }
        let source = team_view(&root, &self.org, &self.team);
        source
            .inherited
            .iter()
            .find(|layer| layer.team == owner)
            .is_some_and(|layer| {
                layer
                    .walk(&parts.as_path().to_string_lossy())
                    .iter()
                    .any(|(_, file)| self.resolve_read(file, None).is_ok())
            })
    }

    fn source_root_for(&self, path: &Path, root: &Path) -> io::Result<Option<PathBuf>> {
        if path.starts_with(root) {
            return Ok(Some(root.to_path_buf()));
        }
        if let Some(source) = path
            .ancestors()
            .find(|p| p.join("teams.yml").is_file() || p.join("org.yml").is_file())
        {
            let slug = org_slug_of(source).ok_or_else(missing)?;
            return Ok((self.org == slug).then(|| source.to_path_buf()));
        }
        Ok(None)
    }

    fn resolve_read(&self, path: &Path, companion_owner: Option<&str>) -> io::Result<AdmittedRead> {
        let path = real_path(path)?;
        self.resolve_real(&mut ScopeMemo::for_path(), path, companion_owner)
    }

    /// The same decision as [`Self::resolve_read`] for an already canonical `path`,
    /// with the reader's root, the source-root scan and team views taken from `memo`.
    fn resolve_real(
        &self,
        memo: &mut ScopeMemo,
        path: PathBuf,
        companion_owner: Option<&str>,
    ) -> io::Result<AdmittedRead> {
        let root = memo.real_dir(&self.root).ok_or_else(missing)?.to_path_buf();
        let source_root = if path.starts_with(&root) {
            Some(root.clone())
        } else {
            let start = if memo.rows_are_files {
                path.parent()
            } else {
                Some(path.as_path())
            };
            match start.and_then(|d| memo.source_dir(d)) {
                Some(source) => {
                    let slug = memo.slug(&source).ok_or_else(missing)?;
                    (self.org == slug).then_some(source)
                }
                None => None,
            }
        };
        let Some(source_root) = source_root else {
            return Ok(AdmittedRead {
                admission: Admit::Private,
                path,
                implicit_companion: false,
            });
        };
        let relative = path.strip_prefix(&source_root).map_err(|_| missing())?;
        let mut parts = relative.components();
        let owner = match parts.next() {
            Some(Component::Normal(owner)) => owner.to_str(),
            _ => None,
        };
        let relative = parts.as_path().to_string_lossy().replace('\\', "/");
        let source_view =
            (source_root != root).then(|| memo.view(&source_root, &self.org, &self.team));
        if self.source_org.as_deref() == Some(self.org.as_str())
            && !self.reader_known
            && owner != Some(self.team.as_str())
        {
            return Err(missing());
        }
        if self.source_org.as_deref() != Some(self.org.as_str())
            && source_view.as_ref().is_some_and(|view| {
                owner != Some(self.team.as_str())
                    && (!view.reader_known
                        || view
                            .inherited
                            .iter()
                            .any(|l| Some(l.team.as_str()) == owner))
            })
        {
            return Err(missing());
        }
        if owner == Some(self.team.as_str()) {
            return Ok(AdmittedRead {
                admission: Admit::Own,
                path,
                implicit_companion: false,
            });
        }
        let Some(layer) = owner.and_then(|owner| self.inherited.iter().find(|l| l.team == owner))
        else {
            return Ok(AdmittedRead {
                admission: Admit::OtherTeam,
                path,
                implicit_companion: false,
            });
        };
        let source_layer = match &source_view {
            Some(view) => view
                .inherited
                .iter()
                .find(|l| l.team == layer.team)
                .ok_or_else(missing)?,
            None => layer,
        };
        let implicit_companion = !(layer.admits(&relative) && source_layer.admits(&relative));
        if implicit_companion {
            if companion_owner != Some(layer.team.as_str())
                || path.parent() != Some(source_root.join(&layer.team).join("panes").as_path())
                || !source_layer.walk("panes").iter().any(|(rel, html)| {
                    rel.strip_prefix("panes/").is_some_and(|p| crate::panes::pane_id_of_file(p).is_some())
                        && self.resolve_read(html, None).is_ok_and(|read| read.path.parent() == path.parent() && matches!(read.admission, Admit::Inherited { team } if team == layer.team))
                }) { return Err(missing()); }
        }
        Ok(AdmittedRead {
            admission: Admit::Inherited {
                team: layer.team.clone(),
            },
            path,
            implicit_companion,
        })
    }
}

type ViewKey = (PathBuf, String, String);
struct CachedView {
    stamps: Vec<(PathBuf, Stamp)>,
    view: Arc<TeamView>,
}
static VIEWS: OnceLock<Mutex<HashMap<ViewKey, CachedView>>> = OnceLock::new();
pub(crate) fn team_view(root: &Path, slug: &str, team: &str) -> Arc<TeamView> {
    let key = (root.to_path_buf(), slug.to_string(), team.to_string());
    let cache = VIEWS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some(hit) = cache.get(&key) {
            if hit.stamps.iter().all(|(p, s)| stamp(p) == *s) {
                return hit.view.clone();
            }
        }
    }
    let mut stamps = vec![
        (root.join("teams.yml"), stamp(&root.join("teams.yml"))),
        (root.join("org.yml"), stamp(&root.join("org.yml"))),
    ];
    let org = crate::domain::load_org(slug, root, None).org;
    let reader_known = super::scratch_space::is_scratch_team(team)
        || org.as_ref().is_some_and(|o| o.team(team).is_some());
    let chain = org.map(|o| o.team_chain(team)).unwrap_or_default();
    let mut inherited = Vec::new();
    let mut gates = Vec::new();
    let mut diagnostics = Vec::new();
    let mut undeclared = Vec::new();
    if validate_path_segment(team).is_none() {
        for ancestor in chain
            .into_iter()
            .skip(1)
            .filter(|t| validate_path_segment(t).is_none())
        {
            let path = root.join(&ancestor).join("config/sharing.yml");
            stamps.push((path.clone(), stamp(&path)));
            let shares = Arc::new(ShareSet::load(&path, &ancestor));
            if let Some(d) = &shares.diagnostic {
                diagnostics.push(d.clone());
            }
            if shares.undeclared {
                undeclared.push(ancestor.clone());
            }
            gates.push(shares);
            inherited.push(InheritedLayer {
                team: ancestor.clone(),
                viewer: team.to_string(),
                slug: slug.to_string(),
                dir: root.join(&ancestor),
                gates: gates.clone(),
            });
        }
    }
    let view = Arc::new(TeamView {
        inherited,
        root: root.to_path_buf(),
        team: team.to_string(),
        source_org: org_slug_of(root),
        org: slug.to_string(),
        reader_known,
        diagnostics,
        undeclared,
    });
    if let Ok(mut cache) = cache.lock() {
        cache.insert(
            key,
            CachedView {
                stamps,
                view: view.clone(),
            },
        );
    }
    view
}

pub(crate) fn inherited_by_active_team(
    path: &Path,
    active: Option<(&str, &str)>,
) -> Option<(PathBuf, String)> {
    let (slug, team) = active?;
    let (root, owner) = content_owner(path)?;
    let view = team_view(&root, slug, team);
    if view.source_org.as_deref() != Some(slug) {
        return None;
    }
    matches!(view.admits_path(path), Admit::Inherited { .. }).then_some((root, owner))
}

#[cfg(test)]
pub(crate) fn test_fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("users/1/orgs/acme/ctx");
    let write = |p: PathBuf, s: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    };
    write(
        root.join("org.yml"),
        "slug: acme/ctx\ndisplay_name: Acme\ncreated_at: \"2026-01-01T00:00:00Z\"\n",
    );
    write(
        root.join("people.yml"),
        "people:\n  - github_user_id: 1\n    github_username: alice\n",
    );
    write(root.join("teams.yml"), "teams:\n  - name: child\n    parent_team: parent\n  - name: parent\n    parent_team: grand\n  - name: grand\n  - name: sibling\n");
    for t in ["grand", "parent", "child", "sibling"] {
        for f in ["secret.md", "shared.md"] {
            write(
                root.join(format!("{t}/content/{f}")),
                &format!("{t}-{f}-bytes"),
            );
        }
        write(
            root.join(format!("{t}/content/.mcp.json")),
            &format!("{{\"mcpServers\":{{\"{t}-srv\":{{\"command\":\"{t}\"}}}}}}"),
        );
        write(
            root.join(format!("{t}/content/.claude/skills/{t}/SKILL.md")),
            &format!("{t}-skill"),
        );
        write(
            root.join(format!("{t}/panes/{t}.html")),
            &format!("<html>{t}</html>"),
        );
    }
    write(root.join("grand/config/sharing.yml"), "share:\n  - content/shared.md\n  - content/.claude/skills/grand/**\n  - content/.mcp.json\n  - panes/grand.html\n");
    write(
        root.join("parent/config/sharing.yml"),
        "share:\n  - content/shared.md\n  - panes/parent.html\n",
    );
    (tmp, root)
}

#[cfg(all(test, unix))]
pub(crate) struct AliasCase {
    pub id: String,
    pub origin: &'static str,
    pub content: PathBuf,
    pub pane: PathBuf,
    pub target_content: PathBuf,
    pub admitted: bool,
}
#[cfg(all(test, unix))]
pub(crate) struct AliasFixture {
    pub home: tempfile::TempDir,
    pub root: PathBuf,
    pub private: PathBuf,
    pub cases: Vec<AliasCase>,
}
#[cfg(all(test, unix))]
pub(crate) fn alias_fixture() -> AliasFixture {
    let (home, root) = test_fixture();
    let (space, _) = super::scratch_space::ensure_org_space(
        home.path(),
        &crate::domain::Principal::from(1u64),
        "acme/ctx",
    )
    .unwrap();
    let private = space.root();
    std::fs::create_dir_all(private.join("panes")).unwrap();
    std::fs::write(private.join("content/own.md"), "private-owned bytes").unwrap();
    std::fs::write(
        private.join("panes/own.html"),
        "<title>Private owned pane</title>",
    )
    .unwrap();
    std::fs::write(root.join("parent/panes/secret.html"), "<title>UNSHARED_ANCESTOR_SENTINEL</title><meta name=\"codesense:icon\" content=\"UNSHARED_ANCESTOR_ICON\">").unwrap();
    std::fs::write(
        root.join("sibling/panes/sibling.html"),
        "<title>SIBLING_SENTINEL</title>",
    )
    .unwrap();
    let mut sharing = std::fs::read_to_string(root.join("parent/config/sharing.yml")).unwrap();
    sharing.push_str("  - 'content/alias-inherited-*.md'\n  - 'panes/alias-inherited-*.html'\n");
    std::fs::write(root.join("parent/config/sharing.yml"), sharing).unwrap();
    let sibling_content_chain = root.join("sibling/content/ancestor-link.md");
    let sibling_pane_chain = root.join("sibling/panes/ancestor-link.html");
    std::os::unix::fs::symlink(
        root.join("parent/content/secret.md"),
        &sibling_content_chain,
    )
    .unwrap();
    std::os::unix::fs::symlink(root.join("parent/panes/secret.html"), &sibling_pane_chain).unwrap();
    let targets = [
        (
            "own",
            root.join("child/content/shared.md"),
            root.join("child/panes/child.html"),
            true,
        ),
        (
            "private",
            private.join("content/own.md"),
            private.join("panes/own.html"),
            true,
        ),
        (
            "shared",
            root.join("parent/content/shared.md"),
            root.join("parent/panes/parent.html"),
            true,
        ),
        (
            "unshared",
            root.join("parent/content/secret.md"),
            root.join("parent/panes/secret.html"),
            false,
        ),
        (
            "sibling",
            root.join("sibling/content/shared.md"),
            root.join("sibling/panes/sibling.html"),
            true,
        ),
        (
            "sibling-chain",
            sibling_content_chain,
            sibling_pane_chain,
            false,
        ),
    ];
    let mut cases = Vec::new();
    for (origin, base) in [
        ("own", root.join("child")),
        ("private", private.clone()),
        ("inherited", root.join("parent")),
    ] {
        for (target, target_content, target_pane, admitted) in &targets {
            let id = format!("alias-{origin}-{target}");
            let content = base.join("content").join(format!("{id}.md"));
            let pane = base.join("panes").join(format!("{id}.html"));
            std::os::unix::fs::symlink(target_content, &content).unwrap();
            std::os::unix::fs::symlink(target_pane, &pane).unwrap();
            cases.push(AliasCase {
                id,
                origin,
                content,
                pane,
                target_content: target_content.clone(),
                admitted: *admitted,
            });
        }
    }
    AliasFixture {
        home,
        root,
        private,
        cases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declaration_fails_closed_and_reports_diagnostics() {
        let (_tmp, root) = test_fixture();
        for text in [
            None,
            Some("share: ["),
            Some("share: ['content/[']"),
            Some("share: ['config/**']"),
        ] {
            let path = root.join("parent/config/sharing.yml");
            if let Some(text) = text {
                std::fs::write(&path, text).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
            let view = team_view(&root, "acme/ctx", "child");
            assert_eq!(
                view.diagnostics.len(),
                1,
                "{text:?}: {:?}",
                view.diagnostics
            );
            if text.is_none() {
                let message = &view.diagnostics[0];
                assert!(message.contains("parent ") && message.contains("config/sharing.yml"));
            }
            assert!(view.inherited.iter().all(|l| l.walk("content").is_empty()));
        }
    }
    #[test]
    fn multilevel_cache_intersects_every_hop() {
        let (_tmp, root) = test_fixture();
        let view = team_view(&root, "acme/ctx", "child");
        let grand = &view.inherited[1];
        assert!(grand.exists("content/shared.md"));
        assert!(!grand.exists("content/.claude/skills/grand/SKILL.md"));
        std::fs::write(root.join("parent/config/sharing.yml"), "share: []\n").unwrap();
        assert!(!team_view(&root, "acme/ctx", "child").inherited[1].exists("content/shared.md"));
    }
    #[test]
    fn watcher_paths_honour_sharing_and_never_leave_the_view() {
        let (_tmp, root) = test_fixture();
        let view = team_view(&root, "acme/ctx", "child");
        assert_eq!(view.own_team_dir(), root.join("child"));
        let gone = |team: &str, id: &str| view.pane_recovery_path(team, id);
        // Own team: always. Inherited: only a pane its sharing admits, deleted or not.
        assert_eq!(gone("child", "gone"), Some(root.join("child/panes/gone.html")));
        assert_eq!(
            gone("parent", "parent"),
            Some(root.join("parent/panes/parent.html"))
        );
        assert_eq!(gone("parent", "secret-pane"), None, "unshared pane id");
        // An ancestor with no sharing declaration shares nothing.
        std::fs::remove_file(root.join("parent/config/sharing.yml")).unwrap();
        let view = team_view(&root, "acme/ctx", "child");
        assert_eq!(view.pane_recovery_path("parent", "parent"), None);
        for team in ["sibling", "missing", "../parent", ""] {
            assert_eq!(view.pane_recovery_path(team, "gone"), None, "{team:?}");
        }
        assert_eq!(view.pane_recovery_path("child", "../x"), None);
    }
    #[test]
    fn denied_and_missing_have_the_same_error() {
        let (_tmp, root) = test_fixture();
        let view = team_view(&root, "acme/ctx", "child");
        let errors: Vec<_> = ["content/secret.md", "content/missing.md"]
            .iter()
            .map(|p| {
                let e = view.inherited[0].open(p).err().unwrap();
                (e.kind(), e.to_string())
            })
            .collect();
        assert_eq!(errors[0], errors[1]);
    }
}

impl From<PathBuf> for ContentRoot {
    fn from(root: PathBuf) -> Self {
        Self::own(root)
    }
}

#[cfg(test)]
mod declaration_and_scope_tests {
    use super::*;
    #[test]
    fn share_globs_and_exclusions_only_export_content_and_panes() {
        let (_home, root) = test_fixture();
        std::fs::write(
            root.join("parent/config/sharing.yml"),
            "share: ['content/**', 'panes/**', '!content/secret.md']\n",
        )
        .unwrap();
        let view = team_view(&root, "acme/ctx", "child");
        let parent = &view.inherited[0];
        assert!(parent.exists("content/shared.md"));
        assert!(parent.exists("panes/parent.html"));
        assert!(!parent.exists("content/secret.md"));
        assert!(!parent.admits("config/sharing.yml"));
        assert!(!parent.admits("content/../config/sharing.yml"));
        let denied = parent.open("content/secret.md").err().unwrap();
        let missing = parent.open("content/missing.md").err().unwrap();
        assert_eq!(
            (denied.kind(), denied.to_string()),
            (missing.kind(), missing.to_string())
        );
    }
    #[test]
    fn missing_sharing_yml_names_the_parent() {
        let (_home, root) = test_fixture();
        std::fs::remove_file(root.join("parent/config/sharing.yml")).unwrap();
        let view = team_view(&root, "acme/ctx", "child");
        assert_eq!(view.diagnostics, [shares_nothing_warning("parent")]);
        assert_eq!(
            view.diagnostics[0],
            "parent shares nothing with its child teams: it has no config/sharing.yml. \
             To share its content, add share: ['content/**'] there."
        );
        assert_eq!(
            view.missing_path_hint().as_deref(),
            Some("parent team parent shares nothing (no config/sharing.yml)")
        );
        assert!(!view.inherited[0].exists("content/shared.md"));
        assert!(!root.join("parent/config/sharing.yml").exists());
        // Writing the file clears the warning without a restart.
        std::fs::write(
            root.join("parent/config/sharing.yml"),
            "share: ['content/**']\n",
        )
        .unwrap();
        let view = team_view(&root, "acme/ctx", "child");
        assert!(view.diagnostics.is_empty(), "{:?}", view.diagnostics);
        assert_eq!(view.missing_path_hint(), None);
        assert!(view.inherited[0].exists("content/shared.md"));
    }
    #[test]
    fn empty_share_list_is_silent() {
        let (_home, root) = test_fixture();
        std::fs::write(root.join("parent/config/sharing.yml"), "share: []\n").unwrap();
        let view = team_view(&root, "acme/ctx", "child");
        assert!(view.diagnostics.is_empty(), "{:?}", view.diagnostics);
        assert_eq!(view.missing_path_hint(), None);
        assert!(view.inherited.iter().all(|l| l.walk("content").is_empty()));
    }
    #[test]
    fn root_team_has_no_sharing_diagnostic() {
        let (_home, root) = test_fixture();
        // Neither a root team nor a team that is nobody's parent speaks, with
        // or without a sharing file of its own.
        std::fs::remove_file(root.join("grand/config/sharing.yml")).unwrap();
        for team in ["grand", "sibling"] {
            let view = team_view(&root, "acme/ctx", team);
            assert!(
                view.diagnostics.is_empty(),
                "{team}: {:?}",
                view.diagnostics
            );
            assert_eq!(view.missing_path_hint(), None, "{team}");
        }
    }
    #[test]
    fn only_the_non_sharing_ancestor_is_named() {
        let (_home, root) = test_fixture();
        std::fs::remove_file(root.join("grand/config/sharing.yml")).unwrap();
        let view = team_view(&root, "acme/ctx", "child");
        assert_eq!(view.diagnostics, [shares_nothing_warning("grand")]);
        assert_eq!(
            view.missing_path_hint().as_deref(),
            Some("parent team grand shares nothing (no config/sharing.yml)")
        );
        assert!(view.inherited[0].exists("content/shared.md"));
        assert!(!view.inherited[1].exists("content/shared.md"));
    }
    #[test]
    fn nesting_warning_reads_and_never_writes_the_declaration() {
        let (_home, root) = test_fixture();
        assert_eq!(nesting_warning(&root, "parent"), None);
        std::fs::write(root.join("parent/config/sharing.yml"), "share: []\n").unwrap();
        assert_eq!(nesting_warning(&root, "parent"), None);
        std::fs::remove_file(root.join("parent/config/sharing.yml")).unwrap();
        assert_eq!(
            nesting_warning(&root, "parent"),
            Some(shares_nothing_warning("parent"))
        );
        assert!(!root.join("parent/config/sharing.yml").exists());
        assert_eq!(nesting_warning(&root, "../parent"), None);
    }
    #[test]
    fn the_private_space_is_unchanged_and_never_inherited() {
        let (home, root) = test_fixture();
        let (space, _) = crate::context_repo::scratch_space::ensure_org_space(
            home.path(),
            &crate::domain::Principal::from(1u64),
            "acme/ctx",
        )
        .unwrap();
        let private = space.content_dir().join("private.md");
        std::fs::create_dir_all(private.parent().unwrap()).unwrap();
        std::fs::write(&private, "personal-only").unwrap();
        let view = team_view(&root, "acme/ctx", "child");
        assert_eq!(view.admits_path(&private), Admit::Private);
        assert!(view
            .inherited
            .iter()
            .all(|l| !l.exists("content/private.md")));
        assert_eq!(std::fs::read_to_string(private).unwrap(), "personal-only");
    }
    #[test]
    fn a_sibling_keeps_its_own_files_without_inheriting_the_chain() {
        let (_home, root) = test_fixture();
        let view = team_view(&root, "acme/ctx", "sibling");
        assert!(view.inherited.is_empty());
        assert!(view.own_content().exists("secret.md"));
        assert_eq!(
            view.admits_path(&root.join("parent/content/shared.md")),
            Admit::OtherTeam
        );
    }
    #[test]
    fn a_parent_still_reads_its_own_unshared_content() {
        let (_home, root) = test_fixture();
        let parent = team_view(&root, "acme/ctx", "parent");
        assert_eq!(
            parent.admits_path(&root.join("parent/content/secret.md")),
            Admit::Own
        );
        assert!(parent.own_content().exists("secret.md"));
        assert!(!team_view(&root, "acme/ctx", "child").inherited[0].exists("content/secret.md"));
    }
}

#[cfg(test)]
mod checkout_tests {
    use super::*;
    #[test]
    fn a_rebased_layer_uses_the_checkouts_sharing_declarations() {
        let (home, root) = test_fixture();
        let checkout = home.path().join("retained-context");
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            let target = checkout.join(entry.path().strip_prefix(&root).unwrap());
            if entry.file_type().is_dir() {
                std::fs::create_dir_all(target).unwrap();
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
        let child = team_view(&root, "acme/ctx", "child");
        let shared = checkout.join("grand/content/shared.md");
        assert!(child.read_path(&shared).is_ok());
        std::fs::write(
            checkout.join("parent/config/sharing.yml"),
            "share: ['content/shared.md', 'panes/**']\n",
        )
        .unwrap();
        assert!(child.read_path(&shared).is_ok());
        std::fs::write(
            checkout.join("parent/config/sharing.yml"),
            "share: ['panes/**']\n",
        )
        .unwrap();
        assert!(
            child.read_path(&shared).is_err(),
            "every real-source intermediate must export the file"
        );
        std::fs::write(
            checkout.join("parent/config/sharing.yml"),
            "share: ['content/shared.md', 'panes/**']\n",
        )
        .unwrap();
        std::fs::write(
            root.join("parent/config/sharing.yml"),
            "share: ['panes/**']\n",
        )
        .unwrap();
        assert!(
            team_view(&root, "acme/ctx", "child")
                .read_path(&shared)
                .is_err(),
            "every selected intermediate must also export the file"
        );
        std::fs::write(
            root.join("parent/config/sharing.yml"),
            "share: ['content/shared.md', 'panes/**']\n",
        )
        .unwrap();
        std::fs::remove_file(checkout.join("org.yml")).unwrap();
        assert!(
            child.read_path(&shared).is_err(),
            "an unidentified context source fails closed"
        );
        std::fs::copy(root.join("org.yml"), checkout.join("org.yml")).unwrap();
        let view = team_view(&root, "acme/ctx", "child");
        let parent = view.inherited[0].content();
        assert!(parent.exists("shared.md"));
        std::fs::write(checkout.join("parent/config/sharing.yml"), "share: []\n").unwrap();
        assert!(!parent.rebased(&root, &checkout).exists("shared.md"));
        assert!(parent.exists("shared.md"));
    }
}

#[cfg(test)]
mod resolved_scope_tests {
    use super::*;
    #[test]
    fn non_ancestor_targets_keep_baseline_admission() {
        let (home, root) = test_fixture();
        let view = team_view(&root, "acme/ctx", "child");
        assert_eq!(
            view.admits_path(&root.join("sibling/content/shared.md")),
            Admit::OtherTeam
        );
        let unrelated = home.path().join("unrelated.md");
        std::fs::write(&unrelated, "baseline").unwrap();
        assert_eq!(view.admits_path(&unrelated), Admit::Private);
        assert!(view.read_path(&unrelated).is_ok());
    }
}
