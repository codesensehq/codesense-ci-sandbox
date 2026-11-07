// SPDX-License-Identifier: MIT
//! `pack.yaml`: a pack's display name and its faces.
//!
//! ```yaml
//! name: Office Legends
//! faces:
//!   - id: grumpy
//!     file: grumpy.png
//!     label: Grumpy
//! ```
//!
//! Reading is tolerant: a face entry that is not a mapping, has a bad id or no
//! file is skipped with a warning. Writing keeps every key and entry it does
//! not understand, so an add never loses what a person put there by hand.

use serde_yaml::{Mapping, Value};

use super::{is_valid_id, FACE_ID_MAX, MAX_FACES_PER_PACK};

/// One face as the manifest lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceEntry {
    pub id: String,
    /// The image file, relative to the pack folder.
    pub file: String,
    pub label: Option<String>,
}

/// A parsed manifest, kept whole for rewriting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Manifest {
    root: Mapping,
}

impl Manifest {
    /// Parse `text`. Fails only when it is not YAML or not a mapping, or
    /// `faces` is not a list.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let value: Value =
            serde_yaml::from_str(text).map_err(|e| format!("not valid YAML: {e}"))?;
        let root = match value {
            Value::Mapping(map) => map,
            Value::Null => Mapping::new(),
            _ => return Err("pack.yaml must be a mapping with `name` and `faces`".to_string()),
        };
        match root.get("faces") {
            None | Some(Value::Null) | Some(Value::Sequence(_)) => Ok(Self { root }),
            Some(_) => Err("`faces` must be a list".to_string()),
        }
    }

    /// The pack's display name, when it has one.
    pub fn name(&self) -> Option<String> {
        match self.root.get("name") {
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        }
    }

    pub fn set_name_if_missing(&mut self, name: &str) {
        if self.name().is_none() {
            self.root
                .insert(Value::from("name"), Value::from(name.to_string()));
        }
    }

    fn faces(&self) -> &[Value] {
        match self.root.get("faces") {
            Some(Value::Sequence(seq)) => seq,
            _ => &[],
        }
    }

    fn faces_mut(&mut self) -> &mut Vec<Value> {
        let key = Value::from("faces");
        if !matches!(self.root.get(&key), Some(Value::Sequence(_))) {
            self.root.insert(key.clone(), Value::Sequence(Vec::new()));
        }
        match self.root.get_mut(&key) {
            Some(Value::Sequence(seq)) => seq,
            _ => unreachable!("faces was just made a list"),
        }
    }

    /// The usable face entries, in order; each skipped one adds a warning.
    pub fn entries(&self, warnings: &mut Vec<String>) -> Vec<FaceEntry> {
        let mut out: Vec<FaceEntry> = Vec::new();
        for (index, value) in self.faces().iter().enumerate() {
            let at = index + 1;
            let Value::Mapping(map) = value else {
                warnings.push(format!("face #{at} is not a mapping with `id` and `file`"));
                continue;
            };
            let id = string_field(map, "id");
            let file = string_field(map, "file");
            let label = string_field(map, "label");
            let Some(id) = id else {
                warnings.push(format!("face #{at} has no `id`"));
                continue;
            };
            if !is_valid_id(&id, FACE_ID_MAX) {
                warnings.push(format!(
                    "face `{id}`: an id is lowercase letters, digits and dashes, at most {FACE_ID_MAX} characters"
                ));
                continue;
            }
            let Some(file) = file else {
                warnings.push(format!("face `{id}` has no `file`"));
                continue;
            };
            if out.iter().any(|e| e.id == id) {
                warnings.push(format!(
                    "face `{id}` is listed twice; the first one is used"
                ));
                continue;
            }
            if out.len() == MAX_FACES_PER_PACK {
                warnings.push(format!(
                    "only the first {MAX_FACES_PER_PACK} faces of a pack are used"
                ));
                break;
            }
            out.push(FaceEntry { id, file, label });
        }
        out
    }

    /// Add `entry`, or replace the entry with its id. True when one was replaced.
    pub fn upsert(&mut self, entry: &FaceEntry) -> bool {
        let mut map = Mapping::new();
        map.insert(Value::from("id"), Value::from(entry.id.clone()));
        map.insert(Value::from("file"), Value::from(entry.file.clone()));
        if let Some(label) = &entry.label {
            map.insert(Value::from("label"), Value::from(label.clone()));
        }
        let faces = self.faces_mut();
        let existing = faces.iter().position(|v| match v {
            Value::Mapping(m) => string_field(m, "id").as_deref() == Some(entry.id.as_str()),
            _ => false,
        });
        match existing {
            Some(i) => {
                faces[i] = Value::Mapping(map);
                true
            }
            None => {
                faces.push(Value::Mapping(map));
                false
            }
        }
    }

    pub fn to_yaml(&self) -> Result<String, String> {
        serde_yaml::to_string(&Value::Mapping(self.root.clone())).map_err(|e| e.to_string())
    }
}

fn string_field(map: &Mapping, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}
