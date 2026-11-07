// SPDX-License-Identifier: MIT
//! Launch warnings: things worth showing before an agent runs. They are only
//! ever shown; no launch, save or tool call is refused because of one.
//!
//! [`warnings`] is the one implementation every surface calls (launch tools,
//! routine save, the editors). It checks:
//!
//! - **The leading slash command.** A prompt passes to the CLI verbatim, so a
//!   prompt that starts with `/<name>` only works if the agent starts with that
//!   command or skill ([`super::provides`]). Only the leading token is checked,
//!   and CLI built-ins (`/compact`) are not special-cased: the warning is
//!   advisory.
//! - **Size.** There is no limit on what an agent starts with, but an unusually
//!   large startup set is usually a mistake. One entry over
//!   [`LARGE_ENTRY_FILES`] files or [`LARGE_ENTRY_BYTES`] bytes, a whole
//!   manifest over [`LARGE_MANIFEST_FILES`] or [`LARGE_MANIFEST_BYTES`], files
//!   under a directory that usually holds build output or dependencies
//!   ([`ACCIDENTAL_DIRS`]), and an entry mostly made of binary files (at least
//!   [`BINARY_HEAVY_BYTES`] bytes and half the entry) each get a warning with
//!   the counts.
//! - **Permissions that shut out CodeSense.** The agent's own Claude `deny` or
//!   `ask` rules apply exactly as written, even over the tools CodeSense
//!   needs ([`super::permissions::blocks_required`]); such an agent gets a
//!   warning that it cannot report back.

use std::collections::BTreeMap;

use serde::Serialize;

use super::manifest::{GeneratedTarget, ManifestEntry, StartupManifest};
use super::permissions::blocks_required;
use super::provides::AgentProvides;
use super::resolve::ResolvedAgent;
use codesense_pty_core::agent_cli::{descriptor, AgentCli};

/// The warning for an agent whose own permissions block a CodeSense tool.
pub const DENIES_CODESENSE_TOOLS_MESSAGE: &str =
    "this agent denies codesense tools — it won't be able to signal_done / read routine_context";

/// One entry over this many files is unusually large.
pub const LARGE_ENTRY_FILES: u64 = 500;
/// One entry over this many bytes (25 MiB) is unusually large.
pub const LARGE_ENTRY_BYTES: u64 = 25 * 1024 * 1024;
/// A manifest over this many files is unusually large.
pub const LARGE_MANIFEST_FILES: u64 = 2_000;
/// A manifest over this many bytes (100 MiB) is unusually large.
pub const LARGE_MANIFEST_BYTES: u64 = 100 * 1024 * 1024;
/// An entry whose binary files reach this many bytes (10 MiB), and at least
/// half its bytes, is binary-heavy.
pub const BINARY_HEAVY_BYTES: u64 = 10 * 1024 * 1024;
/// Directory names that usually hold build output, dependencies or caches
/// rather than context. (Version-control dirs and `node_modules` are never
/// carried at all.)
pub const ACCIDENTAL_DIRS: &[&str] = &[
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    ".gradle",
    ".tox",
    ".pytest_cache",
    ".mypy_cache",
    "coverage",
];
/// File extensions counted as binary.
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "tiff", "psd", "pdf", "zip", "gz", "tgz",
    "tar", "bz2", "xz", "7z", "rar", "jar", "war", "class", "o", "a", "so", "dylib", "dll", "exe",
    "bin", "wasm", "mp3", "mp4", "mov", "avi", "mkv", "wav", "flac", "ttf", "otf", "woff", "woff2",
    "sqlite", "sqlite3", "db", "parquet", "npy", "pt", "onnx",
];

