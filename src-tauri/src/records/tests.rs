// SPDX-License-Identifier: MIT
use super::*;
use tempfile::tempdir;

#[test]
fn care_declaration_is_config_only() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("content/schema")).unwrap();
    std::fs::write(dir.path().join("content/schema/care.yaml"),
        "collection: care\nfolder: hq/data/records/care\nid: slug\ntitle: title\nfields:\n  cadence: {type: choice, allowedValues: [weekly, monthly]}\n").unwrap();
    let collections = load_collections(dir.path(), Scope::Team).unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "care");
    assert_eq!(
        collections[0].fields["cadence"].allowed_values,
        ["weekly", "monthly"]
    );
}

use crate::context_query::{
    index::{IndexRoot, IndexState},
    model::Layer,
};
use serde_json::json;
use std::path::Path;

fn fixture(root: &Path, scope: Scope) -> Store {
    let mut collection = Collection::notes();
    collection.name = "care".into();
    collection.folder = "hq/data/records/care".into();
    collection.scope = scope;
    collection.id = IdRule::Slug;
    collection.options.human_only.clear();
    Store::new(
        root,
        collection,
        (scope == Scope::Private).then(|| root.into()),
    )
    .unwrap()
}
fn agent() -> Actor {
    Actor::agent("Care helper", Some("tab-1".into()), None)
}

#[test]
fn family_care_round_trip_and_no_history_or_commits() {
    let tmp = tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("content/schema")).unwrap();
    std::fs::write(tmp.path().join("content/schema/care.yaml"), "collection: care\nfolder: hq/data/records/care\nid: slug\ntitle: title\nfields:\n  lastDone: {type: date}\n  cadence: {type: choice, allowedValues: [weekly, monthly, yearly]}\n").unwrap();
    let collection = load_collections(tmp.path(), Scope::Team).unwrap().remove(0);
    let store = Store::new(tmp.path(), collection, None).unwrap();
    assert!(store.create(None, &[], "", &agent(), None).is_err());
    let created = store
        .create(
            Some("replace-filter"),
            &[
                ("title".into(), json!("Air filter")),
                ("cadence".into(), json!("monthly")),
                ("custom".into(), json!(["a", "b"])),
            ],
            "Instructions",
            &agent(),
            None,
        )
        .unwrap();
    assert_eq!(created.record.body, "Instructions");
    store
        .set_fields(
            "replace-filter",
            &[("lastDone".into(), json!("2026-10-21"))],
            &agent(),
            None,
        )
        .unwrap();
    assert_eq!(
        store.read("replace-filter").unwrap().fields["custom"],
        json!(["a", "b"])
    );
    assert_eq!(store.list().unwrap().len(), 1);
    for result in [
        store.history("replace-filter", 10).map(|_| ()),
        store.read_at("replace-filter", "aaaa").map(|_| ()),
        store
            .restore("replace-filter", "aaaa", false, &agent(), None)
            .map(|_| ()),
    ] {
        assert_eq!(
            result.unwrap_err(),
            "unavailable: no history for team collections"
        );
    }
    store.delete("replace-filter", &agent(), None).unwrap();
    assert!(!tmp.path().join(".git").exists());
}

#[test]
fn ids_and_scope_paths_are_guarded() {
    for id in ["", "../x", "a..b", "a/b", "a\\b", "_x", "-x", "a b", "é"] {
        assert!(!is_valid_id(id), "{id}");
    }
    assert!(!is_valid_id(&"x".repeat(65)));
    for id in ["a", "A-1", "child_2026-10-21", "a.b"] {
        assert!(is_valid_id(id), "{id}");
    }
    let tmp = tempdir().unwrap();
    let store = fixture(tmp.path(), Scope::Team);
    assert!(store
        .create(Some("../escape"), &[], "", &agent(), None)
        .is_err());
    let mut collection = store.collection.clone();
    collection.folder = "../outside".into();
    assert!(Store::new(tmp.path(), collection, None).is_err());
}

#[cfg(unix)]
#[test]
fn symlink_folder_and_record_are_rejected() {
    use std::os::unix::fs::symlink;
    let tmp = tempdir().unwrap();
    let outside = tempdir().unwrap();
    symlink(outside.path(), tmp.path().join("hq")).unwrap();
    let mut collection = Collection::notes();
    collection.scope = Scope::Team;
    collection.folder = "hq/care".into();
    assert!(Store::new(tmp.path(), collection, None).is_err());
    std::fs::remove_file(tmp.path().join("hq")).unwrap();
    let store = fixture(tmp.path(), Scope::Team);
    std::fs::create_dir_all(tmp.path().join(&store.collection.folder)).unwrap();
    symlink(
        outside.path().join("secret.md"),
        tmp.path().join(&store.collection.folder).join("filter.md"),
    )
    .unwrap();
    assert!(store.read("filter").is_err());
    assert!(store
        .create(Some("filter"), &[], "oops", &agent(), None)
        .is_err());
    assert!(store.delete("filter", &agent(), None).is_err());
}

