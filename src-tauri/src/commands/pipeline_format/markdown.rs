// SPDX-License-Identifier: MIT
//! Markdown pipeline document format — YAML frontmatter + `#`/`##` headings.
//!
//! Implements the 4-function format surface for `.md` files:
//! - `extract_meta_fields` — YAML frontmatter via `gray_matter`
//! - `extract_title` — first `# Heading`
//! - `extract_section` — body of `## {name}` section
//! - `rewrite_field` — line-based YAML frontmatter field update

use gray_matter::engine::YAML;
use gray_matter::Matter;
use serde_yaml::Mapping;

use super::{truncate_to_char_boundary, FrontmatterFields};

/// Forgiving frontmatter parser. Delegates to the `gray_matter` crate for
/// fence detection (line-anchored open/close, trailing-whitespace tolerant,
/// CRLF/LF aware) so we don't reinvent the YAML-frontmatter delimitation
/// logic. Returns `FrontmatterFields::default()` on: no frontmatter
/// present, malformed YAML, or fields of the wrong shape.
///
/// Two cheap guards run before involving gray_matter:
///   1. Strip a leading UTF-8 BOM (gray_matter's delimiter match is literal
///      and won't see `---` past a BOM otherwise — Rust's `is_whitespace`
///      doesn't include U+FEFF, so we strip explicitly).
///   2. Bail when the text doesn't begin with `---` at all. gray_matter's
///      no-frontmatter path still allocates a String for the entire
///      content; the prefix check costs three bytes and recovers the
///      zero-allocation fast path for files without frontmatter (the
///      common case for legacy `pipeline.json`-driven epics).
pub fn extract_meta_fields(text: &str) -> FrontmatterFields {
    read_frontmatter(text).fields
}

/// A plain scalar repaired in memory. Line numbers include the opening fence.
#[derive(Debug, Clone)]
pub(crate) struct UnquotedLine {
    pub line: usize,
    pub key: String,
    pub value: String,
}

#[derive(Default)]
pub(crate) struct MarkdownFrontmatter {
    pub fields: FrontmatterFields,
    pub mapping: Option<Mapping>,
    pub block_parse_ok: bool,
    pub repaired_lines: Vec<UnquotedLine>,
}

fn strict_frontmatter(text: &str) -> Option<MarkdownFrontmatter> {
    raw_meta_block(text)?;
    let parsed = Matter::<YAML>::new()
        .parse::<FrontmatterFields>(text)
        .ok()?;
    let mapping = if parsed.matter.trim().is_empty() {
        None
    } else {
        Some(serde_yaml::from_str::<Mapping>(&parsed.matter).ok()?)
    };
    Some(MarkdownFrontmatter {
        fields: parsed.data.unwrap_or_default(),
        mapping,
        block_parse_ok: true,
        repaired_lines: Vec::new(),
    })
}

/// Only quote top-level plain scalars containing a colon followed by a space.
/// The repaired block must pass the same typed parser and mapping check.
fn recover_frontmatter(text: &str) -> Option<MarkdownFrontmatter> {
    let body = raw_meta_block(text)?;
    let mut repaired_lines = Vec::new();
    let lines: Vec<String> = body
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if let Some((key, value)) = plain_scalar_line(line) {
                let (scalar, comment) = value
                    .split_once(" #")
                    .map_or((value, None), |(scalar, comment)| (scalar, Some(comment)));
                // A colon in an existing comment does not make the scalar invalid.
                if scalar.contains(": ") || (comment.is_some() && scalar.ends_with(':')) {
                    let (value, suffix) = match comment {
                        Some(comment) if comment.chars().next().is_none_or(char::is_whitespace) => {
                            (scalar.trim_end(), format!(" #{comment}"))
                        }
                        _ => (value, String::new()),
                    };
                    repaired_lines.push(UnquotedLine {
                        line: index + 2,
                        key: key.to_string(),
                        value: value.to_string(),
                    });
                    return format!("{key}: {}{suffix}", render_yaml_scalar(value));
                }
            }
            line.to_string()
        })
        .collect();
    if repaired_lines.is_empty() {
        return None;
    }
    let repaired = format!("---\n{}\n---\n", lines.join("\n"));
    let mut parsed = strict_frontmatter(&repaired)?;
    parsed.block_parse_ok = false;
    parsed.repaired_lines = repaired_lines;
    Some(parsed)
}

/// The line shape shared by colon recovery and unquoted-hash diagnostics.
pub(crate) fn plain_scalar_line(line: &str) -> Option<(&str, &str)> {
    if line.starts_with(char::is_whitespace) || line.starts_with(['#', '-']) {
        return None;
    }
    let (key, value) = line.split_once(':')?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() || value.is_empty() || value.starts_with(['\'', '"', '[', '{', '|', '>']) {
        return None;
    }
    Some((key, value))
}

