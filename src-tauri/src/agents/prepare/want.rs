// SPDX-License-Identifier: MIT
//! SW-XS-7: the want set — what an agent's workspace should hold, from the
//! agent-context resolver (SW-XS-13, SPEC §12.2):
//!
//! ```text
//! want = (context ∪ startup) + repo_override.add − repo_override.remove − projection-excludes
//! ```
//!
//! The captured startup manifest is applied first and pinned: its files keep
//! the hash captured at prepare, and a source that changed since then fails
//! the launch, as a startup error always has.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::agents::manifest::StartupManifest;
use crate::agents::manifest_resolve::{
    content_layers, resolve_context, ContextRequest, ContextResolution, ContextTier, ManifestTarget,
};
use crate::agents::AgentKey;
use crate::domain::Principal;
use codesense_pty_core::agent_cli::AgentCli;
use codesense_pty_core::remote_manifest::{normalize_prefixes, normalized_mode, MAX_PREFIXES};

/// One file the workspace should hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantFile {
    pub source_context: Option<String>,
    /// Workspace-relative, `/`-separated, NFC.
    pub dest: String,
    /// Where its bytes are read from.
    pub source: PathBuf,
    pub bytes: u64,
    /// `0o755` or `0o644`.
    pub mode: u32,
    /// The tier it comes from (`team:org/team`, `repo:host/owner/repo`,
    /// `personal`): the managed record's `source_layer`.
    pub layer: String,
    /// The captured startup hash it must still have.
    pub pinned: Option<[u8; 32]>,
}

/// Everything one Step 2 run wants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WantSet {
    /// Sorted by `dest`, unique.
    pub files: Vec<WantFile>,
    /// The declared prefixes, as a machine's manifest is asked for them
    /// (validated, nested ones folded, at most 32).
    pub prefixes: Vec<String>,
    /// The content roots files are read from: a destination at or inside one
    /// is never prepared (SPEC §11.5).
    pub source_roots: Vec<PathBuf>,
    /// Resolution warnings (missing prefixes and the like), for the summary.
    pub warnings: Vec<String>,
    /// SW-XS-8 (review r1): the explicit call's own scope — its paths and
    /// sets (or the runtime defaults), unfolded — which a Step 3 prune must
    /// stay inside (the folded `prefixes` can widen to a top-level folder).
    pub scope: Vec<String>,
}

fn layer_label(tier: &ContextTier) -> String {
    match tier {
        ContextTier::Repo { repo, .. } => format!("repo:{repo}"),
        ContextTier::Personal { org: Some(org) } => format!("personal:{org}"),
        ContextTier::Personal { org: None } => "personal".into(),
        ContextTier::Team { org, team } => format!("team:{org}/{team}"),
    }
}

fn nfc(path: &str) -> String {
    path.nfc().collect()
}

/// The parent prefix that covers `path` for a manifest request: the path
/// itself (a file prefix lists that file).
/// The `source_layer` of a merged workspace file (a launch's generated
/// output, never team content: an explicit publish never writes it back).
pub const GENERATED_LAYER: &str = "generated";

pub(crate) fn fold_prefixes(mut prefixes: Vec<String>) -> Result<Vec<String>, String> {
    prefixes.retain(|p| !p.is_empty());
    prefixes.sort();
    prefixes.dedup();
    if let Ok(folded) = normalize_prefixes(&prefixes) {
        return Ok(folded);
    }
    // Too many: cover them by their first segment instead.
    let tops: BTreeSet<String> = prefixes
        .iter()
        .map(|p| p.split('/').next().unwrap_or(p).to_string())
        .collect();
    let tops: Vec<String> = tops.into_iter().collect();
    if tops.len() > MAX_PREFIXES {
        return Err(format!(
            "the agent's context spans {} top-level paths; at most {MAX_PREFIXES} are supported",
            tops.len()
        ));
    }
    normalize_prefixes(&tops)
}

