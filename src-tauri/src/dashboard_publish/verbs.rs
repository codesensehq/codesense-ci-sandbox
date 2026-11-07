// SPDX-License-Identifier: MIT
//! The local verbs behind `publish_dashboard` and `unpublish_dashboard`
//! (SW-PB-22a AC-7). Each verb validates the manifest first, then checks the
//! DR-13 transition, then builds the publishable payload (so a bridge call or an
//! over-limit file stops the publish before anything is written). Only then does
//! it write the manifest and the local record. Nothing here touches the network;
//! the push is SW-PB-22e.

use std::fmt;
use std::path::Path;

use serde::Serialize;

use super::manifest::{self, ManifestError, PublishState, Visibility};
use super::payload::{self, FolderReader, Mode, PayloadError};
use super::state::{self, Event, LiveApproved, LocalState, TransitionError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerbOutcome {
    pub slug: String,
    /// The manifest's `publish.state` after the verb (`draft` or `published`).
    pub manifest_state: &'static str,
    /// The visibility the publish set. `None` for unpublish, which does not read
    /// the rest of the manifest.
    pub visibility: Option<&'static str>,
    /// The local record's `state` after the verb.
    pub local_state: LocalState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerbError {
    Manifest(ManifestError),
    Payload(PayloadError),
    Transition(TransitionError),
    Storage(String),
}

impl fmt::Display for VerbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerbError::Manifest(e) => write!(f, "{e}"),
            VerbError::Payload(e) => write!(f, "{e}"),
            VerbError::Transition(e) => write!(f, "{e}"),
            VerbError::Storage(e) => write!(f, "{e}"),
        }
    }
}

impl From<ManifestError> for VerbError {
    fn from(e: ManifestError) -> Self {
        VerbError::Manifest(e)
    }
}

impl From<PayloadError> for VerbError {
    fn from(e: PayloadError) -> Self {
        VerbError::Payload(e)
    }
}

impl From<TransitionError> for VerbError {
    fn from(e: TransitionError) -> Self {
        VerbError::Transition(e)
    }
}

/// Mark a dashboard for publishing: manifest `published` (with `visibility`),
/// local record `publishing`. The first commit that makes it `published` is 22e.
///
/// Order: record first, manifest second. The manifest is the switch the watcher
/// reads (DR-13: a draft manifest means the watcher does nothing), so it is set
/// last. If the manifest write fails, the record is put back to its previous
/// state (best effort), so a retry is not refused by the transition.
///
/// `live_approved` is the manifest's `live` block to turn on
/// (SW-DP-4b, DP-D39, `approval::decide`), saved with the record: `None` clears it.
pub fn publish(
    folder: &Path,
    data_dir: &Path,
    visibility: Visibility,
    live_approved: Option<LiveApproved>,
) -> Result<VerbOutcome, VerbError> {
    let manifest = manifest::load(folder)?;
    manifest::validate_pin_scope(
        &manifest.live_args,
        live_approved.as_ref().and_then(|b| b.scope.as_ref()),
    )?;
    let before = state::load(data_dir, &manifest.slug).map_err(VerbError::Storage)?;
    let mut after = before.clone();
    after.folder = Some(state::folder_key(folder));
    after.live_approved = live_approved;
    // A record left in `publishing` while the manifest is still `draft` is the
    // residue of a publish whose manifest write (or its restore) failed. The
    // manifest says nothing is live, so a retry heals the record instead of
    // being refused by the transition.
    let healing = before.state == LocalState::Publishing && manifest.state == PublishState::Draft;
    if !healing {
        after.apply(Event::Publish)?;
    }
    payload::build(&manifest, Mode::Published, &FolderReader { folder })?;

    state::save(data_dir, &after).map_err(VerbError::Storage)?;
    if let Err(e) = manifest::write_publish(folder, PublishState::Published, Some(visibility)) {
        let _ = state::save(data_dir, &before);
        return Err(e.into());
    }
    Ok(VerbOutcome {
        slug: manifest.slug,
        manifest_state: PublishState::Published.as_str(),
        visibility: Some(visibility.as_str()),
        local_state: after.state,
    })
}

