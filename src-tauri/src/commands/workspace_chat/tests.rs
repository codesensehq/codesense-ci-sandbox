// SPDX-License-Identifier: MIT
//! Seam tests for the workspace chat engine. Unit tests live inside each
//! submodule (model.rs, resolve.rs, index.rs, store.rs, state.rs).
//!
//! The tests here cover compositions that span modules, exercised through
//! public API only.

use super::model::{MessageKind, HUMAN};
use super::store::{ChatAppend, WorkspaceChatStore};
use tempfile::TempDir;

fn setup() -> (WorkspaceChatStore, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store = WorkspaceChatStore::default();
    store.install(tmp.path());
    (store, tmp)
}

fn user_post(workspace: &str, text: &str) -> ChatAppend {
    ChatAppend {
        workspace_id: workspace.into(),
        from: HUMAN.into(),
        from_name: "Sam".into(),
        text: text.into(),
        kind: MessageKind::Chat,
        content_type: None,
        on_click: None,
        primary_file: None,
    }
}

#[test]
fn system_message_can_be_the_first_line_in_the_log() {
    let (store, _tmp) = setup();
    let sys = ChatAppend {
        workspace_id: "story:a".into(),
        from: MessageKind::SYSTEM_FROM.into(),
        from_name: MessageKind::SYSTEM_FROM_NAME.into(),
        text: "Chat started.".into(),
        kind: MessageKind::System,
        content_type: Some(super::model::MessageContentType::Plain),
        on_click: None,
        primary_file: None,
    };
    let m = store.append(sys, 1).unwrap();
    assert_eq!(m.message_id, 1);
    assert_eq!(m.from, "system");
    assert!(matches!(m.kind, MessageKind::System));
}

#[test]
fn human_post_from_matches_inbox_sentinel() {
    let (store, _tmp) = setup();
    let m = store.append(user_post("story:a", "hi there"), 42).unwrap();
    assert_eq!(m.from, HUMAN);
    assert_eq!(m.from, crate::weft::inbox::HUMAN);
    assert_eq!(m.from_name, "Sam");
    assert!(m.in_reply_to.is_none(), "v1 stores inReplyTo: null always");
}

#[test]
fn head_id_tracks_latest_append() {
    let (store, _tmp) = setup();
    assert_eq!(store.head_id("story:a"), 0);
    store.append(user_post("story:a", "one"), 1).unwrap();
    assert_eq!(store.head_id("story:a"), 1);
    store.append(user_post("story:a", "two"), 2).unwrap();
    assert_eq!(store.head_id("story:a"), 2);
}

#[test]
fn read_result_carries_head_id_and_archive_stamp_visible_via_list() {
    // CR3-11: ChatReadResult no longer carries the chat-local stamp
    // (dead-code field; the Tauri + MCP read paths overlay the registry
    // flag). The stamp still lives on `ChatIndexEntry` and is surfaced
    // through `WorkspaceChatStore::list()` for the sidebar.
    let (store, _tmp) = setup();
    store.append(user_post("story:a", "one"), 1).unwrap();
    store.append(user_post("story:a", "two"), 2).unwrap();
    let out = store.read("story:a", None, None).unwrap();
    assert_eq!(out.head_id, 2);
    assert_eq!(out.messages.len(), 2);
    let listed = store.list();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].archived_at_ms.is_none());

    store.set_archive_stamp("story:a", Some(999)).unwrap();
    let listed = store.list();
    assert_eq!(listed[0].archived_at_ms, Some(999));
}

#[test]
fn human_posts_then_agent_reads_advances_watermark_past_head() {
    let (store, _tmp) = setup();
    store.append(user_post("story:a", "one"), 10).unwrap();
    store.append(user_post("story:a", "two"), 20).unwrap();
    let out = store
        .read_for_session("story:a", "h-ex-1", "h-ex-1", None, Some(50))
        .unwrap();
    assert_eq!(out.messages.len(), 2);
    assert_eq!(
        store.position("story:a", "h-ex-1", "h-ex-1").unwrap().read,
        2
    );
    store.append(user_post("story:a", "three"), 30).unwrap();
    assert_eq!(
        store.unread("story:a", "h-ex-1", "h-ex-1").unwrap().count,
        1
    );
}

