// SPDX-License-Identifier: MIT
//! The explicit operations' registry — ids, idempotent replay,
//! cancel and status — and the selected forms' mapping.
use super::*;

fn principal() -> Principal {
    Principal::from(7u64)
}

/// An operation id names one run: while it runs a second call answers
/// `running`; once it finished the kept result comes back instead of a
/// second run, also after the in-memory registry is gone (it is on disk).
#[test]
fn an_operation_id_runs_once_and_its_result_is_kept() {
    let home = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let target = json!({"kind":"local","codeRepoPath":"/w"});
    let Begin::Started(cancel_flag) =
        begin(home.path(), &principal(), &id, "hydrate", target.clone())
    else {
        panic!("a new id starts");
    };
    assert!(matches!(
        begin(home.path(), &principal(), &id, "hydrate", target.clone()),
        Begin::Running
    ));
    assert_eq!(
        status(home.path(), &principal(), &id).unwrap().state,
        "running"
    );
    // Cancel reaches the running operation's flag.
    assert!(cancel(&id));
    assert!(cancel_flag.load(Ordering::SeqCst));
    finish(
        home.path(),
        &principal(),
        &id,
        "cancelled",
        &json!({"operationId": id, "status": "cancelled"}),
    );
    assert!(!cancel(&id), "a finished operation is not running");
    let Begin::Done(done) = begin(home.path(), &principal(), &id, "hydrate", target) else {
        panic!("a finished id replays its result");
    };
    assert_eq!(done.state, "cancelled");
    assert_eq!(done.result.unwrap()["status"], "cancelled");
    let kept = stored(home.path(), &principal(), &id).unwrap();
    assert_eq!(kept.state, "cancelled");
    assert!(kept.finished_at_ms.is_some());
    let listed = recent(home.path(), &principal(), 10);
    assert!(listed.iter().any(|record| record.operation_id == id));
}

#[test]
fn selected_refs_and_hashes_map_onto_paths() {
    assert_eq!(doc_path("doc:docs/a.md").unwrap(), "docs/a.md");
    assert!(doc_path("docs/a.md").is_err());
    assert!(doc_path("doc:").is_err());
    let hash = blake3::hash(b"x");
    assert_eq!(
        parse_hash(&format!("blake3:{}", hash.to_hex())).unwrap(),
        *hash.as_bytes()
    );
    assert!(parse_hash("blake3:zz").is_err());
}

/// An operationId names one operation — reusing it for another
/// kind or target is refused, never answered with the other's result.
#[test]
fn an_operation_id_is_bound_to_its_kind_and_target() {
    let home = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let target = json!({"kind":"local","codeRepoPath":"/w"});
    assert!(matches!(
        begin(home.path(), &principal(), &id, "hydrate", target.clone()),
        Begin::Started(_)
    ));
    assert!(matches!(
        begin(home.path(), &principal(), &id, "publish", target.clone()),
        Begin::Mismatch(_)
    ));
    finish(
        home.path(),
        &principal(),
        &id,
        "ready",
        &json!({"status":"ready"}),
    );
    assert!(matches!(
        begin(
            home.path(),
            &principal(),
            &id,
            "hydrate",
            json!({"kind":"local","codeRepoPath":"/other"})
        ),
        Begin::Mismatch(_)
    ));
    assert!(matches!(
        begin(home.path(), &principal(), &id, "hydrate", target),
        Begin::Done(_)
    ));
}

/// Kept results are bounded (age and count).
#[test]
fn kept_results_are_pruned() {
    let home = tempfile::tempdir().unwrap();
    let dir = record_dir(home.path(), &principal());
    std::fs::create_dir_all(&dir).unwrap();
    for index in 0..(KEEP_AT_MOST + 5) {
        std::fs::write(dir.join(format!("{index:04}.json")), b"{}").unwrap();
    }
    let old = dir.join("old.json");
    std::fs::write(&old, b"{}").unwrap();
    let past = std::time::SystemTime::now() - KEEP_FOR - std::time::Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(past)
        .unwrap();
    prune_kept(&dir);
    assert!(!old.exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), KEEP_AT_MOST);
}

/// The client never uses a hostile name as it arrives: an
/// unsafe path becomes an `Unsafe` skipped entry (a launch skips it; the
/// explicit publish refuses such a manifest), and bytes not asked for are
/// dropped.
#[test]
fn the_client_neutralizes_a_hostile_manifest() {
    use crate::mcp::remote_destination::sanitize_machine_paths;
    use codesense_pty_core::remote_manifest::{EntryKind, ManifestEntry};
    let entry = |path: &str| ManifestEntry {
        path: path.into(),
        kind: EntryKind::File,
        hash: Some([1; 32]),
        size: 1,
        mode: 0o644,
        tracked: false,
        oversize: false,
        disk_name: None,
        reason: None,
    };
    let mut entries = vec![
        entry("docs/a.md"),
        entry("docs/../../.ssh/authorized_keys"),
        entry("/etc/passwd"),
        entry(".git/config"),
    ];
    let mut returned = std::collections::BTreeMap::new();
    returned.insert("docs/a.md".to_string(), b"a".to_vec());
    returned.insert("docs/b.md".to_string(), b"b".to_vec());
    sanitize_machine_paths(&mut entries, &mut returned, &["docs/a.md".to_string()]);
    assert_eq!(entries[0].kind, EntryKind::File);
    for hostile in &entries[1..] {
        assert_eq!(hostile.kind, EntryKind::Unsafe, "{}", hostile.path);
        assert!(hostile.hash.is_none());
    }
    assert_eq!(returned.keys().collect::<Vec<_>>(), vec!["docs/a.md"]);
}