/// Read-only recovery for pipeline, metadata-band and context-query readers.
/// A repaired block remains strictly invalid so the band opens its raw editor.
pub(crate) fn read_frontmatter(text: &str) -> MarkdownFrontmatter {
    let text = text.trim_start_matches('\u{feff}');
    if !text.starts_with("---") {
        return MarkdownFrontmatter {
            block_parse_ok: true,
            ..Default::default()
        };
    }
    strict_frontmatter(text)
        .or_else(|| recover_frontmatter(text))
        .unwrap_or_default()
}

/// Return the verbatim YAML frontmatter body (the text **between** the opening
/// `---` and closing `---` fences), byte-for-byte as it appears on disk — no
/// re-serialization. `None` when there is no frontmatter block at all (no
/// leading `---`) or the block is unclosed.
///
/// This is the exact region `frontmatter_editor::replace_markdown_block` swaps,
/// so `raw_meta_block` round-trips with `ReplaceRawBlock` (the malformed-recovery
/// raw editor, AC-12). A BOM prefix is ignored for detection but is not part of
/// the block. Works even when the YAML is malformed — the fences are located
/// line-wise, independent of whether the body parses.
pub fn raw_meta_block(text: &str) -> Option<String> {
    let body = text.trim_start_matches('\u{feff}');
    if !body.starts_with("---") {
        return None;
    }
    let lines: Vec<&str> = body.lines().collect();
    let close_idx = frontmatter_close_idx(&lines)?;
    // Lines strictly between the opening fence (index 0) and the closing fence.
    Some(lines[1..close_idx].join("\n"))
}

/// SW-GT-18 (AC-5): the top-level YAML frontmatter **key names**, canonicalized and
/// preserving the custom keys [`extract_meta_fields`] drops. Reuses [`raw_meta_block`]
/// to isolate the verbatim frontmatter body (DRY with the raw editor), then parses it
/// into a `serde_yaml::Mapping`. Each key is funneled through
/// [`super::canonical_field_key`] (trim + lowercase + top-level collapse) so the key set
/// matches the HTML side for equivalent stories, then de-duplicated first-seen.
///
/// If the body does not parse as a mapping (malformed YAML, or a **duplicate key** —
/// which serde_yaml 0.9 rejects), it degrades to a tolerant top-level line scan rather
/// than silently dropping every key (which would falsely report present fields as
/// missing for the SW-SR runner consumer). Empty only when there is no frontmatter.
pub fn extract_metadata_keys(text: &str) -> Vec<String> {
    let Some(body) = raw_meta_block(text) else {
        return Vec::new();
    };
    let raw_keys: Vec<String> = match serde_yaml::from_str::<Mapping>(&body) {
        Ok(mapping) => mapping
            .keys()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect(),
        Err(_) => scan_top_level_keys(&body),
    };
    let mut keys: Vec<String> = Vec::new();
    for raw in raw_keys {
        let key = super::canonical_field_key(&raw);
        if !key.is_empty() && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// SW-SR-10: the top-level frontmatter fields as `(raw_key, joined_value)` pairs —
/// the **value** sibling of [`extract_metadata_keys`], keyed by the raw YAML name
/// (canonicalization + `{storyPath}` synthesis happen once in
/// [`super::story_template_values`]). Each value is [`super::yaml_value_items`]
/// flattened then `", "`-joined, so a scalar passes through verbatim, a list
/// `[a, b]` becomes `"a, b"` (matching HTML's CSV form — format parity), and a
/// nested `done:` mapping yields `""` (the caller drops it). Unlike the key
/// extractor there is no line-scan fallback: a frontmatter body that does not
/// parse as a mapping yields no values (a malformed story resolves no vars and is
/// held at dispatch rather than dispatched against blanks).
pub fn template_value_pairs(text: &str) -> Vec<(String, String)> {
    let Some(body) = raw_meta_block(text) else {
        return Vec::new();
    };
    let Ok(mapping) = serde_yaml::from_str::<Mapping>(&body) else {
        return Vec::new();
    };
    mapping
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str()?.to_string();
            Some((key, super::yaml_value_items(v).join(", ")))
        })
        .collect()
}

/// Tolerant recovery of top-level YAML keys from a frontmatter body that failed to
/// parse as a mapping (malformed, or a duplicate key). A top-level key line is
/// unindented and carries a `:`; indented lines, list items (`-`), comments (`#`) and
/// blanks are skipped. Heuristic by design — strictly better than dropping every key.
fn scan_top_level_keys(body: &str) -> Vec<String> {
    body.lines()
        .filter(|l| {
            let first = l.chars().next();
            !matches!(first, None | Some(' ') | Some('\t') | Some('#') | Some('-'))
        })
        .filter_map(|l| l.split_once(':').map(|(k, _)| k.trim().to_string()))
        .filter(|k| !k.is_empty())
        .collect()
}

