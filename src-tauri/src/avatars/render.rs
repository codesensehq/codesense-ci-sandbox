// SPDX-License-Identifier: MIT
//! Drawing an SVG face once, on this Mac, as a small square image.
//!
//! A hosted page can never be handed an SVG (it can carry script and links), so a
//! pack's SVG face is cleaned, drawn here and sent as a PNG. The drawing is bounded
//! four ways: the source size, the number of elements before parsing, the number of
//! nodes after `<use>` is expanded, and a fixed [`FACE_SIDE`] pixel canvas. It can
//! read nothing outside the text it is given: no file, no `data:` image, no network,
//! and no fonts (the build has no text support, so a `<text>` draws nothing).

use quick_xml::events::Event;
use quick_xml::Reader;
use resvg::{tiny_skia, usvg};

use super::raster::Rgba;
use super::svg;

/// The side, in pixels, of the PNG a face is drawn to.
pub const FACE_SIDE: u32 = 96;
/// The largest SVG source drawn.
pub const MAX_SVG_BYTES: usize = 256 * 1024;
/// Most elements in the source (the shipped faces have fewer than 20).
pub const MAX_ELEMENTS: usize = 2_000;
/// Most `<use>` elements in the source: each can multiply what it points at.
pub const MAX_USES: usize = 16;
/// Deepest element nesting (the shipped faces nest at most 4 deep). The parser and the
/// drawing recurse, so this keeps a hostile document from exhausting the stack.
pub const MAX_DEPTH: usize = 24;
/// Most nodes in the parsed tree, after `<use>` is expanded.
pub const MAX_TREE_NODES: usize = 10_000;

/// Draw the SVG `source` as a [`FACE_SIDE`] square, centred and never stretched.
/// Fails when it is not a readable SVG, is over a bound, or draws nothing.
pub fn svg_to_rgba(source: &[u8]) -> Result<Rgba, String> {
    if source.len() > MAX_SVG_BYTES {
        return Err(format!(
            "an SVG face is at most {} KB",
            MAX_SVG_BYTES / 1024
        ));
    }
    let text = std::str::from_utf8(source).map_err(|_| "not UTF-8 text".to_string())?;
    let cleaned = svg::sanitize(text)?.svg;
    count_elements(&cleaned)?;
    let tree = usvg::Tree::from_str(&cleaned, &options()).map_err(|e| e.to_string())?;
    let (width, height) = (tree.size().width(), tree.size().height());
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err("the SVG has no usable size".to_string());
    }
    if nodes(tree.root()) > MAX_TREE_NODES {
        return Err("the SVG has too many shapes".to_string());
    }
    // These draw into layers sized by the drawing, not by the canvas (a `<pattern>` tile is
    // allocated at its own size, however large), and a face needs none of them.
    if !tree.patterns().is_empty() || !tree.masks().is_empty() || !tree.filters().is_empty() {
        return Err("a face cannot use patterns, masks or filters".to_string());
    }
    let side = FACE_SIDE as f32;
    let scale = side / width.max(height);
    let transform = tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (side - width * scale) / 2.0,
        (side - height * scale) / 2.0,
    );
    let mut pixmap = tiny_skia::Pixmap::new(FACE_SIDE, FACE_SIDE)
        .ok_or_else(|| "cannot allocate the canvas".to_string())?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut pixels = Vec::with_capacity((FACE_SIDE * FACE_SIDE * 4) as usize);
    for p in pixmap.pixels() {
        let c = p.demultiply();
        pixels.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    if pixels.chunks_exact(4).all(|p| p[3] == 0) {
        return Err("the SVG draws nothing".to_string());
    }
    Ok(Rgba {
        width: FACE_SIDE,
        height: FACE_SIDE,
        pixels,
    })
}

/// Parse options that reach nothing: no resource folder, and both `<image>` href
/// resolvers refuse, so `data:` images and every other href draw nothing.
fn options() -> usvg::Options<'static> {
    usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    }
}

