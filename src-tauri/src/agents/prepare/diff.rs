// SPDX-License-Identifier: MIT
//! SW-XS-7: the one classification both destinations share (SPEC §4, §11.1).
//!
//! | Destination holds | Managed? | Result |
//! |---|---|---|
//! | nothing | – | **add** |
//! | the wanted bytes, wrong mode | yes | **chmod** |
//! | the wanted bytes | no | left as it is: never adopted, never chmodded |
//! | what the engine last wrote, now stale | yes | **replace** |
//! | other bytes | yes | **kept (modified)** — never overwritten or removed |
//! | other bytes | no | **kept (unmanaged)** — never overwritten |
//! | a tracked file | – | **skipped (tracked by repo)** — never written |
//! | a symlink, directory or other type | – | skipped, with the reason |
//!
//! A launch never removes. Only an explicit prune removes managed files that
//! are no longer wanted and still hold what the engine wrote.
use std::collections::{BTreeMap, BTreeSet};

use super::record::{ManagedEntry, LEGACY_ORIGIN};

/// What the destination holds at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    Absent,
    File {
        hash: [u8; 32],
        mode: u32,
    },
    /// A symlink, a directory where a file is wanted, a nested repository,
    /// an unstable or unsafe file: the diff never touches it.
    Other {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub state: Observed,
    /// In the destination's Git index (SPEC §11.3).
    pub tracked: bool,
}

/// One wanted file, as the diff sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted<'a> {
    /// Its index in the want set: what [`Plan::steps`] names.
    pub index: usize,
    pub dest: &'a str,
    pub hash: [u8; 32],
    pub mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Add,
    /// Replace a stale managed file still holding `expected`.
    Replace {
        expected: [u8; 32],
    },
    /// Bytes already right; set the mode.
    Chmod {
        hash: [u8; 32],
    },
    /// Bytes and mode already right.
    Unchanged,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// `(want index, step)` for every wanted file not kept or skipped.
    pub steps: Vec<(usize, Step)>,
    pub kept_modified: Vec<String>,
    pub kept_unmanaged: Vec<String>,
    pub skipped_tracked: Vec<String>,
    pub skipped_other: Vec<(String, String)>,
    /// Prune only: managed, unwanted, unmodified: `(path, expected hash)`.
    pub removes: Vec<(String, [u8; 32])>,
    /// Managed entries whose file is gone and that nobody wants: dropped.
    pub forget: Vec<String>,
    /// Unwanted managed files the old projection wrote: offered to Clean up.
    pub leftovers: Vec<String>,
}

impl Plan {
    pub fn writes(&self) -> impl Iterator<Item = &(usize, Step)> {
        self.steps
            .iter()
            .filter(|(_, step)| matches!(step, Step::Add | Step::Replace { .. }))
    }
}

/// How a run treats what it finds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rules<'a> {
    /// Remove unwanted unmodified managed files (explicit only).
    pub prune: bool,
    /// SW-XS-8 (review r1 M2): a prune only reaches managed paths under these
    /// prefixes (the explicit call's paths / sets, or the runtime defaults it
    /// hydrated). `None`: every managed path.
    pub prune_under: Option<&'a [String]>,
    /// SW-XS-8: replace a wanted file the workspace holds differently,
    /// modified or unmanaged (never a tracked one): the explicit recovery
    /// (`overwriteModified`, "Restore pinned files").
    pub overwrite_modified: bool,
}

