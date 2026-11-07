// SPDX-License-Identifier: MIT
//! The layout model's invariants and its client-agnostic surface, driven
//! directly (no PTYs): the host tests prove the same model against real
//! sessions.

use super::*;

/// A model with viewer `v` attached under boot `boot` (as a client attaches by
/// taking its first snapshot before it opens anything).
fn model() -> LayoutModel {
    let m = LayoutModel::new(ServerIdentity::ephemeral("test-host"));
    m.snapshot(&viewer("v"), &boot("boot")).unwrap();
    m
}

fn sid(id: &str) -> PtySessionId {
    PtySessionId::new(id)
}

fn ws(id: &str) -> WorkspaceKey {
    WorkspaceKey::new(id)
}

fn viewer(id: &str) -> ViewerId {
    ViewerId::new(id)
}

fn boot(id: &str) -> ClientBootId {
    ClientBootId::new(id)
}

fn descriptor(name: &str) -> WorkspaceDescriptor {
    WorkspaceDescriptor {
        display_name: name.to_string(),
        kind: "story".to_string(),
        org: Some("acme".to_string()),
        team: Some("platform".to_string()),
        story_id: Some("ST-1".to_string()),
        repo: Some("acme/app".to_string()),
        worktree_path: Some(PathBuf::from("/work/acme/app-ST-1")),
        branch: Some("feat/ST-1".to_string()),
    }
}

fn request(v: &str, workspace: &str, session: &str) -> OpenTab {
    OpenTab {
        viewer: viewer(v),
        workspace: ws(workspace),
        session: sid(session),
        descriptor: Some(descriptor(workspace)),
        name: None,
        role: None,
        tags: Tags::new(),
        placement: Placement::Tab,
        index: None,
        activate: true,
    }
}

/// Reserve then place, as `open_tab` does around its spawn.
fn open(m: &LayoutModel, v: &str, workspace: &str, session: &str) -> Effects {
    m.reserve(
        &sid(session),
        &ws(workspace),
        Some(&WorkspaceDescriptor::default()),
    )
    .unwrap();
    m.place_opened(request(v, workspace, session)).unwrap().1
}

/// `viewer` closes `session` without keeping it running.
fn close(m: &LayoutModel, v: &str, session: &str) -> Effects {
    m.close(&viewer(v), &sid(session), false).unwrap().1
}

fn snapshot(m: &LayoutModel, v: &str, b: &str) -> LayoutSnapshot {
    m.snapshot(&viewer(v), &boot(b)).unwrap().0
}

fn layout_of(m: &LayoutModel, workspace: &str) -> Option<WorkspaceLayout> {
    snapshot(m, "v", "boot")
        .workspaces
        .get(&ws(workspace))
        .cloned()
}

fn sessions_of(layout: &WorkspaceLayout) -> Vec<&str> {
    layout.tabs.iter().map(|t| t.session.as_str()).collect()
}

