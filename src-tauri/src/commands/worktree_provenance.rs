// SPDX-License-Identifier: MIT
// SW-WT-9: team-sync setup provenance gate.
//
// `worktree.yaml` lives at the SS-8 canonical location
// `{org_root}/_codesense/worktree/{owner}/{repo}.yaml` — inside the
// git-backed context repo. Once that repo carries a teammate's commit,
// running its `setup` commands is running someone else's code. This module
// answers one question without ever touching the network: did the current
// on-disk config originate from a teammate's commit on `origin/HEAD`
// (`TeamSynced`), or did the user author/edit it locally (`Local`)?
//
// Only `TeamSynced` configs are gate-eligible. The decision is a pure local
// read built on `read_from_origin_head` (offline `origin/HEAD` blob read)
// plus a git2 author-of-last-change lookup — see `classify_provenance`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::commands::worktree::ss8_worktree_config_path;
use crate::context_repo::read_from_origin_head;

/// Whether a worktree.yaml's current content came from a teammate via the
/// git-synced context repo (`TeamSynced`) or was authored/edited on this
/// machine (`Local`). Only `TeamSynced` triggers the consent gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupProvenance {
    Local,
    TeamSynced,
}

/// What the frontend gate needs to decide whether to prompt. `config_hash`
/// is `None` when there is nothing to gate (no config, or a legacy
/// in-source-repo config — always `Local`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupProvenanceReport {
    pub provenance: SetupProvenance,
    pub config_hash: Option<String>,
}

impl SetupProvenanceReport {
    fn local() -> Self {
        Self {
            provenance: SetupProvenance::Local,
            config_hash: None,
        }
    }
}

/// SHA-256 (lowercase hex) of the exact config file bytes. The hash is taken
/// over the raw bytes that load — not a re-serialized struct — so the
/// approval key and the execution-time re-check (AC-6) compare the same thing
/// that will run.
pub fn config_content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Evaluate the provenance of the worktree.yaml that governs `source_repo_path`.
///
/// Resolution mirrors `read_worktree_config`: only the SS-8 canonical path
/// (inside the context repo) can be `TeamSynced`. A legacy in-source-repo
/// config, an absent config, or an unresolvable org_root/slug is always
/// `Local` (AC-4, AC-7). Pure local read — no fetch, no disk mutation.
pub fn setup_provenance_report(source_repo_path: &Path, org_root: &Path) -> SetupProvenanceReport {
    let Some(ss8_path) = ss8_worktree_config_path(source_repo_path, org_root) else {
        return SetupProvenanceReport::local();
    };
    let Ok(bytes) = std::fs::read(&ss8_path) else {
        // No SS-8 config on disk → either no config at all or a legacy
        // in-source-repo config. Both are `Local` — nothing teammate-authored
        // can be at the canonical path.
        return SetupProvenanceReport::local();
    };
    // The git-tree path is the SS-8 path relative to org_root, in POSIX form
    // (`read_from_origin_head` requires forward slashes on every platform).
    // Deriving it from `ss8_path` keeps the layout owned solely by
    // `path_to_worktree_config`.
    let Some(rel_in_tree) = tree_relative_posix(&ss8_path, org_root) else {
        return SetupProvenanceReport::local();
    };

    let provenance = classify_provenance(org_root, &rel_in_tree, &bytes);
    SetupProvenanceReport {
        provenance,
        config_hash: Some(config_content_hash(&bytes)),
    }
}

