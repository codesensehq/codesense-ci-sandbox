// SPDX-License-Identifier: MIT
//! SLW-NC-7: the git-shared **team nav pins** store. Pins live in the `pin`
//! list of `designations.yml` under the team content root
//! (`_team/{team}/content/designations.yml`), which is git-versioned and pulled
//! by every teammate.
//!
//! SW-XS-5 (SPEC §2 X4): directory ownership is gone, so pins are the only
//! thing this file means. Older clients also wrote ownership keys
//! (`context`, `install`, `by-source`) into the same file. Until the minimum
//! supported client version passes this release (SPEC §11.11), those keys are
//! left untouched: the loader type has no field for them, and a pin write
//! rewrites only the `pin` key and carries every other key through verbatim.

use std::path::{Path, PathBuf};

use log::{debug, warn};
use serde::{Deserialize, Serialize};

/// The git-shared manifest that holds the team nav pins, a sibling of
/// `projections.yml` under the team's `content/` dir. The name predates pins
/// (older clients kept directory ownership here) and stays so pins keep
/// working across versions.
pub const TEAM_PINS_FILENAME: &str = "designations.yml";

const PIN_KEY: &str = "pin";

/// The loader type: only the team nav pins. Every other key in the file is
/// ignored on read (and preserved on write, see [`set_team_pin`]).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TeamPinsDoc {
    /// Team-shared nav pins: full repo-relative paths (files or dirs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pin: Vec<String>,
}

impl TeamPinsDoc {
    /// The team-shared nav pins, normalized (trimmed, slash-canonical, deduped,
    /// empties dropped) so a malformed on-disk entry never reaches the UI as a
    /// phantom pin. Order is whatever was written (the store keeps it sorted for
    /// stable diffs); the nav re-orders for display anyway.
    pub fn team_pins(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for raw in &self.pin {
            let p = normalize_pin_path(raw);
            if !p.is_empty() && !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }
}

/// The manifest path under a content dir.
fn pins_path(content_dir: &Path) -> PathBuf {
    content_dir.join(TEAM_PINS_FILENAME)
}

/// Read the raw file. A missing file is the common no-pins case (`None`); an
/// unreadable one is logged and treated the same. Never errors.
fn read_raw(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(target: "team_pins", "no team pins at {}", path.display());
            None
        }
        Err(e) => {
            warn!(target: "team_pins", "cannot read {}: {} — no team pins", path.display(), e);
            None
        }
    }
}

/// Load the team nav pins from `{content_dir}/designations.yml`. Tolerant: a
/// missing file is the empty doc, and a malformed one is logged and treated as
/// empty, so a broken file never breaks the nav render. Never returns an error.
pub fn load_team_pins(content_dir: &Path) -> TeamPinsDoc {
    let path = pins_path(content_dir);
    let Some(raw) = read_raw(&path) else {
        return TeamPinsDoc::default();
    };
    match serde_yaml::from_str::<TeamPinsDoc>(&raw) {
        Ok(doc) => doc,
        Err(e) => {
            warn!(target: "team_pins", "malformed {}: {} — no team pins", path.display(), e);
            TeamPinsDoc::default()
        }
    }
}

/// Canonicalize a nav pin path — trim, forward-slash separators, and strip
/// leading/trailing slashes — so `"foo/bar/"`, `"/foo/bar"`, and `"foo\\bar"`
/// all key the same pin. Repo-relative; empties out to `""` (rejected/ignored).
pub fn normalize_pin_path(path: &str) -> String {
    path.trim().replace('\\', "/").trim_matches('/').to_string()
}

/// Serializes pin writes in this process so two concurrent read-modify-writes
/// can't lose each other's pin.
static PIN_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The existing pins in a raw `pin:` value, or an error when the value is not a
/// list of strings (refused rather than silently dropped).
fn existing_pins(value: Option<&serde_yaml::Value>, file: &Path) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let refuse = || {
        format!(
            "refusing to rewrite {}: its `pin:` is not a list of paths",
            file.display()
        )
    };
    let serde_yaml::Value::Sequence(items) = value else {
        return Err(refuse());
    };
    items
        .iter()
        .map(|item| item.as_str().map(str::to_string).ok_or_else(refuse))
        .collect()
}

