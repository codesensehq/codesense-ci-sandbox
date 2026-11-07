// SPDX-License-Identifier: MIT
//! The engine on a toy kind ("group": items belong to a group; a group's aggregate is how many
//! items it has, their total weight, their earliest start and their tags). Nothing here is a
//! story: that is the point.
use super::*;
use crate::session_usage::tests::store;
use serde_json::json;

const ITEM: &str = "item";

struct Toy {
    /// What each queued item belongs to now: item id → (group, weight (None: unknown), start, tag).
    world: BTreeMap<String, (&'static str, Option<f64>, i64, &'static str)>,
}

impl EntityAdapter for Toy {
    const KIND: &'static str = "group";
    const VERSION: i64 = 3;
    const SOURCES: &'static [&'static str] = &[ITEM];
    const REDUCERS: &'static [Reducer] = &[
        Reducer { out: "items", op: Op::Count, of: Of::Row, source: ITEM },
        Reducer { out: "weight", op: Op::Sum, of: Of::Num("weight"), source: ITEM },
        Reducer { out: "start", op: Op::Min, of: Of::First, source: ITEM },
        Reducer { out: "latest", op: Op::Max, of: Of::Last, source: ITEM },
        Reducer { out: "tags", op: Op::Union, of: Of::Extra("tag"), source: ITEM },
    ];
}

impl SourceResolver for Toy {
    fn contributions(&self, _: &UsageStore, queued: &Source) -> Result<(Source, Vec<(String, Contribution)>), String> {
        let source = Source { kind: ITEM, id: queued.id.clone() };
        Ok((source, self.world.get(&queued.id).map(|item| (item.0.to_owned(), row(item))).into_iter().collect()))
    }

    fn rebuild(&self, _: &UsageStore, _: &CallerScope, entity: &str) -> Result<Vec<(Source, Contribution)>, String> {
        Ok(self
            .world
            .iter()
            .filter(|(_, item)| item.0 == entity)
            .map(|(id, item)| (Source { kind: ITEM, id: id.clone() }, row(item)))
            .collect())
    }
}

fn row(item: &(&'static str, Option<f64>, i64, &'static str)) -> Contribution {
    let mut c = Contribution::new();
    c.num("weight", item.1, "no weight recorded");
    c.first_ms = Some(item.2);
    c.last_ms = Some(item.2 + 10);
    c.extra.insert("tag".into(), json!(item.3));
    c
}

fn toy() -> Toy {
    Toy {
        world: [
            ("a".to_owned(), ("g1", Some(1.5), 100, "x")),
            ("b".to_owned(), ("g1", Some(2.0), 50, "y")),
            ("c".to_owned(), ("g2", None, 70, "x")),
        ]
        .into(),
    }
}

fn queue(store: &UsageStore, id: &str) -> Queued {
    store
        .conn
        .execute("INSERT OR REPLACE INTO source_event (source_kind, source_id, enqueued_ms) VALUES (?1, ?2, 1)", params![ITEM, id])
        .unwrap();
    Queued { source: Source { kind: ITEM, id: id.into() }, enqueued_ms: 1 }
}

fn build(store: &UsageStore, adapter: &Toy, entity: &str) {
    let rows = adapter.rebuild(store, &CallerScope::Human, entity).unwrap();
    replace_entity(store, "s", Toy::KIND, entity, &rows, Toy::VERSION, 1).unwrap();
}

fn read_rows(store: &UsageStore, entity: &str) -> Vec<Row> {
    rows_of(store, "s", Toy::KIND, entity, Toy::VERSION).unwrap().unwrap()
}

fn apply_queued(store: &UsageStore, adapter: &Toy, id: &str) -> Applied {
    let queued = queue(store, id);
    let (source, wanted) = adapter.contributions(store, &queued.source).unwrap();
    apply(&|_, _| Ok(()), store, Toy::KIND, Toy::VERSION, &queued, &source, &wanted, 2).unwrap()
}

#[test]
fn the_five_reducers_fold_rows_and_an_unknown_makes_a_sum_null_with_its_reason() {
    let (_dir, store) = store();
    let adapter = toy();
    build(&store, &adapter, "g1");
    let agg = aggregate(Toy::REDUCERS, &read_rows(&store, "g1"));
    assert_eq!(agg.nums["items"], Some(2.0));
    assert_eq!(agg.nums["weight"], Some(3.5));
    assert_eq!((agg.nums["start"], agg.nums["latest"]), (Some(50.0), Some(110.0)));
    assert_eq!(agg.sets["tags"], BTreeSet::from(["x".to_owned(), "y".to_owned()]));
    build(&store, &adapter, "g2");
    let unknown = aggregate(Toy::REDUCERS, &read_rows(&store, "g2"));
    assert_eq!(unknown.nums["weight"], None, "never a partial sum");
    assert_eq!(unknown.reasons["weight"], "no weight recorded");
    assert_eq!(unknown.nums["items"], Some(1.0));
}