/// A panicking operation still ends with exactly one terminal
/// event, and its registry entry is released (it answers `failed`).
#[test]
fn a_panicking_operation_ends_once_and_is_released() {
    let home = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let target = json!({"kind":"local","codeRepoPath":"/w"});
    assert!(matches!(
        begin(home.path(), &principal(), &id, "hydrate", target),
        Begin::Started(_)
    ));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let joined = runtime.block_on(async {
        tokio::task::spawn_blocking(|| -> Result<(Value, &'static str), String> { panic!("boom") })
            .await
    });
    let emitted = std::sync::atomic::AtomicUsize::new(0);
    let ended = AtomicBool::new(false);
    let outcome = joined_outcome(joined, &ended, &|| {
        emitted.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(emitted.load(Ordering::SeqCst), 1);
    assert!(conclude(home.path(), &principal(), &id, true, outcome).is_err());
    assert_eq!(
        status(home.path(), &principal(), &id).unwrap().state,
        "failed"
    );
    assert!(!cancel(&id), "no longer running");
    // An operation that already ended emits no second terminal event.
    let joined = runtime.block_on(async {
        tokio::task::spawn_blocking(|| -> Result<(), String> { panic!("late") }).await
    });
    let late = AtomicBool::new(true);
    let _ = joined_outcome(joined, &late, &|| {
        emitted.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(emitted.load(Ordering::SeqCst), 1);
}

/// "Discard my edits" replaces exact files only.
#[test]
fn xs10_discard_refuses_a_directory_path() {
    let wanted = vec!["docs/a.md".to_string(), "docs/b.md".to_string()];
    let managed = |p: &str| p == "docs/a.md";
    assert!(discard_selection(&["docs".into()], &wanted, &managed).is_err());
    assert!(discard_selection(&["docs/".into()], &wanted, &managed).is_err());
    assert_eq!(
        discard_selection(&["docs/a.md".into()], &wanted, &managed).unwrap(),
        vec!["docs/a.md"]
    );
}

#[test]
fn xs10_discard_refuses_a_path_it_would_have_to_trim() {
    let wanted = vec!["CLAUDE.md".to_string()];
    let managed = |_: &str| true;
    assert!(discard_selection(&[" CLAUDE.md".into()], &wanted, &managed).is_err());
    assert!(discard_selection(&["CLAUDE.md\n".into()], &wanted, &managed).is_err());
    assert!(discard_selection(&["".into()], &wanted, &managed).is_err());
    assert!(discard_selection(&[], &wanted, &managed).is_err());
}

#[test]
fn xs10_discard_needs_a_managed_entry_or_exactly_one_wanted_file() {
    let wanted = vec!["CLAUDE.md".to_string()];
    let unmanaged = |_: &str| false;
    assert!(discard_selection(&["CLAUDE.md".into()], &wanted, &unmanaged).is_ok());
    assert!(discard_selection(&["notes.md".into()], &wanted, &unmanaged).is_err());
    assert!(discard_selection(&["notes.md".into()], &wanted, &|p| p == "notes.md").is_ok());
}

#[test]
fn story_hydration_response_does_not_count_private_ancestor_siblings() {
    use crate::install::frontmatter_cache::FrontmatterCache;
    use crate::install::hydration_closure::{HydrationPolicy, LIGHT};
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    for (team, rel, id, links) in [
        (
            "child",
            "seed/story.md",
            "SEED",
            "[../../../grand/content/epic/shared.md]",
        ),
        ("grand", "epic/shared.md", "SHARED", "[]"),
    ] {
        let file = root.join(team).join("content").join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, format!("---\ntype: story\nsource_repo: acme/api\nstory_id: {id}\nteam: {team}\nsupporting_docs: {links}\n---\n{id} bytes\n")).unwrap();
    }
    for team in ["grand", "parent"] {
        std::fs::write(
            root.join(team).join("config/sharing.yml"),
            "share: ['content/epic/shared.md']\n",
        )
        .unwrap();
    }
    let cache = FrontmatterCache::new_shared();
    FrontmatterCache::rebuild_from_disk(&cache, &root);
    let ctx = crate::commands::content_engine::derive_resolver_context(
        home.path().to_path_buf(),
        1,
        "acme/ctx".into(),
        Some("child".into()),
    )
    .unwrap();
    let target = crate::agents::ManifestTarget::team("acme/ctx", "child");
    let response = || {
        let story = crate::commands::route_to_context::remote_story_files(
            &ctx,
            &crate::domain::SourceRepoSlug::parse_owner_repo("acme/api").unwrap(),
            "SEED",
            &cache,
            Some(HydrationPolicy {
                reference_depth: 1,
                ..LIGHT
            }),
        )
        .unwrap();
        assert!(story
            .files
            .contains(&root.join("grand/content/epic/shared.md")));
        let (want, outside) =
            step3::want_for_files(home.path(), &Principal::from(1u64), &target, &story.files);
        assert_eq!(want.files.len(), 2);
        assert_eq!(outside, 0);
        story_scope_json("SEED", &story, outside)
    };
    let before = response();
    let private = root.join("grand/content/epic/private-sibling.md");
    std::fs::write(&private, "private bytes").unwrap();
    assert_eq!(response(), before);
    std::fs::remove_file(private).unwrap();
    assert_eq!(response(), before);
    assert_eq!(before["filesScoped"], 2);
    assert_eq!(before["outsideTeamContent"], 0);
}
