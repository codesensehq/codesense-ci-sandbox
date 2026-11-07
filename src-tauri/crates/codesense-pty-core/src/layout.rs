// SPDX-License-Identifier: MIT
//! The host-owned terminal layout: which workspaces hold which terminal entries,
//! in what order, under what names, in which grid and placement — kept by the
//! host that owns the processes, so any client (a reloaded webview, a relaunched
//! app, another machine) asks the host for it instead of remembering it.
//!
//! Shared truth (entries, order, names, grid, placement, live/exited) lives in
//! the workspace layouts. Each viewer's focus, scroll hint and requested size
//! are kept per viewer; a workspace's `last_active` only seeds a viewer that has
//! never focused it. The model never interprets a workspace: its key is opaque
//! and its descriptor is stored verbatim for clients that did not create it.
//!
//! Nothing is persisted except the host's [`ServerIdentity`]: the layout lives
//! and dies with the processes it describes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::domain::{
    ClientBootId, PtySessionId, ServerId, TerminalDomainEvent, ViewerId, WorkspaceKey,
};
use crate::runner::ExitInfo;

/// The wire-format version of [`LayoutSnapshot`]. Changes are additive
/// (`#[serde(default)]`), so this only moves on an incompatible change.
pub const LAYOUT_SCHEMA: u16 = 1;

/// Whether an entry shows in its workspace's tab strip or runs closed-but-busy
/// out of sight (a tab closed while its process kept working, or a headless
/// agent session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Placement {
    Tab,
    Background,
}

/// An entry's process state. An exited entry keeps its bounded scrollback
/// readable and stays until it is closed, carrying the child's real exit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EntryState {
    Live,
    #[serde(rename_all = "camelCase")]
    Exited {
        exit_code: Option<u32>,
        signal: Option<String>,
    },
}

impl EntryState {
    fn exited(status: Option<ExitInfo>) -> Self {
        let ExitInfo { exit_code, signal } = status.unwrap_or_default();
        Self::Exited { exit_code, signal }
    }

    pub fn is_exited(&self) -> bool {
        matches!(self, Self::Exited { .. })
    }
}

/// One viewer's scroll position in one terminal, in lines above the bottom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewHint {
    pub scroll_from_bottom: u32,
}

/// The tab-grid arrangement, mirroring the client's pickable preset ring.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GridPreset {
    #[default]
    Columns,
    Rows,
    Grid,
}

/// A workspace's tab grid: the paned subset of its tab entries (in pane order),
/// the arrangement, and whether the grid is collapsed to a single tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GridState {
    pub paned: Vec<PtySessionId>,
    pub preset: GridPreset,
    pub collapsed: bool,
}

/// What a client needs to render a workspace it did not create. The host stores
/// it verbatim and never interprets it; a client resolves `story_id`/`team`
/// through its own context or renders the descriptor as-is. `worktree_path` is
/// a path on the host's disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceDescriptor {
    pub display_name: String,
    pub kind: String,
    pub org: Option<String>,
    pub team: Option<String>,
    pub story_id: Option<String>,
    pub repo: Option<String>,
    pub worktree_path: Option<PathBuf>,
    pub branch: Option<String>,
}

/// Free-form string tags on an entry (e.g. the MCP handle and launcher that let
/// a client rehydrate what the session is for).
pub type Tags = BTreeMap<String, String>;

/// A tag update: `Some` sets a tag, `None` removes it.
pub type TagPatch = BTreeMap<String, Option<String>>;

/// One terminal entry. Its id is its session id — there is no second handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabEntry {
    pub session: PtySessionId,
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    pub placement: Placement,
    pub state: EntryState,
    #[serde(default)]
    pub tags: Tags,
}

/// Architecture: source usage is resolved on its host by canonical path, never by a workspace label.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeUsage {
    pub entries: Vec<(WorkspaceKey, TabEntry)>,
    pub opening: Vec<PtySessionId>,
}

pub const SOURCE_REFUSAL: &str = "sourceIntact: ";
pub const START_SERVER: &str = "couldn't confirm no agents are running or starting; start or restart the terminal server, then retry";
pub const OLD_SERVER: &str =
    "terminal server too old to confirm no agents are starting; restart it";

pub fn source_refusal(reason: impl std::fmt::Display) -> String {
    format!("{SOURCE_REFUSAL}{reason}; refused, source intact")
}

/// One workspace's shared layout. `tabs` is in strip order; `last_active` is
/// the default focus for a viewer attaching fresh.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceLayout {
    pub descriptor: WorkspaceDescriptor,
    pub tabs: Vec<TabEntry>,
    pub last_active: Option<PtySessionId>,
    pub grid: GridState,
    pub next_ordinal: u32,
}

impl WorkspaceLayout {
    fn new(descriptor: WorkspaceDescriptor) -> Self {
        Self {
            descriptor,
            next_ordinal: 1,
            ..Self::default()
        }
    }

    fn index_of(&self, session: &PtySessionId) -> Option<usize> {
        self.tabs.iter().position(|t| &t.session == session)
    }

    /// Insert `entry` at strip position `index`, clamped; `None` appends.
    fn insert_at(&mut self, index: Option<usize>, entry: TabEntry) {
        let at = index.unwrap_or(self.tabs.len()).min(self.tabs.len());
        self.tabs.insert(at, entry);
    }