#[test]
fn archive_stamp_blocks_non_system_append_but_allows_system_rows() {
    // CR3-7: once `set_archive_stamp` has committed, `store.append` MUST
    // refuse further user/agent chat rows. System rows (lifecycle anchors
    // like "Workspace archived.") bypass the gate so the engine can still
    // record the archive event itself.
    let (store, _tmp) = setup();
    store.append(user_post("story:a", "pre"), 1).unwrap();
    store.set_archive_stamp("story:a", Some(42)).unwrap();

    let err = store.append(user_post("story:a", "post"), 2).unwrap_err();
    assert!(
        matches!(err, super::store::ChatStoreError::Archived),
        "expected Archived, got {err:?}"
    );
    assert_eq!(store.head_id("story:a"), 1, "head must not advance");

    // System row still writes.
    let sys = ChatAppend {
        workspace_id: "story:a".into(),
        from: MessageKind::SYSTEM_FROM.into(),
        from_name: MessageKind::SYSTEM_FROM_NAME.into(),
        text: "Workspace archived.".into(),
        kind: MessageKind::System,
        content_type: Some(super::model::MessageContentType::Plain),
        on_click: None,
        primary_file: None,
    };
    let m = store.append(sys, 3).unwrap();
    assert_eq!(m.message_id, 2);

    // Clearing the stamp re-opens writes.
    store.set_archive_stamp("story:a", None).unwrap();
    let m = store.append(user_post("story:a", "resume"), 4).unwrap();
    assert_eq!(m.message_id, 3);
}

#[test]
fn tail_scan_recovers_head_for_a_large_last_message() {
    // CR3-1: an 8 KiB tail-scan window silently lost `head_id` for any
    // workspace whose last log line exceeded the window. With MAX_TEXT_BYTES
    // at 32 KiB a legal post can be ~32 KiB plus JSON envelope — a cold
    // restart's `install` would then fail to parse the tail and reset head
    // to 0, causing the next append to collide on message_id=1.
    let tmp = tempfile::TempDir::new().unwrap();
    // Build a message well above the old 8 KiB cap but comfortably under
    // the current 32 KiB text cap.
    let big = "x".repeat(20_000);
    {
        let store = WorkspaceChatStore::default();
        store.install(tmp.path());
        store.append(user_post("story:a", "first"), 1).unwrap();
        store.append(user_post("story:a", &big), 2).unwrap();
        assert_eq!(store.head_id("story:a"), 2);
    }
    // Fresh store on the same home must recover head=2 from the tail.
    let store2 = WorkspaceChatStore::default();
    store2.install(tmp.path());
    assert_eq!(
        store2.head_id("story:a"),
        2,
        "tail_scan must contain the full final line"
    );
    // Next append should mint message_id=3, not collide on 1 or 2.
    let m = store2.append(user_post("story:a", "third"), 3).unwrap();
    assert_eq!(m.message_id, 3);
}

#[test]
fn archive_stamp_persists_through_restart() {
    // The index is the archive stamp's authority (SW-WC-1 is the admission
    // check source, but the chat-local stamp lives here for the sidebar).
    // CR3-11: the stamp is read via `list()` (ChatIndexEntry) rather than
    // via `read()` (ChatReadResult no longer carries it).
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let store = WorkspaceChatStore::default();
        store.install(tmp.path());
        store.append(user_post("story:a", "one"), 1).unwrap();
        store.set_archive_stamp("story:a", Some(42)).unwrap();
    }
    let store2 = WorkspaceChatStore::default();
    store2.install(tmp.path());
    let listed = store2.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].archived_at_ms, Some(42));
}

