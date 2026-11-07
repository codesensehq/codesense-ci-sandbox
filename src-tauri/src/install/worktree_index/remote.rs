// SPDX-License-Identifier: MIT
//! Remote destination hydration metadata owned by the existing worktree index.
//! This namespace is never traversed by local path enumeration or its reaper.
use crate::content_engine::destination::RemoteDestinationIdentity;
use crate::content_engine::ResolverContext;
use crate::domain::SourceRepoSlug;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Scope {
    destination: RemoteDestinationIdentity,
    org: String,
    team: Option<String>,
    source_repo: SourceRepoSlug,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RemoteHydrationRecord {
    #[serde(default)]
    pub lazy: bool,
    #[serde(default)]
    pub hydrated_epics: Vec<PathBuf>,
    #[serde(default)]
    pub hydration_policy: Option<crate::install::hydration_closure::HydrationPolicy>,
}
impl RemoteHydrationRecord {
    pub(crate) fn hydrate_epic(&mut self, epic: PathBuf) -> bool {
        super::extend_hydrated_epics(&mut self.hydrated_epics, epic)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    scope: Scope,
    hydration: RemoteHydrationRecord,
}
fn scope(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    identity: &RemoteDestinationIdentity,
) -> Result<Scope, String> {
    if identity.owner_id != ctx.user_id {
        return Err("remote hydration owner changed".into());
    }
    Ok(Scope {
        destination: identity.clone(),
        org: ctx.org_slug.clone(),
        team: ctx.team_name.clone(),
        source_repo: slug.clone(),
    })
}
fn path(ctx: &ResolverContext, scope: &Scope) -> Result<PathBuf, String> {
    let key = serde_json::to_vec(scope).map_err(|e| e.to_string())?;
    Ok(
        crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal())
            .join("worktrees")
            .join("remote-hydration-v1")
            .join(format!("{}.json", blake3::hash(&key).to_hex())),
    )
}
pub(crate) fn read(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    identity: &RemoteDestinationIdentity,
) -> Result<Option<RemoteHydrationRecord>, String> {
    let scope = scope(ctx, slug, identity)?;
    let bytes = match std::fs::read(path(ctx, &scope)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let record: Envelope = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if record.version != 1 || record.scope != scope {
        return Err("remote hydration record scope mismatch".into());
    }
    if record
        .hydration
        .hydrated_epics
        .iter()
        .any(|epic| !epic.is_absolute())
    {
        return Err("hydration epics must name canonical source directories".into());
    }
    Ok(Some(record.hydration))
}
pub(crate) fn write(
    ctx: &ResolverContext,
    slug: &SourceRepoSlug,
    identity: &RemoteDestinationIdentity,
    hydration: &RemoteHydrationRecord,
) -> Result<(), String> {
    let scope = scope(ctx, slug, identity)?;
    let destination = path(ctx, &scope)?;
    let bytes = serde_json::to_vec(&Envelope {
        version: 1,
        scope,
        hydration: hydration.clone(),
    })
    .map_err(|e| e.to_string())?;
    crate::content_engine::write_tmp_fsync_rename(&destination, &bytes).map_err(|e| e.to_string())
}

/// One selected hydrate batch sent but not yet confirmed: resumed with the same
/// request id and refs before any new batch, so a lost acknowledgement never
/// leaves a path owned by a request nobody can recover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingSelected {
    pub request_id: uuid::Uuid,
    pub refs: Vec<String>,
    /// Failed resumes so far; the entry is given up after a few.
    #[serde(default)]
    pub attempts: u32,
}

/// Where the selected batches pending on one destination are journaled. Keyed by
/// the destination alone (workspace-wide, like the engine's path ownership), not
/// by team or repository.
fn pending_path(
    ctx: &ResolverContext,
    identity: &RemoteDestinationIdentity,
) -> Result<PathBuf, String> {
    if identity.owner_id != ctx.user_id {
        return Err("remote hydration owner changed".into());
    }
    let key = serde_json::to_vec(identity).map_err(|e| e.to_string())?;
    Ok(
        crate::context_repo::path_to_user_home(&ctx.codesense_home, &ctx.principal())
            .join("worktrees")
            .join("remote-hydration-v1")
            .join(format!(
                "{}.selected-pending.json",
                blake3::hash(&key).to_hex()
            )),
    )
}

/// The selected batches pending on this destination.
pub(crate) fn read_pending(
    ctx: &ResolverContext,
    identity: &RemoteDestinationIdentity,
) -> Result<Vec<PendingSelected>, String> {
    match std::fs::read(pending_path(ctx, identity)?) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
}

/// Replace the destination's pending journal. Callers hold the destination lock.
pub(crate) fn write_pending(
    ctx: &ResolverContext,
    identity: &RemoteDestinationIdentity,
    pending: &[PendingSelected],
) -> Result<(), String> {
    let bytes = serde_json::to_vec(pending).map_err(|e| e.to_string())?;
    crate::content_engine::write_tmp_fsync_rename(&pending_path(ctx, identity)?, &bytes)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_hydration_keys_include_origin_team_and_repository() {
        let home = tempfile::tempdir().unwrap();
        let mut ctx = ResolverContext {
            context_root: None,
            codesense_home: home.path().into(),
            user_id: 41,
            org_slug: "org".into(),
            team_name: Some("a".into()),
        };
        let slug = SourceRepoSlug::new("owner", "repo").unwrap();
        let mut identity = RemoteDestinationIdentity {
            owner_id: 41,
            machine_id: "one".into(),
            server_id: "server".into(),
            workspace_handle: "stable-workspace".into(),
        };
        let mut record = RemoteHydrationRecord::default();
        assert!(record.hydrate_epic(home.path().join("source/epic")));
        assert!(!record.lazy);
        write(&ctx, &slug, &identity, &record).unwrap();
        assert_eq!(
            read(&ctx, &slug, &identity)
                .unwrap()
                .unwrap()
                .hydrated_epics,
            record.hydrated_epics
        );
        identity.machine_id = "two".into();
        assert!(read(&ctx, &slug, &identity).unwrap().is_none());
        identity.machine_id = "one".into();
        ctx.team_name = Some("b".into());
        assert!(read(&ctx, &slug, &identity).unwrap().is_none());
        ctx.team_name = Some("a".into());
        let other = SourceRepoSlug::new("owner", "other").unwrap();
        assert!(read(&ctx, &other, &identity).unwrap().is_none());
    }
}
