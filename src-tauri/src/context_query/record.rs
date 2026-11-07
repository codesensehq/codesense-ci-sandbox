// SPDX-License-Identifier: MIT
//! One indexed context file: where it lives (org / team / layer), what it is
//! (kind), and every frontmatter key it declares.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// Cached schema-1 search chunks; hrefs are relative to their content root.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HeadingEntry {
    pub title: String,
    pub href: String,
    #[serde(default)]
    pub text: String,
}

use super::model::{Kind, Layer};

/// An indexed file. Built once per file version by the index and shared,
/// immutable, by every query against a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextRecord {
    pub headings: Vec<HeadingEntry>,
    /// Absolute path, `/`-separated.
    pub path: String,
    /// Path relative to the content root it was found under.
    pub rel_path: String,
    pub file_name: String,
    /// `None` for all-orgs content.
    pub org: Option<String>,
    /// `None` for private and all-orgs content.
    pub team: Option<String>,
    pub layer: Layer,
    pub kind: Kind,
    /// Epoch milliseconds.
    pub mtime_ms: Option<u64>,
    pub title: Option<String>,
    /// Every frontmatter key (Markdown YAML or HTML `cs:*` meta) with its value.
    /// Lists stay arrays; HTML values are strings (list keys split on commas).
    pub fields: BTreeMap<String, Value>,
    /// Whether the file is a work item the pipeline tracks (story, bug, spike,
    /// epic, or another typed pipeline doc). Only these are `depends_on` targets.
    pub is_pipeline_doc: bool,
    /// True when the metadata block ran past the bytes read, so some keys may
    /// be missing.
    pub metadata_truncated: bool,
    /// Whether observers can treat missing fields as an actual value absence.
    pub metadata_valid: bool,
    /// SW-DNS-2: a pipeline doc's summary — the first configured summary section
    /// (`## Purpose`, then `## Story` by default), whitespace collapsed and capped —
    /// parsed from the head the index already read. `None` for other files or
    /// when no heading matches.
    pub summary: Option<String>,
}

impl ContextRecord {
    /// `story_id`, else the file name.
    pub fn id(&self) -> &str {
        self.str_field("story_id")
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.file_name)
    }

    /// A string-valued frontmatter key, by exact name.
    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(Value::as_str)
    }

    /// A list-valued frontmatter key (a scalar string reads as a one-item list).
    pub fn list_field(&self, key: &str) -> Vec<&str> {
        match self.fields.get(key) {
            Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
            Some(Value::String(s)) if !s.is_empty() => vec![s.as_str()],
            _ => Vec::new(),
        }
    }

    /// The parent epic: `parent_epic` or `epic` frontmatter.
    pub fn parent_epic(&self) -> Option<&str> {
        self.str_field("parent_epic")
            .or_else(|| self.str_field("epic"))
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// The containing directory, relative to the content root (`""` at the root).
    pub fn rel_dir(&self) -> &str {
        match self.rel_path.rfind('/') {
            Some(i) => &self.rel_path[..i],
            None => "",
        }
    }
}
