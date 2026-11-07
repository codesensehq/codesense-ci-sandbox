// SPDX-License-Identifier: MIT
//! Inline acyclic local `$ref` references in served tool input schemas.
//!
//! `rmcp`/`schemars` generates tool params with reusable definitions under
//! `$defs` and `$ref` JSON pointers into them. Some strict providers — Moonshot's
//! Kimi Go, behind OpenCode — reject the whole `tools/list` response when any
//! of those references remain, so the served router inlines every acyclic local
//! reference once before a schema leaves the process, preserving constraints,
//! documentation and sibling keywords.

use serde_json::{Map, Value};

/// Why a served schema could not be inlined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InlineError {
    /// A `$ref` pointed at a definition this schema does not define.
    Unresolved { ref_: String },
    /// A `$ref` (directly or through another) pointed back into itself, so
    /// inlining would never terminate.
    Cycle { ref_: String },
    /// A `$ref` target that is not an object cannot carry sibling keywords.
    NonObject { ref_: String },
}

impl std::fmt::Display for InlineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InlineError::Unresolved { ref_ } => {
                write!(f, "unresolved local schema reference {ref_}")
            }
            InlineError::Cycle { ref_ } => write!(f, "cyclic schema reference at {ref_}"),
            InlineError::NonObject { ref_ } => {
                write!(f, "schema reference {ref_} does not resolve to an object")
            }
        }
    }
}

/// Inline every acyclic local `$ref` in `schema`, drop its `$defs`/`definitions`
/// maps, and leave no reference behind. Fails on a cycle or a missing target
/// instead of serving a partial schema.
pub(crate) fn inline_refs(schema: &mut Map<String, Value>) -> Result<(), InlineError> {
    // Resolve `#/$defs/Name` and `#/definitions/Name` against the root's
    // definition maps. They are gathered first so a definition keeps its
    // original bytes while references into it are expanded one occurrence at a
    // time (no aliasing, no false cycle from a repeated acyclic ref).
    let mut definitions = Map::new();
    for container in ["$defs", "definitions"] {
        if let Some(Value::Object(map)) = schema.get(container) {
            definitions.insert(container.to_string(), Value::Object(map.clone()));
        }
    }
    let definitions = Value::Object(definitions);

    let mut stack: Vec<String> = Vec::new();
    let mut root = Value::Object(std::mem::take(schema));
    inline_value(&mut root, &definitions, &mut stack)?;
    *schema = match root {
        Value::Object(map) => map,
        _ => unreachable!("a served tool input schema is an object"),
    };
    Ok(())
}

/// Resolve one local `$ref` to a fully-inlined, fresh copy of its target.
fn resolve(ref_: &str, definitions: &Value, stack: &mut Vec<String>) -> Result<Value, InlineError> {
    let pointer = ref_
        .strip_prefix('#')
        .ok_or_else(|| InlineError::Unresolved {
            ref_: ref_.to_string(),
        })?;
    if stack.iter().any(|active| active == pointer) {
        return Err(InlineError::Cycle {
            ref_: ref_.to_string(),
        });
    }
    let target = definitions
        .pointer(pointer)
        .cloned()
        .ok_or_else(|| InlineError::Unresolved {
            ref_: ref_.to_string(),
        })?;
    stack.push(pointer.to_string());
    let mut inlined = target;
    let result = inline_value(&mut inlined, definitions, stack);
    stack.pop();
    result?;
    Ok(inlined)
}

