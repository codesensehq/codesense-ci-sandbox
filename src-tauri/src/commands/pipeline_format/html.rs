// SPDX-License-Identifier: MIT
//! HTML pipeline document format — `<meta name="cs:*">` tags + `<title>`/`<h1>`.
//!
//! Implements the 4-function format surface for `.html`/`.htm` files:
//! - `extract_meta_fields` — `<meta name="cs:{field}" content="...">` tags
//! - `extract_title` — `<title>` or first `<h1>`
//! - `extract_section` — `<section data-cs="{name}">` content
//! - `rewrite_field` — update or insert `<meta>` tags
//!
//! Uses regex extraction (no DOM parser) — the 4KB read buffer is tiny and
//! `<meta>` tags are self-closing single-line tags. Same forgiving philosophy
//! as the markdown path: best-effort extraction, default on failure.

use regex::Regex;
use std::sync::LazyLock;

use super::{truncate_to_char_boundary, FrontmatterFields, FIELD_NAME_MAP};

// ----- Regex patterns (compiled once) ----------------------------------------

/// Match `<meta name="cs:{field}" content="{value}">` (name-first order).
static META_NAME_FIRST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<meta\s+name=["']cs:([^"']+)["']\s+content=["']([^"']*)["']\s*/?>"#).unwrap()
});

/// Match `<meta content="{value}" name="cs:{field}">` (content-first order).
static META_CONTENT_FIRST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<meta\s+content=["']([^"']*)["']\s+name=["']cs:([^"']+)["']\s*/?>"#).unwrap()
});

/// Match `<title>...</title>`.
static TITLE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<title>([^<]*)</title>").unwrap());

/// Match `<h1>...</h1>` (first occurrence, innermost text only).
static H1_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<h1[^>]*>([^<]*)</h1>").unwrap());

/// Match `<section data-cs="{name}">` opening tag.
static SECTION_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)<section\s+data-cs=["']([^"']+)["'][^>]*>"#).unwrap());

/// Simple HTML tag stripper for section content.
static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

// ----- Meta tag extraction helpers -------------------------------------------

/// SLW-MD-1 (FIX 2): inverse of [`html_attr_escape`] — decode the entities we
/// emit when writing `content="…"` so a value that was escaped on write reads
/// back byte-for-byte. `&amp;` is decoded LAST so `&amp;lt;` (a literal
/// `&lt;`) survives intact. Only the entities we produce are decoded; any
/// other `&…;` in hand-authored content is left verbatim (best-effort, matching
/// the forgiving regex-not-DOM posture).
fn html_attr_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

/// Collect all `cs:*` meta tag key-value pairs from the text. Values are
/// HTML-attribute-unescaped (see [`html_attr_unescape`]) so they read back
/// byte-for-byte with the write path's [`html_attr_escape`].
fn collect_cs_meta(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    // Name-first: <meta name="cs:field" content="value">
    for cap in META_NAME_FIRST.captures_iter(text) {
        let field = cap[1].to_ascii_lowercase();
        let value = html_attr_unescape(&cap[2]);
        pairs.push((field, value));
    }
    // Content-first: <meta content="value" name="cs:field">
    for cap in META_CONTENT_FIRST.captures_iter(text) {
        let value = html_attr_unescape(&cap[1]);
        let field = cap[2].to_ascii_lowercase();
        // Don't duplicate if already captured by the first regex.
        if !pairs.iter().any(|(f, _)| f == &field) {
            pairs.push((field, value));
        }
    }
    pairs
}

/// Match a full `<meta name="cs:*">` line (either attribute order, self-closing
/// or not), including its surrounding horizontal whitespace and trailing
/// newline. This is the exact pattern `frontmatter_editor::replace_html_block`
/// uses to locate + replace the meta run, so `raw_meta_block` (below) is its
/// faithful inverse.
static CS_META_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)[ \t]*<meta\s+(?:name=["']cs:[^"']+["']\s+content=["'][^"']*["']|content=["'][^"']*["']\s+name=["']cs:[^"']+["'])\s*/?>[ \t]*\r?\n?"#).unwrap()
});