#[test]
fn conflicting_body_edits_keep_both_and_report_the_overlap() {
    let tmp = tempdir().unwrap();
    let store = fixture(tmp.path(), Scope::Team);
    store
        .create(Some("filter"), &[], "first\nbase\nlast\n", &agent(), None)
        .unwrap();
    store
        .write(
            "filter",
            &BodyEdit::Replace("first\ntheirs\nlast\n".into()),
            &[],
            &agent(),
            None,
        )
        .unwrap();
    let result = store
        .write(
            "filter",
            &BodyEdit::ReplaceFrom {
                base: "first\nbase\nlast\n".into(),
                content: "first\nours\nlast\n".into(),
            },
            &[],
            &agent(),
            None,
        )
        .unwrap();
    assert!(result.merged);
    assert!(result.record.body.contains("ours\n") && result.record.body.contains("theirs\n"));
    assert_eq!(
        result.overlaps,
        vec![Overlap {
            ours: "ours\n".into(),
            theirs: "theirs\n".into(),
            at: 2
        }]
    );
    assert_eq!(store.read("filter").unwrap().body, result.record.body);
}

#[test]
fn request_receipts_prevent_duplicate_appends_creates_and_deletes() {
    let tmp = tempdir().unwrap();
    let store = fixture(tmp.path(), Scope::Team);
    let first = store
        .create(Some("filter"), &[], "one", &agent(), Some("create"))
        .unwrap();
    assert_eq!(
        first,
        store
            .create(Some("filter"), &[], "different", &agent(), Some("create"))
            .unwrap()
    );
    let first = store
        .write(
            "filter",
            &BodyEdit::Append("two".into()),
            &[],
            &agent(),
            Some("append"),
        )
        .unwrap();
    let path = tmp.path().join(&store.collection.folder).join("filter.md");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(
        first,
        store
            .write(
                "filter",
                &BodyEdit::Append("two".into()),
                &[],
                &agent(),
                Some("append")
            )
            .unwrap()
    );
    assert_eq!(
        modified,
        std::fs::metadata(path).unwrap().modified().unwrap()
    );
    let deleted = store.delete("filter", &agent(), Some("delete")).unwrap();
    assert_eq!(
        deleted,
        store.delete("filter", &agent(), Some("delete")).unwrap()
    );
    assert!(store
        .create(Some("x"), &[], "", &agent(), Some(&"x".repeat(65)))
        .is_err());
}

#[test]
fn fields_validate_declared_types_and_preserve_unknown_formatting() {
    let tmp = tempdir().unwrap();
    let mut store = fixture(tmp.path(), Scope::Team);
    let field =
        |name: &str, kind: &str, allowed: Vec<String>| crate::commands::field_schema::FieldDef {
            name: name.into(),
            r#type: Some(kind.into()),
            allowed_values: allowed,
            required: false,
            official: false,
        };
    store.collection.fields.insert(
        "cadence".into(),
        field("cadence", "choice", vec!["weekly".into()]),
    );
    store
        .collection
        .fields
        .insert("done".into(), field("done", "date", vec![]));
    store
        .collection
        .fields
        .insert("count".into(), field("count", "number", vec![]));
    store
        .create(Some("filter"), &[], "Body\r\n", &agent(), None)
        .unwrap();
    let path = tmp.path().join(&store.collection.folder).join("filter.md");
    std::fs::write(
        &path,
        "---\r\n# keep me\r\nunknown: {nested: true}\r\ntitle: Filter\r\n---\r\nBody\r\n",
    )
    .unwrap();
    for (name, value) in [
        ("cadence", json!("hourly")),
        ("done", json!("2026-02-30")),
        ("count", json!("two")),
        ("pinned", json!("yes")),
    ] {
        assert!(store
            .set_fields("filter", &[(name.into(), value)], &agent(), None)
            .is_err());
    }
    store
        .set_fields(
            "filter",
            &[
                ("cadence".into(), json!("weekly")),
                ("newKey".into(), json!(true)),
            ],
            &agent(),
            None,
        )
        .unwrap();
    let raw = std::fs::read_to_string(path).unwrap();
    assert!(raw.contains("# keep me\r\nunknown: {nested: true}\r\n"));
    assert!(raw.ends_with("Body\r\n"));
    assert_eq!(
        store.read("filter").unwrap().fields["unknown"],
        json!({"nested": true})
    );
}

#[test]
fn private_history_flushes_and_supports_delete_restore_and_undo() {
    let tmp = tempdir().unwrap();
    let store = fixture(tmp.path(), Scope::Private);
    let created = store
        .create(Some("filter"), &[], "one\ntwo\n", &agent(), None)
        .unwrap();
    let first = created.commit.unwrap();
    store
        .write(
            "filter",
            &BodyEdit::Replace("ONE\ntwo\n".into()),
            &[],
            &Actor::human(),
            None,
        )
        .unwrap();
    let h = store.history("filter", 10).unwrap();
    assert_eq!(h[0].actor.kind, ActorKind::Human);
    assert_eq!(store.read_at("filter", &first).unwrap().body, "one\ntwo\n");
    store
        .restore("filter", &first, false, &agent(), None)
        .unwrap();
    let changed = store
        .write(
            "filter",
            &BodyEdit::Replace("ONE\ntwo\n".into()),
            &[],
            &agent(),
            None,
        )
        .unwrap();
    store
        .restore(
            "filter",
            changed.commit.as_ref().unwrap(),
            true,
            &agent(),
            None,
        )
        .unwrap();
    assert_eq!(store.read("filter").unwrap().body, "one\ntwo\n");
    let deleted = store.delete("filter", &agent(), None).unwrap();
    let repo = git2::Repository::open(tmp.path()).unwrap();
    let msg = repo
        .find_commit(git2::Oid::from_str(deleted.commit.as_ref().unwrap()).unwrap())
        .unwrap()
        .message()
        .unwrap()
        .to_string();
    assert!(msg.contains("Record-Change: delete"), "{msg}");
    store
        .restore("filter", &first, false, &agent(), None)
        .unwrap();
    assert_eq!(store.read("filter").unwrap().body, "one\ntwo\n");
}

