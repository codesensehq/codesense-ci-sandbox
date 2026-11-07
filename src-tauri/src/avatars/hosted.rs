// SPDX-License-Identifier: MIT
//! A face as a hosted page is given it: one small PNG, a glyph or a colour.
//!
//! A face is named by a ref (`pack:id`, `#rrggbb`, a one- or two-character glyph such as
//! an emoji, or, for the human's own, a `data:` image). A pack face is a PNG, a WebP or an
//! SVG on this Mac; a hosted page may be sent none of those as they are (an SVG can carry
//! script and links, a WebP can be animated), so an image face is drawn here to a PNG of at
//! most [`render::FACE_SIDE`] pixels. The PNG is named by its sha256, so the same drawing is
//! always the same face.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::{parse_face_ref, raster, render, FaceFormat};

/// A face ref, by what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FaceRef<'a> {
    /// `pack:id`.
    Pack(&'a str, &'a str),
    /// A colour, as `#rrggbb` in lowercase.
    Colour(String),
    /// A one- or two-character glyph, such as an emoji.
    Glyph(&'a str),
    /// A `data:` image URL (the human's own uploaded face).
    Data(&'a str),
}

/// What a ref names, or `None` when it names nothing a page can be given.
pub fn classify(value: &str) -> Option<FaceRef<'_>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some((pack, id)) = parse_face_ref(value) {
        return Some(FaceRef::Pack(pack, id));
    }
    if let Some(hex) = value.strip_prefix('#') {
        let digits = hex.to_ascii_lowercase();
        return match digits.len() {
            6 if digits.bytes().all(|b| b.is_ascii_hexdigit()) => {
                Some(FaceRef::Colour(format!("#{digits}")))
            }
            3 if digits.bytes().all(|b| b.is_ascii_hexdigit()) => Some(FaceRef::Colour(format!(
                "#{}",
                digits.chars().flat_map(|c| [c, c]).collect::<String>()
            ))),
            _ => None,
        };
    }
    if value.starts_with("data:") {
        return Some(FaceRef::Data(value));
    }
    let glyph =
        value.chars().count() <= 2 && value.chars().all(|c| !c.is_control() && !c.is_whitespace());
    glyph.then_some(FaceRef::Glyph(value))
}

/// An image face, drawn to a PNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceImage {
    pub png: Vec<u8>,
    /// Lowercase hex sha256 of `png`.
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

/// The most faces kept drawn at once; the memo is dropped whole when it fills.
const MEMO_FACES: usize = 256;
type Memo = Mutex<HashMap<String, Result<Arc<FaceImage>, String>>>;

/// Draw `bytes` (a PNG, WebP or SVG face) to a PNG of at most [`render::FACE_SIDE`] pixels.
/// A raster face is re-encoded, which drops every chunk but the pixels. The same source
/// is drawn once: the result, or the reason it failed, is kept.
pub fn normalize(format: FaceFormat, bytes: &[u8]) -> Result<Arc<FaceImage>, String> {
    static MEMO: OnceLock<Memo> = OnceLock::new();
    let key = {
        let mut hasher = Sha256::new();
        hasher.update(format.extension().as_bytes());
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    };
    let memo = MEMO.get_or_init(Default::default);
    if let Some(done) = memo.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return done.clone();
    }
    let drawn = draw(format, bytes).map(Arc::new);
    let mut held = memo.lock().unwrap_or_else(|e| e.into_inner());
    if held.len() >= MEMO_FACES {
        held.clear();
    }
    held.insert(key, drawn.clone());
    drawn
}

fn draw(format: FaceFormat, bytes: &[u8]) -> Result<FaceImage, String> {
    let image = match format {
        FaceFormat::Svg => render::svg_to_rgba(bytes)?,
        FaceFormat::Png | FaceFormat::Webp => {
            let decoded = raster::decode(format, bytes)?;
            raster::small_square(&decoded, render::FACE_SIDE)
        }
    };
    let png = raster::encode_png(&image)?;
    Ok(FaceImage {
        sha256: format!("{:x}", Sha256::digest(&png)),
        width: image.width,
        height: image.height,
        png,
    })
}

