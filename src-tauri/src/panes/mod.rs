// SPDX-License-Identifier: MIT
//! Custom right panes: one HTML file per pane at `<space>/panes/<id>.html`.
//!
//! A space is a team directory (`{org_root}/{team}/`) or the user's private
//! space for an org. `panes/` sits beside `content/` and `config/` and is not a
//! layer marker, so a pane is never content: it is not in the file tree and is
//! never projected. A team's panes travel with the whole-repo sync like
//! `config/`; private panes stay on this machine.
//!
//! A pane is discovered by its filename alone (`<id>.html`, `id` matching
//! `[a-z0-9][a-z0-9-]*`). Anything else in `panes/` (a `lib/chart.js`, an image)
//! is never a pane, only an asset a pane may load relatively. Declarations are
//! validated during discovery; rejected panes remain visible in the listing.

mod builtin;
pub mod commands;
mod identity;
pub mod install;
pub mod manifest;
pub mod profile;
pub(crate) use builtin::{builtin_at, builtin_panes};
pub use identity::{DashboardRef, DashboardScope};
pub mod watch;

#[cfg(test)]
mod declaration_tests;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::content_engine::validate_path_segment;
use crate::context_repo::scratch_space::{self, is_scratch_team};
use crate::context_repo::{path_to_org_root, path_to_team_content_dir, path_to_team_panes_dir};
use crate::domain::Principal;

/// The event a pane change on disk is emitted as.
pub const PANE_CHANGED_EVENT: &str = "pane-changed";

/// The event that asks the app to place and reveal a pane's panel.
pub const PANE_OPEN_EVENT: &str = "pane-open";

/// The extension that makes a file in `panes/` a pane.
const PANE_EXTENSION: &str = "html";

/// The longest pane id: it is a file name, so it stays well inside every
/// filesystem's name limit.
pub const MAX_PANE_ID_LEN: usize = 64;

/// Whether `id` names a pane: `[a-z0-9][a-z0-9-]*`, at most
/// [`MAX_PANE_ID_LEN`] characters.
pub fn is_valid_pane_id(id: &str) -> bool {
    if id.len() > MAX_PANE_ID_LEN {
        return false;
    }
    let mut bytes = id.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The pane id a `panes/` entry named `file_name` is, if it is a pane.
pub(crate) fn pane_id_of_file(file_name: &str) -> Option<&str> {
    let id = file_name.strip_suffix(PANE_EXTENSION)?.strip_suffix('.')?;
    is_valid_pane_id(id).then_some(id)
}

/// Which kind of space a pane lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneScope {
    Team,
    Private,
}

/// The right-pane panel id a pane registers as: `pane:<org>:<team>/<id>` for a
/// team pane (the app's team key), `pane:private:<org>/<id>` for a private one
/// (a private space is per org, so two orgs' private panes never share an id).
pub fn pane_panel_id(scope: PaneScope, org: &str, team: Option<&str>, id: &str) -> String {
    match (scope, team) {
        (PaneScope::Team, Some(team)) => format!("pane:{org}:{team}/{id}"),
        _ => format!("pane:private:{org}/{id}"),
    }
}

/// One discovered pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneEntry {
    pub dashboard_ref: DashboardRef,
    pub declaration: DashboardDeclaration,
    pub status: PaneStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<String>,
    pub update_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_source_version: Option<String>,
    pub id: String,
    pub scope: PaneScope,
    /// The team whose `panes/` holds it; absent for a private pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// The document's `<title>`, or the id when it has none.
    pub title: String,
    /// The Lucide icon name its `<meta name="codesense:icon">` asks for, as written;
    /// the app shows an unknown name as a visible "?" glyph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The surface the dashboard declares, independent of its effective grant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    /// The pane file's absolute path.
    pub path: String,
    /// The `panes/` dir it lives in: the root its relative assets resolve from.
    pub panes_dir: String,
    /// The right-pane panel id it registers as.
    pub panel_id: String,
    /// Last modification time, RFC3339; absent when the filesystem has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

