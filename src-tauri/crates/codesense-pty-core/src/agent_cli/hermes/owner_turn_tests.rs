// SPDX-License-Identifier: MIT
//! Real owner consumers of captured hook payloads, including resume without SessionStart.
use super::*;
use crate::activity::{Action, ActivityOwner, Mutation, Outcome, Snapshot, State, Writer};
use serde_json::Value;

fn turns() -> Vec<Value> {
    include_str!("fixtures/turns.jsonl")
        .lines()
        .map(|raw| serde_json::from_str(raw).unwrap())
        .collect()
}

fn owner() -> (ActivityOwner, Writer) {
    let owner = ActivityOwner::new();
    let writer = owner
        .prepare_native("turns", AgentCli::Hermes, "unused")
        .unwrap();
    owner.register("turns", Some(std::process::id()));
    (owner, writer)
}

fn fold(owner: &ActivityOwner, writer: &Writer, raw: &Value) -> Result<Snapshot, &'static str> {
    let report = Hermes
        .normalize(&serde_json::to_vec(raw).unwrap(), true)
        .unwrap();
    owner.report_for_test(writer, std::process::id(), report)
}

fn mutate(
    owner: &ActivityOwner,
    writer: &Writer,
    generation: u64,
    id: &str,
    action: Action,
) -> Snapshot {
    owner
        .mutate(Mutation {
            session: "turns".into(),
            identity: writer.identity.clone(),
            turn_generation: generation,
            operation_id: id.into(),
            action,
        })
        .unwrap()
}

#[test]
fn first_native_turn_without_session_start_binds_and_shows_inference() {
    let (owner, writer) = owner();
    let first = fold(&owner, &writer, &turns()[1]).unwrap();
    assert_eq!(
        first.state,
        State::Thinking,
        "no-tool inference must be busy"
    );
    assert_eq!(first.turn_generation, 1);
    assert!(first.observed);
}

#[test]
fn later_native_turn_releases_failed_finish_and_resets_shared_turn_state() {
    let (owner, writer) = owner();
    let turns = turns();
    fold(&owner, &writer, &turns[0]).unwrap();
    let first = fold(&owner, &writer, &turns[1]).unwrap();
    mutate(
        &owner,
        &writer,
        first.turn_generation,
        "same-operation",
        Action::AskOpened {
            id: "old-ask".into(),
        },
    );
    let failed = mutate(
        &owner,
        &writer,
        first.turn_generation,
        "finish",
        Action::Done {
            outcome: Outcome::Failed,
        },
    );
    assert_eq!(failed.state, State::Errored);
    let next = fold(&owner, &writer, &turns[2]).unwrap();
    assert_eq!(
        next.state,
        State::Thinking,
        "a new prompt releases the failed finish latch"
    );
    assert_eq!(next.turn_generation, first.turn_generation + 1);
    assert!(
        next.asks.is_empty(),
        "prior-turn asks must not hold the new turn"
    );
    let fresh = mutate(
        &owner,
        &writer,
        next.turn_generation,
        "same-operation",
        Action::AskOpened {
            id: "new-ask".into(),
        },
    );
    assert_eq!(fresh.state, State::NeedsPermission);
    assert_eq!(fresh.asks, ["new-ask"], "per-turn operation IDs are reset");
}

#[test]
fn native_new_and_resume_bind_a_b_a_as_submitted_turns() {
    let (owner, writer) = owner();
    let turns = turns();
    let mut generation = 0;
    // No extra on_session_start: both a new context and /resume are start-capable.
    for index in [1, 3, 4] {
        let next = fold(&owner, &writer, &turns[index]).unwrap();
        generation += 1;
        assert_eq!(next.state, State::Thinking);
        assert_eq!(next.turn_generation, generation);
        assert_eq!(next.identity, writer.identity);
    }
}

#[test]
fn child_native_turn_does_not_submit_or_rebind_the_parent() {
    let (owner, writer) = owner();
    fold(&owner, &writer, &turns()[0]).unwrap();
    let before = mutate(
        &owner,
        &writer,
        0,
        "working",
        Action::Progress { blocked: false },
    );
    assert_eq!(before.state, State::Working);
    let child: Value = include_str!("fixtures/activity.jsonl")
        .lines()
        .map(|raw| serde_json::from_str::<Value>(raw).unwrap())
        .find(|raw| {
            raw["hook_event_name"] == "pre_llm_call" && raw["extra"]["platform"] == "subagent"
        })
        .unwrap();
    assert!(fold(&owner, &writer, &child).is_err());
    let after = owner.snapshot("turns").unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.turn_generation, before.turn_generation);
    assert_eq!(after.revision, before.revision);
}
