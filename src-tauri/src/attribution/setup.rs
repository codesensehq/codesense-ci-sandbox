// SPDX-License-Identifier: MIT
//! The `setup` live query: what a dashboard reads of this machine's attribution [`Doctor`].
//!
//! The in-app page gets the same JSON `describe_setup` serves under `attribution`. A published
//! (shared) dashboard gets [`shared`], an explicit allow-list: nothing is passed through by
//! default, so a field added to [`Doctor`] or [`Capture`] stays out until it is classified here.

use super::catch_up::Capture;
use super::launch::Doctor;
use super::AttributionStatus;
use serde_json::{json, Value};

/// `Doctor` fields a shared dashboard reads.
pub(crate) const SHARED_DOCTOR_FIELDS: &[&str] = &[
    "status",
    "gitAiVersion",
    "daemonReachable",
    "supportedVersions",
    "capture",
    "notesRef",
    "daemonRepoRoots",
];
/// `Doctor` fields only the in-app page reads: host paths, free-text warnings and the
/// identities of repos set to push notes.
pub(crate) const EXCLUDED_DOCTOR_FIELDS: &[&str] =
    &["binary", "warnings", "previousBinary", "notesPushRepos"];
/// `Capture` fields a shared dashboard reads: counters, a time, a fixed trigger
/// (`startup` | `watcher`) and fixed phase messages.
pub(crate) const SHARED_CAPTURE_FIELDS: &[&str] = &[
    "lastSweepAt",
    "trigger",
    "clonesSwept",
    "logsSeen",
    "logsNew",
    "traceRowsStored",
    "sessionMappingRowsStored",
    "watchErrors",
    "lastError",
];
pub(crate) const EXCLUDED_CAPTURE_FIELDS: &[&str] = &[];

/// The in-app answer: the projection `describe_setup` serves.
pub(crate) fn local(doctor: &Doctor) -> Value {
    json!({ "attribution": doctor })
}

