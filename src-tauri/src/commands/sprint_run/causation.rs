// SPDX-License-Identifier: MIT
//! Causal lineage for loop protection ([`Causation`], [`CausalEdge`]) and the scrub-and-bound pass
//! a payload or string runs through before it is persisted ([`scrub_payload`], [`scrub_str`]).

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::shared::secret_scan::redact_secrets;

/// One `(automation_id, target_id)` edge in a [`Causation::ancestry`] chain. The causal guard
/// (L2) rejects a run whose incoming event already carries the edge it is about to add — a
/// repeated edge is a cycle. A named pair (not a bare tuple) so the wire form round-trips as two
/// legible keys and a call site reads as domain intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausalEdge {
    pub automation_id: String,
    pub target_id: String,
}

impl CausalEdge {
    pub fn new(automation_id: impl Into<String>, target_id: impl Into<String>) -> Self {
        CausalEdge {
            automation_id: automation_id.into(),
            target_id: target_id.into(),
        }
    }
}

/// Causal lineage for loop protection: where a run chain started, how deep it is, and the
/// per-edge chain the causal loop guard inspects (`#[serde(default)]`, so an older record without
/// it still deserializes).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Causation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_step_id: Option<String>,
    #[serde(default)]
    pub chain_depth: u32,
    /// The accumulated `(automation_id, target_id)` edges from the run chain that produced this
    /// event, oldest first. Empty on an origin (human/agent) event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestry: Vec<CausalEdge>,
}

impl Causation {
    /// Derive the causation an emitted event carries: the same chain with `edge` appended and the
    /// depth advanced by one. Pure — the parent chain is untouched — so building a child event can
    /// never mutate the event that triggered it.
    pub fn descend(&self, edge: CausalEdge) -> Causation {
        let mut ancestry = self.ancestry.clone();
        ancestry.push(edge);
        Causation {
            origin_run_id: self.origin_run_id.clone(),
            origin_step_id: self.origin_step_id.clone(),
            chain_depth: self.chain_depth.saturating_add(1),
            ancestry,
        }
    }
}

/// Payload bound — a generous ceiling (a normal payload is a few keys); guards a record against a
/// runaway producer.
pub(crate) const PAYLOAD_MAX_BYTES: usize = 64 * 1024;

/// The scrub-and-bound pass run **once** over a raw JSON payload before it is persisted. Reuses the shared credential detector
/// ([`redact_secrets`]) so the seam has one secret-pattern list (DRY — no hand-rolled scrub), then
/// bounds the serialized size so a runaway payload can never bloat a record. Whole-string
/// redaction (not a per-leaf tree walk) is deliberate: the shared keyword patterns need the
/// `"key":"value"` context to mask an opaque secret under a credential key. Over-cap payloads
/// collapse to a legible marker rather than a truncated (invalid) JSON slice. `redact_secrets` masks
/// only secret spans and keeps JSON valid, so the reparse succeeds; on the (defensive) off-chance it
/// does not, the already-scrubbed text is preserved as a string — never silently dropped, never
/// re-leaked.
pub fn scrub_payload(raw: JsonValue) -> JsonValue {
    let Ok(rendered) = serde_json::to_string(&raw) else {
        return JsonValue::Null;
    };
    if rendered.len() > PAYLOAD_MAX_BYTES {
        return serde_json::json!({ "truncated": true, "original_bytes": rendered.len() });
    }
    let scrubbed = redact_secrets(&rendered);
    serde_json::from_str(&scrubbed).unwrap_or(JsonValue::String(scrubbed))
}

/// SW-AU-96: the plain-string companion to [`scrub_payload`] — redact secrets, then bound the
/// length to the same `PAYLOAD_MAX_BYTES` cap, appending a legible marker when truncated (a string
/// channel cannot carry the JSON `{truncated}` object `scrub_payload` returns). One scrub boundary,
/// both shapes — so a summary / artifact reference / label is bounded exactly like a JSON output.
pub fn scrub_str(raw: &str) -> String {
    let scrubbed = redact_secrets(raw);
    if scrubbed.len() <= PAYLOAD_MAX_BYTES {
        return scrubbed;
    }
    let mut end = PAYLOAD_MAX_BYTES;
    while !scrubbed.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = scrubbed[..end].to_string();
    bounded.push_str("…[truncated]");
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causation_descend_appends_edge_and_advances_depth() {
        let base = Causation {
            origin_run_id: Some("run-1".into()),
            chain_depth: 2,
            ..Causation::default()
        };
        let edge = CausalEdge::new("auto-1", "SW-AU-93");
        let child = base.descend(edge.clone());
        // Parent untouched (pure accumulation).
        assert_eq!(base.chain_depth, 2);
        assert!(base.ancestry.is_empty());
        // Child carries the edge and the advanced depth + preserved origin.
        assert_eq!(child.chain_depth, 3);
        assert_eq!(child.ancestry, vec![edge]);
        assert_eq!(child.origin_run_id.as_deref(), Some("run-1"));
    }

    #[test]
    fn causation_ancestry_is_back_compatible_and_roundtrips() {
        // An older SW-AU-74 envelope with no `ancestry` key still deserializes (empty chain).
        let legacy: Causation =
            serde_json::from_str(r#"{"origin_run_id":"r","chain_depth":1}"#).unwrap();
        assert!(legacy.ancestry.is_empty());
        // A chain round-trips through serde unchanged.
        let chain = legacy.descend(CausalEdge::new("a", "t"));
        let json = serde_json::to_string(&chain).unwrap();
        assert_eq!(serde_json::from_str::<Causation>(&json).unwrap(), chain);
    }

    // AC-6: a credential-shaped raw payload yields an envelope whose serialized form contains no
    // secret — at persist AND at any render/export projection (both are `serde` over the struct).
    #[test]
    fn scrub_payload_removes_credentials() {
        let raw = serde_json::json!({
            "note": "clean prose here",
            "password": "hunter2-opaque-secret",
            "gh": "ghp_abcdefghijklmnopqrstuvwxyz0123",
        });
        let scrubbed = scrub_payload(raw);
        let rendered = serde_json::to_string(&scrubbed).expect("scrubbed serializes");
        assert!(
            !rendered.contains("hunter2-opaque-secret"),
            "opaque secret masked: {rendered}"
        );
        assert!(
            !rendered.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
            "provider token masked: {rendered}"
        );
        assert!(rendered.contains("[redacted]"));
    }

    // AC-6: an over-cap payload collapses to a legible truncation marker (not a truncated, invalid
    // JSON slice) and carries no fragment of the original oversized content.
    #[test]
    fn scrub_payload_over_cap_collapses_to_marker() {
        let big = "x".repeat(PAYLOAD_MAX_BYTES + 1);
        let scrubbed = scrub_payload(serde_json::json!({ "blob": big }));
        assert_eq!(scrubbed["truncated"], serde_json::json!(true));
        assert!(scrubbed["original_bytes"].as_u64().unwrap() > PAYLOAD_MAX_BYTES as u64);
        assert!(scrubbed.get("blob").is_none(), "oversized content dropped");
    }
}