/// The panes a team sees, and the content root a pane's reads resolve against
/// (the team's own `content/`, so a pane sees the same stories a dashboard in
/// that team does).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneListing {
    pub content_root: String,
    /// The team's resolution chain, nearest first: the teams whose panes it sees.
    pub chain: Vec<String>,
    pub panes: Vec<PaneEntry>,
    /// Bundled Inbox for recovery when an overriding dashboard fails.
    pub builtin_inbox: Option<PaneEntry>,
    /// Optional overlays from each chain layer's `pane-manifest.yaml`, nearest
    /// layer first. A layer with no manifest contributes nothing; a layer with
    /// a malformed one is skipped (the discovery pass still wins). The viewer
    /// may consult these for `showsOn` / `priority` / `collapsedByDefault`
    /// hints without re-reading the files.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub manifests: Vec<PaneManifestLayer>,
}

/// One layer's parsed `pane-manifest.yaml`, tagged with the layer it came from
/// so the viewer can match overlays to panes by `(scope, team, id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneManifestLayer {
    pub scope: PaneScope,
    /// The team whose `panes/pane-manifest.yaml` this is; absent for a private layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub manifest: manifest::PaneManifest,
}

/// The text of the first `<title>` in `html`, entity-decoded and trimmed;
/// `None` when there is none or it is blank.
pub(crate) fn pane_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find("</title")?;
    let text = decode_entities(&html[start..end]);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// The longest icon name kept; anything longer is not a Lucide name.
const ICON_NAME_MAX: usize = 64;

/// The `content` of the first `<meta name="codesense:icon" content="…">` in
/// `html`, trimmed; `None` when there is none, or it is blank or too long.
pub(crate) fn pane_icon(html: &str) -> Option<String> {
    pane_meta(html, "codesense:icon")
        .filter(|icon| !icon.is_empty() && icon.len() <= ICON_NAME_MAX)
        .map(str::to_string)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneStatus {
    Ready,
    Rejected,
}

/// Where a dashboard command's chord fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandScope {
    Global,
    SlotVisible,
    SlotFocused,
}

/// A command a slot dashboard declares; the host owns its chord, palette row and dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCommand {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Canonical `CmdOrCtrl+Shift+Alt+Key`. A reserved chord is accepted here and
    /// registered unbound by the host, which explains why in Settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_chord: Option<String>,
    pub scope: CommandScope,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub human_only: bool,
}

const COMMAND_ID_MAX: usize = 64;

impl DashboardCommand {
    fn validate(&self) -> Result<(), String> {
        let id = self.id.as_str();
        let valid_id = !id.is_empty()
            && id.len() <= COMMAND_ID_MAX
            && id
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
        if !valid_id {
            return Err(format!("invalid command id {id:?}"));
        }
        if self.label.trim().is_empty() {
            return Err(format!("command {id:?} has no label"));
        }
        if let Some(chord) = &self.default_chord {
            validate_chord(chord).map_err(|why| format!("command {id:?}: {why}"))?;
        }
        Ok(())
    }
}

