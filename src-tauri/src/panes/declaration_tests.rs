// SPDX-License-Identifier: MIT
use super::*;
use serde_json::json;

fn discover(html: &str, yaml: Option<&str>) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("board.html"), html).unwrap();
    if let Some(yaml) = yaml {
        std::fs::write(dir.path().join(manifest::MANIFEST_FILE), yaml).unwrap();
    }
    serde_json::to_value(list_layer(
        dir.path(),
        PaneScope::Team,
        "acme/repo",
        Some("dev"),
    ))
    .unwrap()[0]
        .clone()
}

const META: &str = r#"<meta name="codesense:slots" content="right-pane,dock"><meta name="codesense:surface" content="planning"><meta name="codesense:capabilities" content="sessions:read context:read">"#;
const YAML: &str = "version: 1\npanes:\n  - id: board\n    html: board.html\n    slots: [right-pane, dock]\n    surface: planning\n    capabilities: [sessions:read, context:read]\n";

#[test]
fn private_install_policy_is_surface_specific_and_never_applies_to_dock() {
    for (surface, capability, expected) in [
        (None, "sessions:read", false),
        (Some("human-inbox"), "sessions:read", true),
        (Some("notes"), "sessions:read", true),
        (None, "records:read:own", true),
        (None, "inbox:read:all-teams", true),
        (None, "accounts:manage", false),
    ] {
        let declaration = DashboardDeclaration {
            surface: surface.map(str::to_owned),
            capabilities: vec![capability.into()],
            ..DashboardDeclaration::default()
        };
        assert_eq!(declaration.private_reach("right-pane"), expected);
        assert!(!declaration.private_reach("dock"));
    }
}

#[test]
fn declaration_meta_and_manifest_share_the_runtime_model() {
    let meta = discover(META, None);
    let yaml = discover("", Some(YAML));
    assert_eq!(
        meta["declaration"],
        json!({"slots":["right-pane","dock"],"surface":"planning","capabilities":["sessions:read","context:read"]})
    );
    assert_eq!(meta["declaration"], yaml["declaration"]);
    assert_eq!(meta["status"], "ready");
    assert_eq!(
        meta["dashboardRef"],
        json!({"scope":"team","org":"acme/repo","team":"dev","id":"board"})
    );
}

#[test]
fn declaration_rejections_remain_listed_with_plain_reasons() {
    for (html, yaml, reason) in [
        ("", Some("not: valid: yaml"), "invalid manifest"),
        (
            r#"<meta name="codesense:slots" content="unknown">"#,
            None,
            "unknown slot \"unknown\"",
        ),
        (
            r#"<meta name="codesense:capabilities" content="inbox:nuke">"#,
            None,
            "unknown capability \"inbox:nuke\"",
        ),
        (
            META,
            Some("version: 1\npanes:\n  - id: board\n    html: board.html\n    slots: [dock]\n"),
            "manifest/meta mismatch",
        ),
    ] {
        let entry = discover(html, yaml);
        assert_eq!(entry["status"], "rejected", "{entry}");
        assert!(
            entry["rejectedReason"].as_str().unwrap().contains(reason),
            "{entry}"
        );
    }
}

#[test]
fn declaration_manifest_wins_even_when_rejected_for_a_mismatch() {
    let entry = discover(
        META,
        Some("version: 1\npanes:\n  - id: board\n    html: board.html\n    slots: [dock]\n"),
    );
    assert_eq!(entry["declaration"]["slots"], json!(["dock"]));
}

const COMMANDS_META: &str = r#"<meta name="codesense:slots" content="right-pane"><meta name="codesense:commands" content='[{"id":"archive","label":"Archive thread","icon":"archive","defaultChord":"CmdOrCtrl+Shift+E","scope":"slot-visible"},{"id":"hide","label":"Hide","defaultChord":"CmdOrCtrl+H","scope":"global","humanOnly":true}]'>"#;
const COMMANDS_YAML: &str = "version: 1\npanes:\n  - id: board\n    html: board.html\n    slots: [right-pane]\n    commands:\n      - {id: archive, label: Archive thread, icon: archive, defaultChord: CmdOrCtrl+Shift+E, scope: slot-visible}\n      - {id: hide, label: Hide, defaultChord: CmdOrCtrl+H, scope: global, humanOnly: true}\n";