#[test]
fn context_index_search_stays_inside_the_collection() {
    let tmp = tempdir().unwrap();
    let store = fixture(tmp.path(), Scope::Team);
    store
        .create(
            Some("filter"),
            &[("title".into(), json!("Air filter"))],
            "Replace monthly",
            &agent(),
            None,
        )
        .unwrap();
    std::fs::write(tmp.path().join("outside.md"), "Air filter monthly").unwrap();
    let mut index = IndexState::default();
    index.reconcile(vec![IndexRoot {
        dir: tmp.path().canonicalize().unwrap(),
        org: Some("family".into()),
        team: Some("family".into()),
        layer: Layer::Team,
    }]);
    for (query, matched) in [("filter", "title"), ("monthly", "body")] {
        let hits = store
            .search_snapshot(&index.snapshot(1, true), query, 20)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "filter");
        assert_eq!(hits[0].matched, matched);
    }
}

#[test]
fn notes_migration_backs_up_verifies_and_keeps_history_and_recency_local() {
    let tmp = tempdir().unwrap();
    let nd = tmp.path().join("notes");
    std::fs::create_dir(&nd).unwrap();
    let id = "abc-1";
    let body = "# Real note\r\n\r\nKeep trailing spaces  \r\n";
    std::fs::write(nd.join(format!("{id}.md")), body).unwrap();
    let index = json!([{"id": id, "title": "Real title", "pinned": true, "lastOpenedAt": 17, "updatedAt": 18, "updatedBy": agent()}]);
    std::fs::write(nd.join("index.json"), serde_json::to_vec(&index).unwrap()).unwrap();
    let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
    let before = history::hold_history()
        .commit(&loc, &agent(), history::ChangeKind::Create, "initial", &[])
        .unwrap()
        .unwrap();
    notes::ensure_notes_migrated(tmp.path()).unwrap();
    assert!(!nd.join("index.json").exists());
    let note = notes::read(&loc, id).unwrap();
    assert_eq!(note.content.as_bytes(), body.as_bytes());
    assert_eq!(note.title, "Real title");
    assert!(note.pinned);
    assert_eq!(note.updated_by, Some(agent()));
    assert_eq!(note.updated_at, Some(18));
    assert_eq!(note.last_opened_at, 17);
    let backup =
        std::path::PathBuf::from(std::fs::read_to_string(nd.join(".frontmatter-backup")).unwrap());
    assert_eq!(
        std::fs::read(backup.join(format!("{id}.md"))).unwrap(),
        body.as_bytes()
    );
    assert!(backup.join("index.json").exists());
    let raw = std::fs::read(nd.join(format!("{id}.md"))).unwrap();
    notes::ensure_notes_migrated(tmp.path()).unwrap();
    assert_eq!(raw, std::fs::read(nd.join(format!("{id}.md"))).unwrap());
    assert_eq!(notes::read_at(&loc, id, &before).unwrap().content, body);
    let store = notes::store(&loc).unwrap();
    store.history(id, 20).unwrap();
    let repo = git2::Repository::open(tmp.path()).unwrap();
    let head = repo.head().unwrap().target();
    notes::touch(&nd, id, 42).unwrap();
    store.history(id, 20).unwrap();
    assert_eq!(head, repo.head().unwrap().target());
    assert!(repo
        .status_should_ignore(Path::new("notes/.recency.json"))
        .unwrap());
    assert!(store
        .history(id, 20)
        .unwrap()
        .iter()
        .any(|r| r.commit == before));
}

#[test]
fn agents_can_delete_care_but_cannot_delete_notes() {
    let tmp = tempdir().unwrap();
    let store = Store::new(tmp.path(), Collection::notes(), Some(tmp.path().into())).unwrap();
    let created = store.create(None, &[], "private", &agent(), None).unwrap();
    assert!(store
        .delete(&created.record.id, &agent(), None)
        .unwrap_err()
        .contains("only a human"));
    store
        .delete(&created.record.id, &Actor::human(), None)
        .unwrap();
}

#[test]
fn mcp_and_native_notes_share_write_search_and_list_shapes() {
    use crate::mcp::notes_tools::{self, ToolEnvelope};
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
    let created = notes::create(&loc, "Plan", "one", false, &agent(), None).unwrap();
    let params = notes_tools::NotesWriteParams {
        org: None,
        id: created.note.id.clone(),
        content: None,
        append: Some("two".into()),
        patch: None,
        title: None,
        session_id: None,
        request_id: Some("same-write".into()),
        base: None,
        pinned: None,
    };
    let (reply, _) = notes_tools::write(&loc, "", &params, &agent()).unwrap();
    let ToolEnvelope::Ok(mcp) = reply else {
        panic!("write failed")
    };
    let native = crate::commands::scratch::write_note_in(
        &loc,
        &created.note.id,
        &BodyEdit::Append("two".into()),
        None,
        None,
        Some("same-write"),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(mcp).unwrap(),
        serde_json::to_value(native).unwrap()
    );
    let ToolEnvelope::Ok(mcp) = notes_tools::search(&loc, "", "Plan", Some(200)).unwrap() else {
        panic!("search failed")
    };
    assert_eq!(
        serde_json::to_value(mcp.hits).unwrap(),
        serde_json::to_value(notes::search(&loc, "Plan", 200, None).unwrap()).unwrap()
    );
    let ToolEnvelope::Ok(mcp) = notes_tools::list(&loc, "").unwrap() else {
        panic!("list failed")
    };
    let native = notes::list(&loc).unwrap();
    assert_eq!(
        serde_json::to_value(mcp.notes).unwrap(),
        serde_json::to_value(native).unwrap()
    );
}

