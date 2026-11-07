// SPDX-License-Identifier: MIT
//! SW-DX-1: the team-defined **tag registry** — the "data layer" of the Open
//! Dashboard Runtime. An optional, org-authored schema of typed tags that layers
//! *over* always-allowed open frontmatter. It rides on `org.yml` as an additive
//! `tag_schema:` key (parsed by the same `load_org` spine that reads
//! `teams.yml`/`org.yml`), is validated by REUSING the SLW-MD per-field value
//! predicates (see `commands::frontmatter_validation` — not a second validator),
//! and is exposed to the FE via the `get_tag_schema` read seam.
//!
//! **Empty/absent ⇒ unopinionated, never an error.** A repo with no `tag_schema:`
//! key, an unresolved org, or a tag with an empty `allowed_values` all yield "no
//! opinion" (no badge) — exactly the empty-configured-set escape hatch the
//! validator already uses for `source_repo`/`team`.
//!
//! **No separate `Raw*` twin (deliberate — DRY).** Unlike `Team` (which has real
//! cross-field invariants, so `RawTeamEntry::into_domain` earns its keep), a
//! `TagDef` has no invariant to enforce beyond serde defaulting — a `Raw`
//! mirror would be pure ceremony. `serde(default)` makes parsing total
//! (malformed-tolerant by construction: a missing `type` ⇒ `text`, an UNKNOWN
//! `type` ⇒ `unknown` (free-text — see [`TagType::Unknown`]), a missing
//! `allowed_values` ⇒ `[]`), so `TagSchema` IS the deserialize target and an
//! unknown tag type can never fail the whole `org.yml` load.

use serde::{Deserialize, Serialize};

/// A tag's declared type. Serialized as snake_case strings for the TS union
/// (`"text" | "select" | "multi_select" | "ordered" | "status" | "date" |
/// "relation"`).
///
/// `select`/`ordered`/`status` (single) and `multi_select` (per item) ALL
/// validate as closed-set membership through ONE predicate
/// ([`crate::commands::frontmatter_validation::allowed_value_ok`]) — never three.
/// `ordered` additionally exposes a value's [order index](TagDef::order_index).
/// `status` is a select-with-pill-intent alias — it is **not** the built-in
/// advisory `status` frontmatter field (which stays a `Warning`, untouched).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagType {
    /// Free text — a present value is always valid. The serde default so a tag
    /// authored without a `type` parses as unopinionated free-text.
    #[default]
    Text,
    /// Closed-set single value.
    Select,
    /// Closed-set multiple values (per-item membership). Accepts the kebab
    /// spelling `multi-select` too.
    #[serde(alias = "multi-select")]
    MultiSelect,
    /// Closed-set single value whose `allowed_values` ORDER is a ladder (e.g.
    /// `context_level`); the order index (position) feeds a downstream
    /// altitude/ramp consumer.
    Ordered,
    /// Closed-set single value with pill/column UI intent — validation identical
    /// to [`Select`](TagType::Select).
    Status,
    /// ISO-8601 date — a present value must parse
    /// ([`crate::commands::frontmatter_validation::date_value_ok`]).
    Date,
    /// A reference to another doc — always valid here (targets are resolved by
    /// DX-2 / the Context View, not value-checked in the schema layer).
    Relation,
    /// Any UNRECOGNIZED `type:` string — e.g. a forward-compatible tag authored
    /// against a future DX version. `#[serde(other)]` catches it so an unknown
    /// type degrades to unopinionated free-text (non-closed-set, always valid)
    /// instead of failing the whole `org.yml` deserialize (which would drop
    /// teams/people too — `#[serde(default)]` only covers an ABSENT key, not a
    /// present-but-unknown value). This is what makes the "malformed-tolerant"
    /// contract actually total.
    #[serde(other)]
    Unknown,
}

impl TagType {
    /// Does a value of this type validate against a closed `allowed_values` set?
    /// True for `select`/`ordered`/`status` (single) and `multi_select` (per
    /// item). These share ONE membership predicate — the DX-1 DRY invariant.
    pub fn is_closed_set(self) -> bool {
        matches!(
            self,
            Self::Select | Self::Ordered | Self::Status | Self::MultiSelect
        )
    }