impl WantSet {
    /// The want set of a resolved context (and, first and pinned, a
    /// captured startup manifest).
    pub fn from_resolution(
        home: &Path,
        principal: &Principal,
        manifest: Option<&StartupManifest>,
        target: &ManifestTarget,
        resolution: &ContextResolution,
        source_roots: Vec<PathBuf>,
    ) -> Result<Self, String> {
        let layers = content_layers(home, principal, target);
        let mut files: Vec<WantFile> = Vec::new();
        let mut taken: BTreeSet<String> = BTreeSet::new();
        let mut prefixes: Vec<String> = Vec::new();
        if let Some(manifest) = manifest {
            if !manifest.errors.is_empty() {
                return Err(format!(
                    "the agent's startup manifest has errors: {}",
                    serde_json::to_string(&manifest.errors).unwrap_or_default()
                ));
            }
            for entry in &manifest.entries {
                prefixes.push(nfc(entry.path.as_str()));
                for file in &entry.files {
                    let source = layers
                        .iter()
                        .find(|(id, _)| id == file.layer_in(&entry.layer))
                        .and_then(|(_, root)| root.file_path(&file.logical))
                        .ok_or("not found")?;
                    let hash = blake3::Hash::from_hex(file.hash.trim_start_matches("blake3:"))
                        .map_err(|e| format!("startup hash of {}: {e}", file.destination))?;
                    let dest = nfc(&file.destination);
                    if taken.insert(dest.clone()) {
                        files.push(WantFile {
                            source_context: None,
                            source,
                            dest,
                            bytes: file.bytes,
                            mode: normalized_mode(
                                crate::content_engine::file_mode::selected_destination_mode(
                                    &file.logical,
                                    file.mode,
                                ),
                            ),
                            layer: match file.layer_in(&entry.layer) {
                                crate::agents::manifest::ContentLayer::Team { org, team } => {
                                    format!("team:{org}/{team}")
                                }
                                crate::agents::manifest::ContentLayer::Personal { org, .. } => {
                                    match org {
                                        Some(org) => format!("personal:{org}"),
                                        None => "personal".into(),
                                    }
                                }
                            },
                            pinned: Some(*hash.as_bytes()),
                        });
                    }
                }
            }
        }
        for entry in &resolution.entries {
            if entry.files > 0 {
                prefixes.push(nfc(entry.dest.as_deref().unwrap_or(&entry.path)));
            }
        }
        for file in &resolution.files {
            let dest = nfc(&file.destination);
            if !taken.insert(dest.clone()) {
                continue;
            }
            files.push(WantFile {
                source_context: None,
                source: file.source.clone(),
                dest,
                bytes: file.bytes,
                mode: normalized_mode(crate::content_engine::file_mode::selected_destination_mode(
                    &file.logical,
                    file.mode,
                )),
                layer: layer_label(&file.tier),
                pinned: None,
            });
        }
        files.sort_by(|a, b| a.dest.cmp(&b.dest));
        Ok(Self {
            files,
            prefixes: fold_prefixes(prefixes)?,
            source_roots,
            scope: Vec::new(),
            warnings: resolution
                .warnings
                .iter()
                .map(|warning| warning.message.clone())
                .collect(),
        })
    }

    /// A saved agent's want set at `target` (its repo override applied when
    /// the target names a repo). With a captured startup manifest, the
    /// context is resolved from the exact revision the launch captured, and
    /// a definition that changed since is `newIntentRequired` (SHOULD 15).
    pub fn for_agent(
        home: &Path,
        principal: &Principal,
        key: &AgentKey,
        target: &ManifestTarget,
        team_workspace: bool,
        manifest: Option<&StartupManifest>,
    ) -> Result<Self, String> {
        let resolution = match manifest {
            Some(manifest) => crate::agents::service::resolve_agent_context_pinned(
                home,
                principal,
                key,
                manifest.agent.revision,
                &manifest.agent.definition_hash,
                target,
                team_workspace,
            )?,
            None => crate::agents::service::resolve_agent_context(
                home,
                principal,
                key,
                target,
                team_workspace,
            )
            .map_err(|e| e.to_string())?,
        };
        Self::from_resolution(
            home,
            principal,
            manifest,
            target,
            &resolution,
            source_roots(home, principal, target),
        )
    }

    /// A plain `claude` / `codex` launch (no saved agent): the runtime's
    /// default context (SPEC §12.2).
    pub fn for_runtime(
        home: &Path,
        principal: &Principal,
        runtime: AgentCli,
        target: &ManifestTarget,
        team_workspace: bool,
    ) -> Result<Self, String> {
        Self::for_runtime_at(home, principal, runtime, target, team_workspace, None)
    }