#[test]
fn history_reads_and_restore_flush_pending_human_edits() {
    let tmp = tempdir().unwrap();
    let store = fixture(tmp.path(), Scope::Private);
    let first = store
        .create(Some("care"), &[], "before", &agent(), None)
        .unwrap()
        .commit
        .unwrap();
    store
        .write(
            "care",
            &BodyEdit::Replace("pending".into()),
            &[],
            &Actor::human(),
            None,
        )
        .unwrap();
    store.read_at("care", &first).unwrap();
    assert_eq!(
        store.history("care", 10).unwrap()[0].actor.kind,
        ActorKind::Human
    );
    store
        .write(
            "care",
            &BodyEdit::Replace("also pending".into()),
            &[],
            &Actor::human(),
            None,
        )
        .unwrap();
    store
        .restore("care", &first, false, &agent(), None)
        .unwrap();
    let history = store.history("care", 10).unwrap();
    assert_eq!(history[1].actor.kind, ActorKind::Human);
    assert_eq!(
        store.read_at("care", &history[1].commit).unwrap().body,
        "also pending"
    );
}

#[test]
fn listing_reaps_old_duplicates_once_and_keeps_notes_created_after_boot() {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
    let initial = notes::list(&loc).unwrap();
    assert!(initial.iter().any(|n| n.title == "Note"));
    let created = notes::create(&loc, "Note", "", false, &Actor::human(), None).unwrap();
    assert!(notes::list(&loc)
        .unwrap()
        .iter()
        .any(|n| n.id == created.note.id));
}

#[test]
fn migration_and_create_keep_leading_markdown_bytes_as_body() {
    for body in [
        "---\nA thematic rule\n",
        "---\ntitle: Literal note content\n---\nBody\n",
        "---\nA rule\n---\n",
    ] {
        let tmp = tempdir().unwrap();
        let nd = tmp.path().join("notes");
        std::fs::create_dir(&nd).unwrap();
        std::fs::write(nd.join("abc-1.md"), body).unwrap();
        std::fs::write(
            nd.join("index.json"),
            serde_json::to_vec(
                &json!([{"id":"abc-1","title":"Real title","pinned":true,"lastOpenedAt":21}]),
            )
            .unwrap(),
        )
        .unwrap();
        let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
        notes::ensure_notes_migrated(tmp.path()).unwrap();
        assert_eq!(notes::read(&loc, "abc-1").unwrap().content, body);
        let created = notes::create(&loc, "New", body, false, &agent(), None).unwrap();
        assert_eq!(created.note.content, body);
        assert!(created.note.last_opened_at > 0);
    }
}

#[test]
fn restoring_legacy_notes_keeps_current_fields_and_original_body() {
    let tmp = tempdir().unwrap();
    let nd = tmp.path().join("notes");
    std::fs::create_dir(&nd).unwrap();
    let body = "---\ntitle: Body text\n---\nOriginal\n";
    std::fs::write(nd.join("abc-1.md"), body).unwrap();
    std::fs::write(
        nd.join("index.json"),
        serde_json::to_vec(
            &json!([{"id":"abc-1","title":"Pinned title","pinned":true,"lastOpenedAt":21}]),
        )
        .unwrap(),
    )
    .unwrap();
    let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
    let old = history::hold_history()
        .commit(
            &loc,
            &agent(),
            history::ChangeKind::Restore,
            "old restore",
            &[],
        )
        .unwrap()
        .unwrap();
    notes::ensure_notes_migrated(tmp.path()).unwrap();
    notes::write(
        &loc,
        "abc-1",
        &BodyEdit::Replace("New body".into()),
        Some("Current title"),
        None,
        &agent(),
        None,
    )
    .unwrap();
    let result = notes::restore(&loc, "abc-1", &old, false, &agent(), None).unwrap();
    assert_eq!(result.note.title, "Current title");
    assert!(result.note.pinned);
    assert_eq!(result.note.content, body);
    let read = notes::read_at(&loc, "abc-1", &old).unwrap();
    assert_eq!(read.title.as_deref(), Some("Pinned title"));
    assert_eq!(read.content, body);
    let old_rev = notes::store(&loc)
        .unwrap()
        .history("abc-1", 20)
        .unwrap()
        .into_iter()
        .find(|r| r.commit == old)
        .unwrap();
    assert_eq!(old_rev.change, "restore");
    notes::store(&loc)
        .unwrap()
        .delete("abc-1", &Actor::human(), None)
        .unwrap();
    let restored = notes::restore(&loc, "abc-1", &old, false, &agent(), None).unwrap();
    assert_eq!(restored.note.title, "Pinned title");
    assert!(restored.note.pinned);
    assert_eq!(restored.note.content, body);
}

#[test]
fn local_notes_search_falls_back_when_the_managed_index_has_no_root() {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::local(tmp.path().into());
    let note = notes::create(
        &loc,
        "Plan",
        "Search this body-only phrase",
        false,
        &agent(),
        None,
    )
    .unwrap();
    let index = crate::context_query::ContextIndexService::new();
    let hits = notes::search(&loc, "body-only", 20, Some(index)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, note.note.id);
}

