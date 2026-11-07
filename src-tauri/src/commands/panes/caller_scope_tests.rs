// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::{tests::Fixture, CallerScope};
use serde_json::json;

#[test]
fn private_install_checks_the_parent_asset_folder_before_copying_or_hashing() {
    let f = Fixture::new();
    let html = r#"<title>Shared</title><meta name="codesense:slots" content="right-pane"><meta name="codesense:surface" content="notes">"#;
    f.write("parent", "panes/parent.html", html);
    let profile = || {
        serde_json::from_value::<crate::panes::profile::Profile>(json!({"id":"default","name":"Default","posture":"","cliLayout":"default","slots":{"right-pane":[{"scope":"team","org":"acme/ctx","team":"parent","id":"parent"}]}})).unwrap()
    };
    f.write("parent", "panes/secret.js", "PRIVATE ASSET");
    let mut p = profile();
    let denied =
        install_profile_choices_for(&f.home, &Principal::from(1u64), &mut p, true, &f.scope)
            .unwrap_err();
    std::fs::remove_file(f.path("parent", "panes/parent.html")).unwrap();
    assert_eq!(
        denied,
        install_profile_choices_for(
            &f.home,
            &Principal::from(1u64),
            &mut profile(),
            true,
            &f.scope
        )
        .unwrap_err()
    );
    f.write("parent", "panes/parent.html", html);
    f.write("parent", "config/sharing.yml", "share:\n  - panes/**\n");
    let scope = CallerScope::Team(crate::context_repo::inheritance::team_view(
        &f.root, "acme/ctx", "child",
    ));
    let mut shared = profile();
    install_profile_choices_for(&f.home, &Principal::from(1u64), &mut shared, true, &scope)
        .unwrap();
    let crate::panes::profile::SlotChoice::Many(refs) = &shared.slots["right-pane"] else {
        panic!("pane refs")
    };
    assert_eq!(refs[0].scope, crate::panes::DashboardScope::Private);
}