/// A canonical chord the frame shim forwards: a ⌘/Ctrl or ⌥ chord, a function key or Esc.
fn validate_chord(chord: &str) -> Result<(), String> {
    let mut key = None;
    let (mut cmd, mut alt) = (false, false);
    for segment in chord.split('+') {
        match segment {
            "CmdOrCtrl" => cmd = true,
            "Alt" => alt = true,
            "Shift" => {}
            "" => return Err(format!("chord {chord:?} has an empty segment")),
            other if key.is_none() => key = Some(other),
            _ => return Err(format!("chord {chord:?} names more than one key")),
        }
    }
    let key = key.ok_or_else(|| format!("chord {chord:?} has no key"))?;
    let function_key = key
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n));
    if cmd || alt || function_key || key == "Escape" {
        Ok(())
    } else {
        Err(format!(
            "chord {chord:?} needs CmdOrCtrl or Alt, or a function key or Escape"
        ))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DashboardDeclaration {
    /// Architecture: static hidden-delivery opt-in. Policy: absent means paused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    pub slots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<DashboardCommand>,
    /// Why the `codesense:commands` meta did not parse; reported by `validate`.
    #[serde(skip)]
    commands_error: Option<String>,
}
impl DashboardDeclaration {
    /// Policy: inbox and notes may need a private install; the dock never does.
    pub fn private_reach(&self, slot: &str) -> bool {
        slot == "right-pane"
            && (self
                .surface
                .as_deref()
                .is_some_and(|s| matches!(s, "human-inbox" | "inbox" | "notes"))
                || self.capabilities.iter().any(|cap| {
                    matches!(
                        cap.as_str(),
                        "inbox:read:all-teams" | "records:read:own" | "records:write:own"
                    )
                }))
    }
    fn matches(&self, other: &Self) -> bool {
        let set = |values: &[String]| {
            values
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        };
        self.badge == other.badge
            && self.surface == other.surface
            && set(&self.slots) == set(&other.slots)
            && set(&self.capabilities) == set(&other.capabilities)
            && self.commands == other.commands
    }
    pub fn from_html(html: &str) -> Self {
        let list = |name| {
            pane_meta(html, name)
                .unwrap_or_default()
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let (commands, commands_error) = match pane_meta(html, "codesense:commands")
            .map(decode_entities)
            .filter(|raw| !raw.trim().is_empty())
        {
            None => (Vec::new(), None),
            Some(raw) => match serde_json::from_str(&raw) {
                Ok(commands) => (commands, None),
                Err(e) => (Vec::new(), Some(format!("invalid codesense:commands: {e}"))),
            },
        };
        Self {
            badge: pane_meta(html, "codesense:badge")
                .filter(|value| *value == "live")
                .map(str::to_owned),
            slots: list("codesense:slots"),
            surface: pane_meta(html, "codesense:surface")
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            capabilities: list("codesense:capabilities"),
            commands,
            commands_error,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.badge.as_deref().is_some_and(|value| value != "live") {
            return Err("badge must be live or omitted".into());
        }
        for slot in &self.slots {
            if !crate::verbs::capabilities::SLOTS.contains(&slot.as_str()) {
                return Err(format!("unknown slot {slot:?}"));
            }
        }
        for capability in &self.capabilities {
            if !crate::verbs::capabilities::known(capability) {
                return Err(format!("unknown capability {capability:?}"));
            }
        }
        if let Some(error) = &self.commands_error {
            return Err(error.clone());
        }
        let mut ids = std::collections::BTreeSet::new();
        for command in &self.commands {
            command.validate()?;
            if !ids.insert(command.id.as_str()) {
                return Err(format!("duplicate command id {:?}", command.id));
            }
        }
        Ok(())
    }
}

fn decode_entities(raw: &str) -> String {
    raw.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

pub(crate) fn pane_meta<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<meta") {
        let start = from + at;
        let end = start + lower[start..].find('>')?;
        let tag = &html[start..end];
        if meta_attr(tag, "name").is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return meta_attr(tag, "content").map(str::trim);
        }
        from = end;
    }
    None
}

/// The value of attribute `name` in one tag's source (quoted or bare).
fn meta_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let start = from + at;
        from = start + name.len();
        let preceded = lower[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let rest = lower[from..].trim_start();
        if !preceded || !rest.starts_with('=') {
            continue;
        }
        let value_at = tag.len() - rest.len() + 1;
        let value = tag[value_at..].trim_start();
        let value_at = tag.len() - value.len();
        return match value.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let body = &tag[value_at + 1..];
                Some(&body[..body.find(q)?])
            }
            _ => Some(
                value
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()?,
            ),
        };
    }
    None
}

/// The declaration of the pane file at `path`: its layer manifest's entry, else
/// its own meta. Discovery reads the same two sources.
pub fn declaration_at(path: &Path) -> Option<DashboardDeclaration> {
    let id = pane_id_of_file(path.file_name()?.to_str()?)?;
    let overlay = manifest::load(path.parent()?)
        .and_then(Result::ok)
        .and_then(|m| m.panes.into_iter().find(|p| p.id == id));
    Some(overlay.map(|p| p.declaration).unwrap_or_else(|| {
        DashboardDeclaration::from_html(read_head(path).as_deref().unwrap_or_default())
    }))
}