/// What a [`LaunchWarning`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchWarningCode {
    Inheritance,
    ContextCloneEdits,
    LegacyWorker,
    /// The prompt starts with a `/name` the agent does not start with.
    CommandNotInManifest,
    /// One entry is unusually large.
    LargeEntry,
    /// The whole manifest is unusually large.
    LargeManifest,
    /// Files under a directory that usually holds build output or dependencies.
    AccidentalPath,
    /// An entry made mostly of binary files.
    BinaryHeavy,
    /// The agent's own permissions block a tool CodeSense needs.
    DeniesCodesenseTools,
    BridgeToolsExcluded,
    UnknownBridgeTools,
    BridgeToolDispatch,
    BridgeToolsLocalOnly,
    /// The agent does not resolve on this Mac, so a launch of it fails.
    AgentUnavailable,
    /// SW-XS-13: the agent's `context` is not valid. Shown, never enforced,
    /// until launches apply context (SW-XS-7).
    InvalidContext,
    /// SW-AGR-2: a wake routine on an agent that runs without permission
    /// checks and did not set `allowWake`: its wakes are refused.
    WakeNotAllowed,
    /// SW-AGR-13: wake limits on a routine that starts a new agent each
    /// fire: they count wakes into a live session, so they never apply.
    WakeLimitsIgnored,
    /// SW-AGR-13: a wake routine with limits armed on a machine whose
    /// terminal server is too old to enforce them: it fires nowhere there.
    WakeLimitsUnenforced,
    /// SW-XS-25: a change to a machine-armed routine was saved but not
    /// prepared on its machine (not connected, preparation failed, or still
    /// running): it fires there with what was prepared before.
    MachineNotPrepared,
    /// SW-AN-50: the agent names no MCP servers while the team's MCP config
    /// offers some, so a launch reaches only the CodeSense bridge even though
    /// extra servers are available.
    NoMcpServersOffered,
    /// SW-AN-52: the agent names MCP servers the content-layer config does
    /// not define, so a launch drops them from its scoped set.
    McpServersDropped,
    /// SW-AN-52: the content-layer MCP config cannot be resolved (bytes-limit,
    /// parse error): launch degrades to the cwd `.mcp.json` alone, so a server
    /// defined only in the content layer may silently drop.
    McpConfigUnresolvable,
    /// `open_workspace` was called with `maxRuntime` on a session
    /// that is already live. The existing deadline (or lack of one) is left
    /// alone; a running session's budget never changes mid-flight.
    MaxRuntimeIgnored,
    RoleNotJoined,
    StartupDeliveryUnconfirmed,
    /// The agent's CLI is installed at a version outside the range CodeSense is
    /// qualified on: it still runs, but status, inbox wake and usage may not work.
    CliVersionUnqualified,
    /// A project document outranks the injected root instructions.
    ShadowingRootDocument,
    /// This Mac's managed Claude settings send the session's usage to the organization's own
    /// collector, so CodeSense records no tokens or cost for it and a spend cap can't be enforced.
    UsageNotRecorded,
    AdapterNote,
}

/// A warning shown before a launch. Never enforced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaunchWarning {
    pub code: LaunchWarningCode,
    pub message: String,
}

/// The warning for a CLI installed at `version` (its parsed release number) when that
/// is outside the range `runtime` is qualified on. A version that could not be read
/// is not a warning: nothing is known.
pub fn cli_version_warning(runtime: AgentCli, version: Option<&str>) -> Option<LaunchWarning> {
    let version = version?;
    let release = &descriptor(runtime).release;
    (!release.version_qualified(version)).then(|| LaunchWarning {
        code: LaunchWarningCode::CliVersionUnqualified,
        message: format!(
            "This CLI version ({version}) isn't supported yet (supported: {}). Status, inbox wake and usage may not work.",
            release.supported
        ),
    })
}

/// The warning for a launch of `agent` whose usage this Mac's managed settings send elsewhere,
/// saying too that its spend cap can't be enforced when it has one.
pub fn usage_not_recorded_warning(agent: &super::model::Agent) -> Option<LaunchWarning> {
    let capped = agent
        .budget_usd
        .is_some_and(|cap| cap.per_day.is_some() || cap.per_week.is_some());
    usage_warning(agent.runtime, capped)
}

fn usage_warning(runtime: AgentCli, capped: bool) -> Option<LaunchWarning> {
    let base = "Your organization's Claude settings send this session's usage to its own collector, so CodeSense can't show its tokens or cost";
    crate::session_usage::managed_telemetry::unrecorded(runtime).then(|| LaunchWarning {
        code: LaunchWarningCode::UsageNotRecorded,
        message: if capped {
            format!("{base}, and this agent's spend cap can't be enforced.")
        } else {
            format!("{base}.")
        },
    })
}