    /// A shared context worktree starts with both runtimes' native discovery snapshot.
    pub(crate) fn for_context_worktree(
        home: &Path,
        principal: &Principal,
        target: &ManifestTarget,
        canonical: &Path,
        checkout: &Path,
    ) -> Result<Self, String> {
        let mut want = Self::default();
        for runtime in AgentCli::ALL {
            let native = Self::for_runtime_at(
                home,
                principal,
                runtime,
                target,
                true,
                Some((canonical, checkout)),
            )?;
            want.files.extend(native.files);
            want.prefixes.extend(native.prefixes);
            want.source_roots = native.source_roots;
            want.warnings.extend(native.warnings);
        }
        want.files.sort_by(|a, b| a.dest.cmp(&b.dest));
        want.files.dedup_by(|a, b| a.dest == b.dest);
        want.prefixes = fold_prefixes(want.prefixes)?;
        want.warnings.sort();
        want.warnings.dedup();
        Ok(want)
    }

    fn for_runtime_at(
        home: &Path,
        principal: &Principal,
        runtime: AgentCli,
        target: &ManifestTarget,
        team_workspace: bool,
        checkout: Option<(&Path, &Path)>,
    ) -> Result<Self, String> {
        let agent = crate::agents::Agent {
            id: "runtime-default".into(),
            name: runtime.as_str().into(),
            description: String::new(),
            created_by: String::new(),
            runtime,
            args: Vec::new(),
            launch: Default::default(),
            mcp: Default::default(),
            startup: Vec::new(),
            context: None,
            permissions: None,
            default_prompt: String::new(),
            avatar: None,
            role: None,
            allow_wake: false,
            resume_after_restart: false,
            budget_usd: None,
            revision: 0,
            archived_at: None,
            extra: Default::default(),
            env: Default::default(),
        };
        let request = ContextRequest {
            agent: &agent,
            target,
            repo_override: None,
            team_workspace,
        };
        let resolution = match checkout {
            Some((canonical, checkout)) => {
                crate::agents::manifest_resolve::resolve_context_in_checkout(
                    home, principal, request, canonical, checkout,
                )
            }
            None => resolve_context(home, principal, request),
        };
        Self::from_resolution(
            home,
            principal,
            None,
            target,
            &resolution,
            source_roots(home, principal, target),
        )
    }

