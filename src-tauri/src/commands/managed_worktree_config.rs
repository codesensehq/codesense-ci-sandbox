// SPDX-License-Identifier: MIT
//! The CodeSense config a CodeSense-managed worktree carries for an agent typed there by
//! hand.
//!
//! A launch through CodeSense brings its MCP bridge and status hooks in per-session files
//! ([`crate::commands::mcp_scope::project_profile_launch`]) and needs nothing here. A person
//! who opens a terminal in a story, branch, epic or team-workspace dir CodeSense made and
//! types `claude` or `codex` gets the project config each CLI's adapter names: the
//! `codesense` server in its MCP project config (`.mcp.json`, `.codex/config.toml`), and
//! its status hooks in its personal folder settings (`.claude/settings.local.json`). A
//! CodeSense launch there still gets each hook once: its per-launch settings skip events
//! the folder already wires.
//!
//! [`write`] checks the dir itself, whoever calls it: a dir under the managed `users/`
//! tree (never a context repo under `users/{uid}/orgs/`), or a linked worktree CodeSense
//! recorded. A user's checkout, a context repo or any folder a launch merely targets is
//! never written.
//!
//! In a git worktree the files are ignored by that worktree alone: its own exclude file,
//! kept in its git dir and named as `core.excludesFile` by a config include that applies
//! only to that git dir (`includeIf "gitdir:…"`). The repo's shared `info/exclude` is not
//! touched, so the main checkout, or any other worktree, still shows its own untracked
//! `.mcp.json`. Because that include replaces the user's `core.excludesFile` in the
//! worktree, the exclude file carries a copy of it first. Each worktree has its own files,
//! and `git config` takes its own lock, so worktrees set up at once never race.
//!
//! A file the repo tracks (present or not) is the team's and is left alone, as is one
//! reached through a symlink or one that is unparseable or oddly shaped. Best-effort: a
//! failure leaves the dir as it was and never blocks the open.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use log::warn;
use serde_json::{json, Value};

use crate::shared::config_format::ConfigFormat;
use codesense_pty_core::agent_cli::{descriptor, AgentCli, ScopeInjection};

/// The worktree's own exclude file and the config that names it, in its git dir.
const EXCLUDE_FILE: &str = "codesense-exclude";
const INCLUDE_FILE: &str = "codesense-config";

/// Write the hand-launch config into `dir`, when `dir` is a worktree CodeSense made under
/// the CodeSense home `home`.
pub(crate) fn write(dir: &Path, home: &Path) {
    write_in(
        dir,
        home,
        crate::commands::deps::bridge_symlink_path().as_deref(),
    );
}

pub(crate) fn write_context(
    dir: &Path,
    home: &Path,
    principal: &crate::domain::Principal,
) -> Result<(), String> {
    let written = write_in(
        dir,
        home,
        crate::commands::deps::bridge_symlink_path().as_deref(),
    );
    crate::agents::prepare::record_generated_files(home, principal, dir, &written)
}

fn write_in(dir: &Path, home: &Path, bridge: Option<&Path>) -> Vec<String> {
    if !dir.is_dir() || !is_managed_worktree(dir, home) {
        return Vec::new();
    }
    let mut written: Vec<&str> = Vec::new();
    if let Some(bridge) = bridge.and_then(Path::to_str) {
        for scope in AgentCli::ALL
            .iter()
            .filter_map(|cli| descriptor(*cli).mcp_scope.as_ref())
        {
            if matches!(&scope.injection, ScopeInjection::EnvContent { .. }) {
                continue;
            }
            let file = scope.config_filename.as_str();
            if merge_file(dir, file, scope.format.into(), |config| {
                put_bridge(
                    config,
                    &scope.server_map_key,
                    &scope.bridge_id,
                    &scope.bridge_entry.render(bridge, None),
                )
            }) {
                written.push(file);
            }
        }
    }
    for cli in AgentCli::ALL {
        let Some(hooks) = descriptor(cli).project_hooks.as_ref() else {
            continue;
        };
        if merge_file(dir, &hooks.local, ConfigFormat::Json, |settings| {
            crate::commands::content_engine::merge_local_notify_hooks(settings, dir, cli)
        }) {
            written.push(&hooks.local);
        }
    }
    if !written.is_empty()
        && crate::content_engine::git_worktree::GitWorktree::discover(dir)
            .ok()
            .flatten()
            .is_some()
    {
        if let Err(e) = exclude_in_worktree(dir, &written) {
            warn!(target: "managed_worktree_config", "could not exclude {written:?} in {}: {e}", dir.display());
        }
    }
    written.into_iter().map(str::to_string).collect()
}