#[test]
fn commands_parse_from_meta_and_manifest_into_one_model() {
    let meta = discover(COMMANDS_META, None);
    let yaml = discover("", Some(COMMANDS_YAML));
    assert_eq!(meta["status"], "ready", "{meta}");
    assert_eq!(
        meta["declaration"]["commands"],
        json!([
            {"id":"archive","label":"Archive thread","icon":"archive","defaultChord":"CmdOrCtrl+Shift+E","scope":"slot-visible"},
            {"id":"hide","label":"Hide","defaultChord":"CmdOrCtrl+H","scope":"global","humanOnly":true}
        ])
    );
    assert_eq!(meta["declaration"], yaml["declaration"]);
}

#[test]
fn commands_meta_accepts_entity_encoded_json() {
    let html = r#"<meta name="codesense:commands" content="[{&quot;id&quot;:&quot;next&quot;,&quot;label&quot;:&quot;Next&quot;,&quot;scope&quot;:&quot;slot-focused&quot;}]">"#;
    let entry = discover(html, None);
    assert_eq!(entry["status"], "ready", "{entry}");
    assert_eq!(entry["declaration"]["commands"][0]["scope"], "slot-focused");
}

#[test]
fn invalid_commands_reject_the_dashboard_with_a_plain_reason() {
    let command = |body: &str| format!(r#"<meta name="codesense:commands" content='[{body}]'>"#);
    for (html, reason) in [
        (
            r#"<meta name="codesense:commands" content='not json'>"#.to_owned(),
            "invalid codesense:commands",
        ),
        (
            command(r#"{"id":"Archive","label":"A","scope":"global"}"#),
            "invalid command id",
        ),
        (
            command(r#"{"id":"a","label":" ","scope":"global"}"#),
            "has no label",
        ),
        (
            command(r#"{"id":"a","label":"A","scope":"anywhere"}"#),
            "invalid codesense:commands",
        ),
        (
            command(r#"{"id":"a","label":"A","scope":"global","defaultChord":"Shift+J"}"#),
            "needs CmdOrCtrl or Alt",
        ),
        (
            command(r#"{"id":"a","label":"A","scope":"global","defaultChord":"CmdOrCtrl+J+K"}"#),
            "more than one key",
        ),
        (
            command(
                r#"{"id":"a","label":"A","scope":"global"},{"id":"a","label":"B","scope":"global"}"#,
            ),
            "duplicate command id",
        ),
    ] {
        let entry = discover(&html, None);
        assert_eq!(entry["status"], "rejected", "{entry}");
        assert!(
            entry["rejectedReason"].as_str().unwrap().contains(reason),
            "{reason}: {entry}"
        );
    }
}

#[test]
fn reserved_and_forwardable_default_chords_are_accepted() {
    for chord in [
        "CmdOrCtrl+Q",
        "CmdOrCtrl+Space",
        "Alt+J",
        "F5",
        "Escape",
        "Shift+F12",
    ] {
        let html = format!(
            r#"<meta name="codesense:commands" content='[{{"id":"a","label":"A","scope":"global","defaultChord":"{chord}"}}]'>"#
        );
        assert_eq!(discover(&html, None)["status"], "ready", "{chord}");
    }
}

#[test]
fn manifest_and_meta_commands_must_agree() {
    let entry = discover(
        COMMANDS_META,
        Some("version: 1\npanes:\n  - id: board\n    html: board.html\n    slots: [right-pane]\n"),
    );
    assert_eq!(entry["rejectedReason"], "manifest/meta mismatch");
}

#[test]
fn live_badge_is_static_opt_in_and_travels_with_the_pane_declaration() {
    let html = format!(r#"{META}<meta name="codesense:badge" content="live">"#);
    let entry = discover(&html, None);
    assert_eq!(entry["status"], "ready");
    assert_eq!(entry["declaration"]["badge"], "live");
    assert!(discover(META, None)["declaration"]["badge"].is_null());
    assert!(
        discover(r#"<meta name="codesense:badge" content="other">"#, None)["declaration"]["badge"]
            .is_null()
    );
    assert_eq!(
        entry["declaration"],
        discover("", Some(&format!("{YAML}    badge: live\n")))["declaration"]
    );
}