/// Take a published dashboard down: manifest back to `draft`, local record
/// `unpublished`. The hosted copy is removed by R6 in `push::take_down_hosted`.
///
/// Order: manifest first, record second. A draft manifest is the watcher's off
/// switch, so the takedown holds as soon as the manifest is written. If the
/// record write then fails, the dashboard is already off and a retry is safe.
/// Only the slug is read, so a bad allowlist cannot block takedown.
///
/// The takedown owed to the server is saved with the local takedown, before
/// R6 runs. A takedown already owed is not repeated: the record would refuse
/// `Unpublish`. The manifest is set to draft on every call, so the outcome
/// matches what is on disk.
pub fn unpublish(folder: &Path, data_dir: &Path) -> Result<VerbOutcome, VerbError> {
    let slug = manifest::load_slug(folder)?;
    let mut record = state::load(data_dir, &slug).map_err(VerbError::Storage)?;
    if !record.takedown_pending() {
        record.apply(Event::Unpublish)?;
        record.apply(Event::TakedownOwed)?;
    }
    manifest::write_publish(folder, PublishState::Draft, None)?;
    state::save(data_dir, &record).map_err(VerbError::Storage)?;
    Ok(VerbOutcome {
        slug,
        manifest_state: PublishState::Draft.as_str(),
        visibility: None,
        local_state: record.state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const PAGE: &str = "<h1>AI usage</h1>";

    fn dashboard(page: &str) -> (tempfile::TempDir, tempfile::TempDir) {
        let folder = tempfile::tempdir().unwrap();
        fs::write(folder.path().join("index.html"), page).unwrap();
        fs::write(
            folder.path().join(manifest::MANIFEST_FILE),
            "slug: ai-usage\ntitle: AI usage\npublish:\n  state: draft\n  visibility: private\n  files:\n    - index.html\n",
        )
        .unwrap();
        (folder, tempfile::tempdir().unwrap())
    }

    fn snapshot(folder: &Path, data: &Path) -> (String, Option<String>) {
        let yml = fs::read_to_string(folder.join(manifest::MANIFEST_FILE)).unwrap();
        let rec = fs::read_to_string(data.join("dashboard-publish/ai-usage.json")).ok();
        (yml, rec)
    }

    #[test]
    fn publishing_selectors_requires_the_saved_team_scope() {
        use super::super::{approval, push::tests::dashboard};
        for (selector, scope, allowed) in [
            ("{team: riverside}", Some(("acme", "riverside")), true),
            ("{team: family}", Some(("acme", "riverside")), false),
            ("{team: riverside}", None, false),
            ("{epic: SV-CDE}", None, false),
            ("{epic: SV-CDE}", Some(("acme", "riverside")), true),
        ] {
            let folder = dashboard();
            let path = folder.path().join(manifest::MANIFEST_FILE);
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, format!("{text}\nlive: {{queries: [storyChanges], args: {{storyChanges: {{storyId: [{selector}]}}}}}}\n")).unwrap();
            let m = manifest::load(folder.path()).unwrap();
            let decision = approval::decide(&m, folder.path(), None, scope);
            let data = tempfile::tempdir().unwrap();
            let result = publish(
                folder.path(),
                data.path(),
                Visibility::Private,
                decision.approved,
            );
            assert_eq!(result.is_ok(), allowed, "{selector}/{scope:?}: {result:?}");
            if !allowed {
                assert!(result.unwrap_err().to_string().contains("live.args"));
            }
        }
    }

    #[test]
    fn publish_sets_the_manifest_and_the_local_record() {
        let (folder, data) = dashboard(PAGE);
        let out = publish(
            folder.path(),
            data.path(),
            Visibility::Team,
            None,
        )
        .unwrap();
        assert_eq!(out.manifest_state, "published");
        assert_eq!(out.visibility, Some("team"));
        assert_eq!(out.local_state, LocalState::Publishing);

        let m = manifest::load(folder.path()).unwrap();
        assert_eq!(m.state, PublishState::Published);
        assert_eq!(m.visibility, Visibility::Team);
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().state,
            LocalState::Publishing
        );
    }

    #[test]
    fn unpublish_from_draft_is_refused_and_writes_nothing() {
        let (folder, data) = dashboard(PAGE);
        let before = snapshot(folder.path(), data.path());
        let err = unpublish(folder.path(), data.path()).unwrap_err();
        assert!(matches!(err, VerbError::Transition(_)), "{err:?}");
        assert_eq!(snapshot(folder.path(), data.path()), before);
    }

    #[test]
    fn a_pending_takedown_is_retried_without_a_second_local_step() {
        let (folder, data) = dashboard(PAGE);
        publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        )
        .unwrap();
        unpublish(folder.path(), data.path()).unwrap();
        // R6 failed: the owed takedown is saved on the unpublished record.
        let record = state::load(data.path(), "ai-usage").unwrap();
        assert!(record.takedown_pending());

        let before = snapshot(folder.path(), data.path());
        let outcome = unpublish(folder.path(), data.path()).unwrap();
        assert_eq!(outcome.local_state, LocalState::Unpublished);
        assert_eq!(outcome.manifest_state, "draft");
        assert_eq!(snapshot(folder.path(), data.path()), before);

        // A publish is refused while the takedown is owed, and writes nothing.
        let err = publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        );
        assert!(err.is_err());
        assert_eq!(snapshot(folder.path(), data.path()), before);
    }

    #[test]
    fn unpublish_after_publish_sets_manifest_draft_and_record_unpublished() {
        let (folder, data) = dashboard(PAGE);
        publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        )
        .unwrap();
        // The first commit is 22e. Simulate it so the record is in `published`.
        let mut r = state::load(data.path(), "ai-usage").unwrap();
        r.apply(Event::FirstCommitOk).unwrap();
        state::save(data.path(), &r).unwrap();

        let out = unpublish(folder.path(), data.path()).unwrap();
        assert_eq!(out.manifest_state, "draft");
        assert_eq!(out.local_state, LocalState::Unpublished);
        assert_eq!(
            manifest::load(folder.path()).unwrap().state,
            PublishState::Draft
        );
    }

    #[test]
    fn an_invalid_manifest_returns_the_error_and_writes_nothing() {
        let (folder, data) = dashboard(PAGE);
        fs::write(
            folder.path().join(manifest::MANIFEST_FILE),
            "slug: ai-usage\ntitle: AI usage\npublish:\n  state: live\n  visibility: private\n  files:\n    - index.html\n",
        )
        .unwrap();
        let before = snapshot(folder.path(), data.path());
        let err = publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, VerbError::Manifest(ManifestError::UnknownValue { .. })),
            "{err:?}"
        );
        assert_eq!(snapshot(folder.path(), data.path()), before);
    }

    #[test]
    fn a_bridge_call_stops_publish_before_any_write() {
        let (folder, data) = dashboard("<script>window.codesense.sessions()</script>");
        let before = snapshot(folder.path(), data.path());
        let err = publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, VerbError::Payload(PayloadError::BridgeCalls(_))),
            "{err:?}"
        );
        assert_eq!(snapshot(folder.path(), data.path()), before);
    }

    #[test]
    fn unpublish_works_when_the_allowlist_points_at_a_missing_file() {
        // Takedown reads only the slug: a broken allowlist must not block it.
        let (folder, data) = dashboard(PAGE);
        publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        )
        .unwrap();
        let mut r = state::load(data.path(), "ai-usage").unwrap();
        r.apply(Event::FirstCommitOk).unwrap();
        state::save(data.path(), &r).unwrap();
        fs::remove_file(folder.path().join("index.html")).unwrap();

        let out = unpublish(folder.path(), data.path()).unwrap();
        assert_eq!(out.local_state, LocalState::Unpublished);
        assert_eq!(out.visibility, None);
    }

    #[test]
    fn a_failed_manifest_write_restores_the_previous_record() {
        use std::os::unix::fs::PermissionsExt;
        let (folder, data) = dashboard(PAGE);
        // A read-only folder makes the manifest's temp-file write fail after the record is saved.
        fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        );
        fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(result, Err(VerbError::Manifest(_))), "{result:?}");
        assert_eq!(
            state::load(data.path(), "ai-usage").unwrap().state,
            LocalState::Draft
        );
        assert_eq!(
            manifest::load(folder.path()).unwrap().state,
            PublishState::Draft
        );
    }

    #[test]
    fn a_retry_heals_a_publishing_record_with_a_draft_manifest() {
        // The stuck state: the record was saved as publishing, then the manifest
        // write and its restore both failed. The manifest is still draft.
        let (folder, data) = dashboard(PAGE);
        let mut stuck = state::load(data.path(), "ai-usage").unwrap();
        stuck.apply(Event::Publish).unwrap();
        state::save(data.path(), &stuck).unwrap();

        let out = publish(
            folder.path(),
            data.path(),
            Visibility::Team,
            None,
        )
        .unwrap();
        assert_eq!(out.local_state, LocalState::Publishing);
        assert_eq!(out.manifest_state, "published");
        assert_eq!(
            manifest::load(folder.path()).unwrap().state,
            PublishState::Published
        );
    }

    #[test]
    fn publish_twice_is_refused_because_the_record_is_publishing() {
        let (folder, data) = dashboard(PAGE);
        publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        )
        .unwrap();
        let err = publish(
            folder.path(),
            data.path(),
            Visibility::Private,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, VerbError::Transition(_)), "{err:?}");
    }
}