/// Project files with higher CLI precedence than the injected context package.
pub(crate) fn root_document_warnings(
    runtime: AgentCli,
    cwd: &std::path::Path,
) -> Vec<LaunchWarning> {
    let facts = descriptor(runtime);
    if Vec::is_empty(&facts.shadowing_root_docs) {
        return Vec::new();
    }
    let Ok(cwd) = std::fs::canonicalize(cwd) else {
        return Vec::new();
    };
    let boundary = if facts.shadowing_root_docs_up_to_git_root {
        Iterator::find(&mut cwd.ancestors(), |directory| {
            std::path::Path::exists(&directory.join(".git"))
        })
    } else {
        None
    };
    for directory in cwd.ancestors() {
        if let Some(name) = Iterator::find(&mut facts.shadowing_root_docs.iter(), |name| {
            directory.join(name).is_file()
        }) {
            return vec![LaunchWarning {
                code: LaunchWarningCode::ShadowingRootDocument,
                message: format!("{name} takes precedence over {}; this launch may not read the team's injected instructions", facts.root_doc),
            }];
        }
        // Outside Git (or a cwd-only adapter), no ancestor can supply project rules.
        if boundary.is_none_or(|root| directory == root) {
            break;
        }
    }
    Vec::new()
}

/// Everything worth warning about before launching `resolved` with `prompt`.
pub fn warnings(resolved: &ResolvedAgent, prompt: Option<&str>) -> Vec<LaunchWarning> {
    let mut out: Vec<LaunchWarning> = command_warning(&resolved.provides, prompt)
        .into_iter()
        .collect();
    out.extend(
        std::iter::IntoIterator::into_iter((super::adapter::desktop(
            resolved.account.policy.runtime,
        )
        .launch_notes)(&resolved.launch.args))
        .map(|message| LaunchWarning {
            code: LaunchWarningCode::AdapterNote,
            message,
        }),
    );
    out.extend(size_warnings(&resolved.manifest));
    out.extend(permission_warning(&resolved.manifest));
    if let Some(tools) = &resolved.launch.bridge_tools {
        out.push(LaunchWarning {
            code: LaunchWarningCode::BridgeToolsLocalOnly,
            message: if super::adapter::desktop(resolved.account.policy.runtime).enforces_bridge_tool_allowlist {
                "bridge_tools is enforced for local launches; remote preparation refuses an explicit list"
            } else {
                "bridge_tools is not enforced by this runtime; the requested list controls tool exposure only"
            }.into(),
        });
        let missing: Vec<_> = ["signal_done", "read_messages", "reply"]
            .into_iter()
            .filter(|name| !tools.iter().any(|tool| tool == name))
            .collect();
        if !missing.is_empty() {
            out.push(LaunchWarning { code: LaunchWarningCode::BridgeToolsExcluded,
                message: format!("bridge_tools excludes {}; completion needs signal_done and inbox use needs read_messages and reply", missing.join(", ")) });
        }
        let unknown: Vec<_> = tools
            .iter()
            .filter(|name| !resolved.launch.effective_bridge_tools.contains(name))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            out.push(LaunchWarning {
                code: LaunchWarningCode::UnknownBridgeTools,
                message: format!(
                    "unknown bridge tools grant no access: {}",
                    unknown.join(", ")
                ),
            });
        }
        if tools.iter().any(|name| name == "codesense_call_tool") {
            out.push(LaunchWarning { code: LaunchWarningCode::BridgeToolDispatch,
                message: "codesense_call_tool requires both its own name and the inner tool name in bridge_tools".into() });
        }
    }
    out
}

/// The warning for generated launch settings whose `deny` or `ask` rules
/// (the agent's own, passed through verbatim) block a required tool, naming
/// those rules.
pub(crate) fn permission_warning(manifest: &StartupManifest) -> Option<LaunchWarning> {
    let mut blocking = Vec::new();
    for entry in &manifest.generated {
        if entry.target != GeneratedTarget::LaunchSettings {
            continue;
        }
        for list in ["deny", "ask"] {
            let rules = entry.content["permissions"][list].as_array();
            for rule in rules.into_iter().flatten().filter_map(|r| r.as_str()) {
                if blocks_required(rule) {
                    blocking.push(format!("{list} {rule:?}"));
                }
            }
        }
    }
    (!blocking.is_empty()).then(|| LaunchWarning {
        code: LaunchWarningCode::DeniesCodesenseTools,
        message: format!(
            "{DENIES_CODESENSE_TOOLS_MESSAGE} (permissions.claude {})",
            blocking.join(", ")
        ),
    })
}

