// SPDX-License-Identifier: MIT
//! The retained form of an older build's selected batch transfer. SW-XS-8:
//! the selected executor is gone; a retained intent is still validated when
//! the sync-state store's migrations read it.
use super::*;
use codesense_pty_core::context_batch as wire;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TransferDescriptor {
    version: u32,
    batch_id: Uuid,
    manifest_digest: [u8; 32],
    target: codesense_pty_core::remote_files::FileTarget,
    manifest: wire::BatchManifest,
    /// Exact owned staging identity; never an arbitrary local path.
    stage_id: Uuid,
}

impl TransferDescriptor {
    fn validate(&self, envelope: &Envelope) -> Result<(), AppError> {
        self.manifest.validate().map_err(error)?;
        if self.version != wire::VERSION
            || self.stage_id != self.batch_id
            || envelope.publish
            || self.manifest.request_id != envelope.result.request_id.to_string()
            || self.manifest.generation != envelope.generation
            || self.manifest.digest().map_err(error)? != self.manifest_digest
        {
            return Err(error("invalid retained batch descriptor identity"));
        }
        let mut raw_hash = blake3::Hasher::new();
        let mut raw_length = 0u64;
        for item in &self.manifest.items {
            let snapshot = envelope
                .snapshots
                .iter()
                .find(|snapshot| snapshot.result.index == item.index)
                .ok_or_else(|| error("retained batch member is missing"))?;
            if !matches!(&snapshot.entry.dest_path,DestinationReference::Remote {destination,..} if destination.owner_id==self.target.owner_id && destination.workspace_handle==self.target.handle)
            {
                return Err(error(
                    "retained batch target differs from destination identity",
                ));
            }
            if snapshot
                .result
                .operation_id
                .map(|id| id.to_string())
                .as_deref()
                != Some(item.operation_id.as_str())
                || snapshot.result.path != item.path
                || snapshot.mode != item.mode
                || codesense_pty_core::remote_files::Expected::from(&snapshot.expected)
                    != item.expected
                || !matches!(&snapshot.entry.dest_path,DestinationReference::Remote {relative,..} if relative.as_str()==item.path)
            {
                return Err(error("retained batch member intent differs"));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&snapshot.bytes)
                .map_err(error)?;
            if bytes.len() as u64 != item.length || *blake3::hash(&bytes).as_bytes() != item.hash {
                return Err(error("retained batch member bytes differ"));
            }
            raw_hash.update(&bytes);
            raw_length += bytes.len() as u64;
            let dependencies: Vec<_> = self
                .manifest
                .items
                .iter()
                .filter(|candidate| {
                    envelope
                        .snapshots
                        .iter()
                        .find(|snapshot| snapshot.result.index == candidate.index)
                        .is_some_and(|prior| {
                            snapshot.dependencies.contains(&prior.result.reference)
                        })
                })
                .map(|candidate| candidate.index)
                .collect();
            if item.dependencies != dependencies {
                return Err(error("retained batch dependency set differs"));
            }
        }
        if raw_length != self.manifest.raw_length
            || *raw_hash.finalize().as_bytes() != self.manifest.raw_digest
        {
            return Err(error("retained batch raw payload differs"));
        }
        Ok(())
    }
}
/// Shared by the v2 persistence boundary. No accepted wrapper can smuggle an
/// unrecognized transfer into a reader that would take the single-file path.
pub(crate) fn validate_retained(value: &serde_json::Value) -> Result<bool, String> {
    if value.get("transfer").is_none_or(serde_json::Value::is_null) {
        return Ok(false);
    }
    let envelope: Envelope = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let transfer = envelope
        .transfer
        .as_ref()
        .ok_or("batch descriptor missing")?;
    transfer.validate(&envelope).map_err(|e| e.to_string())?;
    Ok(true)
}
