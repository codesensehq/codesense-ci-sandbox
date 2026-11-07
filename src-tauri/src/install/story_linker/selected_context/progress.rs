// SPDX-License-Identifier: MIT
//! Compact persistence codec for mutable selected progress. Retained inputs and
//! payload bytes are serialized only at admission, never by this view.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperationProgress {
    result: OperationResult,
    snapshots: Vec<SnapshotProgress>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotProgress {
    state: EffectState,
    result: ItemResult,
    settlement: Settlement,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settlement {
    last_materialized_hash: [u8; 32],
    last_materialized_mtime_ns: u128,
    last_materialized_size: Option<u64>,
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemIdentity {
    index: usize,
    reference: String,
    path: String,
    operation_id: Option<Uuid>,
}
impl From<&ItemResult> for ItemIdentity {
    fn from(result: &ItemResult) -> Self {
        Self {
            index: result.index,
            reference: result.reference.clone(),
            path: result.path.clone(),
            operation_id: result.operation_id,
        }
    }
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProgressIdentity {
    request_id: Uuid,
    items: Vec<ItemIdentity>,
}
impl ProgressIdentity {
    pub(crate) fn validate_request(&self, request_id: Uuid) -> Result<(), String> {
        if self.request_id != request_id {
            return Err("selected progress request identity mismatch".into());
        }
        Ok(())
    }

    pub(crate) fn from_envelope(envelope: &serde_json::Value) -> Result<Self, String> {
        let result: OperationResult =
            serde_json::from_value(envelope["result"].clone()).map_err(|e| e.to_string())?;
        let snapshots = envelope["snapshots"]
            .as_array()
            .ok_or("missing retained snapshots")?;
        let items = snapshots
            .iter()
            .map(|snapshot| {
                serde_json::from_value::<ItemResult>(snapshot["result"].clone())
                    .map(|result| ItemIdentity::from(&result))
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            request_id: result.request_id,
            items,
        })
    }
}
impl OperationProgress {
    pub(crate) fn validate(&self, identity: &ProgressIdentity) -> Result<(), String> {
        let actual = ProgressIdentity {
            request_id: self.result.request_id,
            items: self
                .snapshots
                .iter()
                .map(|snapshot| ItemIdentity::from(&snapshot.result))
                .collect(),
        };
        if actual != *identity {
            return Err("selected progress does not match retained request/item identities".into());
        }
        let mut results: Vec<_> = self
            .snapshots
            .iter()
            .map(|snapshot| &snapshot.result)
            .collect();
        results.sort_by_key(|result| result.index);
        if serde_json::to_value(results).map_err(|e| e.to_string())?
            != serde_json::to_value(&self.result.items).map_err(|e| e.to_string())?
        {
            return Err("selected progress aggregate differs from its items".into());
        }
        Ok(())
    }
    pub(crate) fn overlay(&self, envelope: &mut serde_json::Value) -> Result<(), String> {
        self.validate(&ProgressIdentity::from_envelope(envelope)?)?;
        let snapshots = envelope["snapshots"]
            .as_array_mut()
            .ok_or("missing retained snapshots")?;
        for (retained, current) in snapshots.iter_mut().zip(&self.snapshots) {
            retained["state"] = serde_json::to_value(&current.state).map_err(|e| e.to_string())?;
            retained["result"] =
                serde_json::to_value(&current.result).map_err(|e| e.to_string())?;
            let settlement =
                serde_json::to_value(&current.settlement).map_err(|e| e.to_string())?;
            let entry = retained["entry"]
                .as_object_mut()
                .ok_or("missing retained entry")?;
            for (key, value) in settlement.as_object().ok_or("invalid settlement")? {
                entry.insert(key.clone(), value.clone());
            }
        }
        envelope["result"] = serde_json::to_value(&self.result).map_err(|e| e.to_string())?;
        Ok(())
    }
}