/// The warning for a prompt whose leading `/name` the agent does not start
/// with. A namespaced `/ns:name` is found as the command `ns:name`, or as the
/// skill `name` (a plain command `name` is not `/ns:name`).
pub(crate) fn command_warning(
    provides: &AgentProvides,
    prompt: Option<&str>,
) -> Option<LaunchWarning> {
    let rest = prompt?.trim().strip_prefix('/')?;
    let token = rest.split_whitespace().next()?;
    if matches!(
        token,
        "resume"
            | "help"
            | "clear"
            | "compact"
            | "config"
            | "status"
            | "model"
            | "exit"
            | "cost"
            | "context"
            | "permissions"
            | "login"
            | "logout"
    ) {
        return None;
    }
    let short = token.rsplit_once(':').map(|(_, name)| name);
    if provides.has(token) || short.is_some_and(|name| provides.skills.iter().any(|s| s == name)) {
        return None;
    }
    Some(LaunchWarning {
        code: LaunchWarningCode::CommandNotInManifest,
        message: format!(
            "this agent doesn't start with /{token} — add it to the agent or the run won't find it"
        ),
    })
}

/// The size warnings for `manifest`.
pub(crate) fn size_warnings(manifest: &StartupManifest) -> Vec<LaunchWarning> {
    let mut out = Vec::new();
    for entry in &manifest.entries {
        let files = entry.files.len() as u64;
        let bytes: u64 = entry.files.iter().map(|f| f.bytes).sum();
        if files > LARGE_ENTRY_FILES || bytes > LARGE_ENTRY_BYTES {
            out.push(LaunchWarning {
                code: LaunchWarningCode::LargeEntry,
                message: format!(
                    "{} is {} in {files} files — did you mean to include all of it?",
                    entry.path,
                    human_bytes(bytes)
                ),
            });
        }
        out.extend(accidental_dirs(entry));
        let binary: u64 = entry
            .files
            .iter()
            .filter(|f| is_binary(&f.destination))
            .map(|f| f.bytes)
            .sum();
        if binary >= BINARY_HEAVY_BYTES && binary * 2 >= bytes {
            out.push(LaunchWarning {
                code: LaunchWarningCode::BinaryHeavy,
                message: format!(
                    "{} is mostly binary files ({} of {}) — did you mean to include this?",
                    entry.path,
                    human_bytes(binary),
                    human_bytes(bytes)
                ),
            });
        }
    }
    let totals = manifest.totals;
    if totals.files > LARGE_MANIFEST_FILES || totals.bytes > LARGE_MANIFEST_BYTES {
        out.push(LaunchWarning {
            code: LaunchWarningCode::LargeManifest,
            message: format!(
                "this agent starts with {} in {} files — did you mean to include all of it?",
                human_bytes(totals.bytes),
                totals.files
            ),
        });
    }
    out
}

/// One warning per directory in `entry` named like build output or
/// dependencies, outermost first, with what it holds.
fn accidental_dirs(entry: &ManifestEntry) -> Vec<LaunchWarning> {
    let mut found: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for file in &entry.files {
        let segments: Vec<&str> = file.destination.split('/').collect();
        // A skill or command dir is named by its author (`.claude/skills/build/`),
        // so that segment never counts.
        let named = |i: usize| {
            i >= 2
                && matches!(
                    (segments[i - 2], segments[i - 1]),
                    (".claude", "skills") | (".claude", "commands") | (".agents", "skills")
                )
        };
        let Some(at) = segments[..segments.len().saturating_sub(1)]
            .iter()
            .enumerate()
            .position(|(i, s)| ACCIDENTAL_DIRS.contains(s) && !named(i))
        else {
            continue;
        };
        let dir = segments[..=at].join("/");
        let slot = found.entry(dir).or_default();
        slot.0 += 1;
        slot.1 += file.bytes;
    }
    found
        .into_iter()
        .map(|(dir, (files, bytes))| LaunchWarning {
            code: LaunchWarningCode::AccidentalPath,
            message: format!(
                "{} in {files} files under {dir}/ — did you mean to include this?",
                human_bytes(bytes)
            ),
        })
        .collect()
}