/// Whether `dir` is a worktree CodeSense made: under the managed `users/` tree but not in a
/// context repo (`users/{uid}/orgs/…`), or a linked worktree recorded in the worktree index.
/// A repo's main checkout outside `users/` never is, recorded or not.
fn is_managed_worktree(dir: &Path, home: &Path) -> bool {
    let users = home.join(crate::context_repo::USERS_DIR);
    if let Ok(rel) = dir.strip_prefix(&users) {
        let mut parts = rel.components();
        parts.next();
        return parts.next()
            != Some(Component::Normal(std::ffi::OsStr::new(
                crate::context_repo::ORGS_DIR,
            )));
    }
    crate::content_engine::git_worktree::GitWorktree::discover(dir)
        .ok()
        .flatten()
        .is_some_and(|git| {
            git.linked
                && crate::install::worktree_index::is_codesense_managed_target(&git.root, home)
        })
}

/// Set `config[map_key][bridge_id]` to this app's bridge, keeping every other server.
/// `None` when the map is not a table.
fn put_bridge(config: &mut Value, map_key: &str, bridge_id: &str, server: &Value) -> Option<bool> {
    let servers = config
        .as_object_mut()?
        .entry(map_key)
        .or_insert_with(|| json!({}))
        .as_object_mut()?;
    if servers.get(bridge_id) == Some(server) {
        return Some(false);
    }
    servers.insert(bridge_id.to_string(), server.clone());
    Some(true)
}