    /// The snake_case wire string for this type — the SAME token serde emits and
    /// the TS `TagType` union expects. Used by SW-AU-77I's row projection to stamp
    /// a present org tag's `type` so the FE picks the right control without
    /// re-reading the schema (the rejected FE-side key match).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Select => "select",
            Self::MultiSelect => "multi_select",
            Self::Ordered => "ordered",
            Self::Status => "status",
            Self::Date => "date",
            Self::Relation => "relation",
            Self::Unknown => "unknown",
        }
    }
}

/// One tag definition. Serialized camelCase (`allowedValues`) for the TS
/// `TagDef`. `allowed_values` is an ORDERED `Vec` — the order **is** the ladder
/// for `ordered` tags; it is deserialized verbatim and never sorted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagDef {
    pub name: String,
    /// Missing `type:` ⇒ `text` (the [`TagType`] default) — malformed-tolerant.
    #[serde(default)]
    pub r#type: TagType,
    /// Ordered vocabulary for closed-set / ordered tags. Empty ⇒ syntactic-only
    /// (no badge) — the empty-set escape hatch.
    ///
    /// The `alias` lets a hand-authored `org.yml` use snake_case
    /// (`allowed_values`, matching every other registry file) while the
    /// serialized JSON/TS contract stays camelCase (`allowedValues`).
    #[serde(default, alias = "allowed_values")]
    pub allowed_values: Vec<String>,
    /// Optional per-tag color (a `--cs-*` token name or literal). Omitted ⇒ the
    /// FE `tagDisplay` supplies a default. `skip_serializing_if` keeps the JSON
    /// (and any YAML round-trip) free of null noise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Optional per-tag Lucide icon name. Omitted ⇒ the FE supplies a default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl TagDef {
    /// The 0-based order index of `value` in this tag's ordered `allowed_values`
    /// (trimmed match), or `None` when the value is not in the vocabulary.
    ///
    /// DX-1's ordered-tag contract: a downstream altitude/ramp consumer maps
    /// value → ramp position via this accessor, so the order lives in exactly
    /// one place (the `allowed_values` order) — there is no parallel `order`
    /// field. An unknown value resolves to `None`, **not** `0`.
    pub fn order_index(&self, value: &str) -> Option<usize> {
        let v = value.trim();
        self.allowed_values.iter().position(|a| a.trim() == v)
    }
}

/// The validated tag registry. An empty `tags` vec is the unopinionated default
/// (a repo that never opted in).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagSchema {
    #[serde(default)]
    pub tags: Vec<TagDef>,
}

/// Canonical form of a tag key for matching: trimmed, lowercased, and
/// hyphen→underscore folded. This makes a schema tag `context_level` match a
/// markdown key `context_level` / `context-level` / `Context_Level` AND an HTML
/// `cs:context-level` uniformly — the cross-format matching used to disagree
/// (markdown was verbatim, HTML normalized), so canonicalizing BOTH the schema
/// name and the lookup key here is the single source of that rule.
fn canonical_tag_key(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('-', "_")
}