fn is_binary(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && !stem.ends_with('/')
            && BINARY_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    })
}

/// `1.2 GB`, `340 KB`, `12 B`.
fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::keys::AgentKey;
    use crate::agents::manifest::{
        content_hash, AgentRevisionRef, ContentLayer, EntryOrigin, ManifestFile, ManifestTotals,
    };
    use crate::agents::model::{Agent, ClaudePermissions, LaunchMode, McpAccess, Permissions};
    use crate::agents::paths::StartupPath;
    use crate::agents::permissions;
    use codesense_pty_core::agent_cli::AgentCli;

    fn provides(commands: &[&str], skills: &[&str]) -> AgentProvides {
        AgentProvides {
            commands: commands.iter().map(|s| s.to_string()).collect(),
            skills: skills.iter().map(|s| s.to_string()).collect(),
        }
    }

    const MESSAGE: &str =
        "this agent doesn't start with /triage-slack — add it to the agent or the run won't find it";

    #[test]
    fn cli_version_warning_is_neutral_and_only_for_a_readable_out_of_range_version() {
        let warned = cli_version_warning(AgentCli::OpenCode, Some("3.0.1")).unwrap();
        assert_eq!(warned.code, LaunchWarningCode::CliVersionUnqualified);
        assert_eq!(
            warned.message,
            "This CLI version (3.0.1) isn't supported yet (supported: OpenCode 2.1.4 or a newer 2.x release). Status, inbox wake and usage may not work."
        );
        assert!(cli_version_warning(AgentCli::OpenCode, Some("2.1.4")).is_none());
        assert!(cli_version_warning(AgentCli::OpenCode, None).is_none());
        assert!(cli_version_warning(AgentCli::OpenCode, Some("banana")).is_some());
        assert!(cli_version_warning(AgentCli::Claude, Some("2.1.140")).is_some());
        assert!(cli_version_warning(AgentCli::Claude, Some("2.1.141")).is_none());
        assert!(cli_version_warning(AgentCli::Codex, Some("0.158.0")).is_some());
    }

    #[test]
    fn the_leading_command_must_be_in_the_manifest() {
        let command = provides(&["triage-slack"], &[]);
        let skill = provides(&[], &["triage-slack"]);
        let neither = provides(&["other"], &["other"]);
        let go = Some("/triage-slack go");

        assert_eq!(command_warning(&command, go), None);
        assert_eq!(command_warning(&skill, go), None);
        let warned = command_warning(&neither, go).unwrap();
        assert_eq!(warned.code, LaunchWarningCode::CommandNotInManifest);
        assert_eq!(warned.message, MESSAGE);
        assert_eq!(
            command_warning(&neither, Some("  /triage-slack\n"))
                .unwrap()
                .message,
            MESSAGE
        );

        assert_eq!(
            command_warning(&neither, Some("please /triage-slack")),
            None
        );
        assert_eq!(command_warning(&neither, Some("")), None);
        assert_eq!(command_warning(&neither, Some("   ")), None);
        assert_eq!(command_warning(&neither, Some("/")), None);
        assert_eq!(command_warning(&neither, None), None);
    }

    #[test]
    fn a_namespaced_command_matches_in_full_or_by_its_name() {
        assert_eq!(
            command_warning(&provides(&["ops:deploy"], &[]), Some("/ops:deploy now")),
            None
        );
        assert_eq!(
            command_warning(&provides(&[], &["deploy"]), Some("/ops:deploy")),
            None
        );
        assert!(command_warning(&provides(&["ops"], &[]), Some("/ops:deploy")).is_some());
        // A plain command is not the namespaced one.
        assert!(command_warning(&provides(&["deploy"], &[]), Some("/ops:deploy")).is_some());
    }

    fn file(path: &str, bytes: u64) -> ManifestFile {
        ManifestFile {
            logical: path.into(),
            destination: path.into(),
            hash: content_hash(b""),
            bytes,
            mode: 0o644,
            layer: None,
        }
    }

    fn manifest(entries: Vec<(&str, Vec<ManifestFile>)>) -> StartupManifest {
        let entries: Vec<ManifestEntry> = entries
            .into_iter()
            .map(|(path, files)| ManifestEntry {
                path: StartupPath::parse(path).unwrap(),
                origin: EntryOrigin::Declared,
                layer: ContentLayer::Team {
                    org: "acme/api".into(),
                    team: "backend".into(),
                },
                entry_hash: String::new(),
                files,
            })
            .collect();
        let totals =
            entries
                .iter()
                .flat_map(|e| &e.files)
                .fold(ManifestTotals::default(), |t, f| ManifestTotals {
                    files: t.files + 1,
                    bytes: t.bytes + f.bytes,
                });
        StartupManifest {
            agent: AgentRevisionRef {
                key: AgentKey::new("acme/api", "backend", "a"),
                revision: 1,
                definition_hash: content_hash(b""),
            },
            runtime: AgentCli::Claude,
            entries,
            essentials: Vec::new(),
            generated: Vec::new(),
            totals,
            errors: Vec::new(),
            manifest_hash: String::new(),
        }
    }

    fn codes(warnings: &[LaunchWarning]) -> Vec<LaunchWarningCode> {
        warnings.iter().map(|w| w.code).collect()
    }

    #[test]
    fn an_ordinary_skill_dir_gets_no_size_warning() {
        let files = (0..144)
            .map(|i| file(&format!(".claude/skills/wds/f{i}.md"), 4096))
            .collect();
        assert_eq!(
            size_warnings(&manifest(vec![(".claude/skills/wds", files)])),
            Vec::new()
        );
    }

    #[test]
    fn large_entries_and_manifests_warn_with_their_counts() {
        let many: Vec<ManifestFile> = (0..=LARGE_ENTRY_FILES)
            .map(|i| file(&format!("docs/f{i}.md"), 10))
            .collect();
        let w = size_warnings(&manifest(vec![("docs", many)]));
        assert_eq!(codes(&w), vec![LaunchWarningCode::LargeEntry]);
        assert_eq!(
            w[0].message,
            "docs is 4.9 KB in 501 files — did you mean to include all of it?"
        );

        // Entries each at the entry limit, together over the manifest's.
        let entries = ["a.md", "b.md", "c.md", "d.md", "e.md"]
            .into_iter()
            .map(|p| (p, vec![file(p, LARGE_ENTRY_BYTES)]))
            .collect();
        let w = size_warnings(&manifest(entries));
        assert_eq!(codes(&w), vec![LaunchWarningCode::LargeManifest]);
        assert_eq!(
            w[0].message,
            "this agent starts with 125 MB in 5 files — did you mean to include all of it?"
        );
    }

    #[test]
    fn build_and_dependency_dirs_are_named_with_what_they_hold() {
        let mut files = vec![file("app/src/main.rs", 100)];
        files.extend((0..3).map(|i| file(&format!("app/target/debug/o{i}"), 1024 * 1024)));
        files.push(file("app/target/debug/deep/build/x", 10));
        files.push(file("app/build", 5));
        files.push(file(".claude/skills/build/SKILL.md", 5));
        files.push(file(".claude/commands/dist/ship.md", 5));
        let w = size_warnings(&manifest(vec![("app", files)]));
        assert_eq!(codes(&w), vec![LaunchWarningCode::AccidentalPath]);
        assert_eq!(
            w[0].message,
            "3.0 MB in 4 files under app/target/ — did you mean to include this?"
        );
    }

    #[test]
    fn a_mostly_binary_entry_warns() {
        let files = vec![
            file("assets/video.mp4", BINARY_HEAVY_BYTES),
            file("assets/README.md", 100),
        ];
        let w = size_warnings(&manifest(vec![("assets", files)]));
        assert_eq!(codes(&w), vec![LaunchWarningCode::BinaryHeavy]);
        assert_eq!(
            w[0].message,
            "assets is mostly binary files (10.0 MB of 10.0 MB) — did you mean to include this?"
        );
        // Small binaries in a text entry are fine.
        let files = vec![file("docs/logo.png", 50_000), file("docs/a.md", 10)];
        assert_eq!(size_warnings(&manifest(vec![("docs", files)])), Vec::new());
    }

    #[test]
    fn bytes_read_like_a_person_writes_them() {
        assert_eq!(human_bytes(12), "12 B");
        assert_eq!(human_bytes(340 * 1024), "340 KB");
        assert_eq!(human_bytes(1_288_490_189), "1.2 GB");
    }

    fn with_claude_rules(deny: &[&str], ask: &[&str]) -> StartupManifest {
        let agent = Agent {
            role: None,
            id: "a".into(),
            name: "A".into(),
            description: String::new(),
            created_by: "1".into(),
            runtime: AgentCli::Claude,
            args: Vec::new(),
            launch: LaunchMode::Headless,
            mcp: McpAccess::default(),
            startup: Vec::new(),
            context: None,
            permissions: Some(Permissions {
                claude: Some(ClaudePermissions {
                    deny: deny.iter().map(|s| s.to_string()).collect(),
                    ask: ask.iter().map(|s| s.to_string()).collect(),
                    ..ClaudePermissions::default()
                }),
                ..Permissions::default()
            }),
            default_prompt: String::new(),
            avatar: None,
            allow_wake: false,
            resume_after_restart: false,
            budget_usd: None,
            revision: 1,
            archived_at: None,
            extra: Default::default(),
            env: Default::default(),
        };
        let mut m = manifest(Vec::new());
        m.generated = permissions::generate(&agent, AgentCli::Claude, LaunchMode::Headless);
        m
    }

    #[test]
    fn permissions_that_block_codesense_tools_warn_and_name_the_rules() {
        let w = permission_warning(&with_claude_rules(
            &["WebFetch", "mcp__codesense__*"],
            &["mcp__codesense__signal_done", "Bash"],
        ))
        .unwrap();
        assert_eq!(w.code, LaunchWarningCode::DeniesCodesenseTools);
        assert_eq!(
            w.message,
            format!(
                "{DENIES_CODESENSE_TOOLS_MESSAGE} (permissions.claude deny \"mcp__codesense__*\", \
                 ask \"mcp__codesense__signal_done\")"
            )
        );
        assert!(w.message.starts_with(
            "this agent denies codesense tools — it won't be able to signal_done / read routine_context"
        ));

        assert_eq!(
            permission_warning(&with_claude_rules(
                &["WebFetch", "mcp__codesense__gs_publish"],
                &["Bash"]
            )),
            None
        );
        assert_eq!(permission_warning(&manifest(Vec::new())), None);
    }
}

