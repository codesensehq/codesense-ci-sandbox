// SPDX-License-Identifier: MIT
//! SW-WV-F1 store-layer tests for the Weft Board read (`wv_list_attention_board`).
//!
//! Covers AC-1: the five slices are assembled from `wv_attention` (by `state`) and
//! `wv_queue` (by dispatch state), each row lands in exactly one slice, and ON-AGENT
//! includes BOTH `claimed` AND `dispatched` queue rows (ARCHITECTURE §2.4 — omitting
//! `dispatched` hides live workers). The read is read-only: it opens a store B3+E1
//! already migrated (`wv_attention` present) and never issues a `wv_*` DDL.

use super::*;
use crate::weft::queue;
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

#[test]
fn board_slices_every_row_into_exactly_one_section() {
    let (_dir, store) = open_temp();

    // wv_attention rows covering all three attention states (B3-owned writer).
    let owe = wv_upsert_attention(
        &store,
        None,
        Some("needs-you"),
        Some("You owe: review the migration plan."),
        Some("panel"),
        Some("automations"),
        None,
        None,
    )
    .unwrap();
    let owed = wv_upsert_attention(
        &store,
        None,
        Some("waiting"),
        Some("Owed to you: the agent is waiting on your answer."),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let done = wv_upsert_attention(
        &store,
        None,
        Some("done-unacked"),
        Some("Done: the refactor landed — ack to clear."),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    // wv_queue rows: one claimed + one dispatched (both ON-AGENT), one pending (PARKED).
    // A resolved row would be pruned, so it can never appear — no need to seed one.
    let claimed = queue::enqueue(
        &store,
        "triage",
        None,
        None,
        Some("claimed work"),
        None,
        None,
        1,
    )
    .unwrap();
    queue::claim(&store, "worker-A", None, 2).unwrap();
    let dispatched = queue::enqueue(
        &store,
        "triage",
        None,
        None,
        Some("dispatched work"),
        None,
        None,
        3,
    )
    .unwrap();
    queue::claim(&store, "worker-B", None, 4).unwrap();
    queue::mark_dispatched_for_test(&store, dispatched, "worker-B", None, 5).unwrap();
    let pending = queue::enqueue(
        &store,
        "triage",
        None,
        None,
        Some("parked work"),
        None,
        None,
        6,
    )
    .unwrap();

    let board = wv_list_attention_board(&store).unwrap();

    // YOU-OWE = needs-you attention only.
    assert_eq!(
        board.you_owe.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![owe]
    );
    assert_eq!(board.you_owe[0].source.as_deref(), Some("panel"));
    assert_eq!(board.you_owe[0].ref_.as_deref(), Some("automations"));
    // OWED-TO-YOU = waiting attention only.
    assert_eq!(
        board.owed_to_you.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![owed]
    );
    // DONE-UNACKED = done-unacked attention only.
    assert_eq!(
        board.done_unacked.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![done]
    );

    // ON-AGENT = BOTH the claimed AND the dispatched queue row (§2.4).
    let on_agent: Vec<i64> = board.on_agent.iter().map(|r| r.id).collect();
    assert!(
        on_agent.contains(&claimed),
        "claimed row missing from ON-AGENT"
    );
    assert!(
        on_agent.contains(&dispatched),
        "dispatched row missing from ON-AGENT — a live worker would disappear (§2.4)"
    );
    assert_eq!(
        on_agent.len(),
        2,
        "ON-AGENT must hold exactly the two active queue rows"
    );

    // PARKED = the pending queue row only.
    assert_eq!(
        board.parked.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![pending]
    );

    // No leakage: each attention slice holds exactly ONE row (the one seeded for it),
    // and each queue slice holds exactly its queue rows. (The attention and queue id
    // sequences are independent — id 1 exists in both tables — so leak-checking by
    // raw id across tables is meaningless; each slice's exact membership is the proof.)
    assert_eq!(board.you_owe.len(), 1);
    assert_eq!(board.owed_to_you.len(), 1);
    assert_eq!(board.done_unacked.len(), 1);
    assert_eq!(board.parked.len(), 1);
    assert_eq!(board.on_agent.len(), 2);

    // SW-AV-3: each `wv_attention` slice derives its kind from `state` — YOU-OWE
    // (needs-you) is APPROVAL, OWED-TO-YOU (waiting) is QUESTION, DONE-UNACKED is DONE.
    assert_eq!(
        board.you_owe.iter().find(|r| r.id == owe).unwrap().kind,
        AttentionKind::Approval
    );
    assert_eq!(
        board
            .owed_to_you
            .iter()
            .find(|r| r.id == owed)
            .unwrap()
            .kind,
        AttentionKind::Question
    );
    assert_eq!(
        board
            .done_unacked
            .iter()
            .find(|r| r.id == done)
            .unwrap()
            .kind,
        AttentionKind::Done
    );
}

#[test]
fn queue_row_summary_falls_back_to_source_when_note_blank() {
    let (_dir, store) = open_temp();
    // A pending queue row with no note — the reader must still see a non-blank card.
    let id = queue::enqueue(&store, "github:pr-1234", None, None, None, None, None, 1).unwrap();
    let board = wv_list_attention_board(&store).unwrap();
    let row = board.parked.iter().find(|r| r.id == id).unwrap();
    assert_eq!(
        row.summary, "github:pr-1234",
        "blank note falls back to source"
    );
}

// SLW-TL-4: a posted item's DECLARED need surfaces on the board read, and a
// queue-derived (running/parked) row declares none.
#[test]
fn board_surfaces_the_declared_need() {
    let (_dir, store) = open_temp();
    let posted = wv_upsert_attention(
        &store,
        None,
        Some("needs-you"),
        Some("Approve the deploy?"),
        Some("post"),
        Some("run-42"),
        None,
        Some("needs-approval"),
    )
    .unwrap();
    // A legacy attention row with no declared need.
    let legacy = wv_upsert_attention(
        &store,
        None,
        Some("needs-you"),
        Some("legacy"),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    // A parked queue row — a running/queued agent, not a posted item.
    let parked =
        queue::enqueue(&store, "triage", None, None, Some("parked"), None, None, 1).unwrap();

    let board = wv_list_attention_board(&store).unwrap();
    assert_eq!(
        board.you_owe.iter().find(|r| r.id == posted).unwrap().need,
        Some(crate::weft::AttentionNeed::NeedsApproval)
    );
    assert_eq!(
        board.you_owe.iter().find(|r| r.id == legacy).unwrap().need,
        None,
        "a legacy row declares no need"
    );
    assert_eq!(
        board.parked.iter().find(|r| r.id == parked).unwrap().need,
        None,
        "a queue-derived row declares no need"
    );
}

#[test]
fn board_derives_initiator_and_live_session_from_queue_columns() {
    // SW-WA-E5 (AC-2): `wv_list_attention_board` derives `initiator` from the durable
    // `wv_queue` columns and stamps `live_session_id = dispatch_handle` for a
    // `dispatched` row — proven through the real store read the Tauri command wraps.
    let (_dir, store) = open_temp();

    // A controller-claimed, dispatched row: `claimed_by='controller'` (a Keep) with a
    // live dispatch handle → initiator=controller, live_session_id=Some("sess-1").
    let controller = queue::enqueue(
        &store,
        "triage",
        None,
        None,
        Some("controller-dispatched work"),
        None,
        None,
        1,
    )
    .unwrap();
    // `claim` takes the oldest pending row; only `controller` is pending now.
    let claimed = queue::claim(&store, "controller", None, 2).unwrap();
    assert_eq!(claimed, Some(controller), "the controller claims its row");
    queue::mark_dispatched_for_test(&store, controller, "controller", Some("sess-1"), 3).unwrap();

    // A controller-owned escalation: `source='agent'` (per wv_escalate_insert),
    // unclaimed pending → initiator=controller by the source predicate, no live handle.
    let escalation = wv_escalate_insert(&store, "root", "gate blocked", "", 4).unwrap();

    // A human triage row: a Slack ask, unclaimed (`claimed_by=''`) → initiator=human,
    // no live handle.
    let human = queue::enqueue(
        &store,
        "slack",
        None,
        None,
        Some("please fix the flaky test"),
        None,
        None,
        5,
    )
    .unwrap();

    let board = wv_list_attention_board(&store).unwrap();

    // The dispatched controller row rides ON-AGENT with its live handle.
    let on_agent_ctrl = board
        .on_agent
        .iter()
        .find(|r| r.id == controller)
        .expect("the controller-dispatched row is ON-AGENT");
    assert_eq!(on_agent_ctrl.initiator, Initiator::Agent);
    // SW-AV-3: an active (dispatched) queue row's kind is RUNNING.
    assert_eq!(on_agent_ctrl.kind, AttentionKind::Running);
    assert_eq!(
        on_agent_ctrl.live_session_id.as_deref(),
        Some("sess-1"),
        "a dispatched row stamps live_session_id = dispatch_handle verbatim"
    );

    // The escalation + the human row are PARKED (pending). The escalation is
    // controller-provenance (source='agent'); the human row is human-provenance.
    let parked_esc = board
        .parked
        .iter()
        .find(|r| r.id == escalation)
        .expect("the agent escalation is PARKED (pending)");
    assert_eq!(parked_esc.initiator, Initiator::Agent);
    // SW-AV-3: a pending queue row's kind is PARKED.
    assert_eq!(parked_esc.kind, AttentionKind::Parked);
    assert_eq!(
        parked_esc.live_session_id, None,
        "a non-dispatched row carries no live handle"
    );

    let parked_human = board
        .parked
        .iter()
        .find(|r| r.id == human)
        .expect("the human triage row is PARKED (pending)");
    assert_eq!(parked_human.initiator, Initiator::Human);
    assert_eq!(parked_human.kind, AttentionKind::Parked);
    assert_eq!(parked_human.live_session_id, None);
}

#[test]
fn empty_store_yields_five_empty_slices() {
    let (_dir, store) = open_temp();
    let board = wv_list_attention_board(&store).unwrap();
    assert!(board.you_owe.is_empty());
    assert!(board.owed_to_you.is_empty());
    assert!(board.on_agent.is_empty());
    assert!(board.parked.is_empty());
    assert!(board.done_unacked.is_empty());
}
