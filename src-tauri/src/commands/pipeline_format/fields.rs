// SPDX-License-Identifier: MIT
//! The story-field registry: one row per metadata field CodeSense knows.
//!
//! [`STORY_FIELDS`] is the single source for every field list in the crate.
//! [`FIELD_NAME_MAP`], [`METADATA_CONTRACT_FIELDS`] and [`KNOWN_TOP_LEVEL_KEYS`]
//! are derived from it at compile time, the field-schema baseline
//! (`field_schema::official_fields`) iterates it, and `get_story_format`, the
//! story-fields docs page and the live-data contract all print its rows.
//!
//! Registering a field is one row. Add a `FrontmatterFields` field (and make the
//! row [`FieldRole::Typed`]) only when Rust code needs the parsed value; the
//! parity tests then require the HTML reader to fill it.

/// The shape of a field's value. HTML writes a list as one comma-separated
/// `content` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    List,
    Number,
}

impl FieldKind {
    /// The kind as the docs and contracts spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::List => "list",
            FieldKind::Number => "number",
        }
    }
}

/// How CodeSense reads a registered field. Every role is registered: none of them
/// draws an `UNREGISTERED_FIELD` advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldRole {
    /// A `FrontmatterFields` field. Both formats parse it, it is in the authoring
    /// contract, and it is never an open tag.
    Typed,
    /// In the authoring contract, with no typed field. It stays an open tag, which
    /// is how the board, `doc_query` and story titles read it; `context_query`
    /// reads it like any key.
    Open,
    /// A typed field nothing interprets any more. Still parsed so existing
    /// documents display it; left out of the contract and the docs.
    Retired,
    /// Not parsed and not taught. Older documents carry it, so it is not an open
    /// tag either.
    Tolerated,
}

/// One registered story field.
#[derive(Debug, Clone, Copy)]
pub struct StoryField {
    /// The Markdown (YAML) key, e.g. `depends_on`.
    pub yaml: &'static str,
    /// The HTML meta name, e.g. `cs:depends-on`.
    pub cs_meta: &'static str,
    pub kind: FieldKind,
    /// One reader-facing sentence or two: what the field is for and any rule
    /// `validate_story` applies to it.
    pub description: &'static str,
    /// The documented values, in order; empty when the value is free. Not
    /// enforced by this table.
    pub enum_values: &'static [&'static str],
    /// `get_story_format` marks it required for a pipeline story.
    pub required: bool,
    pub role: FieldRole,
}

impl StoryField {
    /// The row as `get_story_format` and the live-data contract publish it. `values`
    /// appears only when the field has documented values.
    pub fn contract_entry(&self) -> serde_json::Value {
        let mut entry = serde_json::json!({
            "name": self.yaml,
            "cs_meta": self.cs_meta,
            "kind": self.kind.as_str(),
            "required": self.required,
            "description": self.description,
        });
        if !self.enum_values.is_empty() {
            entry["values"] = serde_json::json!(self.enum_values);
        }
        entry
    }
}