/// Convert an absolute path under `org_root` into a POSIX, slash-separated
/// path relative to it. Returns `None` if `path` isn't under `org_root` or
/// contains non-normal components.
fn tree_relative_posix(path: &Path, org_root: &Path) -> Option<String> {
    let rel = path.strip_prefix(org_root).ok()?;
    let mut parts = Vec::new();
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Core decision over a known SS-8 config. `wt_bytes` are the working-tree
/// bytes that will execute; `rel_in_tree` is the POSIX path inside the
/// context-repo git tree.
///
/// `Local` when: the path isn't on `origin/HEAD` (never pushed, no remote,
/// detached — AC-7); OR the working-tree bytes differ from the `origin/HEAD`
/// bytes (the user edited it, taking ownership — AC-4); OR the last commit to
/// touch the path on `origin/HEAD` was authored by the current user (AC-4).
/// `TeamSynced` only when the content matches a pushed commit authored by
/// someone else (or an indeterminate author — fail safe by gating, since the
/// content is confirmed to be on the remote).
pub fn classify_provenance(org_root: &Path, rel_in_tree: &str, wt_bytes: &[u8]) -> SetupProvenance {
    let origin_bytes = match read_from_origin_head(org_root, rel_in_tree) {
        Ok(b) => b,
        // Not on the remote / no remote / not fetched → no teammate vector.
        Err(_) => return SetupProvenance::Local,
    };
    if normalize_eol(&origin_bytes) != normalize_eol(wt_bytes) {
        // Working tree diverges from what was pushed → locally edited.
        // Compared with line endings normalized so a CRLF checkout (Windows
        // `core.autocrlf`) of an LF blob isn't misread as a local edit.
        return SetupProvenance::Local;
    }
    match last_author_email_for_path(org_root, rel_in_tree) {
        Some(author) => match current_user_email(org_root) {
            Some(me) if !me.is_empty() && me.eq_ignore_ascii_case(&author) => {
                SetupProvenance::Local
            }
            _ => SetupProvenance::TeamSynced,
        },
        // Content is confirmed on the remote but the author is indeterminate —
        // fail safe by gating rather than silently running.
        None => SetupProvenance::TeamSynced,
    }
}

/// Email of the author of the most recent commit on `origin/HEAD` that
/// changed `rel_in_tree`. Walks `origin/HEAD` history newest-first and stops
/// at the first commit whose blob at the path differs from all its parents.
/// Returns `None` on any git error or if the path is never found (callers
/// treat `None` as gate-safe).
fn last_author_email_for_path(org_root: &Path, rel_in_tree: &str) -> Option<String> {
    let repo = git2::Repository::open(org_root).ok()?;
    let head = repo
        .find_reference("refs/remotes/origin/HEAD")
        .ok()?
        .resolve()
        .ok()?
        .peel_to_commit()
        .ok()?;

    let mut walk = repo.revwalk().ok()?;
    walk.push(head.id()).ok()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)
        .ok()?;

    let path = Path::new(rel_in_tree);
    for oid in walk {
        let oid = oid.ok()?;
        let commit = repo.find_commit(oid).ok()?;
        // Skip merge commits: a merge that pulls in someone else's edit is not
        // the author of the content, so attributing provenance to the merger
        // would misclassify (e.g. my merge of a teammate's edit → wrongly
        // Local). Authorship belongs to a single-parent edit commit.
        if commit.parent_count() > 1 {
            continue;
        }
        let here = blob_oid_at(&commit, path);
        if here.is_none() {
            continue;
        }
        let changed = match commit.parent(0) {
            Ok(parent) => blob_oid_at(&parent, path) != here,
            // Root commit (no parent) that has the path → it introduced it.
            Err(_) => true,
        };
        if changed {
            return Result::ok(commit.author().email()).map(|s| s.to_string());
        }
    }
    None
}

/// Drop CR bytes so a CRLF working-tree checkout (Windows `core.autocrlf`)
/// compares equal to an LF blob in `origin/HEAD` — line-ending conversion is
/// not authorship and must not flip provenance.
fn normalize_eol(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().copied().filter(|&b| b != b'\r').collect()
}

/// Blob OID of `path` in a commit's tree, or `None` if the path is absent or
/// not a blob.
fn blob_oid_at(commit: &git2::Commit<'_>, path: &Path) -> Option<git2::Oid> {
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(path).ok()?;
    Some(entry.id())
}

/// The current user's configured git identity (`user.email`), resolved
/// through the standard git config cascade (repo → global → system).
fn current_user_email(org_root: &Path) -> Option<String> {
    let repo = git2::Repository::open(org_root).ok()?;
    repo.config().ok()?.get_string("user.email").ok()
}

/// Execution-boundary guard (AC-5, AC-6). The single shared check for any phase
/// that executes config-authored commands. Currently wired into
/// `run_worktree_setup` — the only phase with a production execution path.
/// `validate`/`cleanup` reuse this same guard when their execution surfaces
/// exist (the SW-WT-8 "Test my setup" UI); they are not gated speculatively
/// today. Re-derives provenance from the current on-disk config and, when it is
/// `TeamSynced`,
/// requires `approved_config_hash` to match the live config hash — re-reading
/// from disk closes the TOCTOU window between the frontend prompt and the
/// actual run. `has_commands == false` short-circuits to `Ok` (nothing
/// executes, nothing to consent to).
///
/// On rejection returns `Err("NEEDS_APPROVAL:<live-hash>")` — a sentinel the
/// frontend detects to (re-)prompt rather than surface as a setup failure.
/// `Local` provenance always passes.
pub fn require_provenance_approval(
    source_repo_path: &Path,
    org_root: &Path,
    approved_config_hash: Option<&str>,
    has_commands: bool,
) -> Result<(), String> {
    let report = setup_provenance_report(source_repo_path, org_root);
    approval_decision(&report, approved_config_hash, has_commands)
}

