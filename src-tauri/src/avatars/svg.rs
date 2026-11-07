// SPDX-License-Identifier: MIT
//! SVG faces: keep the drawing, drop anything that runs, loads or reaches out.
//!
//! The webview shows faces through `<img>`, which already runs no script, but a
//! face file is team content anyone can edit and may be opened elsewhere, so it
//! is cleaned before it is shown and when it is added: no scripts, no event
//! handlers, no `foreignObject`, no animation that could rewrite a link, no DTD,
//! and no reference to anything outside the file (only `#fragment` links and
//! embedded raster `data:` images stay).

use std::collections::BTreeSet;

use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, Writer, XmlVersion};

/// Elements dropped with everything inside them.
const DROPPED_ELEMENTS: &[&str] = &[
    "script",
    "foreignobject",
    "iframe",
    "object",
    "embed",
    "audio",
    "video",
    "canvas",
    "handler",
    "listener",
    "animate",
    "animatemotion",
    "animatetransform",
    "animatecolor",
    "set",
    "discard",
    "a",
];

/// A cleaned SVG and what was taken out of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Sanitized {
    pub svg: String,
    /// One line per kind of thing removed, e.g. `<script> element`.
    pub removed: Vec<String>,
    /// Width and height from the root's `viewBox`, else its `width`/`height`.
    pub size: Option<(f64, f64)>,
}

impl Sanitized {
    pub fn changed(&self) -> bool {
        !self.removed.is_empty()
    }
}

/// Clean `source`. Fails when it is not an SVG document at all.
pub fn sanitize(source: &str) -> Result<Sanitized, String> {
    let mut reader = Reader::from_str(source);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut removed = BTreeSet::new();
    let mut size = None;
    // Depth inside a dropped element; 0 = writing.
    let mut skip_depth = 0usize;
    let mut saw_root = false;
    // A `<style>` element is buffered and kept only when its text is clean.
    let mut style: Option<(BytesStart<'static>, String)> = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("not a readable SVG: {e}"))?;
        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                Event::Eof => break,
                _ => {}
            }
            continue;
        }
        if let Some((_, text)) = style.as_mut() {
            match event {
                Event::Text(t) => {
                    text.push_str(&t);
                    continue;
                }
                Event::CData(c) => {
                    text.push_str(&c);
                    continue;
                }
                Event::GeneralRef(_) => continue,
                Event::End(_) => {
                    let (start, text) = style.take().expect("style is open");
                    if is_unsafe_css(&text) {
                        removed.insert("<style> with an external reference".to_string());
                    } else {
                        write(&mut writer, Event::Start(start))?;
                        write(
                            &mut writer,
                            Event::Text(quick_xml::events::BytesText::new(&text)),
                        )?;
                        write(
                            &mut writer,
                            Event::End(quick_xml::events::BytesEnd::new("style")),
                        )?;
                    }
                    continue;
                }
                Event::Eof => break,
                _ => {
                    // Markup inside <style> is not CSS: drop the whole element.
                    style = None;
                    removed.insert("<style> with markup inside".to_string());
                    skip_depth = 1;
                    continue;
                }
            }
        }
        match event {
            Event::Start(start) if !saw_root => {
                size = check_root(&start)?;
                saw_root = true;
                write(
                    &mut writer,
                    Event::Start(clean_attributes(&start, &mut removed)),
                )?;
            }
            Event::Empty(start) if !saw_root => {
                size = check_root(&start)?;
                saw_root = true;
                write(
                    &mut writer,
                    Event::Empty(clean_attributes(&start, &mut removed)),
                )?;
            }
            Event::Start(start) => {
                let name = local_name(&start);
                if DROPPED_ELEMENTS.contains(&name.as_str()) {
                    removed.insert(format!("<{name}> element"));
                    skip_depth = 1;
                } else if name == "style" {
                    style = Some((
                        clean_attributes(&start, &mut removed).into_owned(),
                        String::new(),
                    ));
                } else {
                    write(
                        &mut writer,
                        Event::Start(clean_attributes(&start, &mut removed)),
                    )?;
                }
            }
            Event::Empty(start) => {
                let name = local_name(&start);
                if DROPPED_ELEMENTS.contains(&name.as_str()) || name == "style" {
                    removed.insert(format!("<{name}> element"));
                } else {
                    write(
                        &mut writer,
                        Event::Empty(clean_attributes(&start, &mut removed)),
                    )?;
                }
            }
            Event::DocType(_) => {
                removed.insert("DOCTYPE".to_string());
            }
            Event::PI(_) => {
                removed.insert("processing instruction".to_string());
            }
            Event::Comment(_) => {}
            Event::Eof => break,
            other => write(&mut writer, other)?,
        }
    }
    if !saw_root {
        return Err("no <svg> element".to_string());
    }
    let svg = String::from_utf8(writer.into_inner()).map_err(|e| e.to_string())?;
    Ok(Sanitized {
        svg,
        removed: removed.into_iter().collect(),
        size,
    })
}