fn revs(effects: &Effects) -> Vec<u64> {
    effects
        .events
        .iter()
        .map(|e| match e {
            TerminalDomainEvent::LayoutChanged { rev, .. } => *rev,
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

// ---- Invariant 1: entries are live, or exited and not yet closed ----------

#[test]
fn an_exit_during_the_spawn_places_the_entry_exited_with_its_status() {
    let m = model();
    m.reserve(&sid("a"), &ws("w"), Some(&WorkspaceDescriptor::default()))
        .unwrap();
    m.on_exit(
        &sid("a"),
        Some(ExitInfo {
            exit_code: Some(7),
            signal: None,
        }),
    );
    let (entry, _) = m.place_opened(request("v", "w", "a")).unwrap();
    assert_eq!(
        entry.state,
        EntryState::Exited {
            exit_code: Some(7),
            signal: None
        }
    );
}

#[test]
fn a_session_removed_during_the_spawn_is_never_placed() {
    let m = model();
    m.reserve(&sid("a"), &ws("w"), Some(&WorkspaceDescriptor::default()))
        .unwrap();
    m.on_removed(&sid("a"));
    assert_eq!(
        m.place_opened(request("v", "w", "a")).unwrap_err(),
        LayoutError::SessionGone(sid("a"))
    );
    assert!(layout_of(&m, "w").is_none(), "no ghost entry");
}

/// A removal announces itself before the kill it causes, so the exit follows:
/// it must not bring the removed session back as an exited entry.
#[test]
fn an_exit_after_the_removal_during_the_spawn_does_not_place_it() {
    let m = model();
    m.reserve(&sid("a"), &ws("w"), Some(&WorkspaceDescriptor::default()))
        .unwrap();
    m.on_removed(&sid("a"));
    m.on_exit(&sid("a"), None);
    assert_eq!(
        m.place_opened(request("v", "w", "a")).unwrap_err(),
        LayoutError::SessionGone(sid("a"))
    );
    assert!(layout_of(&m, "w").is_none(), "no ghost entry");
}

#[test]
fn an_exited_entry_stays_until_it_is_removed() {
    let m = model();
    open(&m, "v", "w", "a");
    m.on_exit(&sid("a"), None);
    let layout = layout_of(&m, "w").unwrap();
    assert!(layout.tabs[0].state.is_exited());
    close(&m, "v", "a");
    assert!(layout_of(&m, "w").is_none());
}

/// A respawn holds its exited entry: a second hold is refused, a live entry
/// cannot be held, and a new process that exits before the respawn finishes
/// keeps that exit — its entry never turns live over it.
#[test]
fn respawn_quick_exit_keeps_the_exit_and_a_held_entry_refuses_a_second_hold() {
    let m = model();
    open(&m, "v", "w", "a");
    assert_eq!(
        m.reserve_respawn(&sid("a")).unwrap_err(),
        LayoutError::Running(sid("a")),
        "a live entry is not respawned"
    );
    m.on_exit(&sid("a"), None);
    m.reserve_respawn(&sid("a")).unwrap();
    assert_eq!(
        m.reserve_respawn(&sid("a")).unwrap_err(),
        LayoutError::Restarting(sid("a"))
    );
    // The new process ends before the finish.
    let quick = Some(ExitInfo {
        exit_code: Some(9),
        signal: None,
    });
    m.on_exit(&sid("a"), quick);
    let (entry, _) = m.finish_respawn(&sid("a")).unwrap();
    assert_eq!(
        entry.state,
        EntryState::Exited {
            exit_code: Some(9),
            signal: None
        }
    );
    // The hold ended: another respawn may begin, and one that finishes turns live.
    m.reserve_respawn(&sid("a")).unwrap();
    let (entry, _) = m.finish_respawn(&sid("a")).unwrap();
    assert_eq!(entry.state, EntryState::Live);
}

// ---- Invariant 2: a session is in at most one workspace --------------------

#[test]
fn a_session_cannot_be_opened_twice_or_while_opening() {
    let m = model();
    m.reserve(&sid("a"), &ws("w"), Some(&WorkspaceDescriptor::default()))
        .unwrap();
    assert_eq!(
        m.reserve(&sid("a"), &ws("w1"), Some(&WorkspaceDescriptor::default()))
            .unwrap_err(),
        LayoutError::AlreadyPlaced(sid("a"))
    );
    m.place_opened(request("v", "w1", "a")).unwrap();
    assert_eq!(
        m.reserve(&sid("a"), &ws("w1"), Some(&WorkspaceDescriptor::default()))
            .unwrap_err(),
        LayoutError::AlreadyPlaced(sid("a"))
    );
}

#[test]
fn placing_into_another_workspace_moves_the_entry() {
    let m = model();
    open(&m, "v", "w1", "a");
    open(&m, "v", "w1", "b");
    m.place(
        &sid("a"),
        &ws("w2"),
        Placement::Tab,
        None,
        Some(descriptor("w2")),
    )
    .unwrap();
    let snap = snapshot(&m, "v", "boot");
    assert_eq!(sessions_of(&snap.workspaces[&ws("w1")]), vec!["b"]);
    assert_eq!(sessions_of(&snap.workspaces[&ws("w2")]), vec!["a"]);
    // An unknown target without a descriptor is rejected, not invented.
    assert_eq!(
        m.place(&sid("a"), &ws("nowhere"), Placement::Tab, None, None)
            .unwrap_err(),
        LayoutError::UnknownWorkspace(ws("nowhere"))
    );
}

// ---- Invariant 3: the grid panes tab entries; focus names an entry ---------

#[test]
fn the_grid_only_panes_tab_entries_once() {
    let m = model();
    open(&m, "v", "w", "a");
    open(&m, "v", "w", "b");
    let grid = GridState {
        paned: vec![sid("a"), sid("b")],
        preset: GridPreset::Grid,
        collapsed: false,
    };
    m.set_grid(&ws("w"), grid.clone()).unwrap();
    assert_eq!(layout_of(&m, "w").unwrap().grid, grid);

    for bad in [vec![sid("a"), sid("a")], vec![sid("ghost")]] {
        let err = m
            .set_grid(
                &ws("w"),
                GridState {
                    paned: bad,
                    ..GridState::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, LayoutError::InvalidGrid(_)));
    }

    // Backgrounding or removing a paned entry drops it from the grid.
    m.place(&sid("a"), &ws("w"), Placement::Background, None, None)
        .unwrap();
    assert_eq!(layout_of(&m, "w").unwrap().grid.paned, vec![sid("b")]);
    close(&m, "v", "b");
    assert!(layout_of(&m, "w").unwrap().grid.paned.is_empty());
}

#[test]
fn focus_moves_to_the_nearest_tab_when_its_entry_leaves() {
    let m = model();
    for s in ["a", "b", "c"] {
        open(&m, "v", "w", s);
    }
    m.set_active(&viewer("v"), &ws("w"), Some(&sid("b")))
        .unwrap();
    close(&m, "v", "b");
    let snap = snapshot(&m, "v", "boot");
    assert_eq!(snap.workspaces[&ws("w")].last_active, Some(sid("c")));
    assert_eq!(snap.viewer.active[&ws("w")], sid("c"));
    // A background entry is never the focus.
    m.place(&sid("c"), &ws("w"), Placement::Background, None, None)
        .unwrap();
    assert_eq!(
        m.set_active(&viewer("v"), &ws("w"), Some(&sid("c")))
            .unwrap_err(),
        LayoutError::UnknownSession(sid("c"))
    );
    assert_eq!(
        snapshot(&m, "v", "boot").workspaces[&ws("w")].last_active,
        Some(sid("a"))
    );
}

// ---- Invariant 4: an empty workspace is absent -----------------------------

#[test]
fn a_workspace_disappears_with_its_last_entry() {
    let m = model();
    open(&m, "v", "w", "a");
    let effects = m.on_removed(&sid("a"));
    assert!(matches!(
        &effects.events[..],
        [TerminalDomainEvent::LayoutChanged { layout: None, .. }]
    ));
    assert!(layout_of(&m, "w").is_none());
}

#[test]
fn closing_a_workspace_kills_every_entry() {
    let m = model();
    open(&m, "v", "w", "a");
    open(&m, "v", "w", "b");
    let effects = m.close_workspace(&ws("w")).unwrap();
    assert_eq!(effects.kill, vec![sid("a"), sid("b")]);
    assert!(layout_of(&m, "w").is_none());
    assert_eq!(
        m.close_workspace(&ws("w")).unwrap_err(),
        LayoutError::UnknownWorkspace(ws("w"))
    );
}

// ---- Invariant 5: rev rises on every mutation; epoch is per host ----------

#[test]
fn every_mutation_announces_a_higher_rev() {
    let m = model();
    let mut seen = Vec::new();
    seen.extend(revs(&open(&m, "v", "w", "a")));
    seen.extend(revs(&open(&m, "v", "w", "b")));
    seen.extend(revs(&m.rename(&sid("a"), "build".to_string()).unwrap()));
    seen.extend(revs(&m.move_to(&sid("a"), 5).unwrap()));
    seen.extend(revs(
        &m.set_active(&viewer("v"), &ws("w"), Some(&sid("b")))
            .unwrap(),
    ));
    seen.extend(revs(
        &m.set_tags(&sid("a"), TagPatch::from([("k".into(), Some("v".into()))]))
            .unwrap(),
    ));
    seen.extend(revs(
        &m.set_descriptor(&ws("w"), descriptor("renamed")).unwrap(),
    ));
    seen.extend(revs(&m.on_exit(&sid("a"), None)));
    assert_eq!(seen, (1..=8).collect::<Vec<_>>());
    let snap = snapshot(&m, "v", "boot");
    assert_eq!(snap.rev, 8);
    assert_eq!(sessions_of(&snap.workspaces[&ws("w")]), vec!["b", "a"]);

    let epoch = snap.epoch.clone();
    assert_eq!(snapshot(&m, "v", "boot").epoch, epoch, "fixed per host");
    assert_ne!(snapshot(&model(), "v", "boot").epoch, epoch, "new per host");
}

#[test]
fn a_moved_entry_announces_both_workspaces() {
    let m = model();
    open(&m, "v", "w1", "a");
    open(&m, "v", "w1", "b");
    let effects = m
        .place(
            &sid("a"),
            &ws("w2"),
            Placement::Tab,
            Some(0),
            Some(descriptor("w2")),
        )
        .unwrap();
    let touched: Vec<&str> = effects
        .events
        .iter()
        .map(|e| match e {
            TerminalDomainEvent::LayoutChanged { workspace, .. } => workspace.as_str(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(touched, vec!["w1", "w2"]);
    assert_eq!(revs(&effects), vec![3, 4]);
}

// ---- Names, order, tags ----------------------------------------------------

#[test]
fn names_default_to_a_memorable_label_and_index_places_in_the_strip() {
    use crate::memorable_name::memorable_name_from_str;

    let m = model();
    open(&m, "v", "w", "a");
    m.reserve(&sid("b"), &ws("w"), Some(&WorkspaceDescriptor::default()))
        .unwrap();
    m.place_opened(OpenTab {
        index: Some(0),
        name: Some("agent".to_string()),
        ..request("v", "w", "b")
    })
    .unwrap();
    open(&m, "v", "w", "c");
    let layout = layout_of(&m, "w").unwrap();
    assert_eq!(sessions_of(&layout), vec!["b", "a", "c"]);
    let names: Vec<&str> = layout.tabs.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "agent".to_string(),
            memorable_name_from_str("a"),
            memorable_name_from_str("c"),
        ],
    );
    assert_eq!(layout.next_ordinal, 4);
}

#[test]
fn a_tag_patch_sets_and_removes() {
    let m = model();
    open(&m, "v", "w", "a");
    m.set_tags(
        &sid("a"),
        TagPatch::from([
            ("mcpHandle".into(), Some("h1".into())),
            ("launcher".into(), Some("mcp".into())),
        ]),
    )
    .unwrap();
    m.set_tags(&sid("a"), TagPatch::from([("launcher".into(), None)]))
        .unwrap();
    assert_eq!(
        layout_of(&m, "w").unwrap().tabs[0].tags,
        Tags::from([("mcpHandle".into(), "h1".into())])
    );
}

// ---- Reopen pruning --------------------------------------------------

#[test]
fn a_relaunch_prunes_exited_entries_and_a_reload_keeps_them() {
    let m = model();
    open(&m, "v", "w", "live");
    open(&m, "v", "w", "dead");
    m.on_exit(&sid("dead"), Some(ExitInfo::default()));
    let rev_before = snapshot(&m, "v", "boot").rev;

    // A webview reload: same boot id — the exited tab stays, nothing is killed.
    let (reload, effects) = m.snapshot(&viewer("v"), &boot("boot")).unwrap();
    assert!(effects.kill.is_empty() && effects.events.is_empty());
    assert_eq!(reload.rev, rev_before);
    assert!(reload.viewer.hidden.is_empty());
    assert_eq!(
        sessions_of(&reload.workspaces[&ws("w")]),
        vec!["live", "dead"]
    );

    // A relaunch: a new boot id — the only viewer no longer shows the exited
    // entry, so it is pruned (and killed to free its scrollback) before the
    // answer, with a rev bump.
    let (relaunch, effects) = m.snapshot(&viewer("v"), &boot("boot-2")).unwrap();
    assert_eq!(effects.kill, vec![sid("dead")]);
    assert_eq!(revs(&effects), vec![rev_before + 1]);
    assert_eq!(relaunch.rev, rev_before + 1);
    assert_eq!(sessions_of(&relaunch.workspaces[&ws("w")]), vec!["live"]);
    assert!(
        relaunch.viewer.hidden.is_empty(),
        "a freed entry is forgotten"
    );
}

#[test]
fn one_viewers_relaunch_never_hides_another_viewers_exited_tab() {
    let m = model();
    m.snapshot(&viewer("desk-a"), &boot("a-1")).unwrap();
    m.snapshot(&viewer("desk-b"), &boot("b-1")).unwrap();
    open(&m, "desk-a", "w", "live");
    open(&m, "desk-a", "w", "dead");
    m.on_exit(&sid("dead"), None);
    // `v` (attached by the fixture) closes it: hidden for `v` only.
    let effects = close(&m, "v", "dead");
    assert!(effects.kill.is_empty());

    // B relaunches: the exited tab is hidden for B only, not freed.
    let (b, effects) = m.snapshot(&viewer("desk-b"), &boot("b-2")).unwrap();
    assert!(effects.kill.is_empty() && effects.events.is_empty());
    assert!(b.viewer.hidden.contains(&sid("dead")));

    // A (a webview reload) still shows it, exited, with its scrollback.
    let a = snapshot(&m, "desk-a", "a-1");
    assert!(!a.viewer.hidden.contains(&sid("dead")));
    assert!(a.workspaces[&ws("w")].tabs[1].state.is_exited());

    // Once A prunes it too (a relaunch), no viewer shows it: it is freed.
    let (a, effects) = m.snapshot(&viewer("desk-a"), &boot("a-2")).unwrap();
    assert_eq!(effects.kill, vec![sid("dead")]);
    assert_eq!(sessions_of(&a.workspaces[&ws("w")]), vec!["live"]);
}

#[test]
fn an_exited_entry_is_freed_after_every_viewer_closed_or_pruned_it() {
    let m = model();
    m.snapshot(&viewer("desk-b"), &boot("b-1")).unwrap();
    open(&m, "v", "w", "live");
    open(&m, "v", "w", "dead");
    m.on_exit(&sid("dead"), None);
    // B relaunches first; then `v` closes it — the last viewer showing it.
    m.snapshot(&viewer("desk-b"), &boot("b-2")).unwrap();
    let effects = close(&m, "v", "dead");
    assert_eq!(effects.kill, vec![sid("dead")]);
    assert_eq!(sessions_of(&layout_of(&m, "w").unwrap()), vec!["live"]);
}

// ---- Client-agnostic model -------------------------------------------

#[test]
fn the_server_id_is_persisted_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let first = ServerIdentity::load_or_create(dir.path(), "mac-mini").unwrap();
    let restarted = ServerIdentity::load_or_create(dir.path(), "mac-mini").unwrap();
    assert_eq!(first.id, restarted.id);
    assert!(!first.id.as_str().is_empty());

    let host_a = LayoutModel::new(first);
    let host_b = LayoutModel::new(restarted);
    let (a, b) = (snapshot(&host_a, "v", "x"), snapshot(&host_b, "v", "x"));
    assert_eq!(a.server_id, b.server_id, "stable across a host restart");
    assert_eq!(a.server_name, "mac-mini");
    assert_ne!(a.epoch, b.epoch, "the epoch is per host lifetime");

    let elsewhere = tempfile::tempdir().unwrap();
    assert_ne!(
        ServerIdentity::load_or_create(elsewhere.path(), "other")
            .unwrap()
            .id,
        a.server_id,
        "another instance root is another server"
    );
}

#[test]
fn the_descriptor_round_trips_unchanged() {
    let m = model();
    open(&m, "creator", "w", "a");
    let snap = snapshot(&m, "creator", "boot");
    assert_eq!(snap.workspaces[&ws("w")].descriptor, descriptor("w"));

    let wire = serde_json::to_string(&snap).unwrap();
    let back: LayoutSnapshot = serde_json::from_str(&wire).unwrap();
    assert_eq!(back, snap, "the whole snapshot survives the wire");
    let json: serde_json::Value = serde_json::from_str(&wire).unwrap();
    assert_eq!(json["schema"], LAYOUT_SCHEMA);
    assert_eq!(
        json["workspaces"]["w"]["descriptor"]["worktreePath"],
        "/work/acme/app-ST-1"
    );
    assert_eq!(json["workspaces"]["w"]["tabs"][0]["state"]["kind"], "live");
}

#[test]
fn a_client_that_never_created_a_workspace_gets_all_it_needs() {
    let m = model();
    open(&m, "creator", "w", "a");
    open(&m, "creator", "w", "b");
    m.set_grid(
        &ws("w"),
        GridState {
            paned: vec![sid("a"), sid("b")],
            preset: GridPreset::Rows,
            collapsed: false,
        },
    )
    .unwrap();

    let stranger = snapshot(&m, "stranger", "stranger-boot");
    assert_eq!(stranger.server_name, "test-host");
    let layout = &stranger.workspaces[&ws("w")];
    assert_eq!(layout.descriptor, descriptor("w"));
    assert_eq!(sessions_of(layout), vec!["a", "b"]);
    assert_eq!(layout.grid.preset, GridPreset::Rows);
    assert!(layout.tabs.iter().all(|t| !t.name.is_empty()));
    assert_eq!(
        stranger.viewer.active[&ws("w")],
        sid("b"),
        "seeded from the workspace's last active tab"
    );
}

#[test]
fn focus_and_scroll_are_per_viewer_and_last_active_only_seeds_a_fresh_attach() {
    let m = model();
    open(&m, "one", "w", "a");
    open(&m, "one", "w", "b");
    // Viewer two attaches fresh: seeded from last_active (b).
    assert_eq!(
        snapshot(&m, "two", "boot").viewer.active[&ws("w")],
        sid("b")
    );

    m.set_active(&viewer("one"), &ws("w"), Some(&sid("a")))
        .unwrap();
    m.set_view_hint(
        &viewer("one"),
        &sid("a"),
        ViewHint {
            scroll_from_bottom: 40,
        },
    )
    .unwrap();
    m.set_view_hint(
        &viewer("two"),
        &sid("a"),
        ViewHint {
            scroll_from_bottom: 0,
        },
    )
    .unwrap();

    let (one, two) = (snapshot(&m, "one", "boot"), snapshot(&m, "two", "boot"));
    assert_eq!(one.viewer.active[&ws("w")], sid("a"));
    assert_eq!(
        two.viewer.active[&ws("w")],
        sid("b"),
        "another viewer's focus change never forces this one's"
    );
    assert_eq!(one.viewer.views[&sid("a")].scroll_from_bottom, 40);
    assert_eq!(two.viewer.views[&sid("a")].scroll_from_bottom, 0);

    // A third viewer attaching now is seeded from the new default.
    assert_eq!(
        snapshot(&m, "three", "boot").viewer.active[&ws("w")],
        sid("a")
    );
    assert!(m
        .set_view_hint(&viewer("one"), &sid("ghost"), ViewHint::default())
        .is_err());
}

#[test]
fn the_pty_size_follows_the_most_recently_active_viewer() {
    let m = model();
    open(&m, "laptop", "w", "a");
    // The laptop is the only sizer: its request applies.
    assert_eq!(
        m.request_size(&viewer("laptop"), &sid("a"), 100, 30),
        (100, 30)
    );

    // A desktop attaches and focuses — it becomes the most recently active.
    m.set_active(&viewer("desktop"), &ws("w"), Some(&sid("a")))
        .unwrap();
    assert_eq!(
        m.request_size(&viewer("desktop"), &sid("a"), 200, 60),
        (200, 60)
    );
    // The laptop resizing later does not steal the size: it is not the most
    // recently active viewer, so the PTY keeps the desktop's size.
    assert_eq!(
        m.request_size(&viewer("laptop"), &sid("a"), 90, 20),
        (200, 60)
    );

    // The laptop becomes active again: the PTY is resized to its size.
    let effects = m
        .set_active(&viewer("laptop"), &ws("w"), Some(&sid("a")))
        .unwrap();
    assert_eq!(effects.resize, vec![(sid("a"), 90, 20)]);
    // Re-activating the already most recent viewer resizes nothing.
    let effects = m
        .set_active(&viewer("laptop"), &ws("w"), Some(&sid("a")))
        .unwrap();
    assert!(effects.resize.is_empty());

    // A removed session is forgotten by every viewer.
    m.on_removed(&sid("a"));
    assert_eq!(
        m.request_size(&viewer("desktop"), &sid("a"), 10, 5),
        (10, 5)
    );
}

#[test]
fn only_tabs_and_opening_sessions_hold_their_scrollback() {
    let m = model();
    m.reserve(
        &sid("opening"),
        &ws("w"),
        Some(&WorkspaceDescriptor::default()),
    )
    .unwrap();
    assert!(m.is_held("opening"));
    open(&m, "v", "w", "tab");
    open(&m, "v", "w", "headless");
    m.place(
        &sid("headless"),
        &ws("w"),
        Placement::Background,
        None,
        None,
    )
    .unwrap();
    assert!(m.is_held("tab"));
    assert!(
        !m.is_held("headless"),
        "a finished background run is reclaimable"
    );
    assert!(!m.is_held("unknown"));
}

#[test]
fn an_open_into_an_unknown_workspace_needs_a_descriptor_before_it_spawns() {
    let m = model();
    assert_eq!(
        m.reserve(&sid("a"), &ws("absent"), None).unwrap_err(),
        LayoutError::UnknownWorkspace(ws("absent"))
    );
    open(&m, "v", "w", "a");
    // An existing workspace keeps its descriptor when none is sent.
    m.reserve(&sid("b"), &ws("w"), None).unwrap();
    m.place_opened(OpenTab {
        descriptor: None,
        ..request("v", "w", "b")
    })
    .unwrap();
    assert_eq!(layout_of(&m, "w").unwrap().descriptor, descriptor("w"));
}

#[test]
fn cleanup_reservations_are_visible_until_placement_or_release() {
    let m = model();
    let source = tempfile::tempdir().unwrap();
    let mut open = request("v", "generic:source", "opening");
    open.descriptor.as_mut().unwrap().worktree_path = Some(source.path().to_path_buf());
    m.reserve(&open.session, &open.workspace, open.descriptor.as_ref())
        .unwrap();
    assert_eq!(
        m.worktree_usage(source.path()).unwrap().opening,
        vec![sid("opening")]
    );
    m.release(&open.session);
    assert!(m.worktree_usage(source.path()).unwrap().opening.is_empty());
}

#[test]
fn canonical_source_usage_matches_generic_paths_and_openings_without_labels() {
    let m = model();
    let source = tempfile::tempdir().unwrap();
    let nested = source.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let alias = source.path().join("alias");
    std::os::unix::fs::symlink(&nested, &alias).unwrap();
    let mut generic = request("v", "generic:unrelated-label", "agent");
    generic.descriptor.as_mut().unwrap().worktree_path = Some(alias.clone());
    m.reserve(
        &generic.session,
        &generic.workspace,
        generic.descriptor.as_ref(),
    )
    .unwrap();
    assert_eq!(
        m.worktree_usage(source.path()).unwrap().opening,
        vec![sid("agent")]
    );
    m.place_opened(generic).unwrap();
    let usage = m.worktree_usage(source.path()).unwrap();
    assert_eq!(usage.entries[0].1.session, sid("agent"));
    assert!(usage.opening.is_empty());
    let unrelated = tempfile::tempdir().unwrap();
    assert!(m
        .worktree_usage(unrelated.path())
        .unwrap()
        .entries
        .is_empty());
}
