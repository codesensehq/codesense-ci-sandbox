// SPDX-License-Identifier: MIT
//! Avatar packs: folders of faces an agent can wear in the agents list.
//!
//! A pack is a folder `avatars/<pack>/` holding image files and a `pack.yaml`
//! (`name`, `faces: [{id, file, label?}]`). Team packs live in the active team's
//! content (shared like any team content); personal packs live in the user's
//! scratch space for the open org. Three packs ship with the app (`builtin-chess`,
//! `builtin-funny`, `builtin-crest`). A face is named everywhere as `pack:id`.
//!
//! Display only: a face never changes what an agent does, and nothing here
//! checks who may add to a pack. Files arrive through the filesystem; this
//! module only reads, validates and indexes them, and writes a pack's manifest
//! and small variants when asked to add a face.

pub(crate) mod builtin;
pub(crate) mod hosted;
pub(crate) mod manifest;
pub(crate) mod pool;
pub(crate) mod raster;
pub(crate) mod render;
pub(crate) mod store;
pub(crate) mod svg;

#[cfg(test)]
mod tests;

/// The app event sent after a face is added, so open lists reload.
pub const AVATARS_CHANGED: &str = "avatars:changed";
/// The folder under a content root that holds packs.
pub const AVATARS_DIR: &str = "avatars";
/// A pack's manifest file.
pub const MANIFEST_FILE: &str = "pack.yaml";
/// The largest face file accepted.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// The largest side, in pixels, of a raster face.
pub const MAX_SIDE: u32 = 1024;
/// The side of the small variant written next to a raster face.
pub const SMALL_SIDE: u32 = 64;
/// How far from square a face may be (longer side / shorter side).
pub const MAX_ASPECT: f64 = 1.25;
/// The longest pack id; with a face id it must fit a 64-character launch label.
pub const PACK_ID_MAX: usize = 32;
/// The longest face id.
pub const FACE_ID_MAX: usize = 31;
/// The most faces one pack may list.
pub const MAX_FACES_PER_PACK: usize = 256;
/// Pack ids starting with this are the app's own.
pub const BUILTIN_PREFIX: &str = "builtin-";
/// The suffix of a raster face's small variant: `<stem>.small.png`.
pub const SMALL_SUFFIX: &str = ".small.png";
/// The team's avatar settings, beside its packs: `avatars/settings.yaml`.
pub const SETTINGS_FILE: &str = "settings.yaml";

/// Where a pack comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackScope {
    /// The active team's content: every teammate sees it after a sync.
    Team,
    /// The user's scratch space: only its owner sees it.
    Personal,
    /// Shipped with the app.
    Builtin,
}

/// The image formats a face may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaceFormat {
    Png,
    Webp,
    Svg,
}

impl FaceFormat {
    /// The format a file name's extension names, if it is one a face may be.
    pub fn from_file_name(name: &str) -> Option<Self> {
        let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some(Self::Png),
            "webp" => Some(Self::Webp),
            "svg" => Some(Self::Svg),
            _ => None,
        }
    }

    /// The format the bytes themselves are, by their signature.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }
        let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        let head = String::from_utf8_lossy(&text[..text.len().min(4096)]).to_ascii_lowercase();
        (head.trim_start().starts_with('<') && head.contains("<svg")).then_some(Self::Svg)
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Svg => "svg",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Svg => "image/svg+xml",
        }
    }
}

/// Whether `id` is a valid pack or face id: lowercase letters, digits and
/// dashes, starting with a letter or digit, at most `max` characters.
pub fn is_valid_id(id: &str, max: usize) -> bool {
    !id.is_empty()
        && id.len() <= max
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

/// A face reference, `pack:id`.
pub fn face_ref(pack: &str, id: &str) -> String {
    format!("{pack}:{id}")
}

/// Splits a `pack:id` reference, if `value` is one.
pub fn parse_face_ref(value: &str) -> Option<(&str, &str)> {
    let (pack, id) = value.trim().split_once(':')?;
    (is_valid_id(pack, PACK_ID_MAX) && is_valid_id(id, FACE_ID_MAX)).then_some((pack, id))
}

/// Whether two sides are close enough to square.
pub fn is_square_ish(width: f64, height: f64) -> bool {
    if width <= 0.0 || height <= 0.0 {
        return false;
    }
    let (long, short) = if width >= height {
        (width, height)
    } else {
        (height, width)
    };
    long / short <= MAX_ASPECT
}