#[test]
fn undoing_an_agent_created_note_uses_an_empty_parent_body() {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
    notes::create(&loc, "Existing", "existing body", false, &agent(), None).unwrap();
    let created = notes::create(&loc, "New plan", "agent text", true, &agent(), None).unwrap();
    let undone = notes::restore(
        &loc,
        &created.note.id,
        created.commit.as_ref().unwrap(),
        true,
        &Actor::human(),
        None,
    )
    .unwrap();
    assert_eq!(undone.note.content, "");
    assert_eq!(undone.note.title, "New plan");
    assert!(undone.note.pinned);
}

#[test]
fn single_file_migration_keeps_leading_markdown_bytes() {
    for body in [
        "---\nA thematic rule",
        "---\ntitle: Literal content\n---\nBody",
    ] {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.md"), body).unwrap();
        notes::ensure_notes_migrated(tmp.path()).unwrap();
        let meta = notes::read_index(&tmp.path().join("notes"))
            .unwrap()
            .remove(0);
        let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
        assert_eq!(notes::read(&loc, &meta.id).unwrap().content, body);
    }
}

#[test]
fn deleted_notes_restore_historical_fields_and_literal_markdown() {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
    let body = "---\ntitle: Literal body\n---\nKeep all this text";
    let created = notes::create(&loc, "Pinned title", body, true, &agent(), None).unwrap();
    notes::store(&loc)
        .unwrap()
        .delete(&created.note.id, &Actor::human(), None)
        .unwrap();
    let restored = notes::restore(
        &loc,
        &created.note.id,
        created.commit.as_ref().unwrap(),
        false,
        &agent(),
        None,
    )
    .unwrap();
    assert_eq!(restored.note.title, "Pinned title");
    assert!(restored.note.pinned);
    assert_eq!(restored.note.content, body);
}

/// The body the note had just before its deletion, as history restores it.
fn restored_before_delete(loc: &history::NotesLocation, id: &str) -> String {
    let revisions = notes::store(loc).unwrap().history(id, usize::MAX).unwrap();
    let delete = revisions.iter().position(|r| r.change == "delete").unwrap();
    assert_eq!(revisions[delete].actor.kind, ActorKind::Human);
    let before = &revisions[delete + 1].commit;
    notes::restore(loc, id, before, false, &Actor::human(), None)
        .unwrap()
        .note
        .content
}

#[test]
fn deleting_right_after_typing_restores_the_last_body() {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::local(tmp.path().into());
    let created = notes::create(&loc, "Typed", "first", false, &agent(), None).unwrap();
    let id = created.note.id;
    notes::write(
        &loc,
        &id,
        &BodyEdit::Replace("first and then more".into()),
        None,
        None,
        &Actor::human(),
        None,
    )
    .unwrap();
    // Still inside the person's quiet period: their typing is not committed yet.
    let deleted = notes::delete(&loc, &id, None).unwrap();
    assert!(deleted.commit.is_some(), "the delete is its own revision");
    assert_eq!(restored_before_delete(&loc, &id), "first and then more");
}

#[test]
fn reaping_a_note_just_emptied_restores_the_empty_body() {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::local(tmp.path().into());
    let human = Actor::human();
    notes::create(&loc, "Keep", "kept", false, &agent(), None).unwrap();
    let emptied = notes::create(
        &loc,
        notes::DEFAULT_SCRATCH_TITLE,
        "hello",
        false,
        &agent(),
        None,
    )
    .unwrap()
    .note
    .id;
    let erase = BodyEdit::Replace(String::new());
    notes::write(&loc, &emptied, &erase, None, None, &human, None).unwrap();
    let history = notes::store(&loc).unwrap().location().unwrap();
    let boot: Vec<String> = notes::read_index(&loc.notes_dir())
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    notes::reap_empty_scratch_notes_at(&loc.notes_dir(), Some(&history), None, &boot).unwrap();
    assert!(!loc.notes_dir().join(format!("{emptied}.md")).exists());
    assert_eq!(restored_before_delete(&loc, &emptied), "");
}

#[test]
fn production_notes_delete_reseeds_before_returning() {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::local(tmp.path().into());
    notes::ensure_notes_migrated(tmp.path()).unwrap();
    let id = notes::read_index(&loc.notes_dir()).unwrap()[0].id.clone();
    let deleted = notes::delete(&loc, &id, Some("delete-last")).unwrap();
    assert!(deleted.changed);
    assert!(!loc.notes_dir().join(format!("{id}.md")).exists());
    let survivors = notes::read_index(&loc.notes_dir()).unwrap();
    assert_eq!(survivors.len(), 1);
    assert_ne!(survivors[0].id, id);
    let seed = notes::read(&loc, &survivors[0].id).unwrap();
    assert_eq!(seed.title, notes::DEFAULT_SCRATCH_TITLE);
    assert_eq!(seed.content, "");
    assert!(!seed.pinned);
    // A retry returns its receipt without minting another replacement.
    notes::delete(&loc, &id, Some("delete-last")).unwrap();
    assert_eq!(notes::read_index(&loc.notes_dir()).unwrap().len(), 1);
}

