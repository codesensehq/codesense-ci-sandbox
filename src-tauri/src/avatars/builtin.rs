// SPDX-License-Identifier: MIT
//! The packs the app ships: their manifests are bundled with the webview's
//! assets (which draws them), and read here so `avatar_list` can name them.

use serde::Deserialize;

use super::manifest::FaceEntry;

/// One shipped pack.
#[derive(Debug, Clone)]
pub struct BuiltinPack {
    pub id: &'static str,
    pub name: String,
    pub faces: Vec<FaceEntry>,
}

#[derive(Deserialize)]
struct BundledManifest {
    name: String,
    faces: Vec<BundledFace>,
}

#[derive(Deserialize)]
struct BundledFace {
    id: String,
    file: String,
    label: Option<String>,
}

const BUNDLED: &[(&str, &str)] = &[
    (
        "builtin-chess",
        include_str!("../../../src/assets/avatar-packs/builtin-chess/pack.json"),
    ),
    (
        "builtin-funny",
        include_str!("../../../src/assets/avatar-packs/builtin-funny/pack.json"),
    ),
    (
        "builtin-crest",
        include_str!("../../../src/assets/avatar-packs/builtin-crest/pack.json"),
    ),
];

/// The shipped packs, in the order the Face menu lists them.
pub fn packs() -> Vec<BuiltinPack> {
    BUNDLED
        .iter()
        .map(|(id, json)| {
            let manifest: BundledManifest =
                serde_json::from_str(json).expect("a bundled avatar manifest parses");
            BuiltinPack {
                id,
                name: manifest.name,
                faces: manifest
                    .faces
                    .into_iter()
                    .map(|f| FaceEntry {
                        id: f.id,
                        file: f.file,
                        label: f.label,
                    })
                    .collect(),
            }
        })
        .collect()
}