/// Pure decision used by [`require_provenance_approval`], split out so the
/// gate logic is testable without a git fixture. `Local` and "no commands"
/// pass; a `TeamSynced` config requires `approved_config_hash` to equal the
/// live hash.
fn approval_decision(
    report: &SetupProvenanceReport,
    approved_config_hash: Option<&str>,
    has_commands: bool,
) -> Result<(), String> {
    if !has_commands || report.provenance != SetupProvenance::TeamSynced {
        return Ok(());
    }
    let live = report.config_hash.clone().unwrap_or_default();
    // Defense in depth: a TeamSynced report always carries a hash, but never
    // let an empty live hash be satisfiable — an empty approved hash must not
    // match "nothing" and bypass the gate.
    if live.is_empty() {
        return Err("NEEDS_APPROVAL:".to_string());
    }
    match approved_config_hash {
        Some(approved) if approved == live => Ok(()),
        _ => Err(format!("NEEDS_APPROVAL:{}", live)),
    }
}

/// Pure read command backing the frontend gate. Returns the current
/// provenance + content hash so the frontend can decide whether to prompt
/// and, if approved, remember the hash. Never fetches, never mutates.
#[tauri::command]
pub async fn evaluate_setup_provenance(
    repo_path: String,
    org_root: String,
) -> Result<SetupProvenanceReport, String> {
    crate::commands::await_blocking("evaluate_setup_provenance", move || {
        Ok(setup_provenance_report(
            Path::new(&repo_path),
            Path::new(&org_root),
        ))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const REL: &str = "_codesense/worktree/acme/widget.yaml";

    fn write_tree_file(root: &Path, rel: &str, content: &[u8]) {
        let abs = root.join(rel);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, content).unwrap();
    }

    /// Init a git repo at `root`, set the local `user.email` to `me_email`,
    /// commit `rel`=`content` authored by `author_email`, and wire
    /// `refs/remotes/origin/HEAD → origin/main → commit` so
    /// `read_from_origin_head` resolves. Returns nothing; the working tree
    /// holds `content` at `rel`.
    fn init_repo_with_origin(
        root: &Path,
        rel: &str,
        content: &[u8],
        me_email: &str,
        author_email: &str,
    ) {
        let repo = git2::Repository::init(root).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Current User").unwrap();
            cfg.set_str("user.email", me_email).unwrap();
        }
        write_tree_file(root, rel, content);
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(rel)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Author", author_email).unwrap();
        let commit = repo
            .commit(Some("HEAD"), &sig, &sig, "add config", &tree, &[])
            .unwrap();
        repo.reference("refs/remotes/origin/main", commit, true, "seed origin")
            .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            true,
            "seed origin head",
        )
        .unwrap();
    }

    fn tmp() -> PathBuf {
        let mut p = std::env::temp_dir();
        // Unique-ish without Date/rand: use a static counter.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        p.push(format!(
            "swwt9-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn hash_is_deterministic_and_content_sensitive() {
        assert_eq!(config_content_hash(b"abc"), config_content_hash(b"abc"));
        assert_ne!(config_content_hash(b"abc"), config_content_hash(b"abd"));
        // Known SHA-256 of "abc".
        assert_eq!(
            config_content_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn no_remote_is_local() {
        let root = tmp();
        let repo = git2::Repository::init(&root).unwrap();
        write_tree_file(&root, REL, b"setup:\n  - npm ci\n");
        // Commit to HEAD but never wire origin refs.
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(REL)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("X", "x@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "c", &tree, &[])
            .unwrap();
        assert_eq!(
            classify_provenance(&root, REL, b"setup:\n  - npm ci\n"),
            SetupProvenance::Local
        );
    }

    #[test]
    fn absent_on_origin_is_local() {
        let root = tmp();
        // Origin has a DIFFERENT file; the worktree.yaml path isn't on origin.
        init_repo_with_origin(
            &root,
            "_codesense/other.yaml",
            b"x",
            "me@x.com",
            "mate@x.com",
        );
        assert_eq!(
            classify_provenance(&root, REL, b"anything"),
            SetupProvenance::Local
        );
    }

    #[test]
    fn locally_edited_is_local() {
        let root = tmp();
        init_repo_with_origin(
            &root,
            REL,
            b"setup:\n  - npm ci\n",
            "me@x.com",
            "mate@x.com",
        );
        // Working-tree bytes differ from what was pushed → user edited it.
        assert_eq!(
            classify_provenance(&root, REL, b"setup:\n  - npm ci\n  - extra\n"),
            SetupProvenance::Local
        );
    }

    #[test]
    fn self_authored_is_local() {
        let root = tmp();
        let content = b"setup:\n  - npm ci\n";
        init_repo_with_origin(&root, REL, content, "me@x.com", "me@x.com");
        assert_eq!(
            classify_provenance(&root, REL, content),
            SetupProvenance::Local
        );
    }

    #[test]
    fn teammate_authored_matching_content_is_team_synced() {
        let root = tmp();
        let content = b"setup:\n  - npm ci\n";
        init_repo_with_origin(&root, REL, content, "me@x.com", "mate@x.com");
        assert_eq!(
            classify_provenance(&root, REL, content),
            SetupProvenance::TeamSynced
        );
    }

    #[test]
    fn crlf_checkout_of_lf_blob_is_not_misread_as_local() {
        let root = tmp();
        let lf = b"setup:\n  - npm ci\n";
        init_repo_with_origin(&root, REL, lf, "me@x.com", "mate@x.com");
        // Windows checkout converts the LF blob to CRLF on disk; still the
        // teammate's content → TeamSynced, not Local.
        let crlf = b"setup:\r\n  - npm ci\r\n";
        assert_eq!(
            classify_provenance(&root, REL, crlf),
            SetupProvenance::TeamSynced
        );
    }

    #[test]
    fn author_match_is_case_insensitive() {
        let root = tmp();
        let content = b"setup:\n  - npm ci\n";
        init_repo_with_origin(&root, REL, content, "Me@X.com", "me@x.COM");
        assert_eq!(
            classify_provenance(&root, REL, content),
            SetupProvenance::Local
        );
    }

    #[test]
    fn tree_relative_posix_strips_org_root() {
        let org = Path::new("/home/u/org");
        assert_eq!(
            tree_relative_posix(&org.join("_codesense/worktree/acme/widget.yaml"), org).as_deref(),
            Some("_codesense/worktree/acme/widget.yaml")
        );
        // Not under org_root.
        assert_eq!(
            tree_relative_posix(Path::new("/elsewhere/x.yaml"), org),
            None
        );
    }

    fn report(provenance: SetupProvenance, hash: Option<&str>) -> SetupProvenanceReport {
        SetupProvenanceReport {
            provenance,
            config_hash: hash.map(|s| s.to_string()),
        }
    }

    #[test]
    fn guard_passes_when_no_commands() {
        let r = report(SetupProvenance::TeamSynced, Some("h"));
        assert!(approval_decision(&r, None, false).is_ok());
    }

    #[test]
    fn guard_passes_local_provenance() {
        let r = report(SetupProvenance::Local, None);
        assert!(approval_decision(&r, None, true).is_ok());
    }

    #[test]
    fn guard_rejects_unapproved_team_synced() {
        let r = report(SetupProvenance::TeamSynced, Some("h1"));
        let err = approval_decision(&r, None, true).unwrap_err();
        assert_eq!(err, "NEEDS_APPROVAL:h1");
    }

    #[test]
    fn guard_rejects_empty_live_hash_even_if_approved_empty() {
        // An empty approved hash must never satisfy an empty live hash.
        let r = report(SetupProvenance::TeamSynced, Some(""));
        assert_eq!(
            approval_decision(&r, Some(""), true).unwrap_err(),
            "NEEDS_APPROVAL:"
        );
    }

    #[test]
    fn guard_rejects_stale_hash_then_passes_matching() {
        let r = report(SetupProvenance::TeamSynced, Some("h2"));
        // Approved an older hash → re-prompt (TOCTOU, AC-6).
        assert_eq!(
            approval_decision(&r, Some("h1"), true).unwrap_err(),
            "NEEDS_APPROVAL:h2"
        );
        // Approved the live hash → pass.
        assert!(approval_decision(&r, Some("h2"), true).is_ok());
    }
}
