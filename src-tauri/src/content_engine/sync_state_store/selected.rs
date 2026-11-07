// SPDX-License-Identifier: MIT
//! Durable selected intent and workspace lifecycle, separate from rebuildable baselines.
use super::*;
use crate::content_engine::destination::DestinationIdentity;
use crate::install::story_linker::selected_context::progress::{
    OperationProgress, ProgressIdentity,
};

fn authority(ctx: &ResolverContext, slug: &SourceRepoSlug) -> Result<String> {
    Ok(serde_json::to_string(&(
        ctx.user_id,
        &ctx.org_slug,
        &ctx.team_name,
        slug,
    ))?)
}

fn path_key(identity: &DestinationIdentity, path: &str) -> String {
    match identity {
        DestinationIdentity::Local { .. } => {
            crate::context_repo::override_wins_key(Path::new(path))
        }
        DestinationIdentity::Remote(_) => path.to_owned(),
    }
}

pub(super) fn rebuild_path_ownership(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let operations = {
        let mut query = tx.prepare(
            "SELECT o.destination_identity,o.authority,o.request_id,o.envelope,p.identity,p.progress FROM context_operation o LEFT JOIN context_operation_progress p USING(authority,destination_identity,request_id)",
        )?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    tx.execute("DELETE FROM context_operation_path", [])?;
    for (identity_json, authority, request_id, envelope, shape, progress) in operations {
        let identity: DestinationIdentity = serde_json::from_str(&identity_json)?;
        let envelope = merged_envelope(&envelope, shape.as_deref(), progress.as_deref())?;
        let snapshots = envelope["snapshots"].as_array().ok_or_else(|| {
            SyncStateStoreError::Data("missing snapshots during migration".into())
        })?;
        for snapshot in snapshots {
            if snapshot["state"] == "Terminal" {
                continue;
            }
            let path = snapshot["result"]["path"].as_str().ok_or_else(|| {
                SyncStateStoreError::Data("missing selected path during migration".into())
            })?;
            let key = path_key(&identity, path);
            let overlap: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM context_operation_path WHERE destination_identity=?1 AND (path=?2 OR substr(path,1,length(?2)+1)=?2||'/' OR substr(?2,1,length(path)+1)=path||'/'))", params![identity_json,key], |row| row.get(0))?;
            if overlap {
                return Err(SyncStateStoreError::Data(
                    "historical unfinished selected paths overlap after alias normalization".into(),
                ));
            }
            tx.execute("INSERT INTO context_operation_path(destination_identity,path,authority,request_id) VALUES(?1,?2,?3,?4)", params![identity_json,key,authority,request_id])?;
        }
    }
    Ok(())
}

const ENVELOPE_FORMAT: &str = "selected-context-progress-v1";
const BATCH_ENVELOPE_FORMAT: &str = "selected-context-progress-v2";

fn wrapped(envelope: &serde_json::Value) -> Result<serde_json::Value> {
    let batch = crate::install::story_linker::selected_context::validate_batch_intent(envelope)
        .map_err(SyncStateStoreError::Data)?;
    Ok(
        serde_json::json!({"format": if batch {BATCH_ENVELOPE_FORMAT}else{ENVELOPE_FORMAT}, "intent": envelope}),
    )
}

fn unwrap_retained(value: serde_json::Value) -> Result<(serde_json::Value, bool)> {
    if value.get("format").is_none() {
        if value
            .get("transfer")
            .is_some_and(|transfer| !transfer.is_null())
        {
            return Err(SyncStateStoreError::Data(
                "legacy intent cannot carry a batch descriptor".into(),
            ));
        }
        return Ok((value, false));
    }
    if (value["format"] != ENVELOPE_FORMAT && value["format"] != BATCH_ENVELOPE_FORMAT)
        || value.as_object().is_none_or(|object| object.len() != 2)
        || !value["intent"].is_object()
    {
        return Err(SyncStateStoreError::Data(
            "unsupported selected intent format".into(),
        ));
    }
    let is_batch = value["format"] == BATCH_ENVELOPE_FORMAT;
    let mut value = value;
    let intent = value["intent"].take();
    let has_batch = crate::install::story_linker::selected_context::validate_batch_intent(&intent)
        .map_err(SyncStateStoreError::Data)?;
    if is_batch != has_batch {
        return Err(SyncStateStoreError::Data(
            "selected wrapper does not match its batch feature".into(),
        ));
    }
    Ok((intent, true))
}

fn merged_envelope(
    envelope: &str,
    identity: Option<&str>,
    progress: Option<&str>,
) -> Result<serde_json::Value> {
    let (mut envelope, is_wrapped) = unwrap_retained(serde_json::from_str(envelope)?)?;
    if is_wrapped != identity.is_some() || (!is_wrapped && progress.is_some()) {
        return Err(SyncStateStoreError::Data(
            "selected intent/progress representation mismatch".into(),
        ));
    }
    if let Some(identity) = identity {
        let stored: ProgressIdentity = serde_json::from_str(identity)?;
        let actual =
            ProgressIdentity::from_envelope(&envelope).map_err(SyncStateStoreError::Data)?;
        if stored != actual {
            return Err(SyncStateStoreError::Data(
                "retained selected identity changed".into(),
            ));
        }
    }
    if let Some(progress) = progress {
        let progress: OperationProgress = serde_json::from_str(progress)?;
        progress
            .overlay(&mut envelope)
            .map_err(SyncStateStoreError::Data)?;
    }
    Ok(envelope)
}