impl TagSchema {
    /// The `TagDef` for `name`, matched by [`canonical_tag_key`] (case- and
    /// hyphen/underscore-insensitive) so a doc's frontmatter key resolves the
    /// same tag across markdown and HTML.
    pub fn tag(&self, name: &str) -> Option<&TagDef> {
        let n = canonical_tag_key(name);
        self.tags.iter().find(|t| canonical_tag_key(&t.name) == n)
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Leniently build a schema from the raw `tag_schema:` YAML value that
    /// `RawOrgYml` captures verbatim. This is what makes "empty/absent ⇒
    /// unopinionated, **never an error**" total across BOTH axes:
    /// - **value axis** — an unknown `type:` string ⇒ [`TagType::Unknown`];
    /// - **shape axis** — a malformed shape (`tags:` not a list, a nameless tag,
    ///   `allowed_values:` not a list, or `tag_schema:` not a mapping) degrades
    ///   to the empty schema (or drops just the malformed tag) instead of failing
    ///   the whole `org.yml` deserialize — which would take teams/people down too.
    ///
    /// Capturing the raw value (rather than deriving `Deserialize` on a strict
    /// field) ALSO keeps re-serialization lossless: the org-rename write-back
    /// (`context_repo::mutate`) re-emits the authored bytes verbatim instead of
    /// rewriting `type: kanban` → `unknown` or reflowing the block.
    pub fn from_raw_value(value: &serde_yaml::Value) -> Self {
        // `tag_schema:` must be a mapping with a `tags:` sequence; anything else
        // (null/absent, a scalar, a wrong-typed `tags`) ⇒ unopinionated.
        let Some(seq) = value.get("tags").and_then(|t| t.as_sequence()) else {
            return Self::default();
        };
        // Per-tag tolerance: keep every well-formed tag, silently drop a
        // malformed one (a fat-fingered tag never nukes the rest of the registry).
        let tags = seq
            .iter()
            .filter_map(|v| serde_yaml::from_value::<TagDef>(v.clone()).ok())
            .collect();
        Self { tags }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_type_as_str_matches_serde_wire_token() {
        // SW-AU-77I stamps a row's `type` from `as_str`; the FE `TagType` union and
        // its `TAG_TYPE_DEFAULTS` key off the serde wire token. Pin the two together
        // so a variant rename can't silently diverge the hand-written `as_str` from
        // what serde emits (which is what the FE actually receives).
        for ty in [
            TagType::Text,
            TagType::Select,
            TagType::MultiSelect,
            TagType::Ordered,
            TagType::Status,
            TagType::Date,
            TagType::Relation,
            TagType::Unknown,
        ] {
            let serde_token = serde_json::to_string(&ty).unwrap();
            assert_eq!(serde_token, format!("\"{}\"", ty.as_str()), "{ty:?}");
        }
    }

    // ---- AC-1 / AC-12: parse round-trip, empty-config, malformed-tolerant ----

    #[test]
    fn empty_config_yields_empty_schema() {
        // AC-1: an absent/empty tag_schema parses as an empty schema, never an
        // error. (Absent is exercised at the org-load level; here the direct
        // empty case.)
        let schema: TagSchema = serde_yaml::from_str("tags: []").unwrap();
        assert!(schema.is_empty());
        assert!(schema.tag("anything").is_none());
        // And the Default is empty too (the unopinionated default an unresolved
        // org falls back to).
        assert!(TagSchema::default().is_empty());
    }

    #[test]
    fn parses_typed_tags_and_round_trips() {
        let yml = r#"
tags:
  - name: context_level
    type: ordered
    allowed_values: [machine, team, management, leadership]
  - name: priority
    type: select
    allowed_values: [low, high]
    color: "--cs-status-modified"
    icon: flag
  - name: owners
    type: multi_select
    allowed_values: [alice, bob]
"#;
        let schema: TagSchema = serde_yaml::from_str(yml).unwrap();
        assert_eq!(schema.tags.len(), 3);
        assert_eq!(
            schema.tag("context_level").unwrap().r#type,
            TagType::Ordered
        );
        assert_eq!(
            schema.tag("priority").unwrap().color.as_deref(),
            Some("--cs-status-modified")
        );
        assert_eq!(schema.tag("owners").unwrap().r#type, TagType::MultiSelect);

        // Round-trip through YAML preserves the typed shape + vocabulary order.
        let back: TagSchema =
            serde_yaml::from_str(&serde_yaml::to_string(&schema).unwrap()).unwrap();
        assert_eq!(back, schema);
    }

    #[test]
    fn malformed_tolerant_defaults() {
        // A tag missing `type` ⇒ text; missing `allowed_values` ⇒ []; an unknown
        // extra key is ignored. Parsing never errors on a partial tag.
        let yml = r#"
tags:
  - name: freeform
  - name: note
    type: text
    some_future_key: 42
"#;
        let schema: TagSchema = serde_yaml::from_str(yml).unwrap();
        assert_eq!(schema.tags.len(), 2);
        assert_eq!(schema.tag("freeform").unwrap().r#type, TagType::Text);
        assert!(schema.tag("freeform").unwrap().allowed_values.is_empty());
    }

    #[test]
    fn multi_select_accepts_kebab_alias() {
        let yml = "tags:\n  - name: x\n    type: multi-select\n    allowed_values: [a]\n";
        let schema: TagSchema = serde_yaml::from_str(yml).unwrap();
        assert_eq!(schema.tag("x").unwrap().r#type, TagType::MultiSelect);
    }

    #[test]
    fn unknown_type_parses_as_unknown_never_errors() {
        // Review M1: a forward-compatible / typo type string must NOT fail the
        // parse — it degrades to `Unknown` (free-text, non-closed-set).
        let yml = "tags:\n  - name: board\n    type: kanban\n    allowed_values: [a, b]\n";
        let schema: TagSchema = serde_yaml::from_str(yml).unwrap();
        let def = schema.tag("board").unwrap();
        assert_eq!(def.r#type, TagType::Unknown);
        assert!(!def.r#type.is_closed_set(), "unknown ⇒ free-text, no badge");
    }

    #[test]
    fn tag_matching_is_case_and_hyphen_insensitive() {
        // Review m1: a schema tag `context_level` resolves from a markdown key
        // spelled with a hyphen or different case, and from an HTML cs name —
        // one canonical matching rule across formats.
        let yml = "tags:\n  - name: context_level\n    type: ordered\n    allowed_values: [machine, team]\n";
        let schema: TagSchema = serde_yaml::from_str(yml).unwrap();
        assert!(schema.tag("context_level").is_some());
        assert!(schema.tag("context-level").is_some(), "hyphen matches");
        assert!(schema.tag("Context_Level").is_some(), "case-insensitive");
        assert!(schema.tag(" context-level ").is_some(), "trim + fold");
        assert!(schema.tag("other").is_none());
    }

    // ---- AC-5 / AC-6: ordered vocabulary + order index ----------------------

    #[test]
    fn ordered_vocabulary_preserves_authored_order() {
        // AC-5: the ladder order IS the allowed_values order — never sorted.
        let yml = "tags:\n  - name: context_level\n    type: ordered\n    allowed_values: [machine, team, management, leadership]\n";
        let schema: TagSchema = serde_yaml::from_str(yml).unwrap();
        let def = schema.tag("context_level").unwrap();
        assert_eq!(
            def.allowed_values,
            vec!["machine", "team", "management", "leadership"]
        );
    }

    #[test]
    fn order_index_maps_value_to_position_and_unknown_to_none() {
        // AC-6: each value's 0-based order index matches its allowed_values
        // position; an unknown value resolves to None (NOT 0).
        let def = TagDef {
            name: "context_level".into(),
            r#type: TagType::Ordered,
            allowed_values: vec![
                "machine".into(),
                "team".into(),
                "management".into(),
                "leadership".into(),
            ],
            color: None,
            icon: None,
        };
        assert_eq!(def.order_index("machine"), Some(0));
        assert_eq!(def.order_index("team"), Some(1));
        assert_eq!(def.order_index("management"), Some(2));
        assert_eq!(def.order_index("leadership"), Some(3));
        assert_eq!(def.order_index(" leadership "), Some(3), "trimmed match");
        assert_eq!(def.order_index("bogus"), None, "unknown ⇒ None, not 0");
    }

    #[test]
    fn is_closed_set_classification() {
        assert!(TagType::Select.is_closed_set());
        assert!(TagType::Ordered.is_closed_set());
        assert!(TagType::Status.is_closed_set());
        assert!(TagType::MultiSelect.is_closed_set());
        assert!(!TagType::Text.is_closed_set());
        assert!(!TagType::Date.is_closed_set());
        assert!(!TagType::Relation.is_closed_set());
    }

    #[test]
    fn tagdef_serializes_camel_case_for_ts() {
        let def = TagDef {
            name: "priority".into(),
            r#type: TagType::Select,
            allowed_values: vec!["low".into()],
            color: None,
            icon: None,
        };
        let json = serde_json::to_string(&def).unwrap();
        assert!(json.contains("\"allowedValues\""), "camelCase: {json}");
        assert!(json.contains("\"type\":\"select\""), "snake type: {json}");
        // Omitted color/icon do not leak nulls.
        assert!(!json.contains("color"), "no null color: {json}");
    }
}