fn search_fixture() -> (tempfile::TempDir, history::NotesLocation) {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::local(tmp.path().into());
    std::fs::create_dir(loc.notes_dir()).unwrap();
    std::fs::write(loc.notes_dir().join(".records-v1"), "1\n").unwrap();
    (tmp, loc)
}

fn search_note(
    loc: &history::NotesLocation,
    id: &str,
    title: &str,
    body: &str,
    pinned: bool,
    at: u64,
) {
    std::fs::write(
        loc.notes_dir().join(format!("{id}.md")),
        format!(
            "---\ntitle: {}\npinned: {pinned}\nupdatedAt: {at}\n---\n{body}",
            serde_json::to_string(title).unwrap()
        ),
    )
    .unwrap();
}

#[test]
fn notes_search_matches_decoded_quoted_titles() {
    let (_tmp, loc) = search_fixture();
    search_note(&loc, "quoted", "Say \"hello\"", "Ordinary body", false, 1);
    let hits = notes::search(&loc, "\"hello\"", 20, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "quoted");
    assert_eq!(hits[0].matched, "title");
    assert_eq!(hits[0].title, "Say \"hello\"");
}

#[test]
fn notes_search_ranks_all_matches_before_the_default_mcp_limit() {
    use crate::mcp::notes_tools::{self, ToolEnvelope};
    let (_tmp, loc) = search_fixture();
    for i in 0..21 {
        search_note(
            &loc,
            &format!("a-{i:02}"),
            "Other",
            "milk in body",
            false,
            i,
        );
    }
    search_note(&loc, "z-title-old", "Milk old", "", false, 30);
    search_note(&loc, "z-title-new", "Milk new", "", false, 40);
    search_note(&loc, "z-title-pin", "Milk pinned", "milk too", true, 25);
    search_note(&loc, "z-body-pin", "Other pinned", "milk", true, 50);
    notes::touch(&loc.notes_dir(), "z-title-old", notes::now_millis()).unwrap();
    let ToolEnvelope::Ok(found) = notes_tools::search(&loc, "", "milk", None).unwrap() else {
        panic!("search failed")
    };
    assert_eq!(found.hits.len(), 20);
    let first: Vec<_> = found.hits.iter().take(5).map(|h| h.id.as_str()).collect();
    assert_eq!(
        first,
        [
            "z-title-pin",
            "z-title-old",
            "z-title-new",
            "z-body-pin",
            "a-20"
        ]
    );
}

#[test]
fn notes_search_reads_body_past_the_context_text_scan_bound() {
    let (_tmp, loc) = search_fixture();
    let body = format!("{}\nMilk at the end\n", "a".repeat(600 * 1024));
    search_note(&loc, "long", "Long note", &body, false, 1);
    let hits = notes::search(&loc, "milk", 20, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "long");
    assert_eq!(hits[0].matched, "text");
    assert_eq!(hits[0].snippet.as_deref(), Some("Milk at the end"));
}

#[test]
fn native_notes_metadata_keeps_body_edit_attribution() {
    let tmp = tempdir().unwrap();
    let loc = history::NotesLocation::local(tmp.path().into());
    let created = notes::create(&loc, "Agent note", "Agent body", false, &agent(), None).unwrap();
    for (title, pinned) in [(None, Some(true)), (Some("Renamed"), None)] {
        let changed = notes::update_meta(&loc, &created.note.id, title, pinned).unwrap();
        assert!(changed.changed);
        assert_eq!(changed.note.updated_by, Some(agent()));
        assert_eq!(changed.note.updated_at, created.note.updated_at);
        assert_eq!(changed.note.content, "Agent body");
    }
    let note = notes::read(&loc, &created.note.id).unwrap();
    assert_eq!(note.title, "Renamed");
    assert!(note.pinned);
}

#[test]
fn mcp_and_native_notes_search_share_trimmed_case_normalization() {
    use crate::mcp::notes_tools::{self, ToolEnvelope};
    let (_tmp, loc) = search_fixture();
    search_note(&loc, "groceries", "Groceries", "Milk", false, 1);
    let ToolEnvelope::Ok(found) = notes_tools::search(&loc, "", "  MiLk  ", None).unwrap() else {
        panic!("search failed")
    };
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].id, "groceries");
    assert_eq!(
        serde_json::to_value(found.hits).unwrap(),
        serde_json::to_value(notes::search(&loc, "milk", 20, None).unwrap()).unwrap()
    );
}

#[test]
fn legacy_history_counts_literal_leading_yaml_as_body() {
    let tmp = tempdir().unwrap();
    let nd = tmp.path().join("notes");
    std::fs::create_dir(&nd).unwrap();
    let body = "---\ntitle: Literal body heading\n---\nBody\n";
    std::fs::write(nd.join("abc-2.md"), body).unwrap();
    std::fs::write(
        nd.join("index.json"),
        serde_json::to_vec(&json!([
            {"id":"abc-2", "title":"Sidecar title", "pinned":false, "lastOpenedAt":1}
        ]))
        .unwrap(),
    )
    .unwrap();
    let loc = history::NotesLocation::in_scratch(tmp.path().into(), tmp.path().into());
    let before = history::hold_history()
        .commit(
            &loc,
            &agent(),
            history::ChangeKind::Create,
            "Legacy create",
            &[],
        )
        .unwrap()
        .unwrap();
    notes::ensure_notes_migrated(tmp.path()).unwrap();
    let store = notes::store(&loc).unwrap();
    let revisions = store.history("abc-2", 20).unwrap();
    let legacy = revisions.iter().find(|r| r.commit == before).unwrap();
    assert_eq!((legacy.added, legacy.removed), (4, 0));
    let migration = revisions.iter().find(|r| r.commit != before).unwrap();
    assert_eq!((migration.added, migration.removed), (0, 0));
    notes::write(
        &loc,
        "abc-2",
        &BodyEdit::Replace("---\ntitle: Changed literal heading\n---\nBody\n".into()),
        None,
        None,
        &agent(),
        None,
    )
    .unwrap();
    let revisions = store.history("abc-2", 20).unwrap();
    assert_eq!((revisions[0].added, revisions[0].removed), (1, 1));
}