#[test]
fn retained_chat_is_paged_independently_and_persists_per_workspace() {
    let (store, tmp) = setup();
    for n in 0..75 {
        store
            .append(user_post("story:a", &format!("post {n}")), n)
            .unwrap();
    }
    store
        .append(user_post("story:b", "different room"), 1)
        .unwrap();
    let first = store
        .read_for_session("story:a", "one", "one", None, Some(50))
        .unwrap();
    assert_eq!(first.messages[0].message_id, 1);
    assert_eq!(first.messages.len(), 50);
    assert_eq!(first.unread_remaining, 25);
    assert_eq!(store.unread("story:a", "two", "two").unwrap().count, 75);
    // A tail inspection cannot acknowledge a gap.
    store
        .read_for_session("story:a", "two", "two", Some(70), Some(50))
        .unwrap();
    assert_eq!(store.unread("story:a", "two", "two").unwrap().count, 75);
    store.note_told("story:a", "one", "one", 75).unwrap();
    store.note_human_read("story:a", 75).unwrap();
    assert_eq!(store.unread("story:a", "two", "two").unwrap().count, 75);
    let restored = WorkspaceChatStore::default();
    restored.install(tmp.path());
    assert_eq!(restored.position("story:a", "one", "one").unwrap().read, 50);
    assert_eq!(restored.position("story:a", "one", "one").unwrap().told, 75);
    assert_eq!(restored.human_read_id("story:a").unwrap(), 75);
    assert_eq!(restored.human_unread("story:a").unwrap().count, 0);
    assert_eq!(restored.position("story:b", "one", "one").unwrap().read, 0);
    let next = restored
        .read_for_session("story:a", "one", "one", None, None)
        .unwrap();
    assert_eq!(next.messages[0].message_id, 51);
    assert_eq!(next.unread_remaining, 0);
    // A late join needs no new append.
    assert_eq!(
        restored.unread("story:a", "late", "late").unwrap().count,
        75
    );
}

#[test]
fn lifecycle_and_own_posts_do_not_count_or_need_notices() {
    let (store, _tmp) = setup();
    let mut system = user_post("story:a", "Chat started.");
    system.kind = MessageKind::System;
    store.append(system, 1).unwrap();
    let mut own = user_post("story:a", "from me");
    own.from = "one".into();
    store.append(own, 2).unwrap();
    assert_eq!(store.unread("story:a", "one", "one").unwrap().count, 0);
    assert_eq!(store.unread("story:a", "two", "two").unwrap().count, 1);
    store.note_told("story:a", "two", "two", 2).unwrap();
    assert_eq!(store.unread("story:a", "two", "two").unwrap().count, 1);
    assert_eq!(store.unannounced("story:a", "two", "two").unwrap().count, 0);
}

#[test]
fn scoped_positions_prune_only_after_two_missing_snapshots() {
    let (store, _tmp) = setup();
    store.append(user_post("story:a", "post"), 1).unwrap();
    store.position("story:a", "here", "here").unwrap();
    store.note_read("story:a", "here", "here", 1).unwrap();
    store.position("story:a", "gone", "gone").unwrap();
    store.note_read("story:a", "gone", "gone", 1).unwrap();
    let live = ["here".to_string()].into_iter().collect();
    store.prune_positions(&live).unwrap();
    assert_eq!(store.position("story:a", "gone", "gone").unwrap().read, 1);
    store.prune_positions(&live).unwrap();
    assert_eq!(store.position("story:a", "gone", "gone").unwrap().read, 0);
    assert_eq!(store.position("story:a", "here", "here").unwrap().read, 1);
}

#[test]
fn chat_positions_keep_a_reattached_session_but_reset_a_relaunched_handle() {
    let (store, tmp) = setup();
    store.append(user_post("story:a", "first"), 1).unwrap();
    store
        .read_for_session("story:a", "story-handle", "session-1", None, None)
        .unwrap();
    store.append(user_post("story:a", "second"), 2).unwrap();
    store
        .note_told("story:a", "story-handle", "session-1", 2)
        .unwrap();

    // Restarting the desktop and reattaching the same live agent keeps both cursors.
    let reattached = WorkspaceChatStore::default();
    reattached.install(tmp.path());
    let p = reattached
        .position("story:a", "story-handle", "session-1")
        .unwrap();
    assert_eq!((p.read, p.told), (1, 2));
    assert_eq!(
        reattached
            .unread("story:a", "story-handle", "session-1")
            .unwrap()
            .count,
        1
    );
    assert_eq!(
        reattached
            .unannounced("story:a", "story-handle", "session-1")
            .unwrap()
            .count,
        0
    );

    // An immediate relaunch reuses the story handle but has a different live session.
    assert_eq!(
        reattached
            .unannounced("story:a", "story-handle", "session-2")
            .unwrap()
            .count,
        2
    );
    reattached
        .note_told("story:a", "story-handle", "session-1", 2)
        .unwrap();
    let p = reattached
        .position("story:a", "story-handle", "session-2")
        .unwrap();
    assert_eq!(
        (p.read, p.told),
        (0, 0),
        "late old-session confirmations stay isolated"
    );
}