/// The root's size, or why the document is not an SVG.
fn check_root(start: &BytesStart<'_>) -> Result<Option<(f64, f64)>, String> {
    let name = local_name(start);
    if name != "svg" {
        return Err(format!("the root element is <{name}>, not <svg>"));
    }
    Ok(root_size(start))
}

fn write(writer: &mut Writer<Vec<u8>>, event: Event<'_>) -> Result<(), String> {
    writer.write_event(event).map_err(|e| e.to_string())
}

fn local_name(start: &BytesStart<'_>) -> String {
    start.local_name().as_ref().to_ascii_lowercase()
}

/// `start` with every unsafe attribute removed (recorded in `removed`).
fn clean_attributes<'a>(
    start: &'a BytesStart<'a>,
    removed: &mut BTreeSet<String>,
) -> BytesStart<'a> {
    let mut out = BytesStart::new(start.name().as_ref().to_owned());
    for attr in start.attributes().with_checks(false) {
        let Ok(attr) = attr else {
            removed.insert("a malformed attribute".to_string());
            continue;
        };
        let key = attr.key.as_ref().to_ascii_lowercase();
        let local = key.rsplit(':').next().unwrap_or(&key).to_string();
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| attr.value.to_string());
        if let Some(reason) = unsafe_attribute(&local, &value) {
            removed.insert(reason);
            continue;
        }
        out.push_attribute(Attribute {
            key: attr.key,
            value: attr.value.clone(),
        });
    }
    out
}

/// Why an attribute must go, or `None` to keep it.
fn unsafe_attribute(local: &str, value: &str) -> Option<String> {
    if local.starts_with("on") {
        return Some("event handler attributes".to_string());
    }
    let lowered: String = value
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    if local == "href" || local == "src" {
        let ok = lowered.starts_with('#') || is_embedded_raster(&lowered);
        return (!ok).then(|| "links to outside the file".to_string());
    }
    if lowered.contains("javascript:") || lowered.contains("vbscript:") {
        return Some("script URLs".to_string());
    }
    if local == "style" && is_unsafe_css(value) {
        return Some("style attributes with an external reference".to_string());
    }
    if has_external_url(&lowered) {
        return Some("references to outside the file".to_string());
    }
    None
}

fn is_embedded_raster(lowered: &str) -> bool {
    ["png", "jpeg", "jpg", "webp", "gif"]
        .iter()
        .any(|kind| lowered.starts_with(&format!("data:image/{kind};base64,")))
}

/// Whether a value has a `url(…)` pointing anywhere but a `#fragment`.
fn has_external_url(lowered: &str) -> bool {
    let mut rest = lowered;
    while let Some(at) = rest.find("url(") {
        let after = rest[at + 4..].trim_start_matches(['"', '\'']);
        if !after.starts_with('#') && !is_embedded_raster(after) {
            return true;
        }
        rest = &rest[at + 4..];
    }
    false
}

fn is_unsafe_css(css: &str) -> bool {
    let lowered: String = css
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    lowered.contains("@import")
        || lowered.contains("expression(")
        || lowered.contains("javascript:")
        || lowered.contains("-moz-binding")
        || has_external_url(&lowered)
}

/// The drawing's size from the root's `viewBox`, else its `width`/`height`.
fn root_size(start: &BytesStart<'_>) -> Option<(f64, f64)> {
    let mut view_box = None;
    let mut width = None;
    let mut height = None;
    for attr in start.attributes().with_checks(false).flatten() {
        let key = attr.key.as_ref().to_ascii_lowercase();
        let value = attr.value.to_string();
        match key.as_str() {
            "viewbox" => {
                let nums: Vec<f64> = value
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if nums.len() == 4 {
                    view_box = Some((nums[2], nums[3]));
                }
            }
            "width" => width = leading_number(&value),
            "height" => height = leading_number(&value),
            _ => {}
        }
    }
    view_box.or(width.zip(height))
}

fn leading_number(value: &str) -> Option<f64> {
    let digits: String = value
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}
