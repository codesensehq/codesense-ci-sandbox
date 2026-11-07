// SPDX-License-Identifier: MIT
#![cfg(test)]
use super::*;

#[test]
fn selected_launch_cwd_preview_warns_before_preparation_or_spawn() {
    let _guard = crate::routines::test_support::TempHome::new();
    let home = tempfile::tempdir().unwrap();
    team_content(home.path(), &alice());
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join(".hermes.md"), "project rules").unwrap();
    let agent = Agent {
        revision: 1,
        startup: vec![],
        context: Some(vec![]),
        ..draft("registered-preview", AgentCli::Hermes)
    };
    // Direct registered-adapter construction bypasses only the saved availability gate.
    let resolved = preview_agent(
        home.path(),
        &alice(),
        &agent,
        &Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        ResolveOptions::default(),
    )
    .unwrap();
    assert!(!resolved
        .warnings
        .iter()
        .any(|warning| warning.code == LaunchWarningCode::ShadowingRootDocument));
    let selected = resolved.at_launch_cwd(Some(cwd.path()));
    assert!(
        selected.warnings.iter().any(|warning| warning.code
            == LaunchWarningCode::ShadowingRootDocument
            && warning.message.contains(".hermes.md")),
        "selected preview must warn before any launch preparation"
    );
}
