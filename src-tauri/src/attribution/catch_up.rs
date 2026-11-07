// SPDX-License-Identifier: MIT
//! Checkpoint capture passes (startup sweep and watcher clone passes) and their status.
//!
//! Status rule, retained per home for this app process and changed only through `update`:
//! counters, `lastSweepAt` and `trigger` describe the last completed import pass (startup
//! sweep or one watcher clone pass); `watchErrors` is set in place by each watch
//! reconciliation and never touches them; `lastError` is the latest still-unresolved error,
//! where each source (watch registration, the clone catalog, the usage store, one clone's
//! import) clears only its own error when it next succeeds. Clone errors belong to eligible
//! clones: one for a clone that is no longer eligible (capture off, detached, identity
//! changed, git-ai absent) is dropped when the catalog is next read.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// `describe_setup.attribution.capture`; see the module rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub last_sweep_at: Option<u64>,
    pub trigger: Option<&'static str>,
    pub clones_swept: usize,
    pub logs_seen: usize,
    pub logs_new: usize,
    pub trace_rows_stored: usize,
    pub session_mapping_rows_stored: usize,
    pub last_error: Option<String>,
    pub watch_errors: usize,
}

#[derive(Clone, PartialEq, Eq)]
enum Source {
    Watch,
    Catalog,
    Store,
    Clone(PathBuf),
}
#[derive(Default)]
struct Status {
    capture: Capture,
    /// Unresolved errors, oldest first.
    errors: Vec<(Source, String)>,
}
impl Status {
    fn set_error(&mut self, source: Source, error: Option<String>) {
        std::vec::Vec::retain(&mut self.errors, |(s, _)| *s != source);
        if let Some(error) = error {
            self.errors.push((source, error));
        }
    }
}

static STATUS: LazyLock<Mutex<BTreeMap<PathBuf, Status>>> = LazyLock::new(Default::default);
fn home_key(home: &Path) -> PathBuf {
    home.canonicalize().unwrap_or_else(|_| home.to_owned())
}
pub(crate) fn capture(home: &Path) -> Capture {
    STATUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&home_key(home))
        .map(|status| status.capture.clone())
        .unwrap_or_default()
}
/// The one writer of capture status; `last_error` is derived after every change.
fn update(home: &Path, change: impl FnOnce(&mut Status)) {
    let mut all = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    let status = all.entry(home_key(home)).or_default();
    change(status);
    status.capture.last_error = <[_]>::last(&status.errors).map(|(_, error)| error.clone());
}
/// A catalog read without an import pass: records the catalog error and drops the errors of
/// clones outside `eligible`; never touches the last pass's numbers.
pub(crate) fn retain_clone_errors(
    home: &Path,
    eligible: &[(PathBuf, String)],
    catalog_error: Option<String>,
) {
    update(home, |status| {
        std::vec::Vec::retain(&mut status.errors, |(source, _)| match source {
            Source::Clone(repo) => eligible.iter().any(|(clone, _)| clone == repo),
            _ => true,
        });
        status.set_error(Source::Catalog, catalog_error);
    });
}
/// Watch reconciliation outcome: sets `watch_errors` in place, keeps the last pass's numbers.
pub(crate) fn record_watch_errors(home: &Path, count: usize) {
    update(home, |status| {
        status.capture.watch_errors = count;
        status.set_error(
            Source::Watch,
            (count > 0).then(|| "checkpoint watch registration failed".to_owned()),
        );
    });
}

pub(crate) fn run_at(home: &Path, path: &Path) {
    let (clones, error) = eligible_clones(home);
    run_clones(home, path, &clones, "startup", error);
}