#[cfg(test)]
mod native_command_tests {
    use super::*;
    #[test]
    fn resume_and_native_commands_need_no_manifest_entry() {
        let provides = AgentProvides::default();
        for prompt in [
            "/resume",
            "/resume previous-session",
            "/help",
            "/model",
            "/compact",
        ] {
            assert!(
                command_warning(&provides, Some(prompt)).is_none(),
                "{prompt}"
            );
        }
        assert!(command_warning(&provides, Some("/custom-skill")).is_some());
    }

    /// Under managed Claude settings that send usage elsewhere, a Claude launch warns that its
    /// usage isn't shown, and a capped one that its cap can't be enforced; Codex doesn't warn.
    #[test]
    fn a_claude_launch_warns_when_managed_settings_send_its_usage_elsewhere() {
        let agent = |runtime: &str, cap: &str| -> crate::agents::model::Agent {
            serde_yaml::from_str(&format!("id: a\nname: A\nruntime: {runtime}\n{cap}")).unwrap()
        };
        let capped = "budget_usd: {per_day: 5}\n";
        assert_eq!(usage_not_recorded_warning(&agent("claude", capped)), None);
        let read_from_its_log = crate::session_usage::managed_telemetry::Fixture::redirecting();
        assert_eq!(usage_not_recorded_warning(&agent("claude", capped)), None, "Claude's session log is read");
        drop(read_from_its_log);
        let _managed = crate::session_usage::managed_telemetry::Fixture::redirecting_unread();
        let base = "Your organization's Claude settings send this session's usage to its own collector, so CodeSense can't show its tokens or cost";
        assert_eq!(
            usage_not_recorded_warning(&agent("claude", "")),
            Some(LaunchWarning {
                code: LaunchWarningCode::UsageNotRecorded,
                message: format!("{base}."),
            })
        );
        assert_eq!(
            usage_not_recorded_warning(&agent("claude", capped)).map(|w| w.message),
            Some(format!("{base}, and this agent's spend cap can't be enforced."))
        );
        assert_eq!(usage_not_recorded_warning(&agent("codex", capped)), None);
    }
}

#[cfg(test)]
#[path = "launch_check/root_document_tests.rs"]
mod root_document_tests;
