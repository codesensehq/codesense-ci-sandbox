// SPDX-License-Identifier: MIT
//! The local host identity and the arming rule.
//!
//! A routine definition syncs to every teammate's machine, and a person may have
//! more than one Mac. A shared job (`runs_on: one_mac`) must fire exactly once,
//! so it is armed on one Mac only: the one its `armed_host` names. Where it
//! runs is placement, not permission: anyone may move it (`service::arm_here`).
//! One that names no Mac runs nowhere (see [`is_armed_here`]). A recipe each
//! Mac runs (`runs_on: each_mac`) is armed on every Mac, and each Mac's own
//! local switch decides whether it fires there. The host id
//! is minted once into `{codesense_home}` (never the context repo), so it never
//! syncs.
//!
//! Mac-fired routines decide from their own clone, so a placement or mode
//! change reaches another Mac only with its next pull. A shared job that
//! becomes a recipe opens no double-fire window: every other Mac starts off,
//! and the Mac it ran on keeps firing from the same cursor. A shared job moved
//! with "Run on this Mac", or a recipe made a shared job on another Mac, can
//! still fire on the old Mac until it pulls and stops on its first wake.
//! Closing that window needs the old Mac's hand-off acknowledgement; an epoch
//! bump alone cannot stop a Mac firing from its stale clone.
//!
//! Machine-armed routines have a worker fence: every change of managing Mac
//! or machine placement bumps the arm epoch. Once the worker sees it, the old
//! Mac cannot arm or disarm at its old epoch, or import the worker's run log.
//! The current manager imports historical runs too, keyed by the worker's
//! stable run ids. Rows already imported on the old Mac remain local history.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::model::{RoutineDef, RunsOn};

fn host_id_path(codesense_home: &Path) -> PathBuf {
    codesense_home.join("routines").join("host-id")
}

