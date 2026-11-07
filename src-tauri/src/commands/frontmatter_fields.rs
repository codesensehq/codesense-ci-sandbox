// SPDX-License-Identifier: MIT
//! Typed frontmatter field writes: set top-level keys of a Markdown
//! frontmatter block to a string, number, boolean or list, or remove them —
//! changing only the bytes of the keys written.
//!
//! The pure transform behind the `update_frontmatter` command (the page
//! bridge's `setField`). Unlike the line-joining
//! `rewrite_field`, it splices byte ranges, so every untouched line — line
//! endings, comments, blank lines and the body — stays exactly as it was.
//! Numbers and booleans are written as YAML numbers/booleans (not quoted
//! strings); strings are quoted only when YAML would read them as something
//! else.

use serde_json::Value;

use super::pipeline_format::markdown::render_yaml_scalar;

/// The most keys one write may touch.
pub(crate) const MAX_FIELDS_PER_WRITE: usize = 64;

/// Refuse a write that names more keys than [`MAX_FIELDS_PER_WRITE`].
pub(crate) fn validate_field_count(count: usize) -> Result<(), String> {
    if count > MAX_FIELDS_PER_WRITE {
        return Err(format!("at most {MAX_FIELDS_PER_WRITE} fields per write"));
    }
    Ok(())
}

/// A top-level key a write may name: a plain identifier (letters, digits, `_`,
/// `-`), so a key can never smuggle YAML structure (`:`, newlines, quotes,
/// nesting dots) into the block.
pub(crate) fn validate_field_name(field: &str) -> Result<(), String> {
    let mut chars = field.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first_ok
        || field.len() > 64
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "invalid: field name {field:?} must be a plain key (letters, digits, '_' or '-', starting with a letter or '_', at most 64 chars)"
        ));
    }
    Ok(())
}

fn reject_control_chars(field: &str, value: &str) -> Result<(), String> {
    if let Some(c) = value.chars().find(|c| c.is_control()) {
        return Err(format!(
            "invalid: {field} contains a control character ({c:?}); a frontmatter value is a single line"
        ));
    }
    Ok(())
}

/// One list item or scalar as YAML text.
fn render_scalar(field: &str, value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => {
            reject_control_chars(field, s)?;
            Ok(render_yaml_scalar(s))
        }
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Null => Err(format!("invalid: {field}: a list item cannot be null")),
        Value::Array(_) | Value::Object(_) => Err(format!(
            "invalid: {field}: only strings, numbers, booleans, lists of those, or null (to remove the key) can be written"
        )),
    }
}

/// The replacement lines for `field: value` (without a trailing line ending on
/// the last line), or `None` to remove the key.
fn render_field(field: &str, value: &Value, eol: &str) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Array(items) if items.is_empty() => Ok(Some(format!("{field}: []{eol}"))),
        Value::Array(items) => {
            let mut out = format!("{field}:{eol}");
            for item in items {
                out.push_str("  - ");
                out.push_str(&render_scalar(field, item)?);
                out.push_str(eol);
            }
            Ok(Some(out))
        }
        Value::Object(_) => Ok(Some(format!(
            "{field}: {}{eol}",
            serde_json::to_string(value).map_err(|e| e.to_string())?
        ))),
        scalar => Ok(Some(format!(
            "{field}: {}{eol}",
            render_scalar(field, scalar)?
        ))),
    }
}

/// `line` (without its line ending) declares the top-level key `field`.
fn is_key_line(line: &str, field: &str) -> bool {
    line.strip_prefix(field)
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
}

/// `line` continues the value of the key above it: an indented line or a block
/// sequence item written at the key's own indentation.
fn is_continuation(line: &str) -> bool {
    line.starts_with([' ', '\t']) || line == "-" || line.starts_with("- ")
}

fn is_fence(line: &str) -> bool {
    let t = line.trim_end();
    t == "---" || t == "..."
}

/// Set or remove one top-level key in a Markdown document's frontmatter.
pub(crate) fn set_markdown_field(
    content: &str,
    field: &str,
    value: &Value,
) -> Result<String, String> {
    if value.is_object() {
        return Err(format!(
            "invalid: {field}: objects cannot be written; set nested values one key at a time"
        ));
    }
    set_markdown_metadata(content, field, value)
}