/// Every registered story field. Order is the order `get_story_format` and the
/// docs list them in.
pub const STORY_FIELDS: &[StoryField] = &[
    StoryField {
        yaml: "status",
        cs_meta: "cs:status",
        kind: FieldKind::Text,
        description: "The story's pipeline stage, which is also its board column: one of the team's statuses (status-taxonomy.yml). Free text; an unusual value is advisory.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "type",
        cs_meta: "cs:type",
        kind: FieldKind::Text,
        description: "Document type. One of the listed values makes the file a pipeline story; any other value, or none, keeps it a plain document.",
        enum_values: super::PIPELINE_DOCUMENT_TYPES,
        required: true,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "depends_on",
        cs_meta: "cs:depends-on",
        kind: FieldKind::List,
        description: "Story ids this story depends on. Each must resolve; drives the dependency graph.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "tags",
        cs_meta: "cs:tags",
        kind: FieldKind::List,
        description: "Free-form tags, for example [infra, urgent].",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "supporting_docs",
        cs_meta: "cs:supporting-docs",
        kind: FieldKind::List,
        description: "Relative paths to supporting documents. A path that is not found is advisory.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "story_id",
        cs_meta: "cs:story-id",
        kind: FieldKind::Text,
        description: "The story's display id, for example W-5. Unique within an org.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "source_repo",
        cs_meta: "cs:source-repo",
        kind: FieldKind::Text,
        description: "The repo the story's code lives in, as owner/repo. Must be a configured repo: resolve it with list_source_repos, never invent it.",
        enum_values: &[],
        required: true,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "work_repo",
        cs_meta: "cs:work-repo",
        kind: FieldKind::Text,
        description: "Optional. The repo whose history holds the story's work when that is not source_repo, as owner/repo (for example the context repo); story_changes reads it.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "dev_branch",
        cs_meta: "cs:dev-branch",
        kind: FieldKind::Text,
        description: "Legacy. Read only as a fallback on older documents; use branch.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "branch_name",
        cs_meta: "cs:branch-name",
        kind: FieldKind::Text,
        description: "Legacy. Read only as a fallback on older documents; use branch.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "team",
        cs_meta: "cs:team",
        kind: FieldKind::Text,
        description: "The team the document belongs to. If set, it must be a configured team.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "repos",
        cs_meta: "cs:repos",
        kind: FieldKind::List,
        description: "Epics: the repos the epic spans, as owner/repo.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "branch",
        cs_meta: "cs:branch",
        kind: FieldKind::Text,
        description: "Stories: the story's branch, used to create its worktree. Epics: the integration branch (by default integration/ followed by the epic id).",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "scope_context",
        cs_meta: "cs:scope-context",
        kind: FieldKind::List,
        description: "Retired epic field. Nothing reads it; existing documents still display it.",
        enum_values: &[],
        required: false,
        role: FieldRole::Retired,
    },
    StoryField {
        yaml: "scope_install",
        cs_meta: "cs:scope-install",
        kind: FieldKind::List,
        description: "Retired epic field. Nothing reads it; existing documents still display it.",
        enum_values: &[],
        required: false,
        role: FieldRole::Retired,
    },
    StoryField {
        yaml: "sprint",
        cs_meta: "cs:sprint",
        kind: FieldKind::Text,
        description: "The sprint the story belongs to, for example 2026-08-A.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "points",
        cs_meta: "cs:points",
        kind: FieldKind::Number,
        description: "Estimate in points, on the Fibonacci scale up to 13. A non-numeric value is ignored; an off-scale value is advisory.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "assigned_to",
        cs_meta: "cs:assigned-to",
        kind: FieldKind::Text,
        description: "Who the story is assigned to: a person's login or an agent id. A login missing from the org roster is advisory.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "rank",
        cs_meta: "cs:rank",
        kind: FieldKind::Text,
        description: "Order within a board column. The board writes it; you rarely set it by hand.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "subject_id",
        cs_meta: "cs:subject-id",
        kind: FieldKind::Text,
        description: "A standalone document's automation subject id, which makes a plain document an addressable automation subject. A story uses story_id instead.",
        enum_values: &[],
        required: false,
        role: FieldRole::Typed,
    },
    StoryField {
        yaml: "title",
        cs_meta: "cs:title",
        kind: FieldKind::Text,
        description: "The story's title. Without it, Markdown uses the first # heading and HTML uses the title tag or the first h1.",
        enum_values: &[],
        required: false,
        role: FieldRole::Open,
    },
    StoryField {
        yaml: "parent_epic",
        cs_meta: "cs:parent-epic",
        kind: FieldKind::Text,
        description: "The id of the epic the story belongs to. Without it, the epic folder on the story's path counts.",
        enum_values: &[],
        required: false,
        role: FieldRole::Open,
    },
    StoryField {
        yaml: "epic",
        cs_meta: "cs:epic",
        kind: FieldKind::Text,
        description: "Alias of parent_epic, read when parent_epic is absent.",
        enum_values: &[],
        required: false,
        role: FieldRole::Open,
    },
    StoryField {
        yaml: "priority",
        cs_meta: "cs:priority",
        kind: FieldKind::Text,
        description: "Priority, where p0 is the highest. Sorting also ranks words such as critical, high, medium and low.",
        enum_values: &["p0", "p1", "p2", "p3", "p4"],
        required: false,
        role: FieldRole::Open,
    },
    StoryField {
        yaml: "blocked_reason",
        cs_meta: "cs:blocked-reason",
        kind: FieldKind::Text,
        description: "Why the story is blocked, for a board to show on its card. Set it while the status is a blocked one; with any other status, it is advisory.",
        enum_values: &[],
        required: false,
        role: FieldRole::Open,
    },
    StoryField {
        yaml: "wave",
        cs_meta: "cs:wave",
        kind: FieldKind::Number,
        description: "Optional. The story's board wave, for a board to use instead of the wave it would compute from depends_on. A value that is not a positive whole number is advisory.",
        enum_values: &[],
        required: false,
        role: FieldRole::Open,
    },
    StoryField {
        yaml: "stream",
        cs_meta: "cs:stream",
        kind: FieldKind::Text,
        description: "The work stream the story is in, for example frontend or backend, for a board to group by.",
        enum_values: &[],
        required: false,
        role: FieldRole::Open,
    },
    StoryField {
        yaml: "sub_epic",
        cs_meta: "cs:sub-epic",
        kind: FieldKind::Text,
        description: "The sub-epic the story belongs to within its parent epic, for a board to group by.",
        enum_values: &[],
        required: false,
        role: FieldRole::Open,
    },
    StoryField {
        yaml: "gate_evidence",
        cs_meta: "cs:gate-evidence",
        kind: FieldKind::Text,
        description: "Retired. Written by the old status gate; tolerated on older documents and never read.",
        enum_values: &[],
        required: false,
        role: FieldRole::Tolerated,
    },
];