/// This install's host id, minting it on first use. Concurrent first calls
/// agree on one id: the minted file is published with a hard link, which fails
/// when another caller already published, and the loser reads the winner's id.
pub fn local_host_id(codesense_home: &Path) -> io::Result<String> {
    let path = host_id_path(codesense_home);
    if let Some(id) = read_host_id(&path)? {
        return Ok(id);
    }
    // A blank file can only be a torn mint or a bad hand edit (a published id
    // is written and synced before the link), so nothing is armed on it. Clear
    // it and mint again. The content is re-read here, not inferred from the
    // read above, so a concurrent caller's freshly published id is never
    // removed.
    if std::fs::read_to_string(&path).is_ok_and(|raw| raw.trim().is_empty()) {
        let _ = std::fs::remove_file(&path);
    }
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("host-id path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let minted = uuid::Uuid::new_v4().simple().to_string();
    let tmp = dir.join(format!("host-id.{minted}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(format!("{minted}\n").as_bytes())?;
        file.sync_all()?;
    }
    let published = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    match published {
        Ok(()) => Ok(minted),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            read_host_id(&path)?.ok_or_else(|| io::Error::other("host-id file exists but is empty"))
        }
        Err(e) => Err(e),
    }
}

fn read_host_id(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let id = raw.trim();
            Ok((!id.is_empty()).then(|| id.to_string()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// True iff this Mac is a place `def` runs: it is not archived, and either it
/// runs once for the team and `armed_host` names this Mac, or it is a recipe
/// each Mac runs (`runs_on: each_mac`). Whether it actually fires here is then
/// this Mac's own switch (`state::enabled_here`), whose default
/// [`starts_on_here`] gives.
///
/// Every routine CodeSense creates or saves names a Mac, so `armed_host` is
/// unset only in a definition written by hand. Such a `one_mac` routine is
/// armed on no Mac at all until someone picks one with "Run on this Mac": any
/// other default (its creator's Macs, say) could fire it on two Macs at once.
/// A `runs_on` this build cannot read arms nowhere, for the same reason.
pub fn is_armed_here(def: &RoutineDef, host_id: &str) -> bool {
    // SW-AGR-8: a routine armed on a machine fires there, never on a Mac.
    if def.archived_at.is_some() || def.armed_machine().is_some() {
        return false;
    }
    match def.runs_on() {
        Ok(RunsOn::OneMac) => names_this_mac(def, host_id),
        Ok(RunsOn::EachMac) => true,
        Err(_) => false,
    }
}

/// Whether `def` can run on any Mac: a recipe each Mac runs always can, a
/// shared job only once it names a Mac.
pub fn is_armed_anywhere(def: &RoutineDef) -> bool {
    if def.armed_machine().is_some() {
        return true;
    }
    match def.runs_on() {
        Ok(RunsOn::OneMac) => !def.armed_host.trim().is_empty(),
        Ok(RunsOn::EachMac) => true,
        Err(_) => false,
    }
}

/// Whether this Mac's switch for `def` starts on, before anyone here flips it.
/// A shared job is on wherever it is armed. A recipe each Mac runs starts off
/// on every Mac except the one its `armed_host` still names: that is the Mac
/// that ran it as a shared job before it became a recipe, so the switch never
/// silently stops it there. Only the definition decides, so every Mac reaches
/// the same answer from the synced file alone.
pub fn starts_on_here(def: &RoutineDef, host_id: &str) -> bool {
    match def.runs_on() {
        Ok(RunsOn::OneMac) => true,
        Ok(RunsOn::EachMac) => names_this_mac(def, host_id),
        Err(_) => false,
    }
}

/// SW-AGR-8: whether this Mac manages `def`'s arm on its machine: it pushes
/// the arm (and every edit), disarms it when it moves, and records its runs.
pub fn manages_machine_arm(def: &RoutineDef, host_id: &str) -> bool {
    def.armed_machine().is_some() && names_this_mac(def, host_id)
}

/// Whether `def`'s `armed_host` names this Mac.
pub(crate) fn names_mac(def: &RoutineDef, host_id: &str) -> bool {
    names_this_mac(def, host_id)
}

fn names_this_mac(def: &RoutineDef, host_id: &str) -> bool {
    let armed = def.armed_host.trim();
    !armed.is_empty() && armed == host_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routines::model::{AgentRef, Policy, Where};

    fn def(owner: &str, host: &str, archived: bool) -> RoutineDef {
        def_on(owner, host, archived, "")
    }

    fn def_on(owner: &str, host: &str, archived: bool, runs_on: &str) -> RoutineDef {
        RoutineDef {
            action: Default::default(),
            command: Vec::new(),
            after: None,
            result_schema: None,
            id: "r".into(),
            name: "R".into(),
            owner: owner.into(),
            agent: AgentRef::from("a"),
            mode: String::new(),
            task: "t".into(),
            target: Where::team_workspace(),
            triggers: Vec::new(),
            policy: Policy::default(),
            variables: Default::default(),
            revision: 1,
            armed_host: host.into(),
            runs_on: runs_on.into(),
            archived_at: archived.then(|| "2026-09-30T00:00:00Z".to_string()),
            armed_machine: None,
            arm_epoch: 0,
            handover_from: None,
            archived_with_agent: false,
            migrated: None,
            extra: Default::default(),
        }
    }

    // SW-AGR-8: armed on a machine, it fires there and on no Mac (not even
    // the one `armed_host` names, which manages the arm); it is armed
    // somewhere. Moved back, it is the Mac's again.
    #[test]
    fn a_routine_armed_on_a_machine_fires_on_no_mac() {
        let mut routine = def("1", "here", false);
        routine.armed_machine = Some("m-7f3a".into());
        assert!(!is_armed_here(&routine, "here"));
        assert!(!is_armed_here(&routine, "other"));
        assert!(is_armed_anywhere(&routine));
        assert!(manages_machine_arm(&routine, "here"));
        assert!(!manages_machine_arm(&routine, "other"));
        routine.armed_machine = Some("  ".into());
        assert!(is_armed_here(&routine, "here"), "a blank machine is none");
        assert!(!manages_machine_arm(&routine, "here"));
    }

    #[test]
    fn armed_here_truth_table() {
        // owner, armed_host, archived → expected. Who created it never matters.
        let cases = [
            ("1", "here", false, true),
            ("2", "here", false, true), // a teammate's routine, armed on this Mac
            ("1", "other", false, false), // armed on another Mac
            ("2", "other", false, false),
            ("1", "here", true, false), // archived
            ("2", "here", true, false),
            // No armed_host (hand-written): armed on no Mac, so it never
            // fires twice, whoever wrote it and wherever they are.
            ("1", "", false, false),
            ("2", "", false, false),
            ("1", "  ", false, false),
            ("1", "", true, false),
        ];
        for (owner, host, archived, expected) in cases {
            let routine = def(owner, host, archived);
            assert_eq!(
                is_armed_here(&routine, "here"),
                expected,
                "owner={owner} host={host} archived={archived}"
            );
            // Every Mac answers the same for a routine that names none.
            if host.trim().is_empty() {
                assert!(
                    !is_armed_here(&routine, ""),
                    "an empty host id never matches"
                );
                assert!(!is_armed_anywhere(&routine));
            } else {
                assert!(is_armed_anywhere(&routine));
            }
        }
    }

    #[test]
    fn a_recipe_each_mac_runs_is_armed_everywhere_and_starts_on_its_old_mac() {
        // armed_host, archived → (armed here, starts on here) on Mac "here".
        let cases = [
            ("here", false, true, true), // it ran here as a shared job
            ("other", false, true, false),
            ("", false, true, false), // created as a recipe: off everywhere
            ("here", true, false, true),
        ];
        for (host, archived, armed, starts_on) in cases {
            let recipe = def_on("1", host, archived, "each_mac");
            assert_eq!(is_armed_here(&recipe, "here"), armed, "{host} {archived}");
            assert_eq!(starts_on_here(&recipe, "here"), starts_on, "{host}");
            assert!(is_armed_anywhere(&recipe));
        }
        // A shared job is on wherever it is armed.
        assert!(starts_on_here(
            &def_on("1", "other", false, "one_mac"),
            "here"
        ));
        assert!(is_armed_here(
            &def_on("1", "here", false, "one_mac"),
            "here"
        ));
        // A mode this build cannot read runs nowhere.
        let unknown = def_on("1", "here", false, "every_mac");
        assert!(!is_armed_here(&unknown, "here"));
        assert!(!is_armed_anywhere(&unknown));
        assert!(!starts_on_here(&unknown, "here"));
    }

    #[test]
    fn host_id_is_minted_once_and_stable() {
        let home = tempfile::tempdir().unwrap();
        let first = local_host_id(home.path()).unwrap();
        assert_eq!(first.len(), 32);
        assert_eq!(local_host_id(home.path()).unwrap(), first);
        assert_eq!(
            std::fs::read_to_string(home.path().join("routines/host-id"))
                .unwrap()
                .trim(),
            first
        );
        // A different install (home) gets a different id.
        let other = tempfile::tempdir().unwrap();
        assert_ne!(local_host_id(other.path()).unwrap(), first);
    }

    #[test]
    fn a_torn_empty_host_id_is_reminted() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("routines/host-id");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for blank in ["", "\n", "  \n"] {
            std::fs::write(&path, blank).unwrap();
            let id = local_host_id(home.path()).unwrap();
            assert_eq!(id.len(), 32, "{blank:?}");
            assert_eq!(local_host_id(home.path()).unwrap(), id);
        }
    }

    #[test]
    fn concurrent_first_mint_agrees_on_one_id() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().to_path_buf();
        let ids: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| local_host_id(&path).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(ids.windows(2).all(|w| w[0] == w[1]), "{ids:?}");
    }
}