/// How much of a pane is read to find its `<title>`, which sits in the head.
const TITLE_SCAN_BYTES: u64 = 16 * 1024;

/// The first [`TITLE_SCAN_BYTES`] of `path` as text (lossy at a cut character).
pub(crate) fn read_head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(TITLE_SCAN_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    Some(String::from_utf8_lossy(&head).into_owned())
}

fn modified_rfc3339(meta: &std::fs::Metadata) -> Option<String> {
    let modified = meta.modified().ok()?;
    Some(chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339())
}

/// The panes in one `panes/` dir, by id. A missing dir has none; an unreadable
/// pane is still listed (under its id) so it can show its own load error.
pub(crate) fn list_layer(
    dir: &Path,
    scope: PaneScope,
    org: &str,
    team: Option<&str>,
) -> Vec<PaneEntry> {
    list_layer_with_manifest(dir, scope, org, team, &manifest::load(dir))
}

pub(crate) fn list_layer_with_manifest(
    dir: &Path,
    scope: PaneScope,
    org: &str,
    team: Option<&str>,
    manifest: &Option<Result<manifest::PaneManifest, String>>,
) -> Vec<PaneEntry> {
    list_layer_filtered(dir, scope, org, team, |_| true, manifest.clone(), None)
}

fn list_layer_filtered(
    dir: &Path,
    scope: PaneScope,
    org: &str,
    team: Option<&str>,
    admitted: impl Fn(&Path) -> bool,
    manifest: Option<Result<manifest::PaneManifest, String>>,
    view: Option<&crate::context_repo::inheritance::TeamView>,
) -> Vec<PaneEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut panes: Vec<PaneEntry> = entries
        .flatten()
        .filter(|e| admitted(&e.path()))
        .filter_map(|e| read_layer_entry(&e.path(), scope, org, team, &manifest))
        .collect();
    if scope == PaneScope::Private {
        panes.extend(install::discover(dir, org, view));
    }
    panes.sort_by(|a, b| a.id.cmp(&b.id));
    panes
}

pub(crate) fn read_pane_for_view(
    view: &crate::context_repo::inheritance::TeamView,
    path: &Path,
    scope: PaneScope,
    org: &str,
    team: Option<&str>,
) -> Option<PaneEntry> {
    view.read_path(path).ok()?;
    let manifest = match scope {
        PaneScope::Team => view.pane_manifest_at(path.parent()?, Some(team?)),
        PaneScope::Private => view.private_pane_manifest(path.parent()?),
    };
    read_layer_entry(path, scope, org, team, &manifest)
}

/// Read one changed pane with the same declaration rules as discovery.
fn read_layer_entry(
    path: &Path,
    scope: PaneScope,
    org: &str,
    team: Option<&str>,
    manifest: &Option<Result<manifest::PaneManifest, String>>,
) -> Option<PaneEntry> {
    if !path.is_file() {
        return None;
    }
    let dir = path.parent()?;
    let name = path.file_name()?.to_str()?;
    let id = pane_id_of_file(name)?.to_string();
    let head = read_head(&path);
    let meta = DashboardDeclaration::from_html(head.as_deref().unwrap_or_default());
    let overlay = manifest
        .as_ref()
        .and_then(|m| m.as_ref().ok())
        .and_then(|m| m.panes.iter().find(|p| p.id == id));
    let declaration = overlay
        .map(|p| p.declaration.clone())
        .unwrap_or_else(|| meta.clone());
    let rejected_reason = match &manifest {
        Some(Err(reason)) => Some(format!("invalid manifest: {reason}")),
        _ if overlay.is_some()
            && meta != DashboardDeclaration::default()
            && !meta.matches(&declaration) =>
        {
            Some("manifest/meta mismatch".into())
        }
        _ if overlay.is_some_and(|p| p.html != name) => {
            Some("manifest HTML does not match the pane file".into())
        }
        _ => declaration.validate().err(),
    };
    let title = head
        .as_deref()
        .and_then(pane_title)
        .unwrap_or_else(|| id.clone());
    Some(PaneEntry {
        dashboard_ref: DashboardRef {
            scope: if scope == PaneScope::Team {
                DashboardScope::Team
            } else {
                DashboardScope::Private
            },
            org: org.into(),
            team: team.map(str::to_owned),
            id: id.clone(),
            installed_from: None,
        },
        status: if rejected_reason.is_some() {
            PaneStatus::Rejected
        } else {
            PaneStatus::Ready
        },
        rejected_reason,
        update_available: false,
        install_source_version: None,
        panel_id: pane_panel_id(scope, org, team, &id),
        title: overlay.and_then(|p| p.title.clone()).unwrap_or(title),
        icon: overlay
            .and_then(|p| p.icon.clone())
            .or_else(|| head.as_deref().and_then(pane_icon)),
        surface: declaration.surface.clone(),
        declaration,
        path: path.to_string_lossy().into_owned(),
        panes_dir: dir.to_string_lossy().into_owned(),
        modified: path.metadata().ok().as_ref().and_then(modified_rfc3339),
        team: team.map(str::to_string),
        scope,
        id,
    })
}

