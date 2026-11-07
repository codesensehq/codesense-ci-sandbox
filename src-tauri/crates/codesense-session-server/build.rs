// SPDX-License-Identifier: MIT
//! Stamps the build with the CodeSense app release it ships in
//! (`src-tauri/Cargo.toml`'s version — this crate's own version never moves),
//! the git commit and the build time, so a desktop can tell its own terminal
//! server from an older or newer one. Each part is optional: a build outside a
//! checkout (or without git) simply stamps less.
//!
//! Overrides: `CODESENSE_BUILD_COMMIT` (a release pipeline that knows its
//! commit) and `SOURCE_DATE_EPOCH` (seconds; reproducible builds).

use std::path::Path;
use std::process::Command;

fn main() {
    let app_manifest = Path::new("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", app_manifest.display());
    println!("cargo:rerun-if-env-changed=CODESENSE_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    // The app's release is the stamp's whole point: this crate's own version
    // never moves, so never fall back to it.
    let version = std::fs::read_to_string(app_manifest)
        .ok()
        .and_then(|manifest| package_version(&manifest))
        .unwrap_or_else(|| panic!("read the app release from {}", app_manifest.display()));
    println!("cargo:rustc-env=CODESENSE_APP_VERSION={version}");

    let commit = std::env::var("CODESENSE_BUILD_COMMIT")
        .ok()
        .filter(|commit| !commit.trim().is_empty())
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .map(|commit| short_commit(&commit))
        .unwrap_or_default();
    println!("cargo:rustc-env=CODESENSE_BUILD_COMMIT={commit}");
    // Rerun when HEAD moves (a commit, checkout or reset in this worktree), not
    // on every build: `logs/HEAD` is per worktree and appended on each move.
    // A path that does not exist would make Cargo rerun this on every build.
    // (`--git-path` answers relative to this directory, where Cargo runs us.)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    if let Some(log) = git(&["rev-parse", "--git-path", "logs/HEAD"])
        .map(|log| Path::new(&manifest_dir).join(log))
        .filter(|log| log.exists())
    {
        println!("cargo:rerun-if-changed={}", log.display());
    }

    // The build time is when this script last ran: it reruns when HEAD moves
    // (or the app version / overrides change), so it reads as "the time of
    // this commit's first build here", stable across rebuilds of one commit.
    let built_at_ms = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|secs| secs.trim().parse::<u64>().ok())
        .and_then(|secs| secs.checked_mul(1000))
        .or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|elapsed| elapsed.as_millis() as u64)
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=CODESENSE_BUILD_TIME_MS={built_at_ms}");
}

/// The 12-hex form of a commit, whether given short or full.
fn short_commit(commit: &str) -> String {
    commit
        .trim()
        .to_ascii_lowercase()
        .chars()
        .take(12)
        .collect()
}

/// `version = "…"` from a manifest's `[package]` table (a literal string
/// only; anything else reads as absent).
fn package_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.split('\n') {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "version" {
            continue;
        }
        let value = value.trim().strip_prefix('"')?;
        return value
            .split_once('"')
            .map(|(version, _)| version.to_string());
    }
    None
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|out| !out.is_empty())
}