/// The rows `get_story_format` publishes, in order: [`METADATA_CONTRACT_FIELDS`].
pub fn contract_fields() -> impl Iterator<Item = &'static StoryField> {
    STORY_FIELDS
        .iter()
        .filter(|f| includes(Derived::Contract, f.role))
}

/// The HTML twin of [`KNOWN_TOP_LEVEL_KEYS`]: their `cs:*` names without the prefix.
pub fn known_cs_names() -> impl Iterator<Item = &'static str> {
    STORY_FIELDS
        .iter()
        .filter(|f| includes(Derived::KnownKeys, f.role))
        .map(|f| f.cs_meta.strip_prefix("cs:").unwrap_or(f.cs_meta))
}

/// The registered row for a field name, matched on its Markdown key.
pub fn story_field(yaml: &str) -> Option<&'static StoryField> {
    STORY_FIELDS.iter().find(|f| f.yaml == yaml)
}

// ----- Derived lists ---------------------------------------------------------
// Built at compile time, so their consumers keep plain `&[…]` slices.

/// The lists derived from [`STORY_FIELDS`]. Const fns cannot take closures, so
/// each list names its filter here.
#[derive(Clone, Copy)]
enum Derived {
    NameMap,
    Contract,
    KnownKeys,
}

const fn includes(list: Derived, role: FieldRole) -> bool {
    match list {
        Derived::NameMap => matches!(role, FieldRole::Typed | FieldRole::Retired),
        Derived::Contract => matches!(role, FieldRole::Typed | FieldRole::Open),
        Derived::KnownKeys => !matches!(role, FieldRole::Open),
    }
}

const fn count(list: Derived) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < STORY_FIELDS.len() {
        if includes(list, STORY_FIELDS[i].role) {
            n += 1;
        }
        i += 1;
    }
    n
}

const fn yaml_names<const N: usize>(list: Derived) -> [&'static str; N] {
    let mut out = [""; N];
    let mut n = 0;
    let mut i = 0;
    while i < STORY_FIELDS.len() {
        if includes(list, STORY_FIELDS[i].role) {
            out[n] = STORY_FIELDS[i].yaml;
            n += 1;
        }
        i += 1;
    }
    out
}

/// YAML key ↔ HTML `cs:*` name for every typed field: the fields the HTML reader
/// parses into `FrontmatterFields` and `rewrite_field` writes.
pub const FIELD_NAME_MAP: &[(&str, &str)] = &{
    let mut out = [("", ""); count(Derived::NameMap)];
    let mut n = 0;
    let mut i = 0;
    while i < STORY_FIELDS.len() {
        if includes(Derived::NameMap, STORY_FIELDS[i].role) {
            out[n] = (STORY_FIELDS[i].yaml, STORY_FIELDS[i].cs_meta);
            n += 1;
        }
        i += 1;
    }
    out
};

/// The fields `get_story_format` publishes: they affect CodeSense routing, display
/// or flow. Retired and tolerated fields stay readable but are not taught.
pub const METADATA_CONTRACT_FIELDS: &[&str] =
    &yaml_names::<{ count(Derived::Contract) }>(Derived::Contract);