/// Return the verbatim contiguous region of `<meta name="cs:*">` tags — the
/// text spanning from the first `cs:*` meta line to the last, byte-for-byte as
/// on disk (no re-serialization). `None` when there are no `cs:*` meta tags.
///
/// This is the exact region `frontmatter_editor::replace_html_block` swaps (the
/// run of `cs:*` meta lines, `CS_META_LINE`), so `raw_meta_block` round-trips
/// with `ReplaceRawBlock` (the raw editor, AC-12). The span is trimmed of a
/// trailing newline so it reads back as the block body (matching how
/// `replace_html_block` re-inserts `block + "\n"`).
pub fn raw_meta_block(text: &str) -> Option<String> {
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    for m in CS_META_LINE.find_iter(text) {
        if first.is_none() {
            first = Some(m.start());
        }
        last = Some(m.end());
    }
    let (start, end) = (first?, last?);
    Some(text[start..end].trim_end_matches(['\n', '\r']).to_string())
}

/// Split a comma-separated content attribute into a Vec of trimmed strings.
fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ----- 4-function format surface ---------------------------------------------

/// Extract metadata from `<meta name="cs:*">` tags.
///
/// Maps: `cs:status` → status, `cs:type` → document_type,
/// `cs:depends-on` → depends_on (CSV), `cs:supporting-docs` → supporting_docs (CSV),
/// `cs:story-id` → story_id, `cs:source-repo` → source_repo.
///
/// Returns `FrontmatterFields::default()` if no meta tags found (same
/// forgiving pattern as the markdown extractor).
pub fn extract_meta_fields(text: &str) -> FrontmatterFields {
    let pairs = collect_cs_meta(text);
    if pairs.is_empty() {
        return FrontmatterFields::default();
    }
    let mut fm = FrontmatterFields::default();
    for (field, value) in &pairs {
        // Map cs:* name to the struct field. Use FIELD_NAME_MAP in reverse
        // (cs name → yaml name) to find the right field.
        match FIELD_NAME_MAP
            .iter()
            .find(|(_, cs)| {
                // cs is e.g. "cs:status", field is e.g. "status"
                cs.strip_prefix("cs:") == Some(field.as_str())
            })
            .map(|(yaml, _)| *yaml)
        {
            Some("status") => fm.status = Some(value.clone()),
            Some("type") => fm.document_type = Some(value.clone()),
            Some("depends_on") => fm.depends_on = Some(split_csv(value)),
            // SW-AU-77F: `cs:tags` → typed `tags` list (CSV, matching the list write
            // path). Now that `cs:tags` is in FIELD_NAME_MAP it is excluded from HTML
            // open-tags, so this named capture is what keeps the HTML value.
            Some("tags") => fm.tags = Some(split_csv(value)),
            Some("supporting_docs") => fm.supporting_docs = Some(split_csv(value)),
            Some("story_id") => fm.story_id = Some(value.clone()),
            Some("source_repo") => fm.source_repo = Some(value.clone()),
            Some("work_repo") => fm.work_repo = Some(value.clone()),
            // SW-GS-35: story-scoped dev branch (scalar, like `branch` below).
            Some("dev_branch") => fm.dev_branch = Some(value.clone()),
            // SW-WB-1: story-scoped intended branch (scalar; editable).
            Some("branch_name") => fm.branch_name = Some(value.clone()),
            Some("team") => fm.team = Some(value.clone()),

            // SLW-MD-1 (FIX 3): epic fields — symmetric read-back for the
            // newly-mapped cs:repos / cs:branch / cs:scope-context /
            // cs:scope-install so an HTML epic's writes round-trip. Lists go
            // through the CSV split (matching the `rewrite_field` CSV-join write
            // path); `branch` is a scalar.
            Some("repos") => fm.repos = Some(split_csv(value)),
            Some("branch") => fm.branch = Some(value.clone()),
            Some("scope_context") => fm.scope_context = Some(split_csv(value)),
            Some("scope_install") => fm.scope_install = Some(split_csv(value)),
            // SW-SB-1: board metadata (scalars). `points` parses the meta
            // string to f64 tolerantly — a non-numeric OR non-finite value
            // degrades to `None`, matching the markdown `de_lenient_points` posture.
            Some("sprint") => fm.sprint = Some(value.clone()),
            Some("points") => fm.points = value.parse::<f64>().ok().filter(|n| n.is_finite()),
            Some("assigned_to") => fm.assigned_to = Some(value.clone()),
            // SW-SB-22: board-ordering rank — opaque string, same as sprint.
            Some("rank") => fm.rank = Some(value.clone()),
            // SW-AU-77H: standalone-doc automation subject id. Without this arm the
            // FIELD_NAME_MAP entry maps cs:subject-id → subject_id but the value is
            // dropped here, so an HTML doc's marker would silently never read back
            // (markdown gets it free via serde) — the format-parity trap (AC-3).
            Some("subject_id") => fm.subject_id = Some(value.clone()),
            _ => {} // Unrecognized cs:* field — silently ignore
        }
    }
    fm
}