#[test]
fn rows_are_read_latest_activity_first_and_only_when_built_by_this_version() {
    let (_dir, store) = store();
    build(&store, &toy(), "g1");
    let ids: Vec<_> = read_rows(&store, "g1").into_iter().map(|r| r.source.1).collect();
    assert_eq!(ids, ["a", "b"], "a (starts at 100) is the later one");
    assert!(rows_of(&store, "s", Toy::KIND, "g1", Toy::VERSION + 1).unwrap().is_none(), "another version is not built");
    assert!(rows_of(&store, "s", Toy::KIND, "never", Toy::VERSION).unwrap().is_none());
}

#[test]
fn an_event_rewrites_one_row_and_a_replay_changes_nothing() {
    let (_dir, store) = store();
    let mut adapter = toy();
    build(&store, &adapter, "g1");
    let before = read_rows(&store, "g1");
    // Replayed: the same facts again, as after a restart or a duplicate event.
    for _ in 0..3 {
        let applied = apply_queued(&store, &adapter, "a");
        assert!(applied.changed.is_empty(), "{:?}", applied.changed);
    }
    assert_eq!(read_rows(&store, "g1"), before);
    // A real change: exactly that row.
    adapter.world.insert("a".into(), ("g1", Some(9.0), 100, "x"));
    let applied = apply_queued(&store, &adapter, "a");
    assert_eq!(applied.changed, [("s".to_owned(), "g1".to_owned())]);
    let weight = aggregate(Toy::REDUCERS, &read_rows(&store, "g1")).nums["weight"];
    assert_eq!(weight, Some(11.0));
    let unchanged: Vec<_> = read_rows(&store, "g1").into_iter().filter(|r| r.source.1 == "b").collect();
    assert_eq!(unchanged, before.into_iter().filter(|r| r.source.1 == "b").collect::<Vec<_>>());
    let queued: i64 = store.conn.query_row("SELECT count(*) FROM source_event", [], |r| r.get(0)).unwrap();
    assert_eq!(queued, 0, "the queue row went with the facts");
}

#[test]
fn a_reattribution_is_a_delete_and_an_upsert_in_one_apply() {
    let (_dir, store) = store();
    let mut adapter = toy();
    build(&store, &adapter, "g1");
    build(&store, &adapter, "g2");
    adapter.world.insert("a".into(), ("g2", Some(1.5), 100, "x"));
    let applied = apply_queued(&store, &adapter, "a");
    let mut changed = applied.changed;
    changed.sort();
    assert_eq!(changed, [("s".to_owned(), "g1".to_owned()), ("s".to_owned(), "g2".to_owned())]);
    assert_eq!(aggregate(Toy::REDUCERS, &read_rows(&store, "g1")).nums["items"], Some(1.0));
    assert_eq!(aggregate(Toy::REDUCERS, &read_rows(&store, "g2")).nums["items"], Some(2.0));
    // A source that belongs to nothing any more loses its rows.
    adapter.world.remove("a");
    apply_queued(&store, &adapter, "a");
    assert_eq!(aggregate(Toy::REDUCERS, &read_rows(&store, "g2")).nums["items"], Some(1.0));
}

#[test]
fn an_entity_whose_rows_are_not_built_is_reported_not_written() {
    let (_dir, store) = store();
    let adapter = toy();
    let applied = apply_queued(&store, &adapter, "a");
    assert!(applied.changed.is_empty());
    assert_eq!(applied.unbuilt, ["g1"], "the caller marks it, to be built whole when computed");
    let rows: i64 = store.conn.query_row("SELECT count(*) FROM contribution", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0);
}

/// A failure in the caller's step (the marks) rolls the whole apply back: rows, and the queue row.
#[test]
fn a_failing_mark_keeps_the_old_rows_and_the_queue_row() {
    let (_dir, store) = store();
    let mut adapter = toy();
    build(&store, &adapter, "g1");
    let before = read_rows(&store, "g1");
    adapter.world.insert("a".into(), ("g1", Some(9.0), 100, "x"));
    let queued = queue(&store, "a");
    let (source, wanted) = adapter.contributions(&store, &queued.source).unwrap();
    let failed = apply(&|_, _| Err("crash".into()), &store, Toy::KIND, Toy::VERSION, &queued, &source, &wanted, 2);
    assert_eq!(failed.unwrap_err(), "crash");
    assert_eq!(read_rows(&store, "g1"), before, "the new row was not kept");
    let queued_rows: i64 = store.conn.query_row("SELECT count(*) FROM source_event", [], |r| r.get(0)).unwrap();
    assert_eq!(queued_rows, 1, "the source stays queued, so the next sweep redoes it");
}