    fn has_tab(&self, session: &PtySessionId) -> bool {
        self.tabs
            .iter()
            .any(|t| &t.session == session && t.placement == Placement::Tab)
    }

    /// The nearest tab entry to strip position `from`, looking right first.
    fn neighbor_tab(&self, from: usize) -> Option<PtySessionId> {
        let from = from.min(self.tabs.len());
        self.tabs[from..]
            .iter()
            .chain(self.tabs[..from].iter().rev())
            .find(|t| t.placement == Placement::Tab)
            .map(|t| t.session.clone())
    }
}

/// One viewer's own view of the layout: its focus per workspace, its scroll
/// hint per terminal, and the exited entries it no longer shows (it relaunched
/// or closed them while another viewer still shows them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewerLayout {
    pub active: BTreeMap<WorkspaceKey, PtySessionId>,
    pub views: BTreeMap<PtySessionId, ViewHint>,
    pub hidden: BTreeSet<PtySessionId>,
}

/// The host's whole layout as one viewer sees it. `server_id` is stable across
/// host restarts; `epoch` changes per host lifetime; `rev` rises on every
/// shared mutation. Clients key workspaces by `(server_id, workspace)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutSnapshot {
    pub schema: u16,
    pub server_id: ServerId,
    pub server_name: String,
    pub epoch: String,
    pub rev: u64,
    pub workspaces: BTreeMap<WorkspaceKey, WorkspaceLayout>,
    pub viewer: ViewerLayout,
}

/// Open a terminal entry. A descriptor is required to create an absent
/// workspace and replaces the stored one when given for an existing workspace
/// (a client that did not create it may omit it). `name` defaults to a
/// memorable 2-word label derived from the session id (see
/// [`crate::memorable_name`]); `index` defaults to the end of the strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenTab {
    pub viewer: ViewerId,
    pub workspace: WorkspaceKey,
    pub session: PtySessionId,
    pub descriptor: Option<WorkspaceDescriptor>,
    pub name: Option<String>,
    pub role: Option<String>,
    pub tags: Tags,
    pub placement: Placement,
    pub index: Option<usize>,
    pub activate: bool,
}

/// How to close a live entry: keep it running in the background when its
/// foreground is busy, or kill it regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloseMode {
    KeepIfBusy,
    Kill,
}

/// What a close did: killed the process, moved a busy one to the background,
/// or removed an already-exited entry (freeing its scrollback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloseOutcome {
    Killed,
    Backgrounded,
    Removed,
}