impl SyncStateStore {
    /// Exact selected publication provenance, separate from generic projection state.
    pub fn read_context_source_binding(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &DestinationIdentity,
        path: &str,
    ) -> Result<Option<serde_json::Value>> {
        let binding: Option<String> = self.conn()?.query_row(
            "SELECT binding FROM context_source_binding WHERE authority=?1 AND destination_identity=?2 AND path=?3",
            params![authority(ctx,slug)?,serde_json::to_string(identity)?,path_key(identity,path)], |row| row.get(0)
        ).optional()?;
        binding
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    /// Called after applied baseline persistence and before making its request
    /// terminal. Failure keeps that request's path fence until recovery settles both.
    pub fn save_context_source_binding(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &DestinationIdentity,
        path: &str,
        binding: Option<&serde_json::Value>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let owner = authority(ctx, slug)?;
        let identity_json = serde_json::to_string(identity)?;
        let path = path_key(identity, path);
        if let Some(binding) = binding {
            conn.execute("INSERT INTO context_source_binding(authority,destination_identity,path,binding) VALUES(?1,?2,?3,?4) ON CONFLICT(authority,destination_identity,path) DO UPDATE SET binding=excluded.binding",params![owner,identity_json,path,serde_json::to_string(binding)?])?;
        } else {
            conn.execute("DELETE FROM context_source_binding WHERE authority=?1 AND destination_identity=?2 AND path=?3",params![owner,identity_json,path])?;
        }
        Ok(())
    }

    /// Workspace-wide, not team-specific: another team must not revive legacy writers.
    pub fn context_generation(&self, identity: &DestinationIdentity) -> Result<Option<u64>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT generation FROM context_lifecycle WHERE destination_identity=?1",
                [serde_json::to_string(identity)?],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(crate) fn uses_explicit_context(&self, identity: &DestinationIdentity) -> Result<bool> {
        let registered = match identity {
            DestinationIdentity::Local { root } => {
                let home =
                    self.path.parent().and_then(Path::parent).ok_or_else(|| {
                        SyncStateStoreError::Data("missing context namespace".into())
                    })?;
                self.is_registered_product(root)?
                    || !crate::install::worktree_index::story_registrations(home, root)
                        .map_err(|e| SyncStateStoreError::Data(e.to_string()))?
                        .is_empty()
            }
            DestinationIdentity::Remote(_) => false,
        };
        Ok(registered || self.context_generation(identity)?.is_some())
    }

    /// Caller must close legacy admission, drain its held mutation locks, and settle
    /// prior effects before calling. This method does not itself grant admission.
    pub fn admit_explicit_context(&self, identity: &DestinationIdentity) -> Result<u64> {
        let mut conn = self.conn()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<u64> = tx
            .query_row(
                "SELECT generation FROM context_lifecycle WHERE destination_identity=?1",
                [serde_json::to_string(identity)?],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(generation) = existing {
            tx.commit()?;
            return Ok(generation);
        }
        establish_remote_guard(&self.path, &tx)?;
        // A remote pending receipt, even from another team, blocks cutover.
        if let DestinationIdentity::Remote(remote) = identity {
            let pending: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM destination_effect WHERE destination_identity=?1)",
                [serde_json::to_string(remote)?],
                |row| row.get(0),
            )?;
            if pending {
                return Err(SyncStateStoreError::Data(
                    "pending legacy effects block explicit admission".into(),
                ));
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO context_lifecycle(destination_identity,generation) VALUES(?1,1)",
            [serde_json::to_string(identity)?],
        )?;
        let generation = tx.query_row(
            "SELECT generation FROM context_lifecycle WHERE destination_identity=?1",
            [serde_json::to_string(identity)?],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(generation)
    }

    /// SW-XS-7 (SPEC §11.11): legacy admission migration for one destination.
    /// Deletes the legacy engine's leftover `destination_effect` rows for it
    /// (any team) — Step 2 diffs the destination's actual files against
    /// pre-hashes, so an effect whose outcome was never learned no longer
    /// matters — then admits the destination as explicit. Returns how many
    /// rows were cleared. Callers hold the destination lock and the selected
    /// lease (no legacy writer is running).
    pub fn migrate_to_explicit(&self, identity: &DestinationIdentity) -> Result<usize> {
        let mut conn = self.conn()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cleared = match identity {
            DestinationIdentity::Remote(remote) => tx.execute(
                "DELETE FROM destination_effect WHERE destination_identity=?1",
                [serde_json::to_string(remote)?],
            )?,
            DestinationIdentity::Local { .. } => 0,
        };
        let known: Option<u64> = tx
            .query_row(
                "SELECT generation FROM context_lifecycle WHERE destination_identity=?1",
                [serde_json::to_string(identity)?],
                |row| row.get(0),
            )
            .optional()?;
        if known.is_none() {
            establish_remote_guard(&self.path, &tx)?;
            tx.execute(
                "INSERT OR IGNORE INTO context_lifecycle(destination_identity,generation) VALUES(?1,1)",
                [serde_json::to_string(identity)?],
            )?;
        }
        tx.commit()?;
        Ok(cleared)
    }

    pub fn read_context_operation(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &DestinationIdentity,
        request_id: uuid::Uuid,
    ) -> Result<Option<(String, serde_json::Value)>> {
        let row: Option<(String, String, Option<String>, Option<String>)> = self.conn()?.query_row(
            "SELECT o.fingerprint,o.envelope,p.identity,p.progress FROM context_operation o LEFT JOIN context_operation_progress p USING(authority,destination_identity,request_id) WHERE o.authority=?1 AND o.destination_identity=?2 AND o.request_id=?3",
            params![authority(ctx, slug)?, serde_json::to_string(identity)?, request_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
        row.map(|(fingerprint, envelope, shape, progress)| {
            Ok((
                fingerprint,
                merged_envelope(&envelope, shape.as_deref(), progress.as_deref())?,
            ))
        })
        .transpose()
    }

    /// Insert-only admission. Retained intent cannot be replaced by a changed retry.
    pub fn prepare_context_operation(
        &self,
        ctx: &ResolverContext,
        slug: &SourceRepoSlug,
        identity: &DestinationIdentity,
        request_id: uuid::Uuid,
        fingerprint: &str,
        envelope: &serde_json::Value,
    ) -> Result<()> {
        if envelope
            .get("transfer")
            .is_some_and(|transfer| !transfer.is_null())
            && !matches!(identity, DestinationIdentity::Remote(_))
        {
            return Err(SyncStateStoreError::Data(
                "batch intent requires a remote destination".into(),
            ));
        }
        let shape = ProgressIdentity::from_envelope(envelope).map_err(SyncStateStoreError::Data)?;
        shape
            .validate_request(request_id)
            .map_err(SyncStateStoreError::Data)?;
        let mut conn = self.conn()?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let identity_json = serde_json::to_string(identity)?;
        let generation = envelope["generation"]
            .as_u64()
            .ok_or_else(|| SyncStateStoreError::Data("missing generation".into()))?;
        let current: Option<u64> = tx
            .query_row(
                "SELECT generation FROM context_lifecycle WHERE destination_identity=?1",
                [&identity_json],
                |row| row.get(0),
            )
            .optional()?;
        if current.is_some_and(|value| value != generation) || generation != 1 {
            return Err(SyncStateStoreError::Data(
                "stale explicit generation".into(),
            ));
        }
        if current.is_none() {
            if let DestinationIdentity::Remote(remote) = identity {
                let pending: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM destination_effect WHERE destination_identity=?1)",
                    [serde_json::to_string(remote)?],
                    |row| row.get(0),
                )?;
                if pending {
                    return Err(SyncStateStoreError::Data(
                        "legacy effects block cutover".into(),
                    ));
                }
            }
        }
        let snapshots = envelope["snapshots"]
            .as_array()
            .ok_or_else(|| SyncStateStoreError::Data("missing snapshots".into()))?;
        for snapshot in snapshots {
            let path = snapshot["result"]["path"]
                .as_str()
                .ok_or_else(|| SyncStateStoreError::Data("missing selected path".into()))?;
            let key = path_key(identity, path);
            let overlap:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM context_operation_path WHERE destination_identity=?1 AND (path=?2 OR substr(path,1,length(?2)+1)=?2||'/' OR substr(?2,1,length(path)+1)=path||'/'))",params![identity_json,key],|row|row.get(0))?;
            if overlap {
                return Err(SyncStateStoreError::Data(format!(
                    "unfinished selected request owns {path}; recover its original request first"
                )));
            }
            if snapshot["state"] == "Terminal" {
                continue;
            }
            tx.execute("INSERT INTO context_operation_path(destination_identity,path,authority,request_id) VALUES(?1,?2,?3,?4)",params![identity_json,key,authority(ctx,slug)?,request_id.to_string()])?;
        }
        tx.execute("INSERT OR IGNORE INTO context_lifecycle(destination_identity,generation) VALUES(?1,?2)",params![identity_json,generation])?;
        establish_remote_guard(&self.path, &tx)?;
        tx.execute("INSERT INTO context_operation(authority,destination_identity,request_id,fingerprint,envelope) VALUES(?1,?2,?3,?4,?5)",
            params![authority(ctx, slug)?, serde_json::to_string(identity)?, request_id.to_string(), fingerprint, serde_json::to_string(&wrapped(envelope)?)?])?;
        tx.execute("INSERT INTO context_operation_progress(authority,destination_identity,request_id,identity,progress) VALUES(?1,?2,?3,?4,NULL)", params![authority(ctx, slug)?, identity_json, request_id.to_string(), serde_json::to_string(&shape)?])?;
        tx.commit()?;
        Ok(())
    }
}