/// A lost update: an event that is queued while its source is being resolved keeps the queue row,
/// so the next sweep resolves the source again and sees the new facts.
#[test]
fn an_event_queued_during_resolution_keeps_the_queue_row() {
    let (_dir, store) = store();
    let mut adapter = toy();
    build(&store, &adapter, "g1");
    let read = queue(&store, "a");
    let (source, wanted) = adapter.contributions(&store, &read.source).unwrap();
    // The source changes and is queued again (its time moves on) before this apply runs.
    adapter.world.insert("a".into(), ("g1", Some(9.0), 100, "x"));
    store.conn.execute("UPDATE source_event SET enqueued_ms = 2 WHERE source_id = 'a'", []).unwrap();
    apply(&|_, _| Ok(()), &store, Toy::KIND, Toy::VERSION, &read, &source, &wanted, 3).unwrap();
    let left = queued(&store, Toy::SOURCES, 10).unwrap();
    assert_eq!(left, [Queued { source: read.source.clone(), enqueued_ms: 2 }], "the newer event is still queued");
    // The next sweep resolves it again, and sees the new numbers.
    let again = left[0].clone();
    let (source, wanted) = adapter.contributions(&store, &again.source).unwrap();
    apply(&|_, _| Ok(()), &store, Toy::KIND, Toy::VERSION, &again, &source, &wanted, 4).unwrap();
    assert!(queued(&store, Toy::SOURCES, 10).unwrap().is_empty());
    assert_eq!(aggregate(Toy::REDUCERS, &read_rows(&store, "g1")).nums["weight"], Some(11.0));
}

/// Only the kinds a kind is fed by are read, and rows of other kinds never take a place in the
/// limit (a flood of another kind's events cannot starve this kind).
#[test]
fn the_queue_is_read_by_the_kinds_a_kind_is_fed_by_before_the_limit() {
    let (_dir, store) = store();
    for n in 0..5 {
        store
            .conn
            .execute("INSERT INTO source_event (source_kind, source_id, enqueued_ms) VALUES ('other', ?1, 1)", params![format!("o{n}")])
            .unwrap();
    }
    queue(&store, "a");
    let got = queued(&store, Toy::SOURCES, 2).unwrap();
    assert_eq!(got.iter().map(|q| q.source.id.as_str()).collect::<Vec<_>>(), ["a"], "the five others did not use the limit");
    let others: i64 = store.conn.query_row("SELECT count(*) FROM source_event WHERE source_kind = 'other'", [], |r| r.get(0)).unwrap();
    assert_eq!(others, 5, "another kind's rows are left alone");
}

/// An entity's rows can be forgotten (unbuilt): it reads as not built, and the next apply reports
/// it as unbuilt rather than writing into it.
#[test]
fn unbuilding_an_entity_makes_it_read_as_not_built() {
    let (_dir, store) = store();
    let adapter = toy();
    build(&store, &adapter, "g1");
    build(&store, &adapter, "g2");
    unbuild(&store.conn, Toy::KIND, &BTreeSet::from(["g1".to_owned()])).unwrap();
    assert!(rows_of(&store, "s", Toy::KIND, "g1", Toy::VERSION).unwrap().is_none());
    assert!(rows_of(&store, "s", Toy::KIND, "g2", Toy::VERSION).unwrap().is_some());
    assert_eq!(apply_queued(&store, &adapter, "a").unbuilt, ["g1"]);
    unbuild_all(&store.conn, Toy::KIND).unwrap();
    assert!(rows_of(&store, "s", Toy::KIND, "g2", Toy::VERSION).unwrap().is_none());
}

#[test]
fn rows_are_kept_per_scope() {
    let (_dir, store) = store();
    let adapter = toy();
    let rows = adapter.rebuild(&store, &CallerScope::Human, "g1").unwrap();
    for scope in ["human", "team:a"] {
        replace_entity(&store, scope, Toy::KIND, "g1", &rows, Toy::VERSION, 1).unwrap();
    }
    let mut changed_adapter = toy();
    changed_adapter.world.insert("b".into(), ("g1", Some(7.0), 50, "y"));
    let queued = queue(&store, "b");
    let (source, wanted) = changed_adapter.contributions(&store, &queued.source).unwrap();
    let applied = apply(&|_, _| Ok(()), &store, Toy::KIND, Toy::VERSION, &queued, &source, &wanted, 2).unwrap();
    let mut scopes: Vec<_> = applied.changed.into_iter().map(|(scope, _)| scope).collect();
    scopes.sort();
    assert_eq!(scopes, ["human", "team:a"], "every scope that built the entity gets the row");
}