/// Walk schema positions only (never `examples`, `default` or other data),
/// replacing each `$ref` with its inlined target.
fn inline_value(
    node: &mut Value,
    definitions: &Value,
    stack: &mut Vec<String>,
) -> Result<(), InlineError> {
    let Value::Object(map) = node else {
        return Ok(());
    };
    // Definitions are resolved lazily through `resolve`, never inlined in
    // place; dropping the maps here leaves a ref-free schema.
    map.remove("$defs");
    map.remove("definitions");

    if let Some(ref_) = map.get("$ref").and_then(Value::as_str) {
        let ref_ = ref_.to_string();
        let mut target = resolve(&ref_, definitions, stack)?;
        if let Value::Object(target_map) = &mut target {
            // Sibling keywords (description, constraints, …) sit beside `$ref`
            // and are more specific than the target; overlay them after the
            // target so both the target's shape and the siblings survive.
            for (key, value) in map.iter() {
                if key != "$ref" {
                    target_map.insert(key.clone(), value.clone());
                }
            }
            // A sibling may itself carry a reference.
            inline_value(&mut target, definitions, stack)?;
        } else if map.len() > 1 {
            return Err(InlineError::NonObject { ref_ });
        }
        *node = target;
        return Ok(());
    }

    for (key, child) in map.iter_mut() {
        match key.as_str() {
            "properties" | "patternProperties" => {
                if let Value::Object(props) = &mut *child {
                    for sub in props.values_mut() {
                        inline_value(sub, definitions, stack)?;
                    }
                }
            }
            "anyOf" | "oneOf" | "allOf" | "prefixItems" => {
                if let Value::Array(items) = &mut *child {
                    for item in items.iter_mut() {
                        inline_value(item, definitions, stack)?;
                    }
                }
            }
            "items"
            | "additionalProperties"
            | "not"
            | "contains"
            | "if"
            | "then"
            | "else"
            | "propertyNames"
            | "unevaluatedItems"
            | "unevaluatedProperties" => {
                inline_value(child, definitions, stack)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(schema: serde_json::Value) -> Result<serde_json::Value, InlineError> {
        let mut map = match schema {
            Value::Object(map) => map,
            _ => panic!("test schema must be an object"),
        };
        inline_refs(&mut map)?;
        Ok(Value::Object(map))
    }

    fn collect_refs(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    if key == "$ref" || key == "$defs" || key == "definitions" {
                        out.push(key.clone());
                    } else {
                        collect_refs(child, out);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
            _ => {}
        }
    }

    #[test]
    fn shared_definition_is_inlined_at_every_occurrence() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "$ref": "#/$defs/Point" },
                "b": { "$ref": "#/$defs/Point" }
            },
            "$defs": {
                "Point": {
                    "type": "object",
                    "description": "a point",
                    "properties": { "x": { "type": "integer" }, "y": { "type": "integer" } },
                    "required": ["x", "y"]
                }
            }
        });
        let out = inline(schema).unwrap();
        let mut refs = Vec::new();
        collect_refs(&out, &mut refs);
        assert!(refs.is_empty(), "refs/defs remain: {refs:?}");
        for key in ["a", "b"] {
            let point = &out["properties"][key];
            assert_eq!(point["properties"]["x"]["type"], "integer");
            assert_eq!(point["required"], serde_json::json!(["x", "y"]));
            assert_eq!(point["description"], "a point");
        }
    }

    #[test]
    fn nested_arrays_and_unions_are_inlined() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "$ref": "#/$defs/Kind" },
                "budget": { "anyOf": [ { "$ref": "#/$defs/Budget" }, { "type": "null" } ] },
                "rows": { "type": "array", "items": { "type": "array", "items": { "$ref": "#/$defs/Cell" } } }
            },
            "$defs": {
                "Kind": { "type": "string", "enum": ["a", "b"] },
                "Budget": { "type": "object", "properties": { "amount": { "type": "number" } } },
                "Cell": { "type": "string", "minLength": 1 }
            }
        });
        let out = inline(schema).unwrap();
        let mut refs = Vec::new();
        collect_refs(&out, &mut refs);
        assert!(refs.is_empty(), "refs/defs remain: {refs:?}");
        assert_eq!(
            out["properties"]["kind"]["enum"],
            serde_json::json!(["a", "b"])
        );
        assert_eq!(
            out["properties"]["budget"]["anyOf"][0]["properties"]["amount"]["type"],
            "number"
        );
        assert_eq!(out["properties"]["rows"]["items"]["items"]["minLength"], 1);
    }

    #[test]
    fn a_direct_cycle_is_detected() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "node": { "$ref": "#/$defs/Node" } },
            "$defs": { "Node": { "type": "object", "properties": { "child": { "$ref": "#/$defs/Node" } } } }
        });
        assert_eq!(
            inline(schema).unwrap_err(),
            InlineError::Cycle {
                ref_: "#/$defs/Node".into()
            }
        );
    }

    #[test]
    fn a_mutual_cycle_is_detected() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "a": { "$ref": "#/$defs/A" } },
            "$defs": {
                "A": { "type": "object", "properties": { "b": { "$ref": "#/$defs/B" } } },
                "B": { "type": "object", "properties": { "a": { "$ref": "#/$defs/A" } } }
            }
        });
        match inline(schema) {
            Err(InlineError::Cycle { ref_ }) => {
                assert!(ref_ == "#/$defs/A" || ref_ == "#/$defs/B", "{ref_}")
            }
            other => panic!("expected a cycle, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_target_is_diagnosed() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "x": { "$ref": "#/$defs/Missing" } },
            "$defs": {}
        });
        assert_eq!(
            inline(schema).unwrap_err(),
            InlineError::Unresolved {
                ref_: "#/$defs/Missing".into()
            }
        );
    }

    #[test]
    fn ref_siblings_and_target_constraints_are_preserved() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "edit": {
                    "$ref": "#/$defs/Edit",
                    "description": "the field-specific note"
                }
            },
            "$defs": {
                "Edit": {
                    "type": "object",
                    "description": "the shared shape",
                    "properties": { "type": { "type": "string" } },
                    "required": ["type"]
                }
            }
        });
        let out = inline(schema).unwrap();
        let edit = &out["properties"]["edit"];
        // The sibling description (more specific) wins; the shared constraints survive.
        assert_eq!(edit["description"], "the field-specific note");
        assert_eq!(edit["required"], serde_json::json!(["type"]));
        assert_eq!(edit["properties"]["type"]["type"], "string");
        let mut refs = Vec::new();
        collect_refs(&out, &mut refs);
        assert!(refs.is_empty(), "refs/defs remain: {refs:?}");
    }

    #[test]
    fn an_acyclic_repeated_ref_is_not_a_false_cycle() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "left": { "$ref": "#/$defs/Half" },
                "right": { "$ref": "#/$defs/Half" }
            },
            "$defs": {
                "Half": { "type": "object", "properties": { "v": { "type": "string" } } }
            }
        });
        // Repeating the same acyclic ref twice must not trip the cycle detector.
        assert!(inline(schema).is_ok());
    }
}