/// The format and bytes of a `data:` image URL, at most `max_bytes` once decoded.
pub fn from_data_url(url: &str, max_bytes: usize) -> Result<(FaceFormat, Vec<u8>), String> {
    let (header, payload) = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .ok_or_else(|| "not a data: URL".to_string())?;
    if !header.ends_with(";base64") {
        return Err("not a base64 data: URL".to_string());
    }
    if payload.len() > max_bytes.div_ceil(3) * 4 {
        return Err("the image is too large".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| "not valid base64".to_string())?;
    if bytes.len() > max_bytes {
        return Err("the image is too large".to_string());
    }
    let format = FaceFormat::sniff(&bytes).ok_or_else(|| "not a PNG, WebP or SVG".to_string())?;
    Ok((format, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ref_names_a_pack_face_a_colour_a_glyph_or_an_image() {
        assert_eq!(
            classify("builtin-funny:coffee-bot"),
            Some(FaceRef::Pack("builtin-funny", "coffee-bot"))
        );
        assert_eq!(classify(" crew:a "), Some(FaceRef::Pack("crew", "a")));
        assert_eq!(classify("#A1B2C3"), Some(FaceRef::Colour("#a1b2c3".into())));
        assert_eq!(classify("#fa0"), Some(FaceRef::Colour("#ffaa00".into())));
        assert_eq!(classify("🦊"), Some(FaceRef::Glyph("🦊")));
        assert_eq!(classify("AB"), Some(FaceRef::Glyph("AB")));
        assert_eq!(
            classify("data:image/png;base64,AA"),
            Some(FaceRef::Data("data:image/png;base64,AA"))
        );
        for none in [
            "", "   ", "#12", "#12345g", "#1234567", "abc", "a b", "<b>x", "Crew:A", "x\u{7}",
        ] {
            assert_eq!(classify(none), None, "{none:?}");
        }
    }

    fn solid(side: u32) -> Vec<u8> {
        let pixels = [10, 20, 30, 255].repeat((side * side) as usize);
        raster::encode_png(&raster::Rgba {
            width: side,
            height: side,
            pixels,
        })
        .unwrap()
    }

    #[test]
    fn a_raster_face_is_reencoded_small_and_the_same_source_is_the_same_hash() {
        let big = normalize(FaceFormat::Png, &solid(300)).unwrap();
        assert_eq!((big.width, big.height), (96, 96));
        assert_eq!(&big.png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(big.sha256, format!("{:x}", Sha256::digest(&big.png)));
        assert_eq!(normalize(FaceFormat::Png, &solid(300)).unwrap(), big);
        // Never scaled up.
        let small = normalize(FaceFormat::Png, &solid(40)).unwrap();
        assert_eq!((small.width, small.height), (40, 40));
        // A file that is not what its name says is refused, and refused again from the memo.
        assert!(normalize(FaceFormat::Png, b"not a png").is_err());
        assert!(normalize(FaceFormat::Png, b"not a png").is_err());
        assert!(normalize(FaceFormat::Webp, &solid(8)).is_err());
    }

    #[test]
    fn a_data_url_face_is_bounded_and_sniffed_by_its_bytes() {
        use base64::Engine as _;
        let url = |bytes: &[u8]| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        };
        let png = solid(8);
        assert_eq!(
            from_data_url(&url(&png), 1 << 20).unwrap().0,
            FaceFormat::Png
        );
        assert!(from_data_url(&url(&png), png.len() - 1).is_err());
        assert!(from_data_url(&url(b"<html></html>"), 1 << 20).is_err());
        assert!(from_data_url("data:image/png,AAAA", 1 << 20).is_err());
        assert!(from_data_url("data:image/png;base64,!!!", 1 << 20).is_err());
        assert!(from_data_url("https://example.test/a.png", 1 << 20).is_err());
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><rect width="4" height="4"/></svg>"#;
        assert_eq!(
            from_data_url(&url(svg), 1 << 20).unwrap().0,
            FaceFormat::Svg
        );
    }
}