#[test]
fn records_without_pinned_sort_like_unpinned_by_recency() {
    let tmp = tempdir().unwrap();
    let store = fixture(tmp.path(), Scope::Team);
    for (id, pinned) in [("old-pinned", Some(true)), ("old-unpinned", Some(false))] {
        let mut fields = vec![("title".into(), json!(id))];
        fields.extend(pinned.map(|p| ("pinned".into(), json!(p))));
        store.create(Some(id), &fields, "", &agent(), None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    store
        .create(
            Some("page-created"),
            &[("title".into(), json!("From a page"))],
            "",
            &agent(),
            None,
        )
        .unwrap();
    let ids: Vec<_> = store
        .list_records()
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, ["old-pinned", "page-created", "old-unpinned"]);
}

#[test]
fn shared_record_ordering_cases() {
    #[derive(serde::Deserialize)]
    struct Row {
        id: String,
        fields: std::collections::BTreeMap<String, serde_json::Value>,
    }
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        fields: std::collections::BTreeMap<String, crate::commands::field_schema::FieldDef>,
        order: Vec<String>,
        rows: Vec<Row>,
        expected: Vec<String>,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/records/ordering.json"
    ))
    .unwrap();
    for case in cases {
        let tmp = tempdir().unwrap();
        let mut store = fixture(tmp.path(), Scope::Team);
        store.collection.fields = case.fields;
        store.collection.order = case.order;
        let folder = tmp.path().join(&store.collection.folder);
        std::fs::create_dir_all(&folder).unwrap();
        for row in &case.rows {
            std::fs::write(
                folder.join(format!("{}.md", row.id)),
                format!(
                    "---\n{}---\nBody\n",
                    serde_yaml::to_string(&row.fields).unwrap()
                ),
            )
            .unwrap();
        }
        let records = store.list_records().unwrap();
        assert_eq!(
            records.iter().map(|r| &r.id).collect::<Vec<_>>(),
            case.expected.iter().collect::<Vec<_>>(),
            "{}",
            case.name
        );
        for (i, a) in records.iter().enumerate() {
            for (j, b) in records.iter().enumerate() {
                assert_eq!(
                    super::store::compare_records(a, b, &store.collection),
                    i.cmp(&j),
                    "{}: {} vs {}",
                    case.name,
                    a.id,
                    b.id
                );
            }
        }
    }
}

#[test]
fn human_only_makes_a_collection_append_only_for_agents() {
    const REFUSED: &str = "only a human can";
    // (name, humanOnly, operation, agent caller, allowed)
    let cases: &[(&str, &[&str], &str, bool, bool)] = &[
        ("agent write refused", &["write", "setFields"], "write", true, false),
        ("human write ok", &["write", "setFields"], "write", false, true),
        ("agent setFields refused", &["write", "setFields"], "setFields", true, false),
        ("human setFields ok", &["write", "setFields"], "setFields", false, true),
        ("agent create ok", &["write", "setFields"], "create", true, true),
        ("agent delete refused", &["write", "setFields", "delete"], "delete", true, false),
        ("human delete ok", &["delete"], "delete", false, true),
        ("agent restore refused", &["write", "setFields"], "restore", true, false),
        ("no humanOnly: agent write ok", &[], "write", true, true),
        ("humanOnly delete only: agent write ok", &["delete"], "write", true, true),
        ("humanOnly delete only: agent setFields ok", &["delete"], "setFields", true, true),
        ("setFields only: agent field-only write refused", &["setFields"], "keepWrite", true, false),
        ("setFields only: agent body write ok", &["setFields"], "write", true, true),
        ("unknown value ignored by the store", &["bogus"], "write", true, true),
        ("setFields only: agent append with fields refused", &["setFields"], "appendFields", true, false),
        ("setFields only: agent replace with fields refused", &["setFields"], "replaceFields", true, false),
        ("write+setFields: human append with fields ok", &["write", "setFields"], "appendFields", false, true),
        ("delete only: agent append with fields ok", &["delete"], "appendFields", true, true),
        ("write only: agent setFields refused", &["write"], "setFields", true, false),
        ("write only: human setFields ok", &["write"], "setFields", false, true),
        ("write only: agent metadata update refused", &["write"], "meta", true, false),
        ("write only: human metadata update ok", &["write"], "meta", false, true),
        ("no humanOnly: agent metadata update ok", &[], "meta", true, true),
    ];
    for (name, human_only, operation, as_agent, allowed) in cases {
        let tmp = tempdir().unwrap();
        let mut store = fixture(tmp.path(), Scope::Team);
        store.collection.options.human_only = human_only.iter().map(|s| s.to_string()).collect();
        let actor = if *as_agent { agent() } else { Actor::human() };
        store
            .create(Some("one"), &[], "base\n", &Actor::human(), None)
            .unwrap();
        let fields = [("title".to_string(), json!("T"))];
        let result = match *operation {
            "write" => store
                .write("one", &BodyEdit::Append("more\n".into()), &[], &actor, None)
                .map(|_| ()),
            "keepWrite" => store
                .write("one", &BodyEdit::Keep, &fields, &actor, None)
                .map(|_| ()),
            "setFields" => store.set_fields("one", &fields, &actor, None).map(|_| ()),
            "appendFields" => store
                .write("one", &BodyEdit::Append("more\n".into()), &fields, &actor, None)
                .map(|_| ()),
            "replaceFields" => store
                .write("one", &BodyEdit::Replace("base\n".into()), &fields, &actor, None)
                .map(|_| ()),
            "meta" => store.update_metadata("one", &fields, &actor).map(|_| ()),
            "create" => store
                .create(Some("two"), &[], "new\n", &actor, None)
                .map(|_| ()),
            "delete" => store.delete("one", &actor, None).map(|_| ()),
            // Team collections keep no history; the human-only refusal comes first.
            "restore" => store.restore("one", "deadbeef", false, &actor, None).map(|_| ()),
            other => panic!("{other}"),
        };
        match (allowed, result) {
            (true, Ok(())) => {}
            (true, Err(e)) => panic!("{name}: expected ok, got {e}"),
            (false, Err(e)) => assert!(e.starts_with(REFUSED), "{name}: {e}"),
            (false, Ok(())) => panic!("{name}: expected refusal"),
        }
    }
}