/// Read `dir/rel` (absent or empty: an empty table), let `edit` change it, and write it
/// back atomically when it did. Returns whether the file holds CodeSense's entries now
/// (changed or already present), so it is excluded. A tracked file (even one missing from
/// the worktree), a path through a symlink, or an unparseable or non-table file is left
/// untouched (`false`).
fn merge_file(
    dir: &Path,
    rel: &str,
    format: ConfigFormat,
    edit: impl FnOnce(&mut Value) -> Option<bool>,
) -> bool {
    if through_symlink(dir, rel) || is_git_tracked(dir, rel) {
        return false;
    }
    let path = dir.join(rel);
    let existing = std::fs::read(&path).ok();
    let trailing_nl = existing.as_deref().and_then(<[u8]>::last) == Some(&b'\n');
    let mut config = match existing.as_deref() {
        None => json!({}),
        Some(b) if b.iter().all(u8::is_ascii_whitespace) => json!({}),
        Some(b) => match format.parse(b) {
            Ok(v) if v.is_object() => v,
            _ => return false,
        },
    };
    let Some(changed) = edit(&mut config) else {
        return false;
    };
    if !changed {
        return true;
    }
    let Ok(mut bytes) = format.serialize(&config) else {
        return false;
    };
    if trailing_nl && bytes.last() != Some(&b'\n') {
        bytes.push(b'\n');
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    match crate::content_engine::atomic_io::write_tmp_fsync_rename(&path, &bytes) {
        Ok(()) => true,
        Err(e) => {
            warn!(target: "managed_worktree_config", "could not write {}: {e}", path.display());
            false
        }
    }
}

/// Whether any existing component of `dir/rel` below `dir` is a symlink, so a write would
/// land outside the worktree's own files.
fn through_symlink(dir: &Path, rel: &str) -> bool {
    let mut at = dir.to_path_buf();
    Path::new(rel).components().any(|part| {
        at.push(part);
        at.symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    })
}

/// Whether the repo at `dir` tracks `rel`, present in the worktree or not. Any error (not
/// a repo, no git) is "not tracked".
fn is_git_tracked(dir: &Path, rel: &str) -> bool {
    git(dir, &["ls-files", "--error-unmatch", "--", rel]).is_ok()
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Ignore `rels` (root-relative) in the worktree at `dir` alone: write its own exclude file
/// (the user's effective `core.excludesFile` first, then the anchored entries) in its git
/// dir, and name it as `core.excludesFile` from a config file that the repo's config
/// includes only for this git dir.
fn exclude_in_worktree(dir: &Path, rels: &[&str]) -> Result<(), String> {
    let git_dir = PathBuf::from(git(dir, &["rev-parse", "--absolute-git-dir"])?);
    let common = PathBuf::from(git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let common_config = common.join("config");
    let resolved = crate::content_engine::git_worktree::GitWorktree::discover(dir)?;
    let nested = resolved.as_ref().filter(|g| g.nested_linked());
    let exclude = nested
        .map(|g| g.exclude())
        .unwrap_or_else(|| git_dir.join(EXCLUDE_FILE));
    let include = git_dir.join(INCLUDE_FILE);

    let mut body = String::from(
        "# Written by CodeSense for this worktree only: the user's own excludes, then the\n\
         # CodeSense config files an agent typed here reads.\n",
    );
    if let Some(user) = user_excludes(&common_config, &exclude) {
        body.push_str(&user);
        if !body.ends_with('\n') {
            body.push('\n');
        }
    }
    if nested.is_some() {
        if !exclude.exists() {
            write_if_changed(&exclude, body.as_bytes())?;
        }
        let paths: Vec<String> = rels
            .iter()
            .map(|rel| nested.unwrap().relative(rel))
            .collect();
        crate::content_engine::git_exclude::replace_workspace_block_in(
            &exclude,
            "generated-config",
            None,
            &paths,
        )
        .map_err(|e| e.to_string())?;
    } else {
        for rel in rels {
            body.push('/');
            body.push_str(rel);
            body.push('\n');
        }
        write_if_changed(&exclude, body.as_bytes())?;
    }
    let include_body = format!(
        "[core]\n\texcludesFile = {}\n",
        quote_config_value(&exclude.to_string_lossy())
    );
    write_if_changed(&include, include_body.as_bytes())?;

    let key = format!("includeIf.gitdir:{}.path", git_dir.display());
    let include_str = include.to_string_lossy().into_owned();
    let common_config_str = common_config.to_string_lossy().into_owned();
    let current = git(
        dir,
        &["config", "--file", &common_config_str, "--get", &key],
    )
    .ok();
    if current.as_deref() == Some(include_str.as_str()) {
        return Ok(());
    }
    // `git config` takes the config lock itself; another worktree set up at the same
    // moment holds it briefly, so retry.
    let mut last = String::new();
    for _ in 0..20 {
        match git(
            dir,
            &["config", "--file", &common_config_str, &key, &include_str],
        ) {
            Ok(_) => return Ok(()),
            Err(e) => last = e,
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(last)
}

/// The contents of the `core.excludesFile` the worktree would read without CodeSense's
/// include: the repo's own setting, else the global one, else git's default
/// (`$XDG_CONFIG_HOME/git/ignore`, `~/.config/git/ignore`). Never CodeSense's own file.
fn user_excludes(common_config: &Path, ours: &Path) -> Option<String> {
    let common = common_config.to_string_lossy().into_owned();
    let configured = git(
        Path::new("."),
        &["config", "--file", &common, "--get", "core.excludesFile"],
    )
    .ok()
    .or_else(|| {
        git(
            Path::new("."),
            &["config", "--global", "--get", "core.excludesFile"],
        )
        .ok()
    })
    .filter(|p| !p.is_empty());
    let path = match configured {
        Some(p) => expand_home(&p),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?
            .join("git/ignore"),
    };
    if path == ours {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(path), |h| h.join(rest)),
        None => PathBuf::from(path),
    }
}

/// A git config value, quoted so a path with spaces or `#`/`;` survives.
fn quote_config_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if std::fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    crate::content_engine::atomic_io::write_tmp_fsync_rename(path, bytes)
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests;
