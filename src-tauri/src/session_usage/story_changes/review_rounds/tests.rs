// SPDX-License-Identifier: MIT
use super::*;
use sha2::{Digest, Sha256};

/// The fixture `check_records.py` is pinned to as well: the context repo's
/// `reviews/scripts/tests/fixtures/review-rounds` is a byte-identical copy, and
/// `reviews/scripts/tests/test_review_rounds_fixture.py` pins this same digest. Change a marker
/// or `expected.json` and both digests must move together.
const FIXTURE_SHA256: &str = "5489ec5a3c1deac4221443b305be37d08cbd0733af4ce24ad14f3f1b70979c54";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/session_usage/story_changes/fixtures/review-rounds")
}

fn expected() -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture().join("expected.json")).unwrap())
        .unwrap()
}

fn records() -> Vec<Record> {
    team_records(&fixture(), &CallerScope::Human)
        .unwrap()
        .expect("the fixture declares review-rounds")
}

#[test]
fn the_fixture_is_the_one_check_records_is_pinned_to() {
    let root = fixture();
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("content/reviews/rounds"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.push(root.join("expected.json"));
    let mut named: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|p| {
            (
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                p,
            )
        })
        .collect();
    named.sort();
    let mut digest = Sha256::new();
    for (name, path) in named {
        digest.update(name.as_bytes());
        digest.update(b"\n");
        digest.update(std::fs::read(path).unwrap());
        digest.update(b"\n");
    }
    assert_eq!(format!("{:x}", digest.finalize()), FIXTURE_SHA256);
}

fn ids(value: &Value) -> BTreeSet<String> {
    serde_json::from_value(value.clone()).unwrap()
}

/// Cases 7, 8 and 13: the review rounds per story are exactly `check_records.py`'s
/// `review_rounds(load_snapshot(...), story)`, the complete markers its `load_snapshot(...).rounds`
/// and the in-flight ones its `inflight`.
#[test]
fn review_rounds_per_story_match_check_records() {
    let records = records();
    let expected = expected();
    for (story, want) in expected["reviewRounds"].as_object().unwrap() {
        let got: Vec<String> = review_rounds_for_story(&records, story)
            .into_iter()
            .map(|m| m.id)
            .collect();
        let want: Vec<String> = serde_json::from_value(want.clone()).unwrap();
        assert_eq!(got, want, "{story}");
    }
    let (complete, inflight) = (ids(&expected["complete"]), ids(&expected["inflight"]));
    assert_eq!(complete.len() + inflight.len(), records.len());
    for record in &records {
        let is_complete = missing_stamps(&record.fields).is_empty();
        assert_eq!(is_complete, complete.contains(&record.id), "{}", record.id);
        assert_eq!(!is_complete, inflight.contains(&record.id), "{}", record.id);
    }
}

/// Case 13 (walk 1): a story's six recorded rounds (DEMO-1, a fake id, so no
/// query over the team root ever counts the fixture as real rounds) count, though none names a review task
/// and every reviewer task was typed `build` (why today's task-based count reads 0).
#[test]
fn eng_108_shaped_markers_give_six_rounds() {
    let markers = review_rounds_for_story(&records(), "DEMO-1");
    assert_eq!(markers.len(), 6);
    assert!(markers.iter().all(|m| m.task.is_some()));
    assert_eq!(review_rounds_for_story(&records(), " demo-1 ").len(), 6);
}

/// Case 8: a document round and a human-lens round with no review task count; a complete wave
/// or use observation (round 0) is not a review round (manager, 2026-10-29).
#[test]
fn document_and_human_rounds_count_and_wave_or_use_observations_do_not() {
    let records = records();
    let rounds: Vec<String> = review_rounds_for_story(&records, "DEMO-2")
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(rounds, vec!["demo2-doc-r1", "demo2-human-r2"]);
    for observation in ["demo2-use-r0", "demo2-wave-r0"] {
        let marker = records.iter().find(|r| r.id == observation).unwrap();
        assert!(
            missing_stamps(&marker.fields).is_empty(),
            "{observation} is complete"
        );
        assert!(
            !is_review_round(&marker.fields),
            "{observation} is not a review round"
        );
    }
}

#[test]
fn stamp_note_names_a_key_only_as_a_whole_word() {
    assert!(names("author, authorRevision: unknown", "author"));
    assert!(names("author, authorRevision: unknown", "authorRevision"));
    assert!(!names("authors, authorRevisions", "author"));
    assert!(!names("authorRevision", "author"));
    assert!(!names("coauthor", "author"));
    assert!(names("(size)", "size"));
    assert!(!names("size_unit", "size"));
}

#[test]
fn present_means_non_blank_text_and_numbers_count() {
    let mut fields: BTreeMap<String, Value> = STAMP_KEYS
        .iter()
        .map(|k| ((*k).to_owned(), Value::from("x")))
        .collect();
    assert!(missing_stamps(&fields).is_empty());
    fields.insert("size".into(), Value::from(0));
    fields.insert("authorRevision".into(), Value::from(3));
    assert!(missing_stamps(&fields).is_empty());
    fields.insert("epic".into(), Value::from("  "));
    fields.remove("teamKey");
    assert_eq!(missing_stamps(&fields), vec!["epic", "teamKey"]);
    fields.insert("stampNote".into(), Value::from("epic, teamKey: unknown"));
    assert!(missing_stamps(&fields).is_empty());
}

/// Case 9: a closed review task with no marker, and a marker whose task is not a review
/// task, each give a warning; agreement gives none.
#[test]
fn mismatches_are_warnings() {
    let marker = |id: &str, task: Option<&str>| Marker {
        id: id.into(),
        task: task.map(str::to_owned),
    };
    let closed: BTreeSet<String> = ["review-1".to_owned()].into();
    let types: BTreeMap<String, String> = [
        ("review-1".to_owned(), "review".to_owned()),
        ("dev".to_owned(), "build".to_owned()),
    ]
    .into();
    assert_eq!(
        mismatch(&[marker("m1", Some("review-1"))], &closed, &types),
        None
    );
    assert_eq!(
        mismatch(&[marker("doc", None)], &BTreeSet::new(), &types),
        None
    );
    assert_eq!(
        mismatch(
            &[marker("elsewhere", Some("not-here"))],
            &BTreeSet::new(),
            &types
        ),
        None
    );
    let unrecorded = mismatch(&[], &closed, &types).unwrap();
    assert!(unrecorded.contains("1 closed review task(s) have no complete review-rounds marker"));
    let mistyped = mismatch(
        &[marker("m1", Some("review-1")), marker("m2", Some("dev"))],
        &closed,
        &types,
    )
    .unwrap();
    assert_eq!(
        mistyped,
        "1 complete marker(s) name a task that is not a review task"
    );
}