/// SLW-MD-1 (FIX 6 / AC-11): does the markdown frontmatter block parse?
///   - No `---` opener at all ⇒ `true` (a plain doc is not "malformed").
///   - `---` opener present but the YAML body fails to parse into a mapping ⇒
///     `false` (present-but-unparseable — the nav-badge / raw-editor case).
///   - Empty block (`---\n---`) ⇒ `true` (well-formed, if pointless).
///
/// This is the SAME detection the editor's `block_parse_ok` uses (factored here
/// so the scan and the band never disagree on what "unparseable" means).
#[cfg(test)]
pub fn block_parse_ok(text: &str) -> bool {
    read_frontmatter(text).block_parse_ok
}

/// The scanner's typed fields and strict validity from the shared read path.
pub fn extract_meta_and_parse_ok(text: &str) -> (FrontmatterFields, bool) {
    let parsed = read_frontmatter(text);
    (parsed.fields, parsed.block_parse_ok)
}

/// First H1 (`# Heading`) scan over the buffered text. Matches the original
/// `extract_title` semantics: skips lines that don't trim-start to `# `;
/// the first non-empty heading text wins.
pub fn extract_title(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim().to_string();
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

/// Extract the body of a `## {heading}` section from the buffered text.
/// Returns `None` if no matching heading is found or if the section body
/// is empty after trimming. Stops at the next `##` heading or end of text.
/// When `max_chars` is `Some(n)`, the returned body is truncated to at most
/// `n` characters (on a char boundary).
pub fn extract_section(text: &str, heading: &str, max_chars: Option<usize>) -> Option<String> {
    let mut in_section = false;
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !in_section {
            if let Some(rest) = trimmed.strip_prefix("## ") {
                let h = rest.trim();
                if h.eq_ignore_ascii_case(heading) {
                    in_section = true;
                }
            }
        } else {
            // Next ## heading ends the section
            if trimmed.starts_with("## ") {
                break;
            }
            lines.push(line);
        }
    }
    if lines.is_empty() {
        return None;
    }
    let body = lines.join("\n").trim().to_string();
    if body.is_empty() {
        return None;
    }
    match max_chars {
        Some(n) if body.chars().count() > n => Some(truncate_to_char_boundary(&body, n)),
        _ => Some(body),
    }
}

/// Index of the closing `---` fence within frontmatter (scanning lines after
/// the opening fence). `None` when the frontmatter is unclosed. Shared by the
/// line-based rewriters.
fn frontmatter_close_idx(lines: &[&str]) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, l)| l.trim() == "---")
        .map(|(i, _)| i)
}

/// Predicate matching a TOP-LEVEL (unindented) `field:` line — `field: value`,
/// bare `field:`, or `field :`. Never matches an indented line, so a same-named
/// token inside a block scalar can't be clobbered. The shared field-line
/// matcher for `rewrite_field` / `rewrite_list_field`.
fn is_top_level_field(field_name: &str) -> impl Fn(&str) -> bool + '_ {
    let colon_space = format!("{field_name}: ");
    let colon_only = format!("{field_name}:");
    let space_colon = format!("{field_name} :");
    move |line: &str| {
        line.starts_with(&colon_space) || line == colon_only || line.starts_with(&space_colon)
    }
}