/// SW-GT-18 (AC-5): the metadata **key names** carried by `cs:*` meta tags,
/// normalized to the YAML-name convention (the schema namespace) via
/// [`FIELD_NAME_MAP`] — so an HTML story's key set matches the equivalent markdown
/// story's. A `cs:*` name with no mapping (a team custom field) is de-prefixed
/// verbatim. Each name then runs through the shared [`super::canonical_field_key`]
/// (trim + lowercase + top-level collapse), the same funnel the markdown side uses, so
/// a mixed-case custom field and a nested `cs:done.completed_at` reconcile identically.
/// De-duplicated, first-seen order.
pub fn extract_metadata_keys(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for (field, _) in collect_cs_meta(text) {
        let key = super::canonical_field_key(&super::cs_field_to_yaml_name(&field));
        if !key.is_empty() && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// SW-SR-10: the `cs:*` metadata as `(raw_yaml_key, value)` pairs — the **value**
/// sibling of [`extract_metadata_keys`]. Each `cs:*` name is mapped back to its YAML
/// name via [`FIELD_NAME_MAP`] (an unmapped custom `cs:foo` de-prefixes to `foo`),
/// exactly as the key extractor does; the value is the `collect_cs_meta` string
/// verbatim (already HTML-unescaped and, for a list, in the `", "`-joined CSV form
/// that matches markdown's joined sequence — format parity). Canonicalization,
/// dotted-key drop, and `{storyPath}` synthesis happen once in
/// [`super::story_template_values`].
pub fn template_value_pairs(text: &str) -> Vec<(String, String)> {
    collect_cs_meta(text)
        .into_iter()
        .map(|(field, value)| (super::cs_field_to_yaml_name(&field), value))
        .collect()
}

/// Extract title from `<title>` or first `<h1>`.
///
/// Priority: `<title>` wins over `<h1>`. HTML tags are stripped from
/// the extracted text. Returns `None` if neither found or both empty.
pub fn extract_title(text: &str) -> Option<String> {
    // Try <title> first
    if let Some(cap) = TITLE_TAG.captures(text) {
        let title = cap[1].trim().to_string();
        if !title.is_empty() {
            return Some(title);
        }
    }
    // Fall back to first <h1>
    if let Some(cap) = H1_TAG.captures(text) {
        let title = cap[1].trim().to_string();
        if !title.is_empty() {
            return Some(title);
        }
    }
    None
}

/// Extract a named section from `<section data-cs="{section_name}">`.
///
/// Finds the opening tag, extracts inner text up to `</section>`, strips
/// HTML tags, and applies `max_chars` truncation (same char-boundary logic
/// as the markdown extractor).
pub fn extract_section(text: &str, section_name: &str, max_chars: Option<usize>) -> Option<String> {
    // Find the <section data-cs="..."> tag matching section_name
    for cap in SECTION_OPEN.captures_iter(text) {
        if cap[1].eq_ignore_ascii_case(section_name) {
            // Find the start of content (after the closing `>` of the opening tag)
            let tag_end = cap.get(0).unwrap().end();
            let rest = &text[tag_end..];
            // Find closing </section>
            let close_pos = rest
                .to_ascii_lowercase()
                .find("</section>")
                .unwrap_or(rest.len());
            let inner = &rest[..close_pos];
            // Strip HTML tags and trim
            let stripped = HTML_TAG.replace_all(inner, "");
            let body = stripped.trim().to_string();
            if body.is_empty() {
                return None;
            }
            return match max_chars {
                Some(n) if body.chars().count() > n => Some(truncate_to_char_boundary(&body, n)),
                _ => Some(body),
            };
        }
    }
    None
}

/// Rewrite or insert a `<meta name="cs:{field}">` tag.
///
/// The `field_name` is the YAML-style name (e.g. `"status"`, `"type"`);
/// uses `FIELD_NAME_MAP` to get the `cs:` name (e.g. `"cs:status"`).
/// If the tag exists, replaces its `content` attribute value.
/// If absent, inserts before `</head>`, `</html>`, or appends.
/// Preserves surrounding content byte-for-byte.
/// SLW-MD-1 (FIX 2): HTML-attribute-escape a value written into `content="…"`.
/// A raw `"` closes the attribute; `<`/`>` can break out of the tag; `&`
/// starts an entity. We always emit `content="…"` (double-quoted), but the
/// extractor's `[^"']*` capture stops at the first quote of **either** kind, so a
/// raw `'` inside the value truncates it on read-back even though it doesn't close
/// the double-quoted attribute. We therefore escape `'` as well (`&#39;`) — without
/// it, any `cs:*` value containing an apostrophe (a validator `sh -c '…'`, a
/// `couldn't …` reason) reads back truncated → corrupted/invalid content. `&` is
/// escaped first so we never double-escape our own entities.
pub fn html_attr_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn rewrite_field(content: &str, field_name: &str, raw_value: &str) -> String {
    let Some(cs_name) = super::yaml_to_cs_name(field_name) else {
        return content.to_string();
    };
    // SLW-MD-1 (FIX 2): escape the value for the `content="…"` attribute. The
    // CSV-join list path also writes through here, so list items get the same
    // escaping.
    let escaped_value = html_attr_escape(raw_value);
    let new_value = escaped_value.as_str();

    // Try to find and replace existing <meta> tag (name-first order).
    // Capture the opening quote character (group 2) to preserve it on output.
    let pattern_nf = format!(
        r#"(?i)(<meta\s+name=["']{}["']\s+content=)(["'])[^"']*(["'])"#,
        regex::escape(cs_name)
    );
    let re_nf = Regex::new(&pattern_nf).unwrap();
    if re_nf.is_match(content) {
        return re_nf
            .replace(content, |caps: &regex::Captures| {
                let prefix = &caps[1];
                let open_q = &caps[2];
                let close_q = &caps[3];
                format!("{}{}{}{}", prefix, open_q, new_value, close_q)
            })
            .to_string();
    }

    // Try content-first order — same quote-preserving pattern.
    let pattern_cf = format!(
        r#"(?i)(<meta\s+content=)(["'])[^"']*(["'])(\s+name=["']{}["'])"#,
        regex::escape(cs_name)
    );
    let re_cf = Regex::new(&pattern_cf).unwrap();
    if re_cf.is_match(content) {
        return re_cf
            .replace(content, |caps: &regex::Captures| {
                let prefix = &caps[1];
                let open_q = &caps[2];
                let close_q = &caps[3];
                let suffix = &caps[4];
                format!("{}{}{}{}{}", prefix, open_q, new_value, close_q, suffix)
            })
            .to_string();
    }

    // Tag not found — insert new tag
    let new_tag = format!("<meta name=\"{}\" content=\"{}\">", cs_name, new_value);

    // Insert before </head>
    if let Some(pos) = content.to_ascii_lowercase().find("</head>") {
        let mut result = String::with_capacity(content.len() + new_tag.len() + 2);
        result.push_str(&content[..pos]);
        result.push_str(&new_tag);
        result.push('\n');
        result.push_str(&content[pos..]);
        return result;
    }
    // Insert before </html>
    if let Some(pos) = content.to_ascii_lowercase().find("</html>") {
        let mut result = String::with_capacity(content.len() + new_tag.len() + 2);
        result.push_str(&content[..pos]);
        result.push_str(&new_tag);
        result.push('\n');
        result.push_str(&content[pos..]);
        return result;
    }
    // Append
    format!("{}\n{}", content, new_tag)
}

// ----- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ----- extract_meta_fields -------------------------------------------

    #[test]
    fn meta_all_six_fields() {
        let html = r#"
<html>
<head>
  <meta name="cs:status" content="in-progress">
  <meta name="cs:type" content="story">
  <meta name="cs:depends-on" content="W-5-foo, W-6-bar">
  <meta name="cs:supporting-docs" content="./bug-report.md, ./spike.html">
  <meta name="cs:story-id" content="R-1">
  <meta name="cs:source-repo" content="alderco/codesense">
</head>
<body><h1>Title</h1></body>
</html>"#;
        let fm = extract_meta_fields(html);
        assert_eq!(fm.status.as_deref(), Some("in-progress"));
        assert_eq!(fm.document_type.as_deref(), Some("story"));
        assert_eq!(
            fm.depends_on.as_deref(),
            Some(&["W-5-foo".to_string(), "W-6-bar".to_string()][..])
        );
        assert_eq!(
            fm.supporting_docs.as_deref(),
            Some(&["./bug-report.md".to_string(), "./spike.html".to_string()][..])
        );
        assert_eq!(fm.story_id.as_deref(), Some("R-1"));
        assert_eq!(fm.source_repo.as_deref(), Some("alderco/codesense"));
    }

    #[test]
    fn meta_cs_tags_parses_as_csv_list() {
        // SW-AU-77F: `cs:tags` → typed `tags` list, CSV-split like the other lists.
        let html = r#"<html><head>
  <meta name="cs:tags" content="infra, urgent">
</head><body></body></html>"#;
        let fm = extract_meta_fields(html);
        assert_eq!(
            fm.tags.as_deref(),
            Some(&["infra".to_string(), "urgent".to_string()][..])
        );
    }

    #[test]
    fn meta_no_tags_returns_default() {
        let fm = extract_meta_fields("<html><body>No meta</body></html>");
        assert!(fm.status.is_none());
        assert!(fm.depends_on.is_none());
        assert!(fm.tags.is_none());
        assert!(fm.document_type.is_none());
    }

    #[test]
    fn meta_partial_fill() {
        let html = r#"<meta name="cs:status" content="done">"#;
        let fm = extract_meta_fields(html);
        assert_eq!(fm.status.as_deref(), Some("done"));
        assert!(fm.depends_on.is_none());
        assert!(fm.document_type.is_none());
        assert!(fm.story_id.is_none());
    }

    #[test]
    fn meta_self_closing_tag() {
        let html = r#"<meta name="cs:status" content="review" />"#;
        let fm = extract_meta_fields(html);
        assert_eq!(fm.status.as_deref(), Some("review"));
    }

    #[test]
    fn meta_reversed_attribute_order() {
        let html = r#"<meta content="blocked" name="cs:status">"#;
        let fm = extract_meta_fields(html);
        assert_eq!(fm.status.as_deref(), Some("blocked"));
    }

    #[test]
    fn meta_case_insensitive_tags() {
        let html = r#"<META NAME="cs:status" CONTENT="qa">"#;
        let fm = extract_meta_fields(html);
        assert_eq!(fm.status.as_deref(), Some("qa"));
    }

    #[test]
    fn meta_extra_whitespace() {
        let html = r#"<meta   name="cs:status"   content="todo"  >"#;
        let fm = extract_meta_fields(html);
        assert_eq!(fm.status.as_deref(), Some("todo"));
    }

    #[test]
    fn meta_single_quotes() {
        let html = r#"<meta name='cs:status' content='planning'>"#;
        let fm = extract_meta_fields(html);
        assert_eq!(fm.status.as_deref(), Some("planning"));
    }

    #[test]
    fn attr_escape_unescape_symmetry_includes_apostrophe() {
        // SLW-MD-1: the escape↔unescape pair must round-trip every char it touches —
        // including `'` and a literal `&#39;` (which must NOT be mistaken for an apostrophe).
        for v in [
            "'",
            "a'b",
            r#"{"k":"v's"}"#,
            "&#39; literal",
            "mix \"' <>& end",
        ] {
            assert_eq!(
                html_attr_unescape(&html_attr_escape(v)),
                v,
                "round-trip failed for {v:?}"
            );
        }
    }

    #[test]
    fn meta_value_with_apostrophe_round_trips() {
        // SLW-MD-1 regression: a `cs:*` value containing an apostrophe must survive the
        // `content="…"` write→read cycle. Without escaping `'`, the extractor's `[^"']*`
        // capture truncated it at the apostrophe → corrupted read-back. Exercised end-to-end
        // through a durable field (`cs:dev-branch`).
        let value = "feat/couldn't-happen";
        let written = rewrite_field("<head></head>", "dev_branch", value);
        // The apostrophe is entity-escaped in the attribute, not left raw.
        assert!(
            written.contains("&#39;"),
            "apostrophe not escaped: {written}"
        );
        assert!(
            !written.contains("couldn't"),
            "raw apostrophe leaked: {written}"
        );
        // And it reads back byte-for-byte.
        let got = template_value_pairs(&written)
            .into_iter()
            .find(|(k, _)| k == "dev_branch")
            .map(|(_, v)| v);
        assert_eq!(got.as_deref(), Some(value));
    }

    // SW-SB-1 (AC-7): an HTML story participates identically — the three board
    // fields map from `cs:sprint`/`cs:points`/`cs:assigned-to` (dash convention,
    // matching cs:dev-branch) onto `FrontmatterFields`. `points` parses to f64.
    #[test]
    fn meta_board_metadata_round_trips() {
        let html = concat!(
            r#"<meta name="cs:sprint" content="2026-08-A">"#,
            r#"<meta name="cs:points" content="5">"#,
            r#"<meta name="cs:assigned-to" content="octocat-dev">"#,
        );
        let fm = extract_meta_fields(html);
        assert_eq!(fm.sprint.as_deref(), Some("2026-08-A"));
        assert_eq!(fm.points, Some(5.0));
        assert_eq!(fm.assigned_to.as_deref(), Some("octocat-dev"));
    }

    // SW-SB-22: an HTML story's board rank round-trips through `cs:rank` — both the
    // read (extract) and write (rewrite_field, which inserts the meta when absent)
    // paths. Without the FIELD_NAME_MAP entry the write silently no-ops and the read
    // never surfaces (code-review r1).
    #[test]
    fn meta_rank_round_trips() {
        let fm = extract_meta_fields(r#"<meta name="cs:rank" content="h0">"#);
        assert_eq!(fm.rank.as_deref(), Some("h0"));

        let html =
            "<head>\n<meta name=\"cs:status\" content=\"done\">\n</head>\n<body><h1>T</h1></body>";
        let out = rewrite_field(html, "rank", "h5");
        assert!(
            out.contains("cs:rank"),
            "should write a cs:rank meta, got: {out}"
        );
        assert_eq!(extract_meta_fields(&out).rank.as_deref(), Some("h5"));
    }

    // SW-SB-1 (AC-7): a non-numeric OR non-finite `cs:points` degrades to None
    // (tolerant), mirroring the markdown `de_lenient_points` posture.
    #[test]
    fn meta_points_non_numeric_is_none() {
        for bad in ["high", "nan", "inf", "-inf", "infinity"] {
            let html = format!(r#"<meta name="cs:points" content="{bad}">"#);
            assert!(
                extract_meta_fields(&html).points.is_none(),
                "cs:points={bad} must degrade to None"
            );
        }
    }

    // ----- extract_title -------------------------------------------------

    #[test]
    fn title_from_title_tag() {
        assert_eq!(
            extract_title("<html><head><title>My Doc</title></head></html>"),
            Some("My Doc".to_string())
        );
    }

    #[test]
    fn title_from_h1_fallback() {
        assert_eq!(
            extract_title("<html><body><h1>Heading One</h1></body></html>"),
            Some("Heading One".to_string())
        );
    }

    #[test]
    fn title_tag_wins_over_h1() {
        let html = "<title>From Title</title><body><h1>From H1</h1></body>";
        assert_eq!(extract_title(html), Some("From Title".to_string()));
    }

    #[test]
    fn title_none_when_absent() {
        assert_eq!(extract_title("<html><body>No heading</body></html>"), None);
    }

    #[test]
    fn title_empty_tags_returns_none() {
        assert_eq!(extract_title("<title></title><h1></h1>"), None);
    }

    // ----- extract_section -----------------------------------------------

    #[test]
    fn section_by_data_cs() {
        let html = r#"<section data-cs="story"><p>As a user, I want X.</p></section>"#;
        assert_eq!(
            extract_section(html, "story", None),
            Some("As a user, I want X.".to_string())
        );
    }

    #[test]
    fn section_strips_html_tags() {
        let html = r#"<section data-cs="story"><p>Line <strong>one</strong>.</p><p>Line two.</p></section>"#;
        let result = extract_section(html, "story", None).unwrap();
        assert!(!result.contains('<'));
        assert!(result.contains("Line one."));
    }

    #[test]
    fn section_max_chars_truncation() {
        let html = r#"<section data-cs="purpose">A very long purpose text that exceeds the cap.</section>"#;
        let result = extract_section(html, "purpose", Some(10)).unwrap();
        assert_eq!(result.chars().count(), 10);
    }

    #[test]
    fn section_not_found() {
        let html = r#"<section data-cs="other">Content</section>"#;
        assert_eq!(extract_section(html, "story", None), None);
    }

    #[test]
    fn section_empty_returns_none() {
        let html = r#"<section data-cs="story">   </section>"#;
        assert_eq!(extract_section(html, "story", None), None);
    }

    #[test]
    fn section_case_insensitive() {
        let html = r#"<SECTION data-cs="Story">Text</SECTION>"#;
        assert_eq!(
            extract_section(html, "story", None),
            Some("Text".to_string())
        );
    }

    // ----- rewrite_field -------------------------------------------------

    #[test]
    fn rewrite_updates_existing_tag() {
        let html = r#"<head><meta name="cs:status" content="todo"></head>"#;
        let result = rewrite_field(html, "status", "done");
        assert!(result.contains(r#"content="done""#));
        assert!(!result.contains(r#"content="todo""#));
    }

    #[test]
    fn rewrite_inserts_when_absent_before_head_close() {
        let html = "<head>\n<title>T</title>\n</head>";
        let result = rewrite_field(html, "status", "in-progress");
        assert!(result.contains(r#"<meta name="cs:status" content="in-progress">"#));
        assert!(result.contains("</head>"));
        // New tag should appear before </head>
        let meta_pos = result.find("cs:status").unwrap();
        let head_pos = result.find("</head>").unwrap();
        assert!(meta_pos < head_pos);
    }

    #[test]
    fn rewrite_inserts_before_html_close_when_no_head() {
        let html = "<html><body>Content</body></html>";
        let result = rewrite_field(html, "type", "story");
        assert!(result.contains(r#"<meta name="cs:type" content="story">"#));
        let meta_pos = result.find("cs:type").unwrap();
        let html_pos = result.find("</html>").unwrap();
        assert!(meta_pos < html_pos);
    }

    #[test]
    fn rewrite_appends_when_no_head_or_html() {
        let html = "<h1>Hello</h1>";
        let result = rewrite_field(html, "status", "qa");
        assert!(result.contains(r#"<meta name="cs:status" content="qa">"#));
        assert!(result.starts_with("<h1>Hello</h1>"));
    }

    #[test]
    fn rewrite_field_name_mapping() {
        let html = r#"<head><meta name="cs:type" content="bug"></head>"#;
        let result = rewrite_field(html, "type", "spike");
        assert!(result.contains(r#"content="spike""#));
        assert!(!result.contains(r#"content="bug""#));
    }

    #[test]
    fn rewrite_preserves_surrounding_content() {
        let html = "<head>\n<title>My Doc</title>\n<meta name=\"cs:status\" content=\"todo\">\n<link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>Body</body>";
        let result = rewrite_field(html, "status", "done");
        assert!(result.contains("<title>My Doc</title>"));
        assert!(result.contains("<link rel=\"stylesheet\" href=\"style.css\">"));
        assert!(result.contains("<body>Body</body>"));
        assert!(result.contains(r#"content="done""#));
    }

    #[test]
    fn rewrite_handles_content_first_order() {
        let html = r#"<meta content="blocked" name="cs:status">"#;
        let result = rewrite_field(html, "status", "review");
        assert!(result.contains(r#"content="review""#));
        assert!(!result.contains(r#"content="blocked""#));
        // Verify name attribute is preserved
        assert!(result.contains(r#"name="cs:status""#));
    }

    #[test]
    fn rewrite_unknown_field_returns_unchanged() {
        let html = r#"<head><meta name="cs:status" content="todo"></head>"#;
        let result = rewrite_field(html, "nonexistent_field", "value");
        assert_eq!(result, html);
    }
}