/// Top-level keys that are never open tags: every row except the open ones.
pub const KNOWN_TOP_LEVEL_KEYS: &[&str] =
    &yaml_names::<{ count(Derived::KnownKeys) }>(Derived::KnownKeys);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::pipeline_format::{
        extract_meta, extract_metadata_keys, FrontmatterFields, PipelineFileFormat,
    };

    /// The lists this table replaced, verbatim, so deriving them changed nothing
    /// (except `subject_id` joining the known keys, matching HTML). The contract
    /// also carries the open rows registered since.
    #[test]
    fn derived_lists_match_the_hand_kept_lists_they_replaced() {
        let old_name_map = [
            "status",
            "type",
            "depends_on",
            "tags",
            "supporting_docs",
            "story_id",
            "source_repo",
            "work_repo",
            "dev_branch",
            "branch_name",
            "team",
            "repos",
            "branch",
            "scope_context",
            "scope_install",
            "sprint",
            "points",
            "assigned_to",
            "rank",
            "subject_id",
        ];
        let names: Vec<&str> = FIELD_NAME_MAP.iter().map(|(yaml, _)| *yaml).collect();
        assert_eq!(names, old_name_map);
        for (yaml, cs) in FIELD_NAME_MAP {
            assert_eq!(*cs, format!("cs:{}", yaml.replace('_', "-")));
        }

        let mut old_contract: Vec<&str> = old_name_map
            .iter()
            .copied()
            .filter(|n| !matches!(*n, "scope_context" | "scope_install"))
            .collect();
        old_contract.extend([
            "title",
            "parent_epic",
            "epic",
            "priority",
            "blocked_reason",
            "wave",
            "stream",
            "sub_epic",
        ]);
        assert_eq!(METADATA_CONTRACT_FIELDS, old_contract.as_slice());

        let mut old_known: Vec<&str> = old_name_map.to_vec();
        old_known.push("gate_evidence");
        assert_eq!(KNOWN_TOP_LEVEL_KEYS, old_known.as_slice());
    }

    #[test]
    fn every_row_is_unique_and_follows_the_cs_name_convention() {
        let mut seen = std::collections::BTreeSet::new();
        for f in STORY_FIELDS {
            assert!(seen.insert(f.yaml), "duplicate row {}", f.yaml);
            assert_eq!(f.cs_meta, format!("cs:{}", f.yaml.replace('_', "-")));
            assert!(
                !f.description.trim().is_empty(),
                "{} needs a description",
                f.yaml
            );
        }
    }

    /// The shipped docs page lists exactly the fields `get_story_format` publishes,
    /// row for row (regenerate it with `pnpm gen:docs`).
    #[test]
    fn shipped_story_fields_page_matches_the_contract() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../codesense-docs/getting-started/content/generated/story-fields.json");
        let page: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let contract: Vec<serde_json::Value> = contract_fields()
            .map(|f| {
                serde_json::json!({
                    "yaml": f.yaml,
                    "csMeta": f.cs_meta,
                    "kind": f.kind.as_str(),
                    "description": f.description,
                    "enumValues": f.enum_values,
                    "required": f.required,
                })
            })
            .collect();
        assert_eq!(
            page["fields"],
            serde_json::Value::Array(contract),
            "story-fields.json is stale: run pnpm gen:docs"
        );
    }

    /// Typed rows are exactly the `FrontmatterFields` serde fields, so a new
    /// struct field cannot ship unregistered and a typed row cannot lack a field.
    #[test]
    fn typed_rows_match_frontmatter_fields() {
        let value = serde_json::to_value(FrontmatterFields::default()).unwrap();
        let mut struct_names: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        struct_names.sort();
        let mut typed: Vec<String> = FIELD_NAME_MAP.iter().map(|(y, _)| y.to_string()).collect();
        typed.sort();
        assert_eq!(typed, struct_names);
    }

    fn sample(kind: FieldKind) -> (&'static str, &'static str) {
        match kind {
            FieldKind::Text => ("\"x-1\"", "x-1"),
            FieldKind::List => ("[a, b]", "a, b"),
            FieldKind::Number => ("3", "3"),
        }
    }

    /// The HTML reader fills every typed row exactly as the Markdown reader does.
    #[test]
    fn html_and_markdown_parse_every_typed_row_alike() {
        for f in STORY_FIELDS
            .iter()
            .filter(|f| includes(Derived::NameMap, f.role))
        {
            let (md_value, html_value) = sample(f.kind);
            let md = format!("---\n{}: {md_value}\n---\n# T\n", f.yaml);
            let html = format!("<meta name=\"{}\" content=\"{html_value}\">\n", f.cs_meta);
            let from_md = extract_meta(PipelineFileFormat::Markdown, &md);
            let from_html = extract_meta(PipelineFileFormat::Html, &html);
            assert_ne!(
                from_md,
                FrontmatterFields::default(),
                "{} not parsed from Markdown",
                f.yaml
            );
            assert_eq!(
                from_html, from_md,
                "{} parses differently from HTML",
                f.yaml
            );
        }
    }

    /// Open rows (`priority`, `parent_epic`, `wave` and the rest) yield the same key
    /// from both formats and stay open tags, which is where the board reads them.
    #[test]
    fn open_rows_are_the_same_key_in_both_formats_and_stay_open_tags() {
        for f in STORY_FIELDS.iter().filter(|f| f.role == FieldRole::Open) {
            let md = format!("---\n{}: x-1\n---\n# T\n", f.yaml);
            let html = format!("<meta name=\"{}\" content=\"x-1\">\n", f.cs_meta);
            assert_eq!(
                extract_metadata_keys(PipelineFileFormat::Markdown, &md),
                vec![f.yaml]
            );
            assert_eq!(
                extract_metadata_keys(PipelineFileFormat::Html, &html),
                vec![f.yaml]
            );
            for (format, text) in [
                (PipelineFileFormat::Markdown, &md),
                (PipelineFileFormat::Html, &html),
            ] {
                let open = crate::commands::frontmatter_editor::open_tag_values(format, text);
                assert_eq!(
                    open,
                    vec![(f.yaml.to_string(), vec!["x-1".to_string()])],
                    "{}",
                    f.yaml
                );
            }
        }
    }
}