/// A rejected layout verb. It crosses a host's socket whole, so a remote host's
/// refusal reads exactly as a local one's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "camelCase")]
pub enum LayoutError {
    UnknownSession(PtySessionId),
    UnknownWorkspace(WorkspaceKey),
    /// The session is already open: it has an entry, one is being opened for
    /// it, or the registry already runs it.
    AlreadyPlaced(PtySessionId),
    /// The session died and left the registry before its entry could be placed.
    SessionGone(PtySessionId),
    /// The session's entry or process is live: it cannot be respawned.
    Running(PtySessionId),
    /// Another respawn of the session is under way.
    Restarting(PtySessionId),
    CwdConflict(WorkspaceKey),
    InvalidGrid(String),
    Spawn(String),
    /// Dispatch may have created a process; this intent cannot be replayed.
    SpawnUnknown(String),
    /// The layout lock was poisoned by a panic.
    Unavailable,
    /// The host holding the layout could not be asked, or did not answer: the
    /// exact cause.
    Unreachable(String),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSession(s) => write!(f, "layout: no entry for session {s}"),
            Self::UnknownWorkspace(w) => write!(f, "layout: no workspace {w}"),
            Self::AlreadyPlaced(s) => write!(f, "layout: session {s} is already open"),
            Self::SessionGone(s) => write!(f, "layout: session {s} ended before it was placed"),
            Self::Running(s) => write!(f, "layout: session {s} is still running"),
            Self::Restarting(s) => {
                write!(f, "layout: a restart of session {s} is already in progress")
            }
            Self::CwdConflict(w) => write!(f, "layout: cwdConflict for workspace {w}"),
            Self::InvalidGrid(why) => write!(f, "layout: invalid grid: {why}"),
            // The spawn's or the connection's own error text, verbatim.
            Self::Spawn(e) | Self::SpawnUnknown(e) | Self::Unreachable(e) => f.write_str(e),
            Self::Unavailable => f.write_str("layout: unavailable"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// A host's stable identity: its persisted id plus a display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    pub id: ServerId,
    pub name: String,
}

/// The file under the instance's data root that holds the host's id.
const SERVER_ID_FILE: &str = "pty-server-id";

/// Write `contents` to a new file only its owner can read or write.
fn write_owner_only(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    std::io::copy(&mut contents.as_bytes(), &mut file).map(|_| ())
}

impl ServerIdentity {
    /// Read the id persisted under `dir`, minting and persisting one on first
    /// use (or over an empty file), so a restarted host keeps its identity.
    /// The id is written whole to a temporary file and linked into place, so a
    /// concurrent reader never sees a partial id and a racing instance adopts
    /// the first one linked.
    pub fn load_or_create(dir: &Path, name: impl Into<String>) -> std::io::Result<Self> {
        let path = dir.join(SERVER_ID_FILE);
        let id = match read_server_id(&path)? {
            Some(id) => id,
            None => {
                std::fs::create_dir_all(dir)?;
                let minted = uuid::Uuid::new_v4().to_string();
                let staged = dir.join(format!("{SERVER_ID_FILE}.{minted}"));
                write_owner_only(&staged, &minted)?;
                let linked = std::fs::hard_link(&staged, &path);
                let _ = std::fs::remove_file(&staged);
                match linked {
                    Ok(()) => minted,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        match read_server_id(&path)? {
                            Some(theirs) => theirs,
                            // An empty file left by an earlier crash: replace it.
                            None => {
                                write_owner_only(&staged, &minted)?;
                                std::fs::rename(&staged, &path)?;
                                // A racing repair may have landed last: adopt it.
                                read_server_id(&path)?.unwrap_or(minted)
                            }
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        Ok(Self {
            id: ServerId::new(id),
            name: name.into(),
        })
    }

    /// The identity persisted under `dir` ([`Self::load_or_create`]), or one
    /// for this run only when `dir` is unusable — a host always has an id.
    pub fn load_or_ephemeral(dir: &Path, name: impl Into<String>) -> Self {
        let name = name.into();
        Self::load_or_create(dir, name.clone()).unwrap_or_else(|e| {
            log::warn!(target: "pty", "terminal host id unreadable under {}: {e}; per-run id", dir.display());
            Self::ephemeral(name)
        })
    }

    /// An identity that lives only as long as this host — the fail-open
    /// fallback when the data root cannot hold the persisted id.
    pub fn ephemeral(name: impl Into<String>) -> Self {
        Self {
            id: ServerId::new(uuid::Uuid::new_v4().to_string()),
            name: name.into(),
        }
    }

    /// The machine's host name, the default display name of a local host.
    pub fn local_host_name() -> String {
        sysinfo::System::host_name().unwrap_or_else(|| "localhost".to_string())
    }
}

/// Who a host is and where its layout stands: its persisted identity, this host
/// lifetime's epoch, and its layout's current revision — what its hello carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIdentity {
    pub server: ServerIdentity,
    pub epoch: String,
    pub rev: u64,
}

fn read_server_id(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw.trim().to_string()).filter(|id| !id.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// What a layout mutation asks its host to do once the layout lock is released:
/// announce the changed workspaces, and kill or resize sessions. Killing or
/// resizing under the layout lock would re-enter it through the event sink.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    pub(crate) events: Vec<TerminalDomainEvent>,
    pub(crate) kill: Vec<PtySessionId>,
    pub(crate) resize: Vec<(PtySessionId, u16, u16)>,
}

/// An `open_tab` between its reservation and its placement: whether the
/// session's process already exited, or the session already left the registry.
enum Opening {
    Spawning,
    Exited(Option<ExitInfo>),
    Removed,
}

#[derive(Default)]
struct ViewerState {
    /// The client launch this viewer last took a snapshot under.
    boot: Option<ClientBootId>,
    focus: BTreeMap<WorkspaceKey, PtySessionId>,
    views: BTreeMap<PtySessionId, ViewHint>,
    hidden: BTreeSet<PtySessionId>,
    sizes: HashMap<PtySessionId, (u16, u16)>,
    /// The activity tick of this viewer's latest focus change; the viewer with
    /// the highest tick owns the size of every session it has sized.
    last_activity: u64,
}

#[derive(Default)]
struct LayoutState {
    /// This host lifetime's id, minted per model; every announcement names it.
    epoch: String,
    rev: u64,
    workspaces: BTreeMap<WorkspaceKey, WorkspaceLayout>,
    /// Which workspace holds each placed session (a session is in at most one).
    placed: HashMap<PtySessionId, WorkspaceKey>,
    opening: HashMap<PtySessionId, Opening>,
    opening_directories: HashMap<PtySessionId, (WorkspaceKey, Option<PathBuf>)>,
    /// Exited entries a respawn in place holds between its reap and its new
    /// process registering: whether that new process already exited.
    respawning: HashMap<PtySessionId, bool>,
    viewers: HashMap<ViewerId, ViewerState>,
    activity: u64,
}

impl LayoutState {
    fn workspace_of(&self, session: &PtySessionId) -> Result<WorkspaceKey, LayoutError> {
        self.placed
            .get(session)
            .cloned()
            .ok_or_else(|| LayoutError::UnknownSession(session.clone()))
    }

    /// `session`'s entry and the workspace holding it.
    fn entry_mut(
        &mut self,
        session: &PtySessionId,
    ) -> Result<(WorkspaceKey, &mut TabEntry), LayoutError> {
        let key = self.workspace_of(session)?;
        let entry = self
            .workspaces
            .get_mut(&key)
            .and_then(|ws| ws.tabs.iter_mut().find(|t| &t.session == session))
            .ok_or_else(|| LayoutError::UnknownSession(session.clone()))?;
        Ok((key, entry))
    }

    fn workspace_mut(&mut self, key: &WorkspaceKey) -> Result<&mut WorkspaceLayout, LayoutError> {
        self.workspaces
            .get_mut(key)
            .ok_or_else(|| LayoutError::UnknownWorkspace(key.clone()))
    }

    /// Unlink `session`'s entry from its workspace, returning the workspace, the
    /// strip position it held, and the entry.
    fn detach_entry(&mut self, session: &PtySessionId) -> Option<(WorkspaceKey, usize, TabEntry)> {
        let key = self.placed.remove(session)?;
        let ws = self.workspaces.get_mut(&key)?;
        let index = ws.index_of(session)?;
        let entry = ws.tabs.remove(index);
        Some((key, index, entry))
    }

    /// Take `session`'s entry out of the layout for good, moving any focus on
    /// it to the nearest remaining tab. Returns the workspace it left.
    fn take_entry(&mut self, session: &PtySessionId) -> Option<(WorkspaceKey, TabEntry)> {
        let (key, index, entry) = self.detach_entry(session)?;
        self.refocus_away_from(&key, session, index);
        for viewer in self.viewers.values_mut() {
            viewer.views.remove(session);
            viewer.hidden.remove(session);
        }
        Some((key, entry))
    }

    /// Move every focus on `session` in `key` (the shared default and each
    /// viewer's) to the tab nearest strip position `index`.
    fn refocus_away_from(&mut self, key: &WorkspaceKey, session: &PtySessionId, index: usize) {
        let Some(ws) = self.workspaces.get_mut(key) else {
            return;
        };
        let neighbor = ws.neighbor_tab(index);
        if ws.last_active.as_ref() == Some(session) {
            ws.last_active = neighbor.clone();
        }
        for viewer in self.viewers.values_mut() {
            if viewer.focus.get(key) == Some(session) {
                match &neighbor {
                    Some(next) => viewer.focus.insert(key.clone(), next.clone()),
                    None => viewer.focus.remove(key),
                };
            }
        }
    }

    /// Settle the invariants of each touched workspace and announce it: the grid
    /// panes only tab entries (once each), focus names only tab entries, and an
    /// empty workspace is deleted. Every announcement carries its own `rev`.
    fn publish(&mut self, touched: impl IntoIterator<Item = WorkspaceKey>) -> Effects {
        let mut effects = Effects::default();
        let mut seen = BTreeSet::new();
        for key in touched {
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some(ws) = self.workspaces.get_mut(&key) {
                let tabs: HashSet<PtySessionId> = ws
                    .tabs
                    .iter()
                    .filter(|t| t.placement == Placement::Tab)
                    .map(|t| t.session.clone())
                    .collect();
                let mut paned = HashSet::new();
                ws.grid
                    .paned
                    .retain(|s| tabs.contains(s) && paned.insert(s.clone()));
                if ws.last_active.as_ref().is_some_and(|s| !tabs.contains(s)) {
                    ws.last_active = None;
                }
                if ws.tabs.is_empty() {
                    self.workspaces.remove(&key);
                }
            }
            let ws = self.workspaces.get(&key);
            for viewer in self.viewers.values_mut() {
                if viewer
                    .focus
                    .get(&key)
                    .is_some_and(|s| !ws.is_some_and(|ws| ws.has_tab(s)))
                {
                    viewer.focus.remove(&key);
                }
            }
            self.rev += 1;
            effects.events.push(TerminalDomainEvent::LayoutChanged {
                epoch: self.epoch.clone(),
                rev: self.rev,
                workspace: key.clone(),
                layout: ws.cloned().map(Box::new),
            });
        }
        effects
    }

    /// Free each of `candidates` that no attached viewer shows any more — an
    /// exited entry every viewer has relaunched past or closed. Its entry goes
    /// and its session is killed, freeing the scrollback.
    fn free_unseen(&mut self, candidates: Vec<PtySessionId>) -> Effects {
        let unseen: Vec<PtySessionId> = candidates
            .into_iter()
            .filter(|s| {
                !self
                    .viewers
                    .values()
                    .any(|v| v.boot.is_some() && !v.hidden.contains(s))
            })
            .collect();
        let touched: Vec<WorkspaceKey> = unseen
            .iter()
            .filter_map(|s| self.take_entry(s).map(|(key, _)| key))
            .collect();
        let mut effects = self.publish(touched);
        effects.kill = unseen;
        effects
    }

    /// The viewer that owns `session`'s size: the most recently active viewer
    /// that sized it, `prefer` winning a tie.
    fn size_owner(&self, session: &PtySessionId, prefer: &ViewerId) -> Option<(u16, u16)> {
        self.viewers
            .iter()
            .filter_map(|(id, v)| {
                v.sizes
                    .get(session)
                    .map(|size| ((v.last_activity, id == prefer), *size))
            })
            .max_by_key(|(rank, _)| *rank)
            .map(|(_, size)| size)
    }

    /// Mark `viewer` the most recently active one; every session it has sized
    /// whose owning size changes as a result is resized to it.
    fn touch(&mut self, viewer: &ViewerId) -> Vec<(PtySessionId, u16, u16)> {
        let sized: Vec<PtySessionId> = self
            .viewers
            .get(viewer)
            .map(|v| v.sizes.keys().cloned().collect())
            .unwrap_or_default();
        let before: Vec<Option<(u16, u16)>> =
            sized.iter().map(|s| self.size_owner(s, viewer)).collect();
        self.activity += 1;
        self.viewers
            .entry(viewer.clone())
            .or_default()
            .last_activity = self.activity;
        sized
            .into_iter()
            .zip(before)
            .filter_map(|(session, before)| {
                let after = self.size_owner(&session, viewer)?;
                (Some(after) != before).then_some((session, after.0, after.1))
            })
            .collect()
    }
}

/// The host's terminal layout. Every mutation runs under one lock and hands its
/// host the [`Effects`] to carry out after the lock is released.
pub(crate) struct LayoutModel {
    identity: ServerIdentity,
    state: Mutex<LayoutState>,
}

impl LayoutModel {
    pub(crate) fn new(identity: ServerIdentity) -> Self {
        Self {
            identity,
            state: Mutex::new(LayoutState {
                epoch: uuid::Uuid::new_v4().to_string(),
                ..LayoutState::default()
            }),
        }
    }

    pub(crate) fn identity(&self) -> Result<HostIdentity, LayoutError> {
        let state = self.lock()?;
        Ok(HostIdentity {
            server: self.identity.clone(),
            epoch: state.epoch.clone(),
            rev: state.rev,
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, LayoutState>, LayoutError> {
        self.state.lock().map_err(|_| LayoutError::Unavailable)
    }

    /// Every entry of every workspace, in strip order, read-only: no viewer
    /// is touched and nothing is freed (an armed routine's host
    /// looks for its agent's sessions).
    pub(crate) fn entries(&self) -> Result<Vec<(WorkspaceKey, TabEntry)>, LayoutError> {
        let state = self.lock()?;
        Ok(state
            .workspaces
            .iter()
            .flat_map(|(key, ws)| ws.tabs.iter().map(move |tab| (key.clone(), tab.clone())))
            .collect())
    }

    pub(crate) fn worktree_usage(&self, source: &Path) -> Result<WorktreeUsage, String> {
        let state = self.lock().map_err(|e| e.to_string())?;
        let source = source
            .canonicalize()
            .map_err(|e| format!("source path unavailable: {e}"))?;
        let in_source = |cwd: Option<&Path>| -> Result<bool, String> {
            let cwd = cwd.ok_or("a terminal's working directory is unknown; stop the agents or restart the terminal server")?;
            let cwd = cwd.canonicalize().map_err(|e| format!("terminal working directory unavailable: {e}; stop the agents or restart the terminal server"))?;
            Ok(cwd.starts_with(&source))
        };
        let mut usage = WorktreeUsage::default();
        for (key, ws) in &state.workspaces {
            if ws.tabs.iter().any(|tab| !tab.state.is_exited())
                && in_source(ws.descriptor.worktree_path.as_deref())?
            {
                usage.entries.extend(
                    ws.tabs
                        .iter()
                        .filter(|tab| !tab.state.is_exited())
                        .map(|tab| (key.clone(), tab.clone())),
                );
            }
        }
        for (session, (_, cwd)) in &state.opening_directories {
            if matches!(state.opening.get(session), Some(Opening::Spawning))
                && in_source(cwd.as_deref())?
            {
                usage.opening.push(session.clone());
            }
        }
        for session in state.respawning.keys() {
            let cwd = state
                .placed
                .get(session)
                .and_then(|key| state.workspaces.get(key))
                .and_then(|ws| ws.descriptor.worktree_path.as_deref());
            if in_source(cwd)? {
                usage.opening.push(session.clone());
            }
        }
        Ok(usage)
    }

    /// The whole layout for `viewer`. Under a client boot id other than the one
    /// this viewer last used (it relaunched), every exited entry is hidden from
    /// it first, and any exited entry no attached viewer still shows is freed
    /// (its session is killed to free the scrollback). A workspace the viewer
    /// has never focused is seeded from its `last_active`.
    pub(crate) fn snapshot(
        &self,
        viewer: &ViewerId,
        boot: &ClientBootId,
    ) -> Result<(LayoutSnapshot, Effects), LayoutError> {
        let mut state = self.lock()?;
        let relaunched = state
            .viewers
            .get(viewer)
            .is_none_or(|v| v.boot.as_ref() != Some(boot));
        let mut effects = Effects::default();
        if relaunched {
            let exited: Vec<PtySessionId> = state
                .workspaces
                .values()
                .flat_map(|ws| ws.tabs.iter())
                .filter(|t| t.state.is_exited())
                .map(|t| t.session.clone())
                .collect();
            let view = state.viewers.entry(viewer.clone()).or_default();
            view.boot = Some(boot.clone());
            view.hidden.extend(exited.iter().cloned());
            effects = state.free_unseen(exited);
        }
        let seeds: Vec<(WorkspaceKey, PtySessionId)> = state
            .workspaces
            .iter()
            .filter_map(|(key, ws)| Some((key.clone(), ws.last_active.clone()?)))
            .collect();
        let view = state.viewers.entry(viewer.clone()).or_default();
        for (key, session) in seeds {
            view.focus.entry(key).or_insert(session);
        }
        let viewer_layout = ViewerLayout {
            active: view.focus.clone(),
            views: view.views.clone(),
            hidden: view.hidden.clone(),
        };
        let snapshot = LayoutSnapshot {
            schema: LAYOUT_SCHEMA,
            server_id: self.identity.id.clone(),
            server_name: self.identity.name.clone(),
            epoch: state.epoch.clone(),
            rev: state.rev,
            workspaces: state.workspaces.clone(),
            viewer: viewer_layout,
        };
        Ok((snapshot, effects))
    }

    /// Reserve `session` for an `open_tab` whose spawn is about to run, so its
    /// exit or removal during the spawn is recorded rather than missed. An
    /// absent workspace needs a descriptor, checked before anything spawns.
    pub(crate) fn reserve(
        &self,
        session: &PtySessionId,
        workspace: &WorkspaceKey,
        descriptor: Option<&WorkspaceDescriptor>,
    ) -> Result<(), LayoutError> {
        let mut state = self.lock()?;
        if state.placed.contains_key(session) || state.opening.contains_key(session) {
            return Err(LayoutError::AlreadyPlaced(session.clone()));
        }
        if descriptor.is_none() && !state.workspaces.contains_key(workspace) {
            return Err(LayoutError::UnknownWorkspace(workspace.clone()));
        }
        let cwd = descriptor.and_then(|d| d.worktree_path.clone());
        if let Some(cwd) = &cwd {
            let existing_conflict = state.workspaces.get(workspace).is_some_and(|ws| {
                !ws.tabs.is_empty() && ws.descriptor.worktree_path.as_ref() != Some(cwd)
            });
            let pending_conflict = state
                .opening_directories
                .values()
                .any(|(key, dir)| key == workspace && dir.as_ref() != Some(cwd));
            if existing_conflict || pending_conflict {
                return Err(LayoutError::CwdConflict(workspace.clone()));
            }
        }
        let cwd = cwd.or_else(|| {
            state
                .workspaces
                .get(workspace)
                .and_then(|ws| ws.descriptor.worktree_path.clone())
        });
        state
            .opening_directories
            .insert(session.clone(), (workspace.clone(), cwd));
        state.opening.insert(session.clone(), Opening::Spawning);
        Ok(())
    }

    /// Drop a reservation that will not be placed (a no-op once placed).
    pub(crate) fn release(&self, session: &PtySessionId) {
        if let Ok(mut state) = self.state.lock() {
            state.opening.remove(session);
            state.opening_directories.remove(session);
        }
    }

    /// Place a reserved session. One whose process already exited is placed
    /// `Exited` with its real status; one that already left the registry is
    /// not placed at all.
    pub(crate) fn place_opened(
        &self,
        request: OpenTab,
    ) -> Result<(TabEntry, Effects), LayoutError> {
        let mut state = self.lock()?;
        let OpenTab {
            viewer,
            workspace,
            session,
            descriptor,
            name,
            role,
            tags,
            placement,
            index,
            activate,
        } = request;
        state.opening_directories.remove(&session);
        let entry_state = match state.opening.remove(&session) {
            Some(Opening::Spawning) => EntryState::Live,
            Some(Opening::Exited(status)) => EntryState::exited(status),
            Some(Opening::Removed) => return Err(LayoutError::SessionGone(session)),
            None => return Err(LayoutError::UnknownSession(session)),
        };
        let ws = match (state.workspaces.contains_key(&workspace), descriptor) {
            (true, descriptor) => {
                let ws = state.workspace_mut(&workspace)?;
                if let Some(descriptor) = descriptor {
                    ws.descriptor = descriptor;
                }
                ws
            }
            (false, Some(descriptor)) => state
                .workspaces
                .entry(workspace.clone())
                .or_insert(WorkspaceLayout::new(descriptor)),
            (false, None) => return Err(LayoutError::UnknownWorkspace(workspace)),
        };
        ws.next_ordinal += 1;
        let entry = TabEntry {
            session: session.clone(),
            name: name.unwrap_or_else(|| {
                crate::memorable_name::memorable_name_from_str(session.as_str())
            }),
            role,
            placement,
            state: entry_state,
            tags,
        };
        ws.insert_at(index, entry.clone());
        if activate && placement == Placement::Tab {
            ws.last_active = Some(session.clone());
        }
        state.placed.insert(session.clone(), workspace.clone());
        let mut resize = Vec::new();
        if activate && placement == Placement::Tab {
            state
                .viewers
                .entry(viewer.clone())
                .or_default()
                .focus
                .insert(workspace.clone(), session);
            resize = state.touch(&viewer);
        }
        let mut effects = state.publish([workspace]);
        effects.resize = resize;
        Ok((entry, effects))
    }

    /// A session's process exited: its entry turns `Exited` (it is not removed).
    pub(crate) fn on_exit(&self, session: &PtySessionId, status: Option<ExitInfo>) -> Effects {
        let Ok(mut state) = self.state.lock() else {
            return Effects::default();
        };
        if let Some(opening) = state.opening.get_mut(session) {
            // A session that already left the registry stays gone: its exit
            // (which follows a deliberate removal) must not bring it back.
            if !matches!(opening, Opening::Removed) {
                *opening = Opening::Exited(status);
            }
            return Effects::default();
        }
        if let Some(exited) = state.respawning.get_mut(session) {
            // A respawn's new process ended before its entry turned live.
            *exited = true;
        }
        match state.entry_mut(session) {
            Ok((key, entry)) => {
                entry.state = EntryState::exited(status);
                state.publish([key])
            }
            Err(_) => Effects::default(),
        }
    }

    /// A session left the registry: its entry (if any) goes with it, and every
    /// viewer forgets its requested size.
    pub(crate) fn on_removed(&self, session: &PtySessionId) -> Effects {
        let Ok(mut state) = self.state.lock() else {
            return Effects::default();
        };
        for viewer in state.viewers.values_mut() {
            viewer.sizes.remove(session);
        }
        if let Some(opening) = state.opening.get_mut(session) {
            *opening = Opening::Removed;
            return Effects::default();
        }
        match state.take_entry(session) {
            Some((key, _)) => state.publish([key]),
            None => Effects::default(),
        }
    }

    /// Whether `session`'s scrollback must be kept: it shows as a tab, or an
    /// `open_tab` for it is under way. A background entry is not held, so the
    /// registry may reclaim a finished headless session (its entry follows).
    pub(crate) fn is_held(&self, session: &str) -> bool {
        let session = PtySessionId::new(session);
        self.state.lock().is_ok_and(|state| {
            state.opening.contains_key(&session)
                || state.workspace_of(&session).is_ok_and(|key| {
                    state
                        .workspaces
                        .get(&key)
                        .is_some_and(|ws| ws.has_tab(&session))
                })
        })
    }

    /// Whether `session` is an entry of the shared layout — a tab or a
    /// background (hidden) entry, in any workspace — or being opened as one:
    /// every app shows it (its tab strip, or its hidden tabs), and ends it
    /// there.
    pub(crate) fn has_entry(&self, session: &str) -> bool {
        let session = PtySessionId::new(session);
        self.state.lock().is_ok_and(|state| {
            state.opening.contains_key(&session) || state.workspace_of(&session).is_ok()
        })
    }

    /// Hold `session`'s exited entry for a respawn in place: refused while the
    /// entry is live, or while another respawn of it is under way.
    pub(crate) fn reserve_respawn(&self, session: &PtySessionId) -> Result<(), LayoutError> {
        let mut state = self.lock()?;
        if !state.entry_mut(session)?.1.state.is_exited() {
            return Err(LayoutError::Running(session.clone()));
        }
        if state.respawning.contains_key(session) {
            return Err(LayoutError::Restarting(session.clone()));
        }
        state.respawning.insert(session.clone(), false);
        Ok(())
    }

    /// Drop a respawn's hold without reviving the entry (its spawn failed).
    pub(crate) fn release_respawn(&self, session: &PtySessionId) {
        if let Ok(mut state) = self.state.lock() {
            state.respawning.remove(session);
        }
    }

    /// A respawn's new process registered: the entry, kept where it was, turns
    /// live — unless that process already exited, whose exit it keeps — and
    /// shows again to any viewer that had closed it. Decided under the layout
    /// lock, which the process's exit also takes, so an exit can never be
    /// overwritten. The hold ends.
    pub(crate) fn finish_respawn(
        &self,
        session: &PtySessionId,
    ) -> Result<(TabEntry, Effects), LayoutError> {
        let mut state = self.lock()?;
        let exited = state.respawning.remove(session).unwrap_or(false);
        let (key, entry) = state.entry_mut(session)?;
        if !exited {
            entry.state = EntryState::Live;
        }
        let entry = entry.clone();
        for viewer in state.viewers.values_mut() {
            viewer.hidden.remove(session);
        }
        Ok((entry, state.publish([key])))
    }

    pub(crate) fn entry_state(&self, session: &PtySessionId) -> Result<EntryState, LayoutError> {
        let mut state = self.lock()?;
        Ok(state.entry_mut(session)?.1.state.clone())
    }

    /// `viewer` closes `session`'s entry, decided under one lock so a process
    /// exiting mid-close is still closed as exited. An exited entry is hidden
    /// from `viewer` and freed once no attached viewer shows it (`Removed`); a
    /// live one goes to the background when `keep_running` (`Backgrounded`),
    /// or is removed and its session killed (`Killed`).
    pub(crate) fn close(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        keep_running: bool,
    ) -> Result<(CloseOutcome, Effects), LayoutError> {
        let mut state = self.lock()?;
        let (key, entry) = state.entry_mut(session)?;
        if entry.state.is_exited() {
            state
                .viewers
                .entry(viewer.clone())
                .or_default()
                .hidden
                .insert(session.clone());
            return Ok((
                CloseOutcome::Removed,
                state.free_unseen(vec![session.clone()]),
            ));
        }
        if keep_running {
            entry.placement = Placement::Background;
            let index = state.workspace_mut(&key)?.index_of(session).unwrap_or(0);
            state.refocus_away_from(&key, session, index);
            return Ok((CloseOutcome::Backgrounded, state.publish([key])));
        }
        state.take_entry(session);
        let mut effects = state.publish([key]);
        effects.kill.push(session.clone());
        Ok((CloseOutcome::Killed, effects))
    }

    /// Move `session` to `workspace` (created from `descriptor` when absent) at
    /// `index` (default: the end) with `placement`.
    pub(crate) fn place(
        &self,
        session: &PtySessionId,
        workspace: &WorkspaceKey,
        placement: Placement,
        index: Option<usize>,
        descriptor: Option<WorkspaceDescriptor>,
    ) -> Result<Effects, LayoutError> {
        let mut state = self.lock()?;
        state.workspace_of(session)?;
        match (state.workspaces.get_mut(workspace), descriptor) {
            (Some(ws), Some(descriptor)) => ws.descriptor = descriptor,
            (Some(_), None) => {}
            (None, Some(descriptor)) => {
                state
                    .workspaces
                    .insert(workspace.clone(), WorkspaceLayout::new(descriptor));
            }
            (None, None) => return Err(LayoutError::UnknownWorkspace(workspace.clone())),
        }
        let (from, was_at, mut entry) = state
            .detach_entry(session)
            .ok_or_else(|| LayoutError::UnknownSession(session.clone()))?;
        if &from != workspace || placement != Placement::Tab {
            state.refocus_away_from(&from, session, was_at);
        }
        entry.placement = placement;
        state.workspace_mut(workspace)?.insert_at(index, entry);
        state.placed.insert(session.clone(), workspace.clone());
        Ok(state.publish([from, workspace.clone()]))
    }

    pub(crate) fn rename(
        &self,
        session: &PtySessionId,
        name: String,
    ) -> Result<Effects, LayoutError> {
        let mut state = self.lock()?;
        let (key, entry) = state.entry_mut(session)?;
        entry.name = name;
        Ok(state.publish([key]))
    }

    pub(crate) fn move_to(
        &self,
        session: &PtySessionId,
        to_index: usize,
    ) -> Result<Effects, LayoutError> {
        let mut state = self.lock()?;
        let key = state.workspace_of(session)?;
        let ws = state.workspace_mut(&key)?;
        let from = ws
            .index_of(session)
            .ok_or_else(|| LayoutError::UnknownSession(session.clone()))?;
        let entry = ws.tabs.remove(from);
        ws.insert_at(Some(to_index), entry);
        Ok(state.publish([key]))
    }

    /// `viewer` focuses `session` (or nothing) in `workspace`, and becomes the
    /// most recently active viewer. A focused session also becomes the
    /// workspace's default for a fresh attach; clearing one viewer's focus
    /// leaves that default alone.
    pub(crate) fn set_active(
        &self,
        viewer: &ViewerId,
        workspace: &WorkspaceKey,
        session: Option<&PtySessionId>,
    ) -> Result<Effects, LayoutError> {
        let mut state = self.lock()?;
        let ws = state.workspace_mut(workspace)?;
        if let Some(session) = session {
            if !ws.has_tab(session) {
                return Err(LayoutError::UnknownSession(session.clone()));
            }
        }
        if let Some(session) = session {
            ws.last_active = Some(session.clone());
        }
        let focus = &mut state.viewers.entry(viewer.clone()).or_default().focus;
        match session {
            Some(session) => focus.insert(workspace.clone(), session.clone()),
            None => focus.remove(workspace),
        };
        let resize = state.touch(viewer);
        let mut effects = state.publish([workspace.clone()]);
        effects.resize = resize;
        Ok(effects)
    }

    /// Replace a workspace's grid. Every paned session must be one of its tab
    /// entries, at most once.
    pub(crate) fn set_grid(
        &self,
        workspace: &WorkspaceKey,
        grid: GridState,
    ) -> Result<Effects, LayoutError> {
        let mut state = self.lock()?;
        let ws = state.workspace_mut(workspace)?;
        let mut seen = HashSet::new();
        if let Some(bad) = grid
            .paned
            .iter()
            .find(|s| !ws.has_tab(s) || !seen.insert(*s))
        {
            return Err(LayoutError::InvalidGrid(format!(
                "{bad} is not a tab of {workspace} or is paned twice"
            )));
        }
        ws.grid = grid;
        Ok(state.publish([workspace.clone()]))
    }

    /// A per-viewer scroll hint; no other viewer sees it.
    pub(crate) fn set_view_hint(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        hint: ViewHint,
    ) -> Result<(), LayoutError> {
        let mut state = self.lock()?;
        state.workspace_of(session)?;
        state
            .viewers
            .entry(viewer.clone())
            .or_default()
            .views
            .insert(session.clone(), hint);
        Ok(())
    }

    pub(crate) fn set_tags(
        &self,
        session: &PtySessionId,
        patch: TagPatch,
    ) -> Result<Effects, LayoutError> {
        let mut state = self.lock()?;
        let (workspace, entry) = state.entry_mut(session)?;
        for (key, value) in patch {
            match value {
                Some(value) => entry.tags.insert(key, value),
                None => entry.tags.remove(&key),
            };
        }
        Ok(state.publish([workspace]))
    }

    pub(crate) fn set_descriptor(
        &self,
        workspace: &WorkspaceKey,
        descriptor: WorkspaceDescriptor,
    ) -> Result<Effects, LayoutError> {
        let mut state = self.lock()?;
        state.workspace_mut(workspace)?.descriptor = descriptor;
        Ok(state.publish([workspace.clone()]))
    }

    /// Remove a workspace and kill every one of its sessions.
    pub(crate) fn close_workspace(&self, workspace: &WorkspaceKey) -> Result<Effects, LayoutError> {
        let mut state = self.lock()?;
        let sessions: Vec<PtySessionId> = state
            .workspaces
            .get(workspace)
            .ok_or_else(|| LayoutError::UnknownWorkspace(workspace.clone()))?
            .tabs
            .iter()
            .map(|t| t.session.clone())
            .collect();
        for session in &sessions {
            state.take_entry(session);
        }
        let mut effects = state.publish([workspace.clone()]);
        effects.kill = sessions;
        Ok(effects)
    }

    /// Record `viewer`'s requested size for `session` and return the size the
    /// PTY should take: the one requested by the most recently active viewer
    /// that sized it.
    pub(crate) fn request_size(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        cols: u16,
        rows: u16,
    ) -> (u16, u16) {
        let Ok(mut state) = self.state.lock() else {
            return (cols, rows);
        };
        state
            .viewers
            .entry(viewer.clone())
            .or_default()
            .sizes
            .insert(session.clone(), (cols, rows));
        state.size_owner(session, viewer).unwrap_or((cols, rows))
    }
}

#[cfg(test)]
mod tests;