    /// The files under `subtrees` of one content root: a team-workspace
    /// dir's default (`.claude/commands`, `.claude/skills`, SPEC §12.2).
    pub fn from_subtrees(content_root: &Path, subtrees: &[&str], layer: &str) -> Self {
        let mut files = Vec::new();
        for subtree in subtrees {
            for entry in walkdir::WalkDir::new(content_root.join(subtree))
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
            {
                let Ok(relative) = entry.path().strip_prefix(content_root) else {
                    continue;
                };
                let Some(dest) = relative.to_str().map(|d| nfc(&d.replace('\\', "/"))) else {
                    continue;
                };
                if crate::content_engine::destination::DestinationRelativePath::try_from(
                    dest.clone(),
                )
                .is_err()
                {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    meta.permissions().mode()
                };
                #[cfg(not(unix))]
                let mode = 0o644;
                files.push(WantFile {
                    source_context: None,
                    dest,
                    source: entry.path().to_path_buf(),
                    bytes: meta.len(),
                    mode: normalized_mode(mode),
                    layer: layer.to_string(),
                    pinned: None,
                });
            }
        }
        files.sort_by(|a, b| a.dest.cmp(&b.dest));
        files.dedup_by(|a, b| a.dest == b.dest);
        Self {
            files,
            prefixes: fold_prefixes(subtrees.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default(),
            source_roots: vec![content_root.to_path_buf()],
            warnings: Vec::new(),
            scope: Vec::new(),
        }
    }

    /// SW-XS-8 (the XS-14 handoff): the launch's merged workspace files
    /// (`GeneratedTarget::WorkspaceFile`, the deep merge of a merge-marked
    /// file across its layers) join the want set, so the engine writes them
    /// like any other context file: when absent, or replacing an unmodified
    /// copy it wrote, never over an edit or a file it did not write, never a
    /// tracked path; managed, and in this workspace's exclude block, under
    /// the destination's Step 2 lock — locally and on a machine. Each merge's
    /// bytes are kept content-addressed under the user's worktree state, the
    /// file the engine copies from. A generated file wins over any context
    /// file at the same path.
    pub fn add_workspace_files(
        &mut self,
        home: &Path,
        principal: &Principal,
        generated: &[crate::agents::manifest::GeneratedEntry],
    ) -> Result<(), String> {
        use crate::agents::manifest::GeneratedTarget;
        use crate::shared::config_format::ConfigFormat;
        let store = crate::context_repo::path_to_user_home(home, principal)
            .join("worktrees")
            .join("generated-v1");
        let mut prefixes = self.prefixes.clone();
        for entry in generated
            .iter()
            .filter(|entry| entry.target == GeneratedTarget::WorkspaceFile)
        {
            let Some(rel) = entry
                .path
                .as_deref()
                .filter(|p| crate::agents::paths::StartupPath::parse(p).is_ok())
            else {
                log::warn!(target: "agents", "generated workspace file {} has no valid path", entry.id);
                continue;
            };
            let Some(format) = ConfigFormat::detect(Path::new(rel)) else {
                continue;
            };
            // Review r1: a merge that can't be written is skipped (and
            // logged), never the reason a launch fails.
            let bytes = match format.serialize(&entry.content) {
                Ok(bytes) => bytes,
                Err(error) => {
                    log::warn!(target: "agents", "merged {rel} skipped: {error}");
                    self.warnings.push(format!(
                        "{rel}: the merged file could not be written ({error})"
                    ));
                    continue;
                }
            };
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let source = store.join(&hash);
            if source.is_file() {
                // In use: keep it out of the age-based cleanup below.
                if let Ok(file) = std::fs::File::options().write(true).open(&source) {
                    let _ = file.set_modified(std::time::SystemTime::now());
                }
            } else {
                if let Err(error) = crate::content_engine::write_tmp_fsync_rename(&source, &bytes) {
                    log::warn!(target: "agents", "merged {rel} skipped: {error}");
                    self.warnings.push(format!(
                        "{rel}: the merged file could not be staged ({error})"
                    ));
                    continue;
                }
            }
            let dest = nfc(rel);
            self.files.retain(|file| file.dest != dest);
            self.files.push(WantFile {
                source_context: None,
                dest: dest.clone(),
                source,
                bytes: bytes.len() as u64,
                mode: 0o644,
                layer: GENERATED_LAYER.into(),
                pinned: None,
            });
            prefixes.push(dest);
        }
        self.files.sort_by(|a, b| a.dest.cmp(&b.dest));
        self.prefixes = fold_prefixes(prefixes)?;
        prune_generated(&store);
        Ok(())
    }

    /// Whether `dest` (a workspace-relative path) is wanted.
    pub fn wants(&self, dest: &str) -> bool {
        self.files
            .binary_search_by(|file| file.dest.as_str().cmp(dest))
            .is_ok()
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }
}

/// Merged-file blobs unused this long are removed (each launch that uses one
/// refreshes its mtime).
const GENERATED_KEEP: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

/// Review r1: bound `generated-v1` — drop blobs no launch used in
/// [`GENERATED_KEEP`].
fn prune_generated(store: &Path) {
    let Ok(entries) = std::fs::read_dir(store) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified).unwrap_or_default() > GENERATED_KEEP
            });
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The content roots a target resolves through.
pub(crate) fn source_roots(
    home: &Path,
    principal: &Principal,
    target: &ManifestTarget,
) -> Vec<PathBuf> {
    content_layers(home, principal, target)
        .into_iter()
        .map(|(_, root)| root.safety_boundary())
        .collect()
}

/// An entry list for a declared context, for tests and callers that build
/// one by hand.
#[cfg(test)]
pub fn entries(paths: &[&str]) -> Vec<crate::agents::context::ContextEntry> {
    paths
        .iter()
        .map(|p| crate::agents::context::ContextEntry::new(*p))
        .collect()
}
