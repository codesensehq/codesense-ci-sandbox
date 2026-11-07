// SPDX-License-Identifier: MIT
use super::*;
use crate::live_hub::{
    agent_list::{replace_status, AgentListRow},
    projection::TopicRegistry,
};
use serde_json::json;

#[test]
fn projected_overlay_seam_packets() {
    let parity: Value = serde_json::from_str(include_str!("parity.json")).unwrap();
    let references: Vec<AgentListRow> = (0..50).map(|i| serde_json::from_value(json!({
        "id":format!("row-{i}"),"sessionRef":format!("scale-{i:02}"),"inboxRef":"counts:team:o/a","teamKey":"o:a","parentId":null,"parentSource":null,"placementTeamRef":null,"order":i,"workspaceRef":null,"storyRef":null,"workspaceRank":0,"crewParentId":null,"crewOrder":null,"crewLead":false
    })).unwrap()).collect();
    let mut sessions: Vec<Value> = (0..50).map(|i| json!({"id":format!("scale-{i:02}"),"sessionId":format!("scale-{i:02}"),"handle":format!("scale-{i:02}"),"displayName":format!("Session {i}"),"team":{"org":"o","team":"a"},"alive":true,"effectiveStatus":if i==49 {"thinking"} else {"idle"}})).collect();
    let mut providers = FieldProviders::new([] as [&str; 0]);
    crate::agents::session_fields::register(&mut providers);
    register(&mut providers);
    let mut registry = TopicRegistry::default();
    registry
        .replace(
            "sessions",
            sessions.iter().cloned().map(|r| (None, r)).collect(),
            None,
        )
        .unwrap();
    replace_status(
        &mut registry,
        "all|status",
        &references,
        &json!({"ready":true}),
        &sessions,
        &Default::default(),
    )
    .unwrap();
    let initial = json!({"sessions":registry.snapshot("sessions","all").unwrap(),"order":registry.snapshot("agentList","all|status").unwrap()});
    let mut steps = Vec::new();
    for (name, index) in [
        ("managed outcome", 47),
        ("dismissed crash", 90),
        ("working subagents", 14),
        ("headless question", 69),
        ("stall deadline", 23),
    ] {
        let case = &parity["derive"][index];
        let mut source = case["args"][0].as_object().unwrap().clone();
        source.insert("id".into(), json!("scale-49"));
        source.insert("sessionId".into(), json!("scale-49"));
        source.insert("handle".into(), json!("scale-49"));
        source.insert("displayName".into(), json!("Session 49"));
        source.insert("team".into(), json!({"org":"o","team":"a"}));
        source.insert(
            "_managedOutcome".into(),
            case["args"].get(1).cloned().unwrap_or(Value::Null),
        );
        let mut signals = serde_json::to_value(LiveSignals::default()).unwrap();
        if let Some(live) = case["args"].get(2).and_then(Value::as_object) {
            signals.as_object_mut().unwrap().extend(live.clone());
        }
        if let Some(count) = signals["subagents"].as_u64().filter(|count| *count > 0) {
            source.insert("subagents".into(), count.into());
        }
        source.insert("_statusSignals".into(), signals);
        source.insert(
            "_pendingManagedAsk".into(),
            json!(name == "headless question"),
        );
        let mut row = providers.project(&source, source.clone());
        row.as_object_mut().unwrap().remove("_managedOutcome");
        row.as_object_mut().unwrap().remove("_statusSignals");
        row.as_object_mut().unwrap().remove("_pendingManagedAsk");
        assert_eq!(row["effectiveStatus"], case["expected"]);
        sessions[49] = row;
        let session_deltas = registry
            .replace(
                "sessions",
                sessions.iter().cloned().map(|r| (None, r)).collect(),
                None,
            )
            .unwrap();
        let order_deltas = replace_status(
            &mut registry,
            "all|status",
            &references,
            &json!({"ready":true}),
            &sessions,
            &Default::default(),
        )
        .unwrap();
        let bytes = order_deltas
            .iter()
            .map(|d| serde_json::to_vec(d).unwrap().len())
            .max()
            .unwrap_or(0);
        assert!(bytes < 2048, "status reorder: {bytes} bytes");
        let mut ordered = registry
            .snapshot("agentList", "all|status")
            .unwrap()
            .snapshot;
        ordered.sort_by(|a, b| {
            a["order"]
                .as_f64()
                .unwrap_or(0.0)
                .total_cmp(&b["order"].as_f64().unwrap_or(0.0))
        });
        let expected_order: Vec<_> = ordered
            .iter()
            .filter_map(|reference| {
                let id = reference["sessionRef"].as_str()?;
                Some(json!([
                    id,
                    sessions.iter().find(|s| s["id"] == id).unwrap()["effectiveStatus"]
                ]))
            })
            .collect();
        steps.push(json!({"name":name,"case":index,"expected":case["expected"],"expectedOrder":expected_order,"sessionDeltas":session_deltas,"orderDeltas":order_deltas,"reorderBytes":bytes}));
    }
    let packet = json!({"initial":initial,"steps":steps});
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/effective-status-seam.json");
    if std::env::var_os("UPDATE_EFFECTIVE_STATUS_SEAM").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&packet).unwrap() + "\n").unwrap();
    }
    let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        packet, saved,
        "refresh the Rust-produced seam packet before running the browser harness"
    );
}