/// Internal metadata, including structured actor attribution, uses the same byte splicer.
pub(crate) fn set_markdown_metadata(
    content: &str,
    field: &str,
    value: &Value,
) -> Result<String, String> {
    validate_field_name(field)?;
    let (bom, body) = match content.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", content),
    };
    let default_eol = if body.contains("\r\n") { "\r\n" } else { "\n" };

    let opener_len = body.find('\n').map_or(body.len(), |i| i + 1);
    let opener = body[..opener_len].trim_end_matches(['\n', '\r']);
    if opener.trim_end() != "---" {
        // No block: a removal is a no-op; a set adds one.
        return match render_field(field, value, default_eol)? {
            None => Ok(content.to_string()),
            Some(line) => Ok(format!("{bom}---{default_eol}{line}---{default_eol}{body}")),
        };
    }

    let mut pos = opener_len;
    let mut close: Option<(usize, &str)> = None;
    let mut span: Option<(usize, usize, &str)> = None;
    let mut in_span = false;
    for raw in body[opener_len..].split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let eol = &raw[line.len()..];
        if is_fence(line) {
            close = Some((pos, eol));
            break;
        }
        if in_span {
            if is_continuation(line) {
                if let Some((_, end, _)) = span.as_mut() {
                    *end = pos + raw.len();
                }
            } else if !line.trim().is_empty() {
                in_span = false;
            }
        }
        if span.is_none() && is_key_line(line, field) {
            span = Some((pos, pos + raw.len(), eol));
            in_span = true;
        }
        pos += raw.len();
    }
    let Some((close_at, close_eol)) = close else {
        return Err(
            "invalid: the frontmatter block is not closed (no `---` line); not writing".into(),
        );
    };

    let mut out = String::with_capacity(content.len() + 32);
    out.push_str(bom);
    match span {
        Some((start, end, eol)) => {
            // The key's last line may end the file only if the block is unclosed,
            // which was refused above, so it always carries a line ending.
            let eol = if eol.is_empty() { default_eol } else { eol };
            out.push_str(&body[..start]);
            if let Some(text) = render_field(field, value, eol)? {
                out.push_str(&text);
            }
            out.push_str(&body[end..]);
        }
        None => match render_field(
            field,
            value,
            if close_eol.is_empty() {
                default_eol
            } else {
                close_eol
            },
        )? {
            None => return Ok(content.to_string()),
            Some(text) => {
                out.push_str(&body[..close_at]);
                out.push_str(&text);
                out.push_str(&body[close_at..]);
            }
        },
    }
    Ok(out)
}