/// Load `pane-manifest.yaml` from a `panes/` dir, if one is there and parses
/// (and holds any entries). A missing, unreadable, or empty manifest yields
/// `None`.
fn manifest_layer(
    loaded: Option<Result<manifest::PaneManifest, String>>,
    scope: PaneScope,
    team: Option<&str>,
) -> Option<PaneManifestLayer> {
    let manifest = loaded?.ok()?;
    if manifest.panes.is_empty() {
        return None;
    }
    Some(PaneManifestLayer {
        scope,
        team: team.map(str::to_string),
        manifest,
    })
}

/// The panes `org/team` sees: every team in its chain, nearest first, then the
/// private space for `org`. The same id in two layers is two entries; nothing is
/// shadowed away. `chain` is the team's resolution chain (`[team, parent, …]`).
/// Returns the panes together with every layer's `pane-manifest.yaml` overlay
/// (nearest first), skipping layers whose manifest is absent or unparseable.
pub(crate) fn discover_panes(
    org_root: &Path,
    org: &str,
    chain: &[String],
    private_panes_dir: Option<&Path>,
) -> (Vec<PaneEntry>, Vec<PaneManifestLayer>) {
    let mut panes = Vec::new();
    let mut manifests = Vec::new();
    let reader = chain
        .first()
        .map(String::as_str)
        .unwrap_or(scratch_space::SCRATCH_TEAM);
    let view = crate::context_repo::inheritance::team_view(org_root, org, reader);
    if !chain.is_empty() {
        for owner in view.team_names() {
            let (entries, overlays) = discover_team_panes(org_root, org, reader, &owner);
            panes.extend(entries);
            manifests.extend(overlays);
        }
    }
    if let Some(dir) = private_panes_dir {
        let (private, overlays) = discover_private_panes(dir, org, Some(&view));
        panes.extend(private);
        manifests.extend(overlays);
    }
    (panes, manifests)
}

pub(crate) fn discover_team_panes(
    org_root: &Path,
    org: &str,
    reader: &str,
    owner: &str,
) -> (Vec<PaneEntry>, Vec<PaneManifestLayer>) {
    if validate_path_segment(reader).is_some() || validate_path_segment(owner).is_some() {
        return Default::default();
    }
    let view = crate::context_repo::inheritance::team_view(org_root, org, reader);
    let (dir, allowed) = if reader == owner {
        (path_to_team_panes_dir(org_root, reader), None)
    } else {
        let Some(layer) = view.inherited.iter().find(|l| l.team() == owner) else {
            return Default::default();
        };
        let allowed: std::collections::HashSet<_> = layer
            .walk("panes")
            .into_iter()
            .filter(|(rel, _)| {
                rel.strip_prefix("panes/")
                    .is_some_and(|rel| !rel.contains('/') && pane_id_of_file(rel).is_some())
            })
            .map(|(_, path)| path)
            .collect();
        let Some(dir) = allowed
            .iter()
            .next()
            .and_then(|file| file.parent())
            .map(Path::to_path_buf)
        else {
            return Default::default();
        };
        (dir, Some(allowed))
    };
    let manifest = view.pane_manifest(owner);
    let panes = list_layer_filtered(
        &dir,
        PaneScope::Team,
        org,
        Some(owner),
        |path| {
            allowed.as_ref().is_none_or(|files| files.contains(path))
                && view.read_path(path).is_ok()
        },
        manifest.clone(),
        Some(&view),
    );
    let manifests = manifest_layer(manifest, PaneScope::Team, Some(owner))
        .into_iter()
        .collect();
    (panes, manifests)
}