/// Pure string transform: rewrite a top-level YAML frontmatter field, or
/// insert it, or prepend a frontmatter block. No IO.
///
/// Design note: we use `gray_matter` for *reading* frontmatter (forgiving,
/// handles edge cases) but line-based manipulation for *writing* to preserve
/// formatting exactly as-is. A YAML round-trip through serde_yaml would
/// normalize quotes, spacing, and field order — producing noisy git diffs
/// for a single-field update. Line-based rewriting is safe here because
/// frontmatter fields are always simple top-level scalars.
/// SLW-MD-1 (FIX 2): does a scalar value need YAML double-quoting to survive a
/// re-read as *exactly* this string? A plain safe value stays unquoted (clean
/// diffs). We quote conservatively to cover the cases that would otherwise
/// re-parse as a different type/shape or break the `key: value` line:
///   - empty (would read as `null`)
///   - leading/trailing whitespace (YAML trims unquoted scalars)
///   - a `:` followed by space or at end (`k: v: w` / `k: v:` mis-parses)
///   - `#` preceded by a space (starts a comment) — quote if it contains `#`
///   - leading YAML indicator: `[ { ] } , & * ! | > ' " % @ \` ? : - #` and `~`
///   - already contains a quote
///   - `true`/`false`/`null`/`~`/numeric-looking (would read as bool/null/number)
///   - the YAML-1.1 bool words `yes`/`no`/`on`/`off`/`y`/`n` (case-insensitive)
///   - the YAML float specials `.inf`/`-.inf`/`+.inf`/`.nan` (case-insensitive)
///   - hex (`0x1f`) / octal (`0o17`) integer forms (optional sign)
///
/// The word/number checks are case-insensitive (`TRUE`, `Off`, `Y`, `.INF` all
/// quote) so a lenient YAML/MCP consumer can't type-drift on a case variant.
fn yaml_scalar_needs_quoting(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if value != value.trim() {
        return true;
    }
    if value.contains('"') || value.contains('\'') {
        return true;
    }
    // `: ` or a trailing `:` turns the value into a nested mapping / breaks the line.
    if value.contains(": ") || value.ends_with(':') {
        return true;
    }
    // ` #` starts an inline comment.
    if value.contains(" #") {
        return true;
    }
    // Leading YAML indicator characters.
    let first = value.chars().next().unwrap();
    const LEADING_INDICATORS: &[char] = &[
        '[', ']', '{', '}', ',', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`', '?', ':', '-',
        '#', '~', ' ',
    ];
    if LEADING_INDICATORS.contains(&first) {
        return true;
    }
    // Values that would re-read as a non-string scalar under a LENIENT YAML
    // reader. serde_yaml 0.9 (YAML-1.2-ish) coerces our `String` fields back to
    // strings today, so there is no live bug — but a future generic-YAML/MCP
    // consumer could type-drift on any of these, so we quote conservatively.
    let lower = value.to_ascii_lowercase();
    // YAML-1.1 boolean words (case-insensitive): the 1.2 pair plus the legacy
    // `yes/no/on/off/y/n` forms a lenient reader may still coerce.
    if matches!(
        lower.as_str(),
        "true" | "false" | "null" | "~" | "yes" | "no" | "on" | "off" | "y" | "n"
    ) {
        return true;
    }
    // `.inf` / `-.inf` / `+.inf` / `.nan` (YAML float specials, case-insensitive).
    if matches!(lower.as_str(), ".inf" | "-.inf" | "+.inf" | ".nan") {
        return true;
    }
    // Hex (`0x1f`) / octal (`0o17`) integer forms a lenient reader parses as a
    // number. `str::parse::<i64>` does NOT accept these, so detect explicitly
    // (with an optional sign) — only when the remaining digits are valid for the
    // radix (a bare `0x`/`0o` or `0xzz` stays a plain string).
    if is_yaml_radix_int(&lower) {
        return true;
    }
    if value.parse::<f64>().is_ok() || value.parse::<i64>().is_ok() {
        return true;
    }
    false
}

/// Does `lower` (already ASCII-lowercased) look like a YAML hex (`0x1f`) or octal
/// (`0o17`) integer literal? Accepts an optional leading `+`/`-` sign. Requires
/// at least one valid radix digit after the prefix so a bare `0x` or a
/// non-hex `0xzz` is NOT treated as a number (stays an unquoted plain string).
fn is_yaml_radix_int(lower: &str) -> bool {
    let body = lower.strip_prefix(['+', '-']).unwrap_or(lower);
    if let Some(hex) = body.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if let Some(oct) = body.strip_prefix("0o") {
        return !oct.is_empty() && oct.bytes().all(|b| (b'0'..=b'7').contains(&b));
    }
    false
}

/// SLW-MD-1 (FIX 2): render a scalar value for a `key: {value}` frontmatter
/// line — quoted (double-quoted, backslash-escaped) only when necessary, else
/// verbatim (keeps single-field diffs clean). Callers must reject raw
/// newlines/control chars before this (a scalar can't span lines).
pub fn render_yaml_scalar(value: &str) -> String {
    if yaml_scalar_needs_quoting(value) {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

pub fn rewrite_field(content: &str, field_name: &str, raw_value: &str) -> String {
    // SLW-MD-1 (FIX 2): quote/escape the scalar so a YAML-significant value
    // (`:` + space, leading indicator, `#`, quotes, bool/number-looking, …)
    // re-reads as exactly this string instead of corrupting the block.
    let new_value = render_yaml_scalar(raw_value);
    let new_value = new_value.as_str();
    let trimmed = content.trim_start_matches('\u{feff}');

    // No frontmatter at all — prepend a block.
    if !trimmed.starts_with("---") {
        return format!("---\n{}: {}\n---\n{}", field_name, new_value, content);
    }

    let lines: Vec<&str> = content.lines().collect();
    let Some(close_idx) = frontmatter_close_idx(&lines) else {
        // Unclosed frontmatter — treat as no frontmatter, prepend.
        return format!("---\n{}: {}\n---\n{}", field_name, new_value, content);
    };

    let is_field_line = is_top_level_field(field_name);

    // Look for an existing top-level field line within the frontmatter.
    // Only match lines with no leading indentation to avoid false positives
    // inside YAML block scalars (e.g. a multi-line `description:` value
    // that contains "field: something" on an indented line).
    let found = lines[1..close_idx].iter().any(|line| is_field_line(line));

    let mut result = String::with_capacity(content.len() + 20);

    if found {
        // Replace the existing field line.
        for (i, line) in lines.iter().enumerate() {
            if i > 0 && i < close_idx && is_field_line(line) {
                result.push_str(&format!("{}: {}", field_name, new_value));
                result.push('\n');
                continue;
            }
            result.push_str(line);
            result.push('\n');
        }
    } else {
        // Insert field before the closing fence.
        for (i, line) in lines.iter().enumerate() {
            if i == close_idx {
                result.push_str(&format!("{}: {}\n", field_name, new_value));
            }
            result.push_str(line);
            result.push('\n');
        }
    }

    // The original content may or may not end with a newline. `lines()` strips
    // the trailing newline. If the original ended with \n, our loop already
    // added it. If it didn't, we have one extra. Align with the original.
    if !content.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}

/// Pure string transform: rewrite a top-level YAML frontmatter *list* field as
/// a block sequence, insert it, or remove it (empty `items`). The block-list
/// sibling of [`rewrite_field`] — same line-based, format-preserving approach
/// (NO `serde_yaml` round-trip), so a hand-edited doc keeps its other fields,
/// ordering, comments, and body byte-for-byte; only the target field's span
/// changes. No IO.
///
/// Span handling:
///   - inline `field: [a, b]` → replaced with a block sequence
///   - block `field:\n  - a\n  - b` → the field line plus its indented `- `
///     items are replaced as one span
///   - absent → inserted before the closing fence
///   - empty `items` → the field is removed entirely (no empty `field:` left)
///
/// Like [`rewrite_field`], this only matches a **top-level** (unindented) field
/// line, so a `repos:`-looking token inside a block scalar can't be clobbered.
pub fn rewrite_list_field(content: &str, field_name: &str, items: &[String]) -> String {
    let trimmed = content.trim_start_matches('\u{feff}');

    let render_block = || -> String {
        let mut b = String::with_capacity(field_name.len() + items.len() * 16 + 2);
        b.push_str(field_name);
        b.push_str(":\n");
        for item in items {
            b.push_str("  - ");
            b.push_str(item);
            b.push('\n');
        }
        b
    };

    // No frontmatter at all — prepend a block (nothing to remove if empty).
    if !trimmed.starts_with("---") {
        if items.is_empty() {
            return content.to_string();
        }
        return format!("---\n{}---\n{}", render_block(), content);
    }

    let lines: Vec<&str> = content.lines().collect();
    let Some(close_idx) = frontmatter_close_idx(&lines) else {
        // Unclosed frontmatter — treat as no frontmatter, prepend.
        if items.is_empty() {
            return content.to_string();
        }
        return format!("---\n{}---\n{}", render_block(), content);
    };

    let is_field_line = is_top_level_field(field_name);
    // An indented `- …` item line belonging to the field's block sequence.
    let is_list_item = |line: &str| -> bool {
        let t = line.trim_start();
        line.len() != t.len() && t.starts_with('-')
    };

    let field_idx = (1..close_idx).find(|&i| is_field_line(lines[i]));

    let mut result = String::with_capacity(content.len() + 32);

    match field_idx {
        Some(fi) => {
            // The span = the field line + any following indented list items.
            let mut span_end = fi + 1;
            while span_end < close_idx && is_list_item(lines[span_end]) {
                span_end += 1;
            }
            for (i, line) in lines.iter().enumerate() {
                if i == fi {
                    if !items.is_empty() {
                        result.push_str(&render_block()); // ends with '\n'
                    }
                    // Empty items → field removed: emit nothing for the span.
                    continue;
                }
                if i > fi && i < span_end {
                    continue; // old block-list items — dropped
                }
                result.push_str(line);
                result.push('\n');
            }
        }
        None => {
            if items.is_empty() {
                return content.to_string(); // nothing to insert or remove
            }
            for (i, line) in lines.iter().enumerate() {
                if i == close_idx {
                    result.push_str(&render_block());
                }
                result.push_str(line);
                result.push('\n');
            }
        }
    }

    // Trailing-newline parity with the original (see `rewrite_field`).
    if !content.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}

/// SLW-MD-1 (FIX 7): remove a top-level frontmatter key AND all of its
/// more-indented continuation lines — list items (`- x`) AND nested mapping
/// children (`  child: val`). `rewrite_list_field(&[])` only drops `- ` items,
/// so clearing a key whose value is a nested mapping (e.g. `owner:\n  name: x`)
/// orphaned the child lines into a malformed block. This block-aware removal
/// drops the whole span: the key line plus every following line that is more
/// indented than the key line (any non-blank content), stopping at the first
/// line indented ≤ the key. Blank lines inside the span are absorbed. No IO;
/// same line-based, format-preserving posture as `rewrite_list_field`.
///
/// A no-op when the key is absent or there is no (closed) frontmatter block.
pub fn remove_block_field(content: &str, field_name: &str) -> String {
    let trimmed = content.trim_start_matches('\u{feff}');
    if !trimmed.starts_with("---") {
        return content.to_string();
    }
    let lines: Vec<&str> = content.lines().collect();
    let Some(close_idx) = frontmatter_close_idx(&lines) else {
        return content.to_string();
    };

    let is_field_line = is_top_level_field(field_name);
    let Some(field_idx) = (1..close_idx).find(|&i| is_field_line(lines[i])) else {
        return content.to_string(); // key absent — nothing to remove
    };

    // Indentation of a line (count of leading spaces/tabs). A top-level key has
    // indent 0; its continuation lines (list items, mapping children) are more
    // indented. Blank lines are treated as part of the span (indent = usize::MAX
    // so they never terminate it prematurely).
    let indent_of = |line: &str| -> usize {
        if line.trim().is_empty() {
            return usize::MAX;
        }
        line.len() - line.trim_start().len()
    };
    let key_indent = indent_of(lines[field_idx]); // 0 for a top-level key

    // Span = the field line + following lines strictly more indented than it
    // (within the frontmatter). Absorb trailing blank lines that belong to it,
    // but never the closing fence.
    let mut span_end = field_idx + 1;
    while span_end < close_idx {
        let ind = indent_of(lines[span_end]);
        if ind == usize::MAX {
            // blank line: only absorb if a more-indented continuation follows.
            let mut look = span_end + 1;
            while look < close_idx && indent_of(lines[look]) == usize::MAX {
                look += 1;
            }
            if look < close_idx && indent_of(lines[look]) > key_indent {
                span_end = look; // continues; skip the blank(s)
                continue;
            }
            break;
        }
        if ind > key_indent {
            span_end += 1;
        } else {
            break;
        }
    }

    let mut result = String::with_capacity(content.len());
    for (i, line) in lines.iter().enumerate() {
        if i >= field_idx && i < span_end {
            continue; // dropped span
        }
        result.push_str(line);
        result.push('\n');
    }

    // Trailing-newline parity with the original (see `rewrite_field`).
    if !content.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_plain_colons_without_accepting_structural_errors() {
        let text = "---\ntype: story\nstory_id: A\ntitle: Inbox: first pass\nreadiness_pass: yes: with notes\n---\n";
        let (fields, ok) = extract_meta_and_parse_ok(text);
        assert_eq!(fields.story_id.as_deref(), Some("A"));
        let parsed = read_frontmatter(text);
        assert_eq!(parsed.repaired_lines.len(), 2);
        let map = parsed.mapping.unwrap();
        assert_eq!(map["title"].as_str(), Some("Inbox: first pass"));
        assert_eq!(map["readiness_pass"].as_str(), Some("yes: with notes"));
        assert!(!ok);
        assert_eq!(fields, extract_meta_fields(text));
        assert_eq!(ok, block_parse_ok(text));
        for bad in [
            "---\ntype: story\n  note: a: b\n---\n",
            "---\ntype: story\ntitle: a: b\ndepends_on: [A\n---\n",
        ] {
            assert!(extract_meta_fields(bad).is_empty());
            assert!(!block_parse_ok(bad));
        }
    }

    #[test]
    fn recovery_preserves_inline_comments_and_ignores_their_colons() {
        let text = "---\ntype: story\nstatus: todo # next: review\ntitle: Inbox # note: first pass\nreadiness_pass: yes: with notes\n---\n";
        let parsed = read_frontmatter(text);
        assert_eq!(parsed.fields.status.as_deref(), Some("todo"));
        assert_eq!(parsed.repaired_lines.len(), 1);
        assert_eq!(parsed.repaired_lines[0].key, "readiness_pass");
        assert_eq!(parsed.mapping.unwrap()["title"].as_str(), Some("Inbox"));
        let text = "---\ntype: story\ntitle: Inbox: first pass # note: design\n---\n";
        let parsed = read_frontmatter(text);
        assert_eq!(
            parsed.mapping.unwrap()["title"].as_str(),
            Some("Inbox: first pass")
        );
        let text = "---\ntype: story\ntitle: Inbox: Fix #42\n---\n";
        assert_eq!(
            read_frontmatter(text).mapping.unwrap()["title"].as_str(),
            Some("Inbox: Fix #42")
        );
    }

    #[test]
    fn recovery_keeps_a_colon_immediately_before_a_hash() {
        for (title, expected) in [("Inbox: #42", "Inbox: #42"), ("Inbox: # note", "Inbox:")] {
            let text = format!("---\ntype: story\nstory_id: A\ntitle: {title}\n---\n");
            let parsed = read_frontmatter(&text);
            assert_eq!(parsed.fields.story_id.as_deref(), Some("A"), "{title}");
            assert!(!parsed.block_parse_ok);
            assert_eq!(parsed.repaired_lines.len(), 1);
            assert_eq!(parsed.mapping.unwrap()["title"].as_str(), Some(expected));
        }
    }

    /// The single-pass `extract_meta_and_parse_ok` must return byte-identical
    /// results to calling `extract_meta_fields` + `block_parse_ok` separately —
    /// across no-block, well-formed, empty-block, malformed, and duplicate-key
    /// inputs. Locks the equivalence so the parse-consolidation can't silently
    /// drift the fields or the nav-badge validity bit (esp. duplicate-key
    /// rejection, which the strict `Mapping` check owns).
    #[test]
    fn extract_meta_and_parse_ok_matches_separate_calls() {
        let cases = [
            ("# Title only, no frontmatter\n", "no block"),
            ("---\nstatus: done\ntitle: X\n---\n# Body\n", "well-formed"),
            ("---\n---\n# Body\n", "empty block"),
            (
                "---\nstatus: done\n  bad: : indent\n\t- broken\n---\n",
                "malformed",
            ),
            ("---\nstatus: done\nstatus: qa\n---\n", "duplicate key"),
            ("\u{feff}---\nstatus: done\n---\n", "bom prefix"),
            // Valid YAML mapping whose value is the wrong shape for the typed field
            // (`status` is a String, here a mapping) — the field-parse Errs while the
            // body still parses as a Mapping. This is the one input where the combined
            // fn's Err arm and its no-BOM strict-mapping arm take different code paths,
            // so it locks the "byte-identical to the two separate calls" claim there.
            (
                "---\nstatus:\n  nested: value\n---\n# Body\n",
                "valid mapping, typed-field-parse-fails",
            ),
        ];
        for (text, label) in cases {
            let (combined_fields, combined_ok) = extract_meta_and_parse_ok(text);
            let sep_fields = extract_meta_fields(text);
            let sep_ok = block_parse_ok(text);
            assert_eq!(combined_ok, sep_ok, "block_parse_ok mismatch for {label}");
            assert_eq!(combined_fields, sep_fields, "fields mismatch for {label}");
        }
    }

    // ---- rewrite_list_field (SW-EW-9) ----

    fn slugs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rewrite_list_replaces_existing_block() {
        let input = "---\ntype: epic\nrepos:\n  - a/one\n  - b/two\nbranch: x\n---\n# Body\n";
        let out = rewrite_list_field(input, "repos", &slugs(&["c/three"]));
        assert_eq!(
            out,
            "---\ntype: epic\nrepos:\n  - c/three\nbranch: x\n---\n# Body\n"
        );
    }

    #[test]
    fn rewrite_list_converts_inline_to_block() {
        let input = "---\ntype: epic\nrepos: [a/one, b/two]\n---\n# Body\n";
        let out = rewrite_list_field(input, "repos", &slugs(&["a/one", "b/two", "c/three"]));
        assert_eq!(
            out,
            "---\ntype: epic\nrepos:\n  - a/one\n  - b/two\n  - c/three\n---\n# Body\n"
        );
    }

    #[test]
    fn rewrite_list_inserts_when_absent() {
        let input = "---\ntype: epic\nstatus: draft\n---\n# Body\n";
        let out = rewrite_list_field(input, "repos", &slugs(&["a/one"]));
        assert_eq!(
            out,
            "---\ntype: epic\nstatus: draft\nrepos:\n  - a/one\n---\n# Body\n"
        );
    }

    #[test]
    fn rewrite_list_empty_removes_block_field() {
        let input = "---\ntype: epic\nrepos:\n  - a/one\n  - b/two\nstatus: draft\n---\n# Body\n";
        let out = rewrite_list_field(input, "repos", &[]);
        assert_eq!(out, "---\ntype: epic\nstatus: draft\n---\n# Body\n");
    }

    #[test]
    fn rewrite_list_empty_removes_inline_field() {
        let input = "---\ntype: epic\nrepos: [a/one]\nstatus: draft\n---\n# Body\n";
        let out = rewrite_list_field(input, "repos", &[]);
        assert_eq!(out, "---\ntype: epic\nstatus: draft\n---\n# Body\n");
    }

    #[test]
    fn rewrite_list_empty_absent_is_noop() {
        let input = "---\ntype: epic\nstatus: draft\n---\n# Body\n";
        let out = rewrite_list_field(input, "repos", &[]);
        assert_eq!(out, input);
    }

    #[test]
    fn rewrite_list_prepends_when_no_frontmatter() {
        let input = "# Just a body\n";
        let out = rewrite_list_field(input, "repos", &slugs(&["a/one"]));
        assert_eq!(out, "---\nrepos:\n  - a/one\n---\n# Just a body\n");
    }

    #[test]
    fn rewrite_list_preserves_trailing_newline_absence() {
        // No trailing newline on input → none on output.
        let input = "---\ntype: epic\nrepos:\n  - a/one\n---\n# Body";
        let out = rewrite_list_field(input, "repos", &slugs(&["b/two"]));
        assert_eq!(out, "---\ntype: epic\nrepos:\n  - b/two\n---\n# Body");
        assert!(!out.ends_with('\n'));
    }

    #[test]
    fn rewrite_list_preserves_other_fields_and_comments() {
        let input = "---\ntype: epic\n# keep me\nrepos:\n  - a/one\nstatus: draft\n---\nBody\n";
        let out = rewrite_list_field(input, "repos", &slugs(&["z/nine"]));
        assert!(out.contains("# keep me"), "comment preserved");
        assert!(out.contains("type: epic"), "other field preserved");
        assert!(out.contains("status: draft"), "trailing field preserved");
        assert!(out.contains("  - z/nine"), "new item present");
        assert!(!out.contains("a/one"), "old item gone");
    }

    #[test]
    fn rewrite_list_round_trips_through_extract() {
        // The block we emit must re-parse to the same slugs via the read path.
        let input = "---\ntype: epic\n---\n# Body\n";
        let added = rewrite_list_field(input, "repos", &slugs(&["a/one", "b/two"]));
        let fields = extract_meta_fields(&added);
        assert_eq!(fields.repos, Some(slugs(&["a/one", "b/two"])));
    }

    // ---- remove_block_field (SLW-MD-1 FIX 7) ----

    #[test]
    fn remove_block_field_drops_nested_mapping_children() {
        // Clearing `owner:` whose value is a nested map must remove the whole
        // block (key + indented children), leaving a parseable doc.
        let input = "---\ntype: story\nowner:\n  name: x\n  id: 1\nstatus: todo\n---\n# Body\n";
        let out = remove_block_field(input, "owner");
        assert_eq!(out, "---\ntype: story\nstatus: todo\n---\n# Body\n");
        // And it re-parses cleanly.
        let fields = extract_meta_fields(&out);
        assert_eq!(fields.status.as_deref(), Some("todo"));
    }

    #[test]
    fn remove_block_field_drops_list_items_too() {
        let input = "---\ntype: epic\nrepos:\n  - a/one\n  - b/two\nstatus: draft\n---\n# B\n";
        let out = remove_block_field(input, "repos");
        assert_eq!(out, "---\ntype: epic\nstatus: draft\n---\n# B\n");
    }

    #[test]
    fn remove_block_field_scalar_and_absent() {
        let input = "---\ntype: story\nstatus: todo\n---\n# B\n";
        assert_eq!(
            remove_block_field(input, "status"),
            "---\ntype: story\n---\n# B\n"
        );
        // Absent key → no-op.
        assert_eq!(remove_block_field(input, "nope"), input);
    }

    // ---- FIX 2: YAML scalar quoting robustness (hex/octal/.inf/.nan/1.1 bools) ----

    #[test]
    fn yaml_quoting_covers_ambiguous_scalars() {
        // Each of these could be interpreted as a non-string scalar by a lenient
        // YAML reader — the predicate must quote them all.
        for v in [
            "0x1f", "0X1F", "0o17", ".inf", ".INF", "-.inf", "+.inf", ".nan", "on", "off", "OFF",
            "on", "y", "Y", "n", "N", "yes", "YES", "no", "true", "TRUE", "false", "null", "NULL",
        ] {
            assert!(
                yaml_scalar_needs_quoting(v),
                "{v:?} must be quoted (ambiguous scalar)"
            );
        }
    }

    #[test]
    fn yaml_quoting_leaves_plain_tokens_unquoted() {
        // Clean diffs: a plain string that no lenient reader would coerce stays
        // unquoted — including near-misses that are NOT valid radix ints.
        for v in [
            "in-progress",
            "story",
            "owner/repo",
            "0xzz", // bad hex digits → plain string
            "0x",   // bare prefix, no digits → plain string
            "0o8",  // 8 is not an octal digit → plain string
            "yesish",
            "online", // starts with "on" but is not the bool word
        ] {
            assert!(
                !yaml_scalar_needs_quoting(v),
                "{v:?} must stay unquoted (plain string)"
            );
        }
    }

    #[test]
    fn render_yaml_scalar_ambiguous_values_round_trip_and_are_quoted() {
        // `0x1f`, `.inf`, `on`, `Y`, `OFF` each: (a) appear QUOTED in the written
        // frontmatter line, and (b) re-read back as the EXACT original string.
        for v in ["0x1f", ".inf", "on", "Y", "OFF"] {
            let rendered = render_yaml_scalar(v);
            assert!(
                rendered.starts_with('"') && rendered.ends_with('"'),
                "{v:?} must be quoted in output, got {rendered}"
            );
            // Written into a real frontmatter line and re-parsed → exact string.
            let doc = format!("---\nstory_id: {rendered}\n---\n# B\n");
            let fields = extract_meta_fields(&doc);
            assert_eq!(
                fields.story_id.as_deref(),
                Some(v),
                "{v:?} must round-trip as the exact string, doc: {doc}"
            );
        }
    }
}