/// The shared-dashboard answer: only the allow-listed fields. Naming every field in the
/// destructuring means a new `Doctor` or `Capture` field does not compile until classified here.
pub(crate) fn shared(doctor: &Doctor) -> Value {
    let Doctor {
        status,
        binary: _,
        git_ai_version,
        daemon_reachable,
        warnings: _,
        previous_binary: _,
        supported_versions,
        capture,
        notes_ref,
        // Repo identities stay out of published reads; in-app JSON and MCP carry them.
        notes_push_repos: _,
        // A count, never the paths.
        daemon_repo_roots,
    } = doctor;
    let Capture {
        last_sweep_at,
        trigger,
        clones_swept,
        logs_seen,
        logs_new,
        trace_rows_stored,
        session_mapping_rows_stored,
        watch_errors,
        last_error,
    } = capture;
    // Only the kind, plus the reason when off (a closed enum; `doctor()` itself never reports off); message and version text from
    // git-ai output can carry host paths.
    let mut status_kind = json!({ "kind": json!(status)["kind"] });
    if let AttributionStatus::Off { reason } = status {
        status_kind["reason"] = json!(reason);
    }
    json!({ "attribution": {
        "status": status_kind,
        "gitAiVersion": git_ai_version,
        "daemonReachable": daemon_reachable,
        "supportedVersions": supported_versions,
        "notesRef": notes_ref,
        "daemonRepoRoots": daemon_repo_roots,
        "capture": {
            "lastSweepAt": last_sweep_at,
            "trigger": trigger,
            "clonesSwept": clones_swept,
            "logsSeen": logs_seen,
            "logsNew": logs_new,
            "traceRowsStored": trace_rows_stored,
            "sessionMappingRowsStored": session_mapping_rows_stored,
            "watchErrors": watch_errors,
            "lastError": last_error,
        },
    }})
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::attribution::catch_up::{self, Capture};
    use crate::attribution::{AttributionStatus, OffReason};
    use std::collections::BTreeSet;

    const SECRET: &str = "/Users/someone/.codesense/bin/git-ai";

    fn capture_full() -> Capture {
        Capture {
            last_sweep_at: Some(1_700_000_000_000),
            trigger: Some("startup"),
            clones_swept: 2,
            logs_seen: 5,
            logs_new: 3,
            trace_rows_stored: 7,
            session_mapping_rows_stored: 4,
            watch_errors: 0,
            last_error: Some("checkpoint import failed".into()),
        }
    }

    fn doctor(status: AttributionStatus, capture: Capture) -> Doctor {
        Doctor {
            status,
            binary: Some(SECRET.into()),
            git_ai_version: Some("1.7.5".into()),
            daemon_reachable: true,
            warnings: vec![format!("could not write {SECRET}")],
            previous_binary: Some(crate::attribution::install::PreviousBinary {
                version: "1.7.4".into(),
                replacement: "bundled".into(),
                path: SECRET.into(),
            }),
            supported_versions: ">=1.7.5 <1.8.0",
            capture,
            notes_ref: "sense",
            notes_push_repos: vec!["github.com/acme/private".into()],
            daemon_repo_roots: Some(2),
        }
    }

    fn keys(value: &Value) -> BTreeSet<String> {
        value.as_object().unwrap().keys().cloned().collect()
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// Adding a field to `Doctor` or `Capture` fails here until it is allow-listed or excluded.
    #[test]
    fn every_doctor_and_capture_field_is_allowed_or_excluded() {
        let d = doctor(AttributionStatus::Active, capture_full());
        let full = serde_json::to_value(&d).unwrap();
        let classified: BTreeSet<_> = SHARED_DOCTOR_FIELDS
            .iter()
            .chain(EXCLUDED_DOCTOR_FIELDS)
            .map(|n| n.to_string())
            .collect();
        assert_eq!(keys(&full), classified, "Doctor field not classified");
        assert!(SHARED_DOCTOR_FIELDS
            .iter()
            .all(|n| !EXCLUDED_DOCTOR_FIELDS.contains(n)));
        let capture_classified: BTreeSet<_> = SHARED_CAPTURE_FIELDS
            .iter()
            .chain(EXCLUDED_CAPTURE_FIELDS)
            .map(|n| n.to_string())
            .collect();
        assert_eq!(
            keys(&full["capture"]),
            capture_classified,
            "Capture field not classified"
        );
        let shared = shared(&d);
        assert_eq!(keys(&shared["attribution"]), set(SHARED_DOCTOR_FIELDS));
        assert_eq!(
            keys(&shared["attribution"]["capture"]),
            set(SHARED_CAPTURE_FIELDS)
        );
    }

    /// The story's case table, one row per entry.
    #[test]
    fn setup_case_table() {
        let swept = capture_full();
        let never = Capture::default();
        let no_error = Capture {
            last_error: None,
            ..capture_full()
        };
        let watcher_pass = Capture {
            trigger: Some("watcher"),
            last_error: None,
            ..capture_full()
        };
        let watch_errors = Capture {
            watch_errors: 3,
            last_error: Some("checkpoint watch registration failed".into()),
            ..capture_full()
        };
        let rows: Vec<(&str, Doctor)> = vec![
            ("on_swept", doctor(AttributionStatus::Active, swept.clone())),
            // `doctor()` never returns Off (per-launch states live on worktrees rows), so this row is a
            // constructed Doctor that pins the projection should a machine-level Off ever appear.
            (
                "off_constructed_doctor",
                doctor(
                    AttributionStatus::Off {
                        reason: OffReason::DisabledForRepo,
                    },
                    never.clone(),
                ),
            ),
            (
                "not_installed",
                doctor(AttributionStatus::NotInstalled, never.clone()),
            ),
            (
                "installing",
                doctor(AttributionStatus::Installing, never.clone()),
            ),
            (
                "install_failed",
                doctor(
                    AttributionStatus::InstallFailed {
                        message: SECRET.into(),
                    },
                    never.clone(),
                ),
            ),
            (
                "unsupported_version",
                doctor(
                    AttributionStatus::UnsupportedVersion {
                        found: SECRET.into(),
                    },
                    never.clone(),
                ),
            ),
            (
                "unsupported",
                doctor(
                    AttributionStatus::Unsupported {
                        reason: SECRET.into(),
                    },
                    never.clone(),
                ),
            ),
            (
                "never_ran",
                doctor(AttributionStatus::Active, never.clone()),
            ),
            (
                "last_error",
                doctor(AttributionStatus::Active, swept.clone()),
            ),
            ("no_error", doctor(AttributionStatus::Active, no_error)),
            (
                "watcher_pass",
                doctor(AttributionStatus::Active, watcher_pass),
            ),
            (
                "watch_errors",
                doctor(AttributionStatus::Active, watch_errors),
            ),
        ];
        for (name, d) in rows {
            let local = local(&d);
            let shared = shared(&d);
            // In-app JSON keeps the host fields the shared projection drops.
            assert_eq!(
                local["attribution"]["binary"], SECRET,
                "{name}: local keeps binary"
            );
            assert_eq!(
                keys(&local["attribution"]),
                set(&[
                    "status",
                    "binary",
                    "gitAiVersion",
                    "daemonReachable",
                    "warnings",
                    "previousBinary",
                    "supportedVersions",
                    "capture",
                    "notesRef",
                    "notesPushRepos",
                    "daemonRepoRoots"
                ]),
                "{name}: local keys"
            );
            // Shared: only allow-listed keys, same kind as the Doctor, no host text anywhere.
            let kind = serde_json::to_value(&d.status).unwrap()["kind"].clone();
            let a = &shared["attribution"];
            assert_eq!(a["status"]["kind"], kind, "{name}: kind");
            assert_eq!(a["gitAiVersion"], "1.7.5", "{name}");
            assert_eq!(a["daemonReachable"], true, "{name}");
            assert_eq!(a["supportedVersions"], ">=1.7.5 <1.8.0", "{name}");
            assert_eq!(
                a["capture"],
                serde_json::to_value(&d.capture).unwrap(),
                "{name}: capture"
            );
            match name {
                "watcher_pass" => assert_eq!(a["capture"]["trigger"], "watcher"),
                "watch_errors" => {
                    assert_eq!(a["capture"]["watchErrors"], 3);
                    assert_eq!(
                        a["capture"]["lastError"],
                        "checkpoint watch registration failed"
                    );
                }
                _ => {}
            }
            assert!(
                !shared.to_string().contains("/Users"),
                "{name}: host text leaked"
            );
            assert_eq!(a["notesRef"], "sense", "{name}");
            assert_eq!(a["daemonRepoRoots"], 2, "{name}: a count, never the paths");
            assert!(
                !shared.to_string().contains("acme/private"),
                "{name}: push repos leaked"
            );
            assert!(
                a.get("binary").is_none() && a.get("warnings").is_none(),
                "{name}"
            );
            match &d.status {
                AttributionStatus::Off { .. } => {
                    assert_eq!(
                        a["status"],
                        json!({"kind": "off", "reason": "disabled_for_repo"})
                    );
                    assert!(a["capture"].is_object(), "off still carries capture");
                }
                _ => assert_eq!(keys(&a["status"]), set(&["kind"]), "{name}: status keys"),
            }
        }
        // Capture never ran since start: present, empty counters, null sweep time.
        let never = shared(&doctor(AttributionStatus::Active, Capture::default()));
        let c = &never["attribution"]["capture"];
        assert_eq!(c["lastSweepAt"], Value::Null);
        assert_eq!(
            (
                c["clonesSwept"].clone(),
                c["logsSeen"].clone(),
                c["logsNew"].clone()
            ),
            (json!(0), json!(0), json!(0))
        );
        assert_eq!(c["watchErrors"], 0);
        assert_eq!(c["lastError"], Value::Null);
    }

    /// A real sweep and a failed one reach the projection through the same `capture()` read
    /// `describe_setup` uses, per home (this Mac's values only, never another's).
    #[test]
    fn capture_comes_from_this_homes_own_sweep() {
        let swept = tempfile::tempdir().unwrap();
        let failed = tempfile::tempdir().unwrap();
        let idle = tempfile::tempdir().unwrap();
        catch_up::run_at(swept.path(), &swept.path().join("usage.sqlite"));
        // A later watch reconciliation with registration errors keeps the sweep's numbers.
        catch_up::record_watch_errors(swept.path(), 2);
        // A directory where the store file should be: it cannot be opened.
        catch_up::run_at(failed.path(), failed.path());
        let on = |home: &std::path::Path| {
            shared(&crate::mcp::setup_tools::local_doctor(
                home,
                std::env::var_os("PATH").as_deref(),
                crate::attribution::install::ReplacementNotice::Peek,
            ))["attribution"]["capture"]
                .clone()
        };
        let (swept, failed, idle) = (on(swept.path()), on(failed.path()), on(idle.path()));
        assert!(swept["lastSweepAt"].is_u64() && swept["trigger"] == "startup");
        assert_eq!(swept["watchErrors"], 2);
        assert_eq!(swept["lastError"], "checkpoint watch registration failed");
        assert_eq!(failed["lastError"], "checkpoint store unavailable");
        assert_eq!(
            idle["lastSweepAt"],
            Value::Null,
            "another home's sweep never shows here"
        );
    }

    /// Plants a pending replacement receipt, as a bundled replacement leaves it.
    pub(crate) fn plant_pending_notice(home: &std::path::Path) -> std::path::PathBuf {
        std::fs::create_dir_all(home.join("bin")).unwrap();
        let receipt = home.join("bin/.git-ai-previous");
        std::fs::write(
            &receipt,
            r#"{"version":"1.7.4","replacement":"1.7.5","path":"/prev/git-ai","sha256":"00"}"#,
        )
        .unwrap();
        receipt
    }

    /// Case-table row: a pending replacement notice. The passive `setup` read shows it without
    /// consuming it; only the `describe_setup` mode acknowledges it, once.
    #[test]
    fn setup_reads_peek_at_the_replacement_notice_and_describe_setup_acknowledges_once() {
        use crate::attribution::install::ReplacementNotice::{Acknowledge, Peek};
        let home = tempfile::tempdir().unwrap();
        let receipt = plant_pending_notice(home.path());
        let before = std::fs::read(&receipt).unwrap();
        let read = |mode| crate::mcp::setup_tools::local_doctor(home.path(), None, mode);
        let notice = |d: &Doctor| {
            d.warnings
                .iter()
                .any(|w| w.starts_with("replaced git-ai 1.7.4"))
        };
        for _ in 0..2 {
            let peeked = read(Peek);
            assert!(
                notice(&peeked),
                "the in-app JSON still shows the pending notice"
            );
            assert_eq!(
                std::fs::read(&receipt).unwrap(),
                before,
                "a peek never writes"
            );
            assert!(!shared(&peeked).to_string().contains("replaced git-ai"));
        }
        assert!(notice(&read(Acknowledge)), "describe_setup shows it");
        assert_ne!(
            std::fs::read(&receipt).unwrap(),
            before,
            "and acknowledges it"
        );
        assert!(!notice(&read(Acknowledge)), "exactly once");
        assert!(
            !notice(&read(Peek)),
            "an acknowledged notice is not shown again"
        );
    }
}