pub(crate) fn discover_private_panes(
    dir: &Path,
    org: &str,
    view: Option<&crate::context_repo::inheritance::TeamView>,
) -> (Vec<PaneEntry>, Vec<PaneManifestLayer>) {
    let manifest = view.map_or_else(|| manifest::load(dir), |v| v.private_pane_manifest(dir));
    let panes = list_layer_filtered(
        dir,
        PaneScope::Private,
        org,
        None,
        |path| view.is_none_or(|v| v.read_path(path).is_ok()),
        manifest.clone(),
        view,
    );
    let manifests = manifest_layer(manifest, PaneScope::Private, None)
        .into_iter()
        .collect();
    (panes, manifests)
}

pub(crate) fn list_private_panes(
    home: &Path,
    principal: &Principal,
    org: &str,
    reader: &str,
) -> PaneListing {
    let private = scratch_space::org_space(home, principal, org);
    let root = path_to_org_root(home, principal, org).ok().or_else(|| {
        crate::context_repo::context_attachments::registered(home, principal, org)
            .ok()
            .flatten()
            .map(|a| a.root)
    });
    let view = root.map(|root| crate::context_repo::inheritance::team_view(&root, org, reader));
    let (panes, manifests) = private.as_ref().map_or_else(Default::default, |space| {
        discover_private_panes(&space.panes_dir(), org, view.as_deref())
    });
    PaneListing {
        builtin_inbox: None,
        content_root: private
            .map(|space| space.content_dir().to_string_lossy().into_owned())
            .unwrap_or_default(),
        chain: Vec::new(),
        panes,
        manifests,
    }
}