/// Shared startup/watch catalog: one attachment, identity and capture-choice rule.
pub(crate) fn eligible_clones(home: &Path) -> (Vec<(PathBuf, String)>, Option<String>) {
    let mut last_error: Option<String> = None;
    let mut clones = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for principal in crate::context_repo::org_enumeration::principals(home) {
        let result = (|| {
            let configured = crate::mcp::setup_tools::describe_orgs(home, &principal)?
                .into_iter()
                .flat_map(|org| org.teams)
                .flat_map(|team| team.source_repos)
                .map(|repo| repo.remote_key)
                .collect::<std::collections::BTreeSet<_>>();
            let attachments = crate::context_repo::attachments_io::read_attachments(
                &crate::context_repo::path_to_user_attachments_file(home, &principal),
            )
            .map_err(|error| error.to_string())?;
            for attachment in attachments.attachments {
                let Some(key) = crate::commands::attachments::attachment_key(&attachment)
                    .filter(|key| configured.contains(key))
                else {
                    continue;
                };
                let repo = Path::new(&attachment.repo_path);
                let attempt = (|| {
                    let Ok(repo) = repo.canonicalize() else {
                        return Ok::<_, String>(());
                    };
                    if !repo.is_dir() {
                        return Ok(());
                    }
                    let current = crate::commands::attachments::existing_checkout_attachment(&repo)
                        .map_err(|error| {
                            last_error = Some("attached repository identity unavailable".into());
                            error
                        })?;
                    let matches = crate::commands::attachments::attachment_key(&current).as_deref()
                        == Some(std::string::String::as_str(&key))
                        || crate::commands::git::remotes_at(&repo)
                            .map_err(|error| {
                                last_error =
                                    Some("attached repository identity unavailable".into());
                                error
                            })?
                            .iter()
                            .any(|(_, url)| {
                                crate::context_repo::canonical_remote_key(url).as_deref()
                                    == Some(std::string::String::as_str(&key))
                            });
                    if !matches {
                        last_error = Some("attached repository identity changed".into());
                        return Err("attached repository identity changed".to_owned());
                    }
                    let (preference_key, _) =
                        super::prefs::repo_key_for_checkout(&repo).map_err(|error| {
                            last_error = Some("capture preference identity unavailable".into());
                            error
                        })?;
                    let prefs =
                        super::prefs::read(home, &principal, &preference_key).map_err(|error| {
                            last_error = Some("capture preferences unavailable".into());
                            error
                        })?;
                    if !super::launch::captures(&prefs) {
                        return Ok::<_, String>(());
                    }
                    if !seen.insert(repo.clone()) {
                        return Ok(());
                    }
                    clones.push((repo, key.clone()));
                    Ok(())
                })();
                if let Err(error) = attempt {
                    log::warn!(target: "attribution", "checkpoint catch-up {}: {error}", repo.display());
                }
            }
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            log::warn!(target: "attribution", "checkpoint catch-up principal {principal}: {error}");
            last_error = Some("attached clone enumeration unavailable".into());
        }
    }
    (clones, last_error)
}

pub(crate) fn run_clones(
    home: &Path,
    path: &Path,
    clones: &[(PathBuf, String)],
    trigger: &'static str,
    catalog_error: Option<String>,
) {
    let mut pass = Capture {
        trigger: Some(trigger),
        ..std::default::Default::default()
    };
    let mut results = Vec::new();
    let store_error = match crate::session_usage::UsageStore::open(path) {
        Ok(mut store) => {
            for (repo, key) in clones {
                pass.clones_swept += 1;
                let (stats, error) = match super::checkpoints::harvest_all(repo, key, &mut store) {
                    Ok(stats) => (stats, None),
                    Err(failure) => (failure.stats, Some(failure.error)),
                };
                pass.logs_seen += stats.logs_seen;
                pass.logs_new += stats.logs_new;
                pass.trace_rows_stored += stats.rows_stored;
                pass.session_mapping_rows_stored += stats.session_mapping_rows_stored;
                if let Some(error) = &error {
                    log::warn!(target: "attribution", "checkpoint catch-up {}: {error}", repo.display());
                }
                results.push((
                    repo.clone(),
                    error.map(|_| "checkpoint import failed".to_owned()),
                ));
            }
            None
        }
        Err(error) => {
            log::warn!(target: "attribution", "checkpoint catch-up store: {error}");
            Some("checkpoint store unavailable".to_owned())
        }
    };
    pass.last_sweep_at = Some(super::launch::now_ms());
    update(home, |status| {
        pass.watch_errors = status.capture.watch_errors;
        status.capture = pass;
        status.set_error(Source::Catalog, catalog_error);
        status.set_error(Source::Store, store_error);
        for (repo, error) in results {
            status.set_error(Source::Clone(repo), error);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::git_ai::tests::{git, rebuild};
    #[test]
    fn sweep_startup_catches_all_teams_clones_and_principals_excludes_off_unattached() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("usage.sqlite");
        let mut clones = Vec::new();
        for (i, enabled) in [(1, true), (2, true), (3, false), (4, true)] {
            let repo = rebuild();
            let profile = home.path().join(format!("profile-{i}"));
            crate::attribution::why_line::tests::attach(&profile, repo.path());
            let src = profile.join("users/1");
            let dst = home.path().join(format!("users/{i}"));
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::rename(&src, &dst).unwrap();
            if i == 2 {
                std::fs::write(dst.join("orgs/acme/teams.yml"), "teams:\n  - name: dev\n    source_repos:\n      - owner: acme\n        repo: other\n  - name: backend\n    source_repos:\n      - owner: acme\n        repo: api\n").unwrap();
            }
            let principal = crate::domain::Principal::from(i as u64);
            crate::attribution::prefs::update(
                home.path(),
                &principal,
                "github.com/acme/api",
                |p| {
                    p.mode = Some(if enabled {
                        crate::attribution::prefs::AttributionMode::On
                    } else {
                        crate::attribution::prefs::AttributionMode::Off
                    });
                },
            )
            .unwrap();
            if i == 4 {
                std::fs::remove_file(crate::context_repo::path_to_user_attachments_file(
                    home.path(),
                    &principal,
                ))
                .unwrap();
            }
            let head = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_owned();
            let dir = repo
                .path()
                .join(".git/ai/working_logs")
                .join(format!("old-{head}"));
            std::fs::create_dir_all(&dir).unwrap();
            let text = include_str!("testdata/git_ai_1_7_5_checkpoints/live-claude.jsonl")
                .replace("toolu_", &format!("toolu_{i}"));
            let mut json: serde_json::Value =
                serde_json::from_str(text.lines().next().unwrap()).unwrap();
            json["agent_id"]["id"] = serde_json::json!(format!("native-{i}"));
            std::fs::write(dir.join("checkpoints.jsonl"), format!("{json}\n")).unwrap();
            std::fs::write(dir.join(".archived_at"), "1").unwrap();
            clones.push(repo);
        }
        run_at(home.path(), &db);
        let store = crate::session_usage::UsageStore::open(&db).unwrap();
        let count: i64 = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM checkpoint_trace", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        drop(store);
        run_at(home.path(), &db);
    }
    #[test]
    fn sweep_startup_preserves_multiple_clones_of_one_remote() {
        let home = tempfile::tempdir().unwrap();
        let first = rebuild();
        let second = rebuild();
        crate::attribution::why_line::tests::attach(home.path(), first.path());
        git(
            second.path(),
            &["remote", "add", "origin", "https://github.com/acme/api.git"],
        );
        let principal = crate::domain::Principal::from(1);
        let path = crate::context_repo::path_to_user_attachments_file(home.path(), &principal);
        let mut attachments = crate::context_repo::attachments_io::read_attachments(&path).unwrap();
        attachments.attachments.push(
            crate::commands::attachments::existing_checkout_attachment(second.path()).unwrap(),
        );
        crate::context_repo::attachments_io::write_attachments_atomic(&path, &attachments).unwrap();
        for (repo, tool) in [(first.path(), "tool-first"), (second.path(), "tool-second")] {
            let head = git(repo, &["rev-parse", "HEAD"]).trim().to_owned();
            let dir = repo
                .join(".git/ai/working_logs")
                .join(format!("old-{head}"));
            std::fs::create_dir_all(&dir).unwrap();
            let mut record: serde_json::Value = serde_json::from_str(
                include_str!("testdata/git_ai_1_7_5_checkpoints/live-claude.jsonl")
                    .lines()
                    .next()
                    .unwrap(),
            )
            .unwrap();
            record["agent_metadata"]["tool_use_id"] = serde_json::json!(tool);
            std::fs::write(dir.join("checkpoints.jsonl"), format!("{record}\n")).unwrap();
            std::fs::write(dir.join(".archived_at"), "1").unwrap();
        }
        let db = home.path().join("usage.sqlite");
        run_at(home.path(), &db);
        let _store = crate::session_usage::UsageStore::open(&db).unwrap();
        let ambiguous: bool = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("SELECT ambiguous FROM checkpoint_trace", [], |r| r.get(0))
            .unwrap();
        assert!(ambiguous);
        let cursors: i64 = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM checkpoint_log_cursor", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cursors, 2);
    }
    #[test]
    fn review_startup_uses_current_preferences_and_requires_a_current_source_binding() {
        use crate::attribution::turns::tests::{replay_log, replay_repo};
        for retain_upstream in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let repo = replay_repo(true);
            crate::attribution::why_line::tests::attach(home.path(), repo.path());
            let principal = crate::domain::Principal::from(1);
            super::super::prefs::set_for_checkout(
                home.path(),
                &principal,
                repo.path().to_str().unwrap(),
                Some(super::super::prefs::AttributionChoice::On),
                None,
            )
            .unwrap();
            if retain_upstream {
                git(
                    repo.path(),
                    &[
                        "remote",
                        "add",
                        "upstream",
                        "https://github.com/acme/api.git",
                    ],
                );
            }
            git(
                repo.path(),
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "https://github.com/fork-owner/api.git",
                ],
            );
            let log = replay_log(repo.path());
            std::fs::write(log.parent().unwrap().join(".archived_at"), "1").unwrap();
            let db = home.path().join("usage.sqlite");
            for on in [false, true] {
                super::super::prefs::set_for_checkout(
                    home.path(),
                    &principal,
                    repo.path().to_str().unwrap(),
                    Some(if on {
                        super::super::prefs::AttributionChoice::On
                    } else {
                        super::super::prefs::AttributionChoice::Off
                    }),
                    None,
                )
                .unwrap();
                run_at(home.path(), &db);
                let count: i64 = rusqlite::Connection::open(&db)
                    .unwrap()
                    .query_row("SELECT COUNT(*) FROM checkpoint_trace", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(
                    count,
                    if retain_upstream && on { 2 } else { 0 },
                    "upstream={retain_upstream}, current on={on}"
                );
            }
        }
    }
}