/// Refuse a document with more elements, more `<use>`, deeper nesting, or a marker than a face
/// needs. A `<marker>` is drawn at every vertex of every path that names it (by element, by
/// `marker-*` attribute or by CSS), and the parser expands that before any node budget can
/// run, so a small document could be made to build millions of shapes.
fn count_elements(cleaned: &str) -> Result<(), String> {
    let mut reader = Reader::from_str(cleaned);
    let (mut elements, mut uses, mut depth) = (0usize, 0usize, 0usize);
    let marker = || Err("a face cannot use markers".to_string());
    loop {
        let (opens, element) = match reader
            .read_event()
            .map_err(|e| format!("not a readable SVG: {e}"))?
        {
            Event::Start(e) => (true, e),
            Event::Empty(e) => (false, e),
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                continue;
            }
            Event::Text(text) => {
                if text.to_ascii_lowercase().contains("marker") {
                    return marker();
                }
                continue;
            }
            Event::CData(data) => {
                if data.to_ascii_lowercase().contains("marker") {
                    return marker();
                }
                continue;
            }
            Event::Eof => return Ok(()),
            _ => continue,
        };
        if opens {
            depth += 1;
        }
        elements += 1;
        let name = element.local_name();
        if name.as_ref().eq_ignore_ascii_case("use") {
            uses += 1;
        }
        if name.as_ref().eq_ignore_ascii_case("marker") {
            return marker();
        }
        for attribute in element.attributes().flatten() {
            let key = attribute.key.as_ref().to_ascii_lowercase();
            let value = attribute.value.to_ascii_lowercase();
            if key.starts_with("marker") || (key == "style" && value.contains("marker")) {
                return marker();
            }
        }
        if depth > MAX_DEPTH {
            return Err("the SVG is nested too deeply".to_string());
        }
        if elements > MAX_ELEMENTS || uses > MAX_USES {
            return Err("the SVG has too many elements".to_string());
        }
    }
}