#[test]
fn records_accept_a_fully_stamped_review_record_but_cap_the_write() {
    let cases: &[(&str, usize, bool)] = &[
        ("40 fields ok", 40, true),
        ("64 fields ok", 64, true),
        ("65 fields refused", 65, false),
    ];
    for (name, count, allowed) in cases {
        let tmp = tempdir().unwrap();
        let store = fixture(tmp.path(), Scope::Team);
        let fields: Vec<(String, serde_json::Value)> = (0..*count)
            .map(|i| (format!("f{i}"), json!(i)))
            .collect();
        let result = store.create(Some("one"), &fields, "", &agent(), None);
        assert_eq!(result.is_ok(), *allowed, "{name}: {result:?}");
        if !allowed {
            assert_eq!(result.unwrap_err(), "at most 64 fields per write", "{name}");
        }
    }
}

#[test]
fn human_only_guards_restore_by_what_it_rewrites() {
    // (name, humanOnly, agent caller, allowed)
    let cases: &[(&str, &[&str], bool, bool)] = &[
        ("human restore ok", &["write", "setFields", "delete"], false, true),
        ("write: agent restore refused", &["write"], true, false),
        ("setFields only: agent full restore refused", &["setFields"], true, false),
        ("delete only: agent restore ok", &["delete"], true, true),
        ("no humanOnly: agent restore ok", &[], true, true),
    ];
    for (name, human_only, as_agent, allowed) in cases {
        let tmp = tempdir().unwrap();
        let mut store = fixture(tmp.path(), Scope::Private);
        let first = store
            .create(Some("one"), &[], "base\n", &agent(), None)
            .unwrap()
            .commit
            .unwrap();
        store
            .write("one", &BodyEdit::Replace("next\n".into()), &[], &Actor::human(), None)
            .unwrap();
        store.collection.options.human_only = human_only.iter().map(|s| s.to_string()).collect();
        let actor = if *as_agent { agent() } else { Actor::human() };
        let result = store.restore("one", &first, false, &actor, None);
        match (allowed, result) {
            (true, Ok(_)) => {}
            (true, Err(e)) => panic!("{name}: expected ok, got {e}"),
            (false, Err(e)) => assert!(e.starts_with("only a human can"), "{name}: {e}"),
            (false, Ok(_)) => panic!("{name}: expected refusal"),
        }
    }
}

#[test]
fn human_only_is_a_top_level_schema_key_and_names_known_actions() {
    let page = include_str!(
        "../../../codesense-docs/getting-started/content/arc-i-dashboard-primitives/records.html"
    );
    let section = &page[page.find("id=\"human-only\"").unwrap()..];
    let start = section.find("<pre><code>").unwrap() + "<pre><code>".len();
    let snippet = &section[start..start + section[start..].find("</code>").unwrap()];
    let write = |yaml: &str| {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("content/schema")).unwrap();
        std::fs::write(tmp.path().join("content/schema/care.yaml"), yaml).unwrap();
        load_collections(tmp.path(), Scope::Team)
    };
    let base = "collection: care\nfolder: hq/data/records/care\nid: slug\n";
    let loaded = write(&format!("{base}{snippet}\n")).unwrap();
    assert_eq!(
        loaded[0].options.human_only,
        ["write", "setFields", "delete"],
        "the documented example must parse into the guard"
    );
    // The nested form is silently dropped by the flattened options, so it must not be documented.
    assert!(!snippet.contains("options:"), "{snippet}");
    let err = write(&format!("{base}humanOnly: [write, bogus]\n")).unwrap_err();
    assert!(err.contains("humanOnly") && err.contains("bogus"), "{err}");
}

#[test]
fn field_count_cap_is_64_for_records_and_the_frontmatter_editor() {
    use crate::commands::frontmatter_fields::validate_field_count;
    for (count, allowed) in [(40, true), (64, true), (65, false)] {
        assert_eq!(validate_field_count(count).is_ok(), allowed, "{count}");
    }
}