/// The panes `principal` sees in `org/team`, with the content root their reads
/// resolve against, then the built-ins no listed pane shadows. Blocking.
pub fn list_panes(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<PaneListing, String> {
    let mut listing = list_space_panes(home, principal, org, team)?;
    let builtins = builtin::builtin_panes(home, principal, org);
    listing.builtin_inbox = builtins.iter().find(|pane| pane.id == "inbox").cloned();
    let visible: Vec<_> = builtins
        .into_iter()
        .filter(|builtin| !listing.panes.iter().any(|pane| pane.id == builtin.id))
        .collect();
    listing.panes.extend(visible);
    Ok(listing)
}

/// The bundled dashboards and a validated read context, independent of team/private discovery.
/// Architecture: recovery must bind to the requested team even when its listing is unavailable.
pub fn list_builtin_panes(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<PaneListing, String> {
    if let Some(reason) = crate::context_repo::org_slug::validate_org_slug(org) {
        return Err(format!("org {org:?} is not an org slug: {reason}"));
    }
    let panes = builtin::builtin_panes(home, principal, org);
    let content_root = if is_scratch_team(team) {
        scratch_space::org_space(home, principal, org)
            .ok_or("No private space for Inbox recovery")?
            .content_dir()
    } else {
        if let Some(reason) = validate_path_segment(team) {
            return Err(format!("team {team:?} is not a team name: {reason}"));
        }
        // Architecture: a missing attached volume cannot hide the bundled reader.
        // Its authenticated frame carries the team when the checkout has no usable root.
        path_to_org_root(home, principal, org)
            .map(|root| {
                crate::context_repo::inheritance::team_view(&root, org, team)
                    .own_content()
                    .own_path()
                    .expect("own content root")
                    .to_path_buf()
            })
            .unwrap_or_else(|_| builtin::builtin_panes_dir(home, principal))
    };
    Ok(PaneListing {
        builtin_inbox: panes.iter().find(|pane| pane.id == "inbox").cloned(),
        content_root: content_root.to_string_lossy().into_owned(),
        panes,
        chain: Vec::new(),
        manifests: Vec::new(),
    })
}

pub(crate) fn list_space_panes(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<PaneListing, String> {
    if is_scratch_team(team) {
        return Ok(list_private_panes(home, principal, org, team));
    }
    if let Some(reason) = validate_path_segment(team) {
        return Err(format!("team {team:?} is not a team name: {reason}"));
    }
    let private = scratch_space::org_space(home, principal, org);
    let private_dir = private.as_ref().map(|space| space.panes_dir());
    let org_root = path_to_org_root(home, principal, org)?;
    // The chain comes from the synced teams.yml: a parent name that is not one
    // directory (`../x`) must never steer discovery out of the org.
    let chain = crate::context_repo::inheritance::team_view(&org_root, org, team).team_names();
    let (panes, manifests) = discover_panes(&org_root, org, &chain, private_dir.as_deref());
    Ok(PaneListing {
        builtin_inbox: None,
        content_root: crate::context_repo::inheritance::team_view(&org_root, org, team)
            .own_content()
            .own_path()
            .ok_or("File not found")?
            .to_string_lossy()
            .into_owned(),
        panes,
        manifests,
        chain,
    })
}

/// Where a pane is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneTarget {
    Team { org: String, team: String },
    Private { org: String },
}

impl PaneTarget {
    pub fn scope(&self) -> PaneScope {
        match self {
            PaneTarget::Team { .. } => PaneScope::Team,
            PaneTarget::Private { .. } => PaneScope::Private,
        }
    }

    pub fn org(&self) -> &str {
        match self {
            PaneTarget::Team { org, .. } | PaneTarget::Private { org } => org,
        }
    }

    pub fn team(&self) -> Option<&str> {
        match self {
            PaneTarget::Team { team, .. } => Some(team),
            PaneTarget::Private { .. } => None,
        }
    }

    /// The target's `panes/` dir. A private target creates its space on first
    /// use; a team target must be a team cloned on this machine.
    pub fn panes_dir(&self, home: &Path, principal: &Principal) -> Result<PathBuf, String> {
        match self {
            PaneTarget::Team { org, team } => {
                let org_root = path_to_org_root(home, principal, org)?;
                if !org_root.join(team).is_dir() {
                    return Err(format!("team {org}/{team} is not synced on this machine"));
                }
                Ok(path_to_team_panes_dir(&org_root, team))
            }
            PaneTarget::Private { org } => scratch_space::ensure_org_space(home, principal, org)
                .map(|(space, _)| space.panes_dir())
                .map_err(|e| e.to_string()),
        }
    }
}

/// Write `html` as pane `id` in `dir`, replacing any existing pane of that id.
/// The bytes land through a synced dot-file rename, so a watcher never reloads
/// a half-written pane (the dot file is noise to it) and two concurrent writes
/// of one pane each land whole, the last rename winning. Returns the pane's path.
pub fn write_pane(dir: &Path, id: &str, html: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

    if !is_valid_pane_id(id) {
        return Err(format!(
            "pane id {id:?} must match [a-z0-9][a-z0-9-]* (it is the file name, {id}.html)"
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{id}.{PANE_EXTENSION}"));
    let staging = dir.join(format!(
        ".{id}.{PANE_EXTENSION}.{}.{}.writing",
        std::process::id(),
        STAGING_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let staged = std::fs::File::create(&staging).and_then(|mut file| {
        file.write_all(html.as_bytes())?;
        file.sync_all()
    });
    let landed = staged.and_then(|()| std::fs::rename(&staging, &path));
    if let Err(e) = landed {
        let _ = std::fs::remove_file(&staging);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