/// How many nodes `group` holds, counting groups and what they hold.
fn nodes(group: &usvg::Group) -> usize {
    group
        .children()
        .iter()
        .map(|node| match node {
            usvg::Node::Group(g) => 1 + nodes(g),
            _ => 1,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: &str = r##"<rect x="0" y="0" width="10" height="10" fill="#00ff00"/>"##;

    fn svg(inner: &str) -> Vec<u8> {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 10 10">{inner}</svg>"#).into_bytes()
    }

    fn pixel(image: &Rgba, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * image.width + x) * 4) as usize;
        image.pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn a_face_is_drawn_on_a_fixed_square_canvas_centred_and_unstretched() {
        let square = svg_to_rgba(&svg(RECT)).unwrap();
        assert_eq!((square.width, square.height), (FACE_SIDE, FACE_SIDE));
        assert_eq!(pixel(&square, 48, 48), [0, 255, 0, 255]);
        // A 2:1 drawing keeps its shape: bars above and below stay clear.
        let wide = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10"><rect width="20" height="10" fill="#00ff00"/></svg>"##;
        let wide = svg_to_rgba(wide).unwrap();
        assert_eq!((wide.width, wide.height), (FACE_SIDE, FACE_SIDE));
        assert_eq!(pixel(&wide, 48, 48), [0, 255, 0, 255]);
        assert_eq!(pixel(&wide, 48, 2)[3], 0);
        assert_eq!(pixel(&wide, 48, 93)[3], 0);
    }

    #[test]
    fn an_image_href_reads_no_file_and_no_data_url() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret.png");
        let red = Rgba {
            width: 8,
            height: 8,
            pixels: [255, 0, 0, 255].repeat(64),
        };
        std::fs::write(&secret, super::super::raster::encode_png(&red).unwrap()).unwrap();
        let data = {
            use base64::Engine as _;
            let png = super::super::raster::encode_png(&red).unwrap();
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )
        };
        let plain = svg_to_rgba(&svg(RECT)).unwrap();
        for href in [
            secret.to_string_lossy().into_owned(),
            format!("file://{}", secret.display()),
            "secret.png".to_string(),
            data,
        ] {
            // The red picture is drawn over the whole face if the href is followed.
            let with_image = svg(&format!(
                r#"{RECT}<image href="{href}" xlink:href="{href}" x="0" y="0" width="10" height="10"/>"#
            ));
            assert_eq!(
                svg_to_rgba(&with_image).unwrap().pixels,
                plain.pixels,
                "{href}"
            );
        }
    }

    #[test]
    fn text_draws_nothing_and_a_face_of_only_text_is_no_face() {
        let plain = svg_to_rgba(&svg(RECT)).unwrap();
        let with_text = svg(&format!(
            r#"{RECT}<text x="0" y="8" font-size="12" fill="red">Hi</text>"#
        ));
        assert_eq!(svg_to_rgba(&with_text).unwrap().pixels, plain.pixels);
        let only_text = svg(r#"<text x="0" y="8" font-size="12">Hi</text>"#);
        assert!(svg_to_rgba(&only_text)
            .err()
            .expect("refused")
            .contains("draws nothing"));
        let only_image = svg(r#"<image href="/etc/hosts" width="10" height="10"/>"#);
        assert!(svg_to_rgba(&only_image).is_err());
    }

    #[test]
    fn script_and_links_never_run() {
        let dirty = svg(&format!(
            r##"<script>throw new Error("x")</script><a href="https://example.test"><use href="https://example.test/x.svg#a"/></a>{RECT}"##
        ));
        let plain = svg_to_rgba(&svg(RECT)).unwrap();
        assert_eq!(svg_to_rgba(&dirty).unwrap().pixels, plain.pixels);
    }

    #[test]
    fn oversized_and_bomb_sources_are_refused_before_they_are_drawn() {
        // Over the source cap.
        let big = svg(&format!("{RECT}<!-- {} -->", "x".repeat(MAX_SVG_BYTES)));
        assert!(svg_to_rgba(&big)
            .err()
            .expect("refused")
            .contains("at most"));
        // A node bomb: more elements than a face needs.
        let nodes = svg(&format!("{RECT}{}", "<g/>".repeat(MAX_ELEMENTS + 1)));
        assert!(svg_to_rgba(&nodes)
            .err()
            .expect("refused")
            .contains("too many elements"));
        // An expansion bomb: each level draws the one below twice.
        let mut chain =
            String::from(r##"<defs><g id="a0"><rect width="10" height="10" fill="#0f0"/></g>"##);
        for level in 1..=20 {
            chain.push_str(&format!(
                r##"<g id="a{level}"><use href="#a{}"/><use href="#a{}"/></g>"##,
                level - 1,
                level - 1
            ));
        }
        chain.push_str(r##"</defs><use href="#a20"/>"##);
        assert!(svg_to_rgba(&svg(&chain))
            .err()
            .expect("refused")
            .contains("too many elements"));
        // Deep nesting is refused before the parser or the drawing recurse into it.
        let deep = |n: usize| svg(&format!("{}{RECT}{}", "<g>".repeat(n), "</g>".repeat(n)));
        assert!(svg_to_rgba(&deep(MAX_DEPTH - 1)).is_ok());
        assert!(svg_to_rgba(&deep(1_900))
            .err()
            .expect("refused")
            .contains("nested too deeply"));
        // Not UTF-8, not an SVG, and not even XML.
        assert!(svg_to_rgba(&[0xff, 0xfe, 0x00]).is_err());
        assert!(svg_to_rgba(b"<html></html>").is_err());
        assert!(svg_to_rgba(b"").is_err());
    }

    #[test]
    fn a_huge_viewbox_or_size_is_scaled_onto_the_fixed_canvas_or_refused() {
        for dims in [
            r#"viewBox="0 0 1e12 1e12""#,
            r#"width="1e30" height="1e30""#,
            r#"width="0.0000001" height="0.0000001" viewBox="0 0 0.0000001 0.0000001""#,
            r#"viewBox="0 0 -5 10""#,
        ] {
            let source = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" {dims}><rect width="1e12" height="1e12" fill="#0f0"/></svg>"##
            );
            // Whatever the answer, it is quick, and any image is the fixed canvas.
            if let Ok(image) = svg_to_rgba(source.as_bytes()) {
                assert_eq!(
                    (image.width, image.height),
                    (FACE_SIDE, FACE_SIDE),
                    "{dims}"
                );
            }
        }
    }

    #[test]
    fn patterns_masks_and_filters_are_refused_before_anything_is_allocated_for_them() {
        let huge_pattern = svg(
            r##"<defs><pattern id="p" patternUnits="userSpaceOnUse" width="1000000" height="1000000"><rect width="1" height="1" fill="#f00"/></pattern></defs><rect width="10" height="10" fill="url(#p)"/>"##,
        );
        let mask = svg(
            r##"<defs><mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="1000000" height="1000000"><rect width="5" height="5" fill="#fff"/></mask></defs><rect width="10" height="10" mask="url(#m)" fill="#0f0"/>"##,
        );
        let filter = svg(
            r##"<defs><filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="1000000" height="1000000"><feGaussianBlur stdDeviation="3"/></filter></defs><rect width="10" height="10" filter="url(#f)" fill="#0f0"/>"##,
        );
        let css_filter =
            svg(r##"<rect width="10" height="10" style="filter: blur(1000000px)" fill="#0f0"/>"##);
        for (name, source) in [
            ("pattern", huge_pattern),
            ("mask", mask),
            ("filter", filter),
            ("css filter", css_filter),
        ] {
            let started = std::time::Instant::now();
            let refusal = svg_to_rgba(&source).err().expect("refused");
            assert!(
                refusal.contains("patterns, masks or filters"),
                "{name}: {refusal}"
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "{name} was slow"
            );
        }
    }

    #[test]
    fn markers_are_refused_before_the_parser_can_expand_them() {
        let path = r##"<path d="M0 0 L1 1 L2 2 L3 3 L4 4 L5 5" stroke="#000" fill="none""##;
        let marker_def = r##"<defs><marker id="m" markerWidth="2" markerHeight="2"><circle r="1"/></marker></defs>"##;
        for (name, source) in [
            ("element", svg(&format!("{marker_def}{RECT}"))),
            (
                "attribute",
                svg(&format!("{RECT}{path} marker-mid=\"url(#m)\"/>")),
            ),
            (
                "marker shorthand",
                svg(&format!("{RECT}{path} marker=\"url(#m)\"/>")),
            ),
            (
                "style attribute",
                svg(&format!("{RECT}{path} style=\"marker-end:url(#m)\"/>")),
            ),
            (
                "style element",
                svg(&format!(
                    "<style>path {{ marker-start: url(#m) }}</style>{RECT}"
                )),
            ),
            (
                "upper case",
                svg(&format!("{RECT}{path} MARKER-MID=\"url(#m)\"/>")),
            ),
            (
                "style element CDATA",
                svg(&format!(
                    "<style><![CDATA[path {{ marker-start: url(#m) }}]]></style>{RECT}"
                )),
            ),
            (
                "CDATA outside style",
                svg(&format!("<g><![CDATA[marker]]></g>{RECT}")),
            ),
        ] {
            let started = std::time::Instant::now();
            let refusal = svg_to_rgba(&source).err().expect("refused");
            assert!(refusal.contains("markers"), "{name}: {refusal}");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "{name} was slow"
            );
        }
    }

    #[test]
    fn every_shipped_face_is_drawn() {
        let mut count = 0;
        for pack in crate::avatars::builtin::packs() {
            for face in pack.faces {
                let bytes = crate::http::routes::dashboard_kit::builtin_face(pack.id, &face.id)
                    .unwrap_or_else(|| panic!("{}:{} is not bundled", pack.id, face.id));
                let image =
                    svg_to_rgba(bytes).unwrap_or_else(|e| panic!("{}:{}: {e}", pack.id, face.id));
                assert!(image.pixels.chunks_exact(4).any(|p| p[3] > 0));
                count += 1;
            }
        }
        assert!(count >= 50, "only {count} faces drawn");
    }
}