#[test]
fn a_fill_updates_a_source_s_rows_and_never_inserts_or_deletes() {
    let (_dir, store) = store();
    let adapter = toy();
    build(&store, &adapter, "g1");
    build(&store, &adapter, "g2");
    let first = queue(&store, "a");
    let mut counted = Contribution::new();
    counted.num("weight", Some(9.0), "");
    counted.extra.insert("counted".into(), json!(true));
    let marked = std::cell::RefCell::new(vec![]);
    let pairs = fill(&|_, filled| { marked.borrow_mut().extend(filled.to_vec()); Ok(()) }, &store, Toy::KIND, &first, &counted, 5).unwrap();
    assert_eq!(pairs, [("s".to_owned(), "g1".to_owned())]);
    assert_eq!(*marked.borrow(), pairs);
    let row = read_rows(&store, "g1").into_iter().find(|r| r.source.1 == "a").unwrap().contribution;
    assert_eq!(row.nums["weight"], Some(9.0));
    assert_eq!(row.extra["tag"], "x", "the patch is merged, not replaced");
    assert_eq!(row.extra["counted"], true);
    assert_eq!(row.first_ms, Some(100), "times are the walk's");
    assert_eq!(read_rows(&store, "g1").len(), 2, "no row added or removed");
    let queue_rows: i64 = store.conn.query_row("SELECT count(*) FROM source_event", [], |r| r.get(0)).unwrap();
    assert_eq!(queue_rows, 0);

    // A source no entity has: nothing is inserted, and the queue row still goes.
    let ghost = queue(&store, "ghost");
    assert!(fill(&|_, _| Ok(()), &store, Toy::KIND, &ghost, &counted, 6).unwrap().is_empty());
    let total: i64 = store.conn.query_row("SELECT count(*) FROM contribution WHERE source_id = 'ghost'", [], |r| r.get(0)).unwrap();
    assert_eq!(total, 0);

    // Queued again since it was read: the newer event keeps its queue row.
    let again = queue(&store, "b");
    store.conn.execute("UPDATE source_event SET enqueued_ms = 2 WHERE source_id = 'b'", []).unwrap();
    fill(&|_, _| Ok(()), &store, Toy::KIND, &again, &counted, 7).unwrap();
    assert_eq!(queued(&store, Toy::SOURCES, 10).unwrap().len(), 1);
}

#[test]
fn a_failed_source_waits_and_a_new_event_starts_it_over() {
    let (_dir, store) = store();
    let first = queue(&store, "a");
    assert_eq!(fail(&store, &first, 1_000, &|n| backoff_ms(n, 100, 1_000)).unwrap(), Some(1));
    assert!(queued_due(&store, Toy::SOURCES, 10, 1_099, false).unwrap().is_empty(), "not due inside the wait");
    assert_eq!(queued_due(&store, Toy::SOURCES, 10, 1_100, false).unwrap().len(), 1);
    assert_eq!(fail(&store, &first, 1_100, &|n| backoff_ms(n, 100, 1_000)).unwrap(), Some(2));
    assert!(queued_due(&store, Toy::SOURCES, 10, 1_299, false).unwrap().is_empty(), "the wait doubled");
    assert_eq!([backoff_ms(1, 100, 1_000), backoff_ms(4, 100, 1_000), backoff_ms(9, 100, 1_000)], [100, 800, 1_000], "capped");
    let source = Source { kind: ITEM, id: "a".into() };
    enqueue(&store.conn, &source, 5_000).unwrap();
    let due = queued_due(&store, Toy::SOURCES, 10, 5_000, false).unwrap();
    assert_eq!(due.len(), 1, "a new event is due at once, with its attempts reset");
    assert_eq!(fail(&store, &first, 5_000, &|n| backoff_ms(n, 100, 1_000)).unwrap(), None, "a failure of the old read changes nothing");
}

#[test]
fn due_sources_can_be_taken_newest_first() {
    let (_dir, store) = store();
    for (id, at) in [("old", 1), ("new", 9), ("mid", 5)] {
        store
            .conn
            .execute("INSERT INTO source_event (source_kind, source_id, enqueued_ms) VALUES (?1, ?2, ?3)", params![ITEM, id, at])
            .unwrap();
    }
    let ids = |newest| queued_due(&store, Toy::SOURCES, 10, 100, newest).unwrap().into_iter().map(|q| q.source.id).collect::<Vec<_>>();
    assert_eq!(ids(false), ["old", "mid", "new"]);
    assert_eq!(ids(true), ["new", "mid", "old"]);
}