/// SLW-NC-7 (AC-1): add or remove one team nav pin in `{content_dir}/designations.yml`.
/// Read-modify-write, serialized in-process and written atomically (temp file +
/// rename):
/// - `pinned = true` adds the (normalized) path; `false` removes it;
/// - the list is kept deduped and sorted, so a re-pin is idempotent and the git
///   diff is stable regardless of pin order;
/// - a path that normalizes empty is rejected;
/// - every key other than `pin` is carried through untouched (SW-XS-5);
/// - a file that isn't a YAML mapping, or whose `pin:` isn't a list of paths, is
///   refused with an error and left as it is — never overwritten.
///
/// Returns the written path. Creates `content_dir` if absent.
pub fn set_team_pin(content_dir: &Path, path: &str, pinned: bool) -> Result<PathBuf, String> {
    let p = normalize_pin_path(path);
    if p.is_empty() {
        return Err(format!("pin path must not be empty (got {path:?})"));
    }
    let _guard = PIN_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let file = pins_path(content_dir);
    let mut doc = match std::fs::read_to_string(&file) {
        Ok(raw) => match serde_yaml::from_str::<serde_yaml::Value>(&raw) {
            Ok(serde_yaml::Value::Mapping(map)) => map,
            Ok(serde_yaml::Value::Null) => serde_yaml::Mapping::new(),
            Ok(_) | Err(_) => {
                return Err(format!(
                    "refusing to rewrite {}: it is not a YAML mapping",
                    file.display()
                ))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_yaml::Mapping::new(),
        Err(e) => return Err(format!("failed to read {}: {}", file.display(), e)),
    };

    let key = serde_yaml::Value::from(PIN_KEY);
    let mut pins: Vec<String> = Vec::new();
    for raw in existing_pins(doc.get(&key), &file)? {
        let existing = normalize_pin_path(&raw);
        if !existing.is_empty() {
            pins.push(existing);
        }
    }
    pins.retain(|existing| existing != &p);
    if pinned {
        pins.push(p);
    }
    pins.sort();
    pins.dedup();

    if pins.is_empty() {
        doc.remove(&key);
    } else {
        let list = pins.into_iter().map(serde_yaml::Value::from).collect();
        if let Some(slot) = doc.get_mut(&key) {
            *slot = serde_yaml::Value::Sequence(list);
        } else {
            doc.insert(key, serde_yaml::Value::Sequence(list));
        }
    }

    let yaml =
        serde_yaml::to_string(&doc).map_err(|e| format!("failed to serialize team pins: {e}"))?;
    crate::shared::yaml_io::write_yaml_bytes_atomic(&file, yaml.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", file.display(), e))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// SW-XS-5 (X4, r1): the loader type carries ONLY team nav pins. A file
    /// full of designation keys loads, and nothing designation-shaped survives
    /// into the in-memory doc.
    #[test]
    fn xs5_loader_type_has_no_designation_field() {
        let yaml = "context:\n  - docs\ninstall:\n  - .claude\nby-source:\n  alderco/codesense:\n    context:\n      - scratch\npin:\n  - a/b\n";
        let doc: TeamPinsDoc = serde_yaml::from_str(yaml).unwrap();
        let value = serde_yaml::to_value(&doc).unwrap();
        let keys: Vec<String> = value
            .as_mapping()
            .expect("the doc serializes as a mapping")
            .keys()
            .map(|k| k.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            keys,
            vec!["pin".to_string()],
            "the loader type holds only `pin`"
        );
    }

    /// SW-XS-5 AC1 (r1): designation keys are left untouched — neither read
    /// nor written — when a pin changes, so an older client that still reads
    /// them sees exactly what it wrote. Pins keep working.
    #[test]
    fn xs5_pin_write_leaves_designation_keys_untouched() {
        let dir = tmp();
        let original = "context:\n  - ' docs '\n  - docs\ninstall: []\nby-source:\n  alderco/codesense:\n    context:\n      - scratch\n    install: []\nx-future-key: 7\npin:\n  - z/old\n";
        std::fs::write(pins_path(dir.path()), original).unwrap();
        set_team_pin(dir.path(), "a/new", true).unwrap();

        let before: serde_yaml::Mapping = serde_yaml::from_str(original).unwrap();
        let after: serde_yaml::Mapping =
            serde_yaml::from_str(&std::fs::read_to_string(pins_path(dir.path())).unwrap()).unwrap();
        for key in ["context", "install", "by-source", "x-future-key"] {
            assert_eq!(
                after.get(key),
                before.get(key),
                "`{key}` must be carried through a pin write verbatim"
            );
        }
        assert_eq!(
            load_team_pins(dir.path()).team_pins(),
            vec!["a/new".to_string(), "z/old".to_string()]
        );
    }

    /// Unpinning the last pin drops the `pin` key and still leaves the other
    /// keys alone.
    #[test]
    fn xs5_last_unpin_drops_only_the_pin_key() {
        let dir = tmp();
        std::fs::write(
            pins_path(dir.path()),
            "install:\n  - .claude\npin:\n  - a/b\n",
        )
        .unwrap();
        set_team_pin(dir.path(), "a/b", false).unwrap();
        let after: serde_yaml::Mapping =
            serde_yaml::from_str(&std::fs::read_to_string(pins_path(dir.path())).unwrap()).unwrap();
        assert!(after.get("pin").is_none());
        assert_eq!(
            after.get("install"),
            Some(&serde_yaml::Value::Sequence(vec![".claude".into()]))
        );
    }

    #[test]
    fn missing_file_has_no_pins() {
        let dir = tmp();
        assert_eq!(load_team_pins(dir.path()), TeamPinsDoc::default());
    }

    #[test]
    fn malformed_file_degrades_to_empty_on_read() {
        let dir = tmp();
        std::fs::write(pins_path(dir.path()), "- not a mapping\n").unwrap();
        assert_eq!(load_team_pins(dir.path()), TeamPinsDoc::default());
    }

    /// SW-XS-5 (review): a pin write never overwrites a file it can't parse as
    /// a mapping — it refuses and leaves the bytes alone.
    #[test]
    fn xs5_pin_write_refuses_a_non_mapping_file() {
        let dir = tmp();
        for original in ["- not a mapping\n", "pin: [unclosed\n", "just a string\n"] {
            std::fs::write(pins_path(dir.path()), original).unwrap();
            let err = set_team_pin(dir.path(), "a/b", true).unwrap_err();
            assert!(err.contains("refusing"), "{err}");
            assert_eq!(
                std::fs::read_to_string(pins_path(dir.path())).unwrap(),
                original,
                "the file is left untouched"
            );
        }
    }

    /// SW-XS-5 (review): a wrong-typed `pin:` is refused, never replaced by a
    /// list that silently drops what was there.
    #[test]
    fn xs5_pin_write_refuses_a_wrong_typed_pin_list() {
        let dir = tmp();
        for original in ["pin: a/b\n", "pin:\n  - a/b\n  - {x: 1}\n"] {
            std::fs::write(pins_path(dir.path()), original).unwrap();
            assert!(set_team_pin(dir.path(), "c/d", true).is_err());
            assert_eq!(
                std::fs::read_to_string(pins_path(dir.path())).unwrap(),
                original
            );
        }
    }

    /// SW-XS-5 (review): concurrent pin writes are serialized — none is lost.
    #[test]
    fn xs5_concurrent_pin_writes_lose_nothing() {
        let dir = tmp();
        let root = dir.path().to_path_buf();
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let root = root.clone();
                std::thread::spawn(move || set_team_pin(&root, &format!("p/{i:02}"), true).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(load_team_pins(&root).team_pins().len(), 16);
    }

    #[test]
    fn team_pin_round_trips_sorted() {
        let dir = tmp();
        set_team_pin(dir.path(), "_bmad-output/some/epic", true).unwrap();
        set_team_pin(dir.path(), "_bmad-output/a/file.md", true).unwrap();
        assert_eq!(
            load_team_pins(dir.path()).team_pins(),
            vec![
                "_bmad-output/a/file.md".to_string(),
                "_bmad-output/some/epic".to_string()
            ],
            "pins stored sorted; both a dir and a file path pin"
        );
    }

    #[test]
    fn team_pin_unpin_removes_only_the_target() {
        let dir = tmp();
        set_team_pin(dir.path(), "a/one", true).unwrap();
        set_team_pin(dir.path(), "a/two", true).unwrap();
        set_team_pin(dir.path(), "a/one", false).unwrap();
        assert_eq!(
            load_team_pins(dir.path()).team_pins(),
            vec!["a/two".to_string()]
        );
    }

    #[test]
    fn team_pin_is_idempotent_and_path_normalized() {
        let dir = tmp();
        set_team_pin(dir.path(), "a/b/", true).unwrap();
        set_team_pin(dir.path(), "/a/b", true).unwrap();
        set_team_pin(dir.path(), "a\\b", true).unwrap();
        assert_eq!(
            load_team_pins(dir.path()).team_pins(),
            vec!["a/b".to_string()],
            "re-pinning the same path (any slash shape) never duplicates"
        );
        set_team_pin(dir.path(), "a/b", false).unwrap();
        assert!(load_team_pins(dir.path()).team_pins().is_empty());
    }

    #[test]
    fn empty_pin_path_is_rejected() {
        let dir = tmp();
        assert!(set_team_pin(dir.path(), "   ", true).is_err());
        assert!(set_team_pin(dir.path(), "///", true).is_err());
    }
}