/// Classify every wanted file, and (for a prune) every unwanted managed one.
/// `observe` answers for any path; `unwanted` names the managed paths no
/// wanted file covers that were observed (a prune observes them all).
pub fn classify(
    wanted: &[Wanted<'_>],
    observe: &dyn Fn(&str) -> Observation,
    managed: &BTreeMap<String, ManagedEntry>,
    rules: Rules<'_>,
) -> Plan {
    let prune = rules.prune;
    let mut plan = Plan::default();
    let wanted_paths: BTreeSet<&str> = wanted.iter().map(|w| w.dest).collect();
    for want in wanted {
        let index = want.index;
        let seen = observe(want.dest);
        if seen.tracked {
            plan.skipped_tracked.push(want.dest.to_string());
            continue;
        }
        let record = managed.get(want.dest);
        match seen.state {
            Observed::Absent => plan.steps.push((index, Step::Add)),
            Observed::Other { reason } => plan.skipped_other.push((want.dest.to_string(), reason)),
            Observed::File { hash, mode } if hash == want.hash => {
                // An unmanaged file is never chmodded (nor adopted).
                let step = if mode != want.mode && record.is_some() {
                    Step::Chmod { hash }
                } else {
                    Step::Unchanged
                };
                plan.steps.push((index, step));
            }
            Observed::File { hash, .. } => match record {
                Some(entry) if entry.hash.0 == hash => {
                    plan.steps.push((index, Step::Replace { expected: hash }))
                }
                _ if rules.overwrite_modified => {
                    plan.steps.push((index, Step::Replace { expected: hash }))
                }
                Some(_) => plan.kept_modified.push(want.dest.to_string()),
                None => plan.kept_unmanaged.push(want.dest.to_string()),
            },
        }
    }
    for (path, entry) in managed {
        if wanted_paths.contains(path.as_str()) {
            continue;
        }
        if !prune {
            if entry.origin == LEGACY_ORIGIN {
                plan.leftovers.push(path.clone());
            }
            continue;
        }
        let in_scope = rules.prune_under.is_none_or(|scope| {
            scope.iter().any(|prefix| {
                path == prefix
                    || path
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|tail| tail.starts_with('/'))
            })
        });
        if !in_scope {
            continue;
        }
        let seen = observe(path);
        match seen.state {
            Observed::Absent => plan.forget.push(path.clone()),
            _ if seen.tracked => plan.skipped_tracked.push(path.clone()),
            Observed::File { hash, .. } if hash == entry.hash.0 => {
                plan.removes.push((path.clone(), hash))
            }
            Observed::File { .. } => plan.kept_modified.push(path.clone()),
            Observed::Other { reason } => plan.skipped_other.push((path.clone(), reason)),
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::super::record::Hash32;
    use super::*;

    fn h(byte: u8) -> [u8; 32] {
        [byte; 32]
    }
    fn managed(entries: &[(&str, u8, &str)]) -> BTreeMap<String, ManagedEntry> {
        entries
            .iter()
            .map(|(path, hash, origin)| {
                (
                    path.to_string(),
                    ManagedEntry {
                        source_context: None,
                        hash: Hash32(h(*hash)),
                        origin: origin.to_string(),
                        source_layer: "team:o/t".into(),
                        mode: 0o644,
                        dest_stat: None,
                        source_stat: None,
                        source: None,
                    },
                )
            })
            .collect()
    }
    fn file(hash: u8) -> Observation {
        Observation {
            state: Observed::File {
                hash: h(hash),
                mode: 0o644,
            },
            tracked: false,
        }
    }

    /// The launch rule (§11.1): add missing, replace unmodified stale, never
    /// remove, never touch modified, unmanaged or tracked files.
    #[test]
    fn launch_adds_and_replaces_but_never_removes_or_touches_edits() {
        let wanted = [
            Wanted {
                index: 0,
                dest: "a",
                hash: h(1),
                mode: 0o644,
            }, // absent → add
            Wanted {
                index: 1,
                dest: "b",
                hash: h(2),
                mode: 0o644,
            }, // stale managed → replace
            Wanted {
                index: 2,
                dest: "c",
                hash: h(3),
                mode: 0o644,
            }, // edited managed → kept
            Wanted {
                index: 3,
                dest: "d",
                hash: h(4),
                mode: 0o644,
            }, // unmanaged other → kept
            Wanted {
                index: 4,
                dest: "e",
                hash: h(5),
                mode: 0o644,
            }, // tracked → skipped
            Wanted {
                index: 5,
                dest: "f",
                hash: h(6),
                mode: 0o755,
            }, // mode only → chmod
            Wanted {
                index: 6,
                dest: "g",
                hash: h(7),
                mode: 0o644,
            }, // same → unchanged
        ];
        let records = managed(&[
            ("b", 20, "step2:x"),
            ("c", 30, "step2:x"),
            ("f", 6, "step2:x"),
            ("old", 9, LEGACY_ORIGIN),
            ("mine", 9, "step3:op"),
        ]);
        let observe = |path: &str| match path {
            "a" => Observation {
                state: Observed::Absent,
                tracked: false,
            },
            "b" => file(20),
            "c" => file(31),
            "d" => file(40),
            "e" => Observation {
                state: Observed::File {
                    hash: h(50),
                    mode: 0o644,
                },
                tracked: true,
            },
            "f" => file(6),
            "g" => file(7),
            other => panic!("a launch observed the unwanted {other}"),
        };
        let plan = classify(&wanted, &observe, &records, Rules::default());
        assert_eq!(
            plan.steps,
            vec![
                (0, Step::Add),
                (1, Step::Replace { expected: h(20) }),
                (5, Step::Chmod { hash: h(6) }),
                (6, Step::Unchanged),
            ]
        );
        assert_eq!(plan.kept_modified, vec!["c"]);
        assert_eq!(plan.kept_unmanaged, vec!["d"]);
        assert_eq!(plan.skipped_tracked, vec!["e"]);
        assert!(plan.removes.is_empty(), "a launch never removes");
        assert_eq!(
            plan.leftovers,
            vec!["old"],
            "only the old projection's files are leftovers"
        );
    }

    /// SHOULD 14: a file CodeSense did not write is never chmodded.
    #[test]
    fn an_identical_unmanaged_file_is_left_alone() {
        let wanted = [Wanted {
            index: 0,
            dest: "u",
            hash: h(1),
            mode: 0o755,
        }];
        let plan = classify(&wanted, &|_| file(1), &BTreeMap::new(), Rules::default());
        assert_eq!(plan.steps, vec![(0, Step::Unchanged)]);
    }

    /// An explicit prune removes unwanted managed files that are unmodified,
    /// keeps edited ones and forgets ones already gone.
    #[test]
    fn prune_removes_only_unmodified_unwanted_managed_files() {
        let records = managed(&[
            ("x", 1, "step2:a"),
            ("y", 2, "step2:a"),
            ("z", 3, LEGACY_ORIGIN),
        ]);
        let observe = |path: &str| match path {
            "x" => file(1),
            "y" => file(22),
            _ => Observation {
                state: Observed::Absent,
                tracked: false,
            },
        };
        let plan = classify(
            &[],
            &observe,
            &records,
            Rules {
                prune: true,
                ..Rules::default()
            },
        );
        assert_eq!(plan.removes, vec![("x".to_string(), h(1))]);
        assert_eq!(plan.kept_modified, vec!["y"]);
        assert_eq!(plan.forget, vec!["z"]);
    }

    /// SW-XS-8: `overwriteModified` replaces a modified managed file and an
    /// unmanaged one, never a tracked one.
    #[test]
    fn overwrite_modified_replaces_modified_and_unmanaged_never_tracked() {
        let wanted = [
            Wanted {
                index: 0,
                dest: "c",
                hash: h(3),
                mode: 0o644,
            },
            Wanted {
                index: 1,
                dest: "d",
                hash: h(4),
                mode: 0o644,
            },
            Wanted {
                index: 2,
                dest: "e",
                hash: h(5),
                mode: 0o644,
            },
        ];
        let records = managed(&[("c", 30, "step2:x")]);
        let observe = |path: &str| match path {
            "c" => file(31),
            "d" => file(40),
            _ => Observation {
                state: Observed::File {
                    hash: h(50),
                    mode: 0o644,
                },
                tracked: true,
            },
        };
        let plan = classify(
            &wanted,
            &observe,
            &records,
            Rules {
                overwrite_modified: true,
                ..Rules::default()
            },
        );
        assert_eq!(
            plan.steps,
            vec![
                (0, Step::Replace { expected: h(31) }),
                (1, Step::Replace { expected: h(40) }),
            ]
        );
        assert!(plan.kept_modified.is_empty() && plan.kept_unmanaged.is_empty());
        assert_eq!(plan.skipped_tracked, vec!["e"]);
    }
}
