// SPDX-License-Identifier: MIT
use super::*;
use crate::context_repo::caller_scope::tests::Fixture;

#[test]
fn non_team_frame_live_catalog_hides_parent_updates_while_native_catalog_keeps_them() {
    let f = Fixture::new();
    let parent = f.agent("parent", "private-parent");
    let child = f.agent("child", "child-reader");
    let caller = Caller {
        home: f.home.clone(),
        principal: Principal::from(1u64),
    };
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (page, _) = registry
        .open(
            &mut topics,
            &caller,
            "frame",
            Scope::PageCaller("acme/ctx".into(), "child".into()),
            None,
            true,
            1,
        )
        .unwrap();
    let before = serde_json::to_string(&page.snapshot).unwrap();
    assert!(before.contains(&child.to_string()));
    assert!(!before.contains(&parent.to_string()));
    let mut definition = service::read_current(&f.home, &caller.principal, &parent).unwrap();
    let revision = definition.revision;
    definition.name = "HIDDEN TITLE SENTINEL".into();
    service::save(
        &f.home,
        &caller.principal,
        service::SaveTarget::Update {
            key: parent.clone(),
            base_revision: revision,
        },
        definition,
        &crate::routines::Author::user("1"),
    )
    .unwrap();
    registry.note(&parent.to_string(), false);
    registry.refresh().unwrap();
    registry.project(&mut topics).unwrap();
    let after = topics.snapshot(TOPIC, &page.view_key).unwrap();
    assert_eq!(page.snapshot, after.snapshot);
    let (native, _) = registry
        .open(&mut topics, &caller, "native", Scope::Native, None, true, 1)
        .unwrap();
    assert!(serde_json::to_string(&native.snapshot)
        .unwrap()
        .contains("HIDDEN TITLE SENTINEL"));
    std::fs::remove_file(f.path("parent", "config/agents/private-parent/agent.yaml")).unwrap();
    registry.note(&parent.to_string(), true);
    registry.refresh().unwrap();
    registry.project(&mut topics).unwrap();
    assert_eq!(
        after.snapshot,
        topics.snapshot(TOPIC, &page.view_key).unwrap().snapshot
    );
}

#[test]
fn a_page_frames_definition_rows_carry_the_agents_chosen_face() {
    let f = Fixture::new();
    let faced = f.agent("child", "faced");
    let plain = f.agent("child", "plain");
    let caller = Caller {
        home: f.home.clone(),
        principal: Principal::from(1u64),
    };
    let mut definition = service::read_current(&f.home, &caller.principal, &faced).unwrap();
    let revision = definition.revision;
    definition.avatar = Some("builtin-funny:coffee-bot".into());
    service::save(
        &f.home,
        &caller.principal,
        service::SaveTarget::Update {
            key: faced.clone(),
            base_revision: revision,
        },
        definition,
        &crate::routines::Author::user("1"),
    )
    .unwrap();
    let mut registry = Registry::default();
    let mut topics = TopicRegistry::default();
    let (page, _) = registry
        .open(
            &mut topics,
            &caller,
            "frame",
            Scope::PageCaller("acme/ctx".into(), "child".into()),
            None,
            true,
            1,
        )
        .unwrap();
    let row_of = |key: &AgentKey| {
        let wanted = format!("definition:{key}");
        page.snapshot
            .iter()
            .find(|r| r["id"] == wanted)
            .map(|r| r["row"].clone())
            .unwrap_or_else(|| panic!("no definition row for {key}"))
    };
    assert_eq!(row_of(&faced)["avatar"], "builtin-funny:coffee-bot");
    assert!(row_of(&plain)["avatar"].is_null());
}