/// Apply several key writes in order; any invalid one fails the whole write.
pub(crate) fn set_markdown_fields(
    content: &str,
    fields: &[(String, Value)],
) -> Result<String, String> {
    let mut out = content.to_string();
    for (field, value) in fields {
        out = set_markdown_field(&out, field, value)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DOC: &str = "---\r\ntype: story\r\nstory_id: SW-1\r\n# a comment\r\nstatus: todo\r\npoints: 2\r\ndepends_on:\r\n- SW-0\r\n- SW-00\r\n\r\ntitle: x\r\n---\r\n# SW-1\r\n\r\nBody stays   \r\nexactly.";

    fn set(content: &str, field: &str, value: Value) -> String {
        set_markdown_field(content, field, &value).unwrap()
    }

    #[test]
    fn replacing_a_key_changes_only_its_line() {
        let out = set(DOC, "points", json!(5));
        assert_eq!(out, DOC.replace("points: 2\r\n", "points: 5\r\n"));
    }

    #[test]
    fn numbers_and_booleans_are_yaml_scalars_strings_quote_when_ambiguous() {
        let out = set(DOC, "points", json!(3.5));
        assert!(out.contains("points: 3.5\r\n"));
        let out = set(DOC, "blocked", json!(true));
        assert!(out.contains("blocked: true\r\n---\r\n"));
        // A string that YAML would read as a number stays a string.
        let out = set(DOC, "sprint", json!("2026"));
        assert!(out.contains("sprint: \"2026\"\r\n"));
        let out = set(DOC, "assigned_to", json!("person:alice"));
        assert!(
            out.contains("assigned_to: person:alice\r\n")
                || out.contains("assigned_to: \"person:alice\"\r\n")
        );
    }

    #[test]
    fn a_new_key_goes_before_the_closing_fence_with_the_files_line_ending() {
        let out = set(DOC, "rank", json!(10));
        assert_eq!(
            out,
            DOC.replace("title: x\r\n---\r\n", "title: x\r\nrank: 10\r\n---\r\n")
        );
    }

    #[test]
    fn a_list_replaces_the_whole_block_sequence_and_keeps_the_blank_line_after_it() {
        let out = set(DOC, "depends_on", json!(["SW-9"]));
        assert_eq!(
            out,
            DOC.replace(
                "depends_on:\r\n- SW-0\r\n- SW-00\r\n",
                "depends_on:\r\n  - SW-9\r\n"
            )
        );
        let out = set(DOC, "depends_on", json!([]));
        assert!(out.contains("depends_on: []\r\n\r\ntitle: x"));
    }

    #[test]
    fn null_removes_the_key_and_its_items_and_is_a_no_op_when_absent() {
        let out = set(DOC, "depends_on", Value::Null);
        assert_eq!(out, DOC.replace("depends_on:\r\n- SW-0\r\n- SW-00\r\n", ""));
        assert_eq!(set(DOC, "sprint", Value::Null), DOC);
        assert_eq!(set("# no block\n", "sprint", Value::Null), "# no block\n");
    }

    #[test]
    fn a_doc_without_a_block_gets_one_and_a_bom_is_kept() {
        assert_eq!(
            set("# T\n", "epic", json!("SW")),
            "---\nepic: SW\n---\n# T\n"
        );
        let out = set("\u{feff}---\nstatus: todo\n---\n", "status", json!("done"));
        assert_eq!(out, "\u{feff}---\nstatus: done\n---\n");
    }

    #[test]
    fn indented_lookalikes_and_prefix_keys_are_not_the_key() {
        let doc = "---\ndescription: |\n  points: 9\npoints_total: 4\n---\n";
        let out = set(doc, "points", json!(1));
        assert_eq!(
            out,
            "---\ndescription: |\n  points: 9\npoints_total: 4\npoints: 1\n---\n"
        );
    }

    #[test]
    fn refuses_bad_names_values_and_unclosed_blocks_without_writing() {
        for bad in ["", "a b", "a:b", "x.y", "1st", "a\nb: c"] {
            assert!(
                set_markdown_field(DOC, bad, &json!("v")).is_err(),
                "{bad:?}"
            );
        }
        assert!(set_markdown_field(DOC, "title", &json!("a\nb")).is_err());
        assert!(set_markdown_field(DOC, "title", &json!({ "a": 1 })).is_err());
        assert!(set_markdown_field(DOC, "tags", &json!([["x"]])).is_err());
        assert!(set_markdown_field(DOC, "tags", &json!([null])).is_err());
        assert!(set_markdown_field("---\nstatus: todo\n", "status", &json!("done")).is_err());
    }

    #[test]
    fn several_fields_apply_in_order_and_one_bad_value_fails_them_all() {
        let fields = vec![
            ("points".to_string(), json!(8)),
            ("sprint".to_string(), json!("2026-10-A")),
            ("status".to_string(), Value::Null),
        ];
        let out = set_markdown_fields(DOC, &fields).unwrap();
        assert!(out.contains("points: 8\r\n"));
        assert!(out.contains("sprint: 2026-10-A\r\n---"));
        assert!(!out.contains("status:"));
        assert!(out.ends_with("# SW-1\r\n\r\nBody stays   \r\nexactly."));
        let bad = vec![
            ("points".to_string(), json!(1)),
            ("x y".to_string(), json!(1)),
        ];
        assert!(set_markdown_fields(DOC, &bad).is_err());
    }
}
