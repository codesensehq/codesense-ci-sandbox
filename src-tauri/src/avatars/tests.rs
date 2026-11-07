// SPDX-License-Identifier: MIT
use std::path::Path;

use super::manifest::Manifest;
use super::raster::{self, Rgba};
use super::store::{self, AddRequest, PackRoot};
use super::svg::sanitize;
use super::*;

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let pixels = (0..width * height)
        .flat_map(|i| [(i % 256) as u8, 80, 160, 255])
        .collect();
    raster::encode_png(&Rgba {
        width,
        height,
        pixels,
    })
    .unwrap()
}

const CLEAN_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" fill="#345"/><circle cx="32" cy="30" r="14" fill="url(#g)"/></svg>"##;

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn team_root(dir: &Path) -> PackRoot {
    PackRoot {
        scope: PackScope::Team,
        avatars_dir: dir.join("team/avatars"),
    }
}

fn personal_root(dir: &Path) -> PackRoot {
    PackRoot {
        scope: PackScope::Personal,
        avatars_dir: dir.join("me/avatars"),
    }
}

// ---- ids and refs ----------------------------------------------------------

#[test]
fn ids_are_short_lowercase_slugs() {
    assert!(is_valid_id("office-legends", PACK_ID_MAX));
    assert!(is_valid_id("cat2", FACE_ID_MAX));
    assert!(!is_valid_id("Cat", FACE_ID_MAX));
    assert!(!is_valid_id("-cat", FACE_ID_MAX));
    assert!(!is_valid_id("cat face", FACE_ID_MAX));
    assert!(!is_valid_id("../x", FACE_ID_MAX));
    assert!(!is_valid_id(&"a".repeat(PACK_ID_MAX + 1), PACK_ID_MAX));
    // The longest ref still fits a launch label.
    const { assert!(PACK_ID_MAX + 1 + FACE_ID_MAX <= 64) };
    assert_eq!(parse_face_ref("memes:doge"), Some(("memes", "doge")));
    assert_eq!(parse_face_ref("owl"), None);
    assert_eq!(parse_face_ref("memes:Doge"), None);
}

#[test]
fn formats_are_sniffed_from_their_bytes() {
    assert_eq!(FaceFormat::sniff(&png_bytes(2, 2)), Some(FaceFormat::Png));
    assert_eq!(
        FaceFormat::sniff(b"RIFF\0\0\0\0WEBPVP8 "),
        Some(FaceFormat::Webp)
    );
    assert_eq!(
        FaceFormat::sniff(CLEAN_SVG.as_bytes()),
        Some(FaceFormat::Svg)
    );
    assert_eq!(
        FaceFormat::sniff(b"<?xml version=\"1.0\"?>\n<svg/>"),
        Some(FaceFormat::Svg)
    );
    assert_eq!(FaceFormat::sniff(b"<html><body/></html>"), None);
    assert_eq!(FaceFormat::sniff(b"\xff\xd8\xff"), None);
}

// ---- manifest ---------------------------------------------------------------

#[test]
fn manifest_lists_good_faces_and_warns_about_the_rest() {
    let manifest = Manifest::parse(
        "name: Office Legends\nfaces:\n  - id: grumpy\n    file: grumpy.png\n    label: Grumpy\n  - just a string\n  - id: Bad Id\n    file: x.png\n  - id: nofile\n  - id: grumpy\n    file: other.png\n  - id: calm\n    file: calm.svg\n",
    )
    .unwrap();
    assert_eq!(manifest.name().as_deref(), Some("Office Legends"));
    let mut warnings = Vec::new();
    let entries = manifest.entries(&mut warnings);
    let ids: Vec<_> = entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["grumpy", "calm"]);
    assert_eq!(entries[0].label.as_deref(), Some("Grumpy"));
    assert_eq!(warnings.len(), 4, "{warnings:?}");
}

#[test]
fn manifest_refuses_what_it_cannot_rewrite_safely() {
    assert!(Manifest::parse("- a\n- b\n").is_err());
    assert!(Manifest::parse("faces: nope\n").is_err());
    assert!(Manifest::parse("faces: [\n").is_err());
    assert_eq!(Manifest::parse("").unwrap(), Manifest::default());
}

#[test]
fn manifest_upsert_keeps_unknown_keys_and_entries() {
    let mut manifest = Manifest::parse(
        "name: X\nauthor: someone\nfaces:\n  - weird\n  - id: a\n    file: a.png\n",
    )
    .unwrap();
    let replaced = manifest.upsert(&manifest::FaceEntry {
        id: "a".into(),
        file: "a2.png".into(),
        label: Some("A".into()),
    });
    assert!(replaced);
    assert!(!manifest.upsert(&manifest::FaceEntry {
        id: "b".into(),
        file: "b.svg".into(),
        label: None,
    }));
    let yaml = manifest.to_yaml().unwrap();
    assert!(yaml.contains("author: someone"));
    assert!(yaml.contains("- weird"));
    let reread = Manifest::parse(&yaml).unwrap();
    let mut warnings = Vec::new();
    let entries = reread.entries(&mut warnings);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].file, "a2.png");
    assert_eq!(entries[1].id, "b");
}

// ---- svg ---------------------------------------------------------------------

#[test]
fn svg_sanitize_keeps_a_clean_drawing() {
    let out = sanitize(CLEAN_SVG).unwrap();
    assert!(!out.changed(), "{:?}", out.removed);
    assert_eq!(out.size, Some((64.0, 64.0)));
    assert!(out.svg.contains("<circle"));
    assert!(out.svg.contains("url(#g)"));
}

#[test]
fn svg_sanitize_strips_scripts_handlers_and_outside_references() {
    let dirty = r##"<?xml version="1.0"?>
<!DOCTYPE svg [<!ENTITY x "boom">]>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 10 10" onload="alert(1)">
  <script>alert(1)</script>
  <foreignObject><div xmlns="http://www.w3.org/1999/xhtml">hi</div></foreignObject>
  <style>@import url(https://evil.example/x.css);</style>
  <style>.ok { fill: red }</style>
  <image href="https://evil.example/p.png" width="10" height="10"/>
  <image xlink:href="data:image/png;base64,AAAA" width="10" height="10"/>
  <use href="#ok"/>
  <a href="javascript:alert(1)"><rect width="1" height="1"/></a>
  <rect width="10" height="10" fill="url(https://evil.example/f)" style="fill: url(http://x)"/>
  <circle r="2" onclick="x()"><set attributeName="href" to="javascript:alert(1)"/></circle>
</svg>"##;
    let out = sanitize(dirty).unwrap();
    let svg = out.svg.to_lowercase();
    for gone in [
        "<script",
        "alert",
        "foreignobject",
        "evil.example",
        "onload",
        "onclick",
        "<set",
        "<a ",
        "javascript",
        "doctype",
        "@import",
    ] {
        assert!(!svg.contains(gone), "{gone} survived: {svg}");
    }
    assert!(svg.contains(".ok { fill: red }"));
    assert!(svg.contains("data:image/png;base64,aaaa"));
    assert!(svg.contains("href=\"#ok\""));
    assert!(out.changed());
    assert_eq!(out.size, Some((10.0, 10.0)));
}

#[test]
fn svg_sanitize_reads_cdata_styles_and_unescapes_attribute_values() {
    let source = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\">\
        <style><![CDATA[.cd { fill: blue }]]></style>\
        <style><![CDATA[@import url(https://evil.example/y.css);]]></style>\
        <g filter=\"java\tscript:alert(1)\"/>\
        <g filter=\"&#106;avascript:alert(2)\"/>\
        <g clip-path=\"url(&quot;#ok&quot;)\"/>\
        </svg>";
    let out = sanitize(source).unwrap();
    let svg = out.svg.to_lowercase();
    assert!(svg.contains(".cd { fill: blue }"), "{svg}");
    for gone in ["evil.example", "@import", "alert"] {
        assert!(!svg.contains(gone), "{gone} survived: {svg}");
    }
    assert!(svg.contains("clip-path=\"url(&quot;#ok&quot;)\""), "{svg}");
    assert!(
        out.removed.iter().any(|r| r == "script URLs"),
        "{:?}",
        out.removed
    );
}

#[test]
fn svg_sanitize_refuses_what_is_not_an_svg() {
    assert!(sanitize("<html><body/></html>").is_err());
    assert!(sanitize("not xml at all").is_err());
    assert!(sanitize("").is_err());
}

#[test]
fn svg_size_falls_back_to_width_and_height() {
    let out =
        sanitize(r#"<svg xmlns="http://www.w3.org/2000/svg" width="120px" height="40"/>"#).unwrap();
    assert_eq!(out.size, Some((120.0, 40.0)));
    assert!(!is_square_ish(120.0, 40.0));
}

// ---- raster ----------------------------------------------------------------

#[test]
fn small_square_crops_the_centre_and_never_upscales() {
    let image = raster::decode(FaceFormat::Png, &png_bytes(300, 260)).unwrap();
    let small = raster::small_square(&image, SMALL_SIDE);
    assert_eq!((small.width, small.height), (64, 64));
    let tiny = raster::decode(FaceFormat::Png, &png_bytes(20, 20)).unwrap();
    let kept = raster::small_square(&tiny, SMALL_SIDE);
    assert_eq!((kept.width, kept.height), (20, 20));
    let encoded = raster::encode_png(&small).unwrap();
    assert_eq!(
        raster::dimensions(FaceFormat::Png, &encoded).unwrap(),
        (64, 64)
    );
}

#[test]
fn decode_refuses_oversized_images_before_decoding() {
    let err = raster::decode(FaceFormat::Png, &png_bytes(1025, 1025))
        .err()
        .unwrap();
    assert!(err.contains("1024"), "{err}");
}

// ---- add ---------------------------------------------------------------------

#[test]
fn add_copies_a_png_in_writes_its_small_variant_and_the_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let avatars = tmp.path().join("team/avatars");
    let source = tmp.path().join("downloads/grumpy cat.png");
    write(&source, &png_bytes(200, 200));
    let out = store::add_face(
        &avatars,
        &AddRequest {
            pack: "office-legends".into(),
            id: "grumpy".into(),
            file: source.clone(),
            label: Some("Grumpy".into()),
            pack_name: None,
        },
    )
    .unwrap();
    assert_eq!(out.reference, "office-legends:grumpy");
    assert_eq!(out.file, avatars.join("office-legends/grumpy.png"));
    let small = out.small_file.clone().unwrap();
    assert_eq!(small, avatars.join("office-legends/grumpy.small.png"));
    assert_eq!(
        raster::dimensions(FaceFormat::Png, &std::fs::read(&small).unwrap()).unwrap(),
        (SMALL_SIDE, SMALL_SIDE)
    );
    assert!(!out.replaced);
    let yaml = std::fs::read_to_string(avatars.join("office-legends/pack.yaml")).unwrap();
    assert!(yaml.contains("name: Office Legends"), "{yaml}");
    assert!(yaml.contains("file: grumpy.png"));
    assert!(yaml.contains("label: Grumpy"));
    // The source is left alone.
    assert!(source.is_file());

    // A second face, already written into the pack folder, is used where it is.
    write(
        &avatars.join("office-legends/calm-one.svg"),
        CLEAN_SVG.as_bytes(),
    );
    let second = store::add_face(
        &avatars,
        &AddRequest {
            pack: "office-legends".into(),
            id: "calm".into(),
            file: "calm-one.svg".into(),
            label: None,
            pack_name: Some("Ignored, the pack has a name".into()),
        },
    )
    .unwrap();
    assert_eq!(second.file, avatars.join("office-legends/calm-one.svg"));
    assert_eq!(second.small_file, None);

    let found = store::discover(&[PackRoot {
        scope: PackScope::Team,
        avatars_dir: avatars.clone(),
    }]);
    assert!(found.warnings.is_empty(), "{:?}", found.warnings);
    let pack = &found.packs[0];
    assert_eq!(pack.name, "Office Legends");
    let ids: Vec<_> = pack.faces.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["grumpy", "calm"]);
    let src = store::display_source(&pack.faces[0]).unwrap();
    assert!(src.starts_with("data:image/png;base64,"));
    let src = store::display_source(&pack.faces[1]).unwrap();
    assert!(src.starts_with("data:image/svg+xml;base64,"));
}

#[test]
fn add_replaces_a_face_by_id_and_cleans_a_dirty_svg_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let avatars = tmp.path().join("avatars");
    let dirty = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><script>x()</script><rect width="8" height="8"/></svg>"#;
    write(&avatars.join("p/face.svg"), dirty.as_bytes());
    let req = AddRequest {
        pack: "p".into(),
        id: "face".into(),
        file: "face.svg".into(),
        label: None,
        pack_name: None,
    };
    let first = store::add_face(&avatars, &req).unwrap();
    assert!(
        first.warnings.iter().any(|w| w.contains("<script>")),
        "{:?}",
        first.warnings
    );
    let written = std::fs::read_to_string(avatars.join("p/face.svg")).unwrap();
    assert!(!written.contains("script"));
    let again = store::add_face(&avatars, &req).unwrap();
    assert!(again.replaced);
    let yaml = std::fs::read_to_string(avatars.join("p/pack.yaml")).unwrap();
    assert_eq!(yaml.matches("id: face").count(), 1);
}

#[test]
fn add_refuses_bad_input_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let avatars = tmp.path().join("avatars");
    let wide = tmp.path().join("wide.png");
    write(&wide, &png_bytes(300, 100));
    let fake = tmp.path().join("fake.png");
    write(&fake, CLEAN_SVG.as_bytes());
    let jpeg = tmp.path().join("photo.jpg");
    write(&jpeg, b"\xff\xd8\xff\xe0");
    let huge = tmp.path().join("huge.svg");
    write(&huge, &vec![b' '; (MAX_FILE_BYTES + 1) as usize]);
    let base = |pack: &str, id: &str, file: &Path| AddRequest {
        pack: pack.into(),
        id: id.into(),
        file: file.to_path_buf(),
        label: None,
        pack_name: None,
    };
    let cases = [
        (base("p", "wide", &wide), "square"),
        (base("p", "fake", &fake), "is a SVG"),
        (base("p", "photo", &jpeg), ".png, .webp and .svg"),
        (base("p", "huge", &huge), "limit"),
        (
            base("p", "missing", &tmp.path().join("nope.png")),
            "not found",
        ),
        (base("Bad Pack", "x", &wide), "pack"),
        (base("builtin-funny", "x", &wide), "built-in"),
        (base("p", "Bad Id", &wide), "id"),
    ];
    for (req, expect) in cases {
        let err = store::add_face(&avatars, &req).unwrap_err();
        assert!(err.contains(expect), "{expect}: {err}");
    }
    assert!(
        !avatars.exists(),
        "nothing may be written for a refused add"
    );
}

#[test]
fn add_refuses_to_clobber_an_unreadable_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let avatars = tmp.path().join("avatars");
    write(&avatars.join("p/pack.yaml"), b"faces: [\n");
    write(&avatars.join("p/a.svg"), CLEAN_SVG.as_bytes());
    let err = store::add_face(
        &avatars,
        &AddRequest {
            pack: "p".into(),
            id: "a".into(),
            file: "a.svg".into(),
            label: None,
            pack_name: None,
        },
    )
    .unwrap_err();
    assert!(err.contains("unreadable"), "{err}");
    assert_eq!(
        std::fs::read_to_string(avatars.join("p/pack.yaml")).unwrap(),
        "faces: [\n"
    );
}

// ---- discovery -------------------------------------------------------------

#[test]
fn discovery_skips_bad_packs_and_faces_with_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let team = team_root(tmp.path());
    let dir = &team.avatars_dir;
    // A good pack with one good face and several bad ones.
    write(&dir.join("good/a.png"), &png_bytes(100, 100));
    write(&dir.join("good/wide.png"), &png_bytes(400, 100));
    write(&dir.join("good/lies.webp"), &png_bytes(10, 10));
    write(&dir.join("good/photo.jpg"), b"\xff\xd8\xff");
    write(
        &dir.join("good/pack.yaml"),
        b"name: Good\nfaces:\n  - {id: a, file: a.png}\n  - {id: wide, file: wide.png}\n  - {id: lies, file: lies.webp}\n  - {id: photo, file: photo.jpg}\n  - {id: gone, file: gone.png}\n  - {id: escape, file: ../../secret.png}\n",
    );
    // Packs that are skipped whole.
    write(&dir.join("no-manifest/a.png"), &png_bytes(10, 10));
    write(&dir.join("broken/pack.yaml"), b"faces: [\n");
    write(&dir.join("builtin-funny/pack.yaml"), b"faces: []\n");
    write(&dir.join("Bad Name/pack.yaml"), b"faces: []\n");
    let found = store::discover(&[team]);
    let ids: Vec<_> = found.packs.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["good"]);
    let faces: Vec<_> = found.packs[0].faces.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(faces, ["a"]);
    // A face without a small variant still shows, from its original.
    assert_eq!(found.packs[0].faces[0].small, None);
    assert!(store::display_source(&found.packs[0].faces[0])
        .unwrap()
        .starts_with("data:image/png;base64,"));
    assert_eq!(found.warnings.len(), 9, "{:#?}", found.warnings);
    assert!(found
        .warnings
        .iter()
        .all(|w| w.starts_with("team avatars/")));
}

#[test]
fn team_packs_come_first_and_a_personal_pack_of_the_same_name_is_the_teams() {
    let tmp = tempfile::tempdir().unwrap();
    let team = team_root(tmp.path());
    let me = personal_root(tmp.path());
    for (root, pack) in [(&team, "shared"), (&me, "shared"), (&me, "mine")] {
        write(
            &root.avatars_dir.join(pack).join("a.svg"),
            CLEAN_SVG.as_bytes(),
        );
        write(
            &root.avatars_dir.join(pack).join("pack.yaml"),
            b"faces:\n  - {id: a, file: a.svg}\n",
        );
    }
    let found = store::discover(&[team, me]);
    let packs: Vec<_> = found
        .packs
        .iter()
        .map(|p| (p.id.as_str(), p.scope))
        .collect();
    assert_eq!(
        packs,
        [("shared", PackScope::Team), ("mine", PackScope::Personal)]
    );
    assert_eq!(found.packs[1].name, "mine");
    // Listed once, as the team's, with no warning: the personal one is its copy.
    assert!(found.warnings.is_empty(), "{:?}", found.warnings);
    assert!(found.packs[0].personal_copy);
    assert!(!found.packs[1].personal_copy);
}

#[test]
fn a_different_personal_pack_with_a_team_packs_name_is_skipped_with_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let team = team_root(tmp.path());
    let me = personal_root(tmp.path());
    for (root, label) in [(&team, "Team"), (&me, "Mine")] {
        write(&root.avatars_dir.join("crew/a.svg"), CLEAN_SVG.as_bytes());
        write(
            &root.avatars_dir.join("crew/pack.yaml"),
            format!("faces:\n  - {{id: a, file: a.svg, label: {label}}}\n").as_bytes(),
        );
    }
    let found = store::discover(&[team, me]);
    assert_eq!(found.packs.len(), 1);
    assert!(
        !found.packs[0].personal_copy,
        "not a copy: no Shared with team"
    );
    assert_eq!(found.warnings.len(), 1);
    assert!(found.warnings[0].starts_with("personal avatars/crew"));
    assert!(found.warnings[0].contains("rename the folder"));
}

#[test]
fn a_personal_add_lands_only_in_the_personal_root() {
    let tmp = tempfile::tempdir().unwrap();
    let team = team_root(tmp.path());
    let me = personal_root(tmp.path());
    let source = tmp.path().join("me.svg");
    write(&source, CLEAN_SVG.as_bytes());
    store::add_face(
        &me.avatars_dir,
        &AddRequest {
            pack: "mine".into(),
            id: "me".into(),
            file: source,
            label: None,
            pack_name: None,
        },
    )
    .unwrap();
    assert!(store::discover(std::slice::from_ref(&team))
        .packs
        .is_empty());
    let found = store::discover(&[team, me]);
    assert_eq!(found.packs.len(), 1);
    assert_eq!(found.packs[0].scope, PackScope::Personal);
}

#[test]
fn personal_packs_show_in_every_team_of_the_org_even_one_not_using_scratch_items() {
    use crate::context_repo::scratch_space;
    use crate::domain::Principal;

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let me = Principal::from(42u64);
    let org = "acme/team-context";

    // No private space yet: nothing personal is listed.
    let roots = store::roots_for_team(home, &me, org, "default");
    assert!(roots.iter().all(|r| r.scope != PackScope::Personal));

    let pack = home
        .join("users/42/scratch/orgs/acme/team-context/content/avatars")
        .join("yard-pack");
    write(&pack.join("a.svg"), CLEAN_SVG.as_bytes());
    write(
        &pack.join("pack.yaml"),
        b"faces:\n  - {id: a, file: a.svg}\n",
    );
    scratch_space::set_team_uses_scratch(home, &me, org, "default", false).unwrap();
    assert!(!scratch_space::team_uses_scratch(home, &me, org, "default"));

    for team in ["default", "other"] {
        let roots = store::roots_for_team(home, &me, org, team);
        let personal: Vec<_> = roots
            .iter()
            .filter(|r| r.scope == PackScope::Personal)
            .collect();
        assert_eq!(personal.len(), 1, "{team}");
        assert_eq!(personal[0].avatars_dir, pack.parent().unwrap(), "{team}");
        assert_eq!(
            roots.last().unwrap().scope,
            PackScope::Personal,
            "team packs first"
        );
        let found = store::discover(&roots);
        assert!(
            found
                .packs
                .iter()
                .any(|p| p.id == "yard-pack" && p.scope == PackScope::Personal),
            "{team}: {:?}",
            found.packs.iter().map(|p| &p.id).collect::<Vec<_>>()
        );
    }
}

// ---- built-ins -----------------------------------------------------------------

#[test]
fn builtin_packs_name_files_that_ship() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/assets/avatar-packs");
    let packs = builtin::packs();
    assert_eq!(
        packs.iter().map(|p| p.id).collect::<Vec<_>>(),
        ["builtin-chess", "builtin-funny", "builtin-crest"]
    );
    for pack in packs {
        assert!(!pack.faces.is_empty(), "{} has no faces", pack.id);
        let mut seen = std::collections::HashSet::new();
        for face in &pack.faces {
            assert!(is_valid_id(&face.id, FACE_ID_MAX), "{}", face.id);
            assert!(seen.insert(face.id.clone()), "duplicate {}", face.id);
            let path = root.join(pack.id).join(&face.file);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("{} is missing", path.display()));
            let clean = sanitize(&text).unwrap();
            assert!(!clean.changed(), "{}: {:?}", face.file, clean.removed);
            assert_eq!(clean.size, Some((64.0, 64.0)), "{}", face.file);
            assert!(face.label.is_some(), "{} needs a label", face.id);
        }
    }
}

#[test]
fn the_team_default_pack_round_trips_and_keeps_other_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("avatars");
    assert_eq!(store::read_default_pack(&dir), None);
    store::write_default_pack(&dir, Some("crew")).unwrap();
    assert_eq!(store::read_default_pack(&dir).as_deref(), Some("crew"));
    let path = dir.join(SETTINGS_FILE);
    let text = std::fs::read_to_string(&path).unwrap() + "note: kept\n";
    std::fs::write(&path, text).unwrap();
    store::write_default_pack(&dir, None).unwrap();
    assert_eq!(store::read_default_pack(&dir), None);
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("note: kept"));
    assert!(store::write_default_pack(&dir, Some("Bad Id")).is_err());
    // The settings file is not a pack.
    assert!(store::discover(&[PackRoot {
        scope: PackScope::Team,
        avatars_dir: dir
    }])
    .packs
    .is_empty());
}

// ---- sharing a personal pack ----------------------------------------------

#[test]
fn sharing_a_personal_pack_copies_it_to_the_team_where_its_faces_keep_their_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let (team, me) = (team_root(tmp.path()), personal_root(tmp.path()));
    let source = tmp.path().join("riley.png");
    write(&source, &png_bytes(64, 64));
    store::add_face(
        &me.avatars_dir,
        &AddRequest {
            pack: "yard".into(),
            id: "riley".into(),
            file: source,
            label: Some("Riley".into()),
            pack_name: Some("Yard Crew".into()),
        },
    )
    .unwrap();

    // A crashed share's hidden copy is swept, never left to be committed.
    write(&team.avatars_dir.join(".yard.sharing-1/junk.png"), b"x");

    let shared = store::share_pack(&me.avatars_dir, &team.avatars_dir, "yard").unwrap();
    assert_eq!(shared, team.avatars_dir.join("yard"));
    assert!(
        me.avatars_dir.join("yard/pack.yaml").is_file(),
        "copied: the personal pack stays for the org's other teams"
    );
    let found = store::discover(&[team.clone(), me.clone()]);
    // The team lists it exactly once, as the team's.
    assert_eq!(found.packs.len(), 1, "{:?}", found.warnings);
    assert!(found.warnings.is_empty(), "{:?}", found.warnings);
    let pack = &found.packs[0];
    assert!(pack.personal_copy);
    assert_eq!(pack.dir, team.avatars_dir.join("yard"));
    // Another team of the org, without the pack, still lists the personal one.
    let other = PackRoot {
        scope: PackScope::Team,
        avatars_dir: tmp.path().join("other/avatars"),
    };
    let elsewhere: Vec<_> = store::discover(&[other, me.clone()])
        .packs
        .iter()
        .map(|p| (p.id.clone(), p.scope))
        .collect();
    assert_eq!(elsewhere, [("yard".to_string(), PackScope::Personal)]);
    assert_eq!(
        (pack.id.as_str(), pack.name.as_str(), pack.scope),
        ("yard", "Yard Crew", PackScope::Team)
    );
    assert_eq!(face_ref(&pack.id, &pack.faces[0].id), "yard:riley");
    assert!(
        pack.faces[0].small.is_some(),
        "the small variant came along"
    );
    let leftovers: Vec<_> = std::fs::read_dir(&team.avatars_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(leftovers, ["yard"], "no staging folder is left behind");
}

#[test]
fn sharing_a_pack_the_team_already_has_is_refused_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (team, me) = (team_root(tmp.path()), personal_root(tmp.path()));
    write(
        &me.avatars_dir.join("crew/pack.yaml"),
        b"name: Mine\nfaces: []\n",
    );
    write(
        &team.avatars_dir.join("crew/pack.yaml"),
        b"name: Theirs\nfaces: []\n",
    );
    let err = store::share_pack(&me.avatars_dir, &team.avatars_dir, "crew").unwrap_err();
    assert!(err.contains("already has a pack named `crew`"), "{err}");
    assert_eq!(
        std::fs::read_to_string(team.avatars_dir.join("crew/pack.yaml")).unwrap(),
        "name: Theirs\nfaces: []\n"
    );
    assert!(me.avatars_dir.join("crew/pack.yaml").is_file());

    for missing in ["nope", "builtin-funny", "../crew"] {
        assert!(
            store::share_pack(&me.avatars_dir, &team.avatars_dir, missing).is_err(),
            "{missing}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_pack_holding_a_link_is_not_shared() {
    let tmp = tempfile::tempdir().unwrap();
    let (team, me) = (team_root(tmp.path()), personal_root(tmp.path()));
    write(&me.avatars_dir.join("crew/pack.yaml"), b"faces: []\n");
    write(&tmp.path().join("secret.svg"), CLEAN_SVG.as_bytes());
    std::os::unix::fs::symlink(
        tmp.path().join("secret.svg"),
        me.avatars_dir.join("crew/a.svg"),
    )
    .unwrap();
    let err = store::share_pack(&me.avatars_dir, &team.avatars_dir, "crew").unwrap_err();
    assert!(err.contains("holds a link"), "{err}");
    assert!(!team.avatars_dir.join("crew").exists());
}

#[test]
fn a_share_never_replaces_a_pack_folder_that_appeared_meanwhile() {
    // Even an empty folder of the same name keeps its place: the share is
    // refused and the folder is left as it was.
    let tmp = tempfile::tempdir().unwrap();
    let (team, me) = (team_root(tmp.path()), personal_root(tmp.path()));
    write(&me.avatars_dir.join("crew/pack.yaml"), b"faces: []\n");
    std::fs::create_dir_all(team.avatars_dir.join("crew")).unwrap();
    let err = store::share_pack(&me.avatars_dir, &team.avatars_dir, "crew").unwrap_err();
    assert!(err.contains("already has a pack named `crew`"), "{err}");
    assert_eq!(
        std::fs::read_dir(team.avatars_dir.join("crew"))
            .unwrap()
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn publishing_a_shared_pack_never_renames_over_an_empty_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = tmp.path().join(".crew.sharing-1");
    write(&staged.join("pack.yaml"), b"faces: []\n");
    let target = tmp.path().join("crew");
    std::fs::create_dir(&target).unwrap();
    let err = store::publish_new(&staged, &target).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    assert!(staged.join("pack.yaml").is_file());
}

#[test]
fn find_face_reads_one_pack_the_way_discovery_would_and_source_bytes_prefers_the_small_png() {
    let dir = tempfile::tempdir().unwrap();
    let team = dir.path().join("team/avatars/crew");
    let personal = dir.path().join("me/avatars/crew");
    write(&team.join("pack.yaml"), b"name: Crew\nfaces:\n  - id: red\n    file: red.png\n  - id: logo\n    file: logo.svg\n  - id: bad\n    file: missing.png\n");
    write(&team.join("red.png"), &png_bytes(200, 200));
    write(&team.join("red.small.png"), &png_bytes(64, 64));
    write(&team.join("logo.svg"), CLEAN_SVG.as_bytes());
    // The earlier root's pack of that name is the one that answers.
    write(
        &personal.join("pack.yaml"),
        b"faces:\n  - id: mine\n    file: mine.png\n",
    );
    write(&personal.join("mine.png"), &png_bytes(64, 64));
    let roots = [team_root(dir.path()), personal_root(dir.path())];

    let red = store::find_face(&roots, "crew", "red").unwrap();
    let (format, bytes) = store::source_bytes(&red).unwrap();
    assert_eq!(format, FaceFormat::Png);
    assert_eq!(
        raster::dimensions(FaceFormat::Png, &bytes).unwrap(),
        (64, 64)
    );
    let (format, bytes) =
        store::source_bytes(&store::find_face(&roots, "crew", "logo").unwrap()).unwrap();
    assert_eq!(
        (format, bytes),
        (FaceFormat::Svg, CLEAN_SVG.as_bytes().to_vec())
    );
    // A face that does not validate, one that is not listed, a pack that is not there,
    // a shipped pack's name and malformed ids are all nothing.
    for (pack, id) in [
        ("crew", "bad"),
        ("crew", "mine"),
        ("nope", "red"),
        ("builtin-funny", "coffee-bot"),
        ("Crew", "red"),
        ("crew", "../red"),
        ("../x", "red"),
    ] {
        assert!(store::find_face(&roots, pack, id).is_none(), "{pack}:{id}");
    }
}

#[cfg(unix)]
#[test]
fn a_face_cannot_borrow_an_image_through_a_linked_folder_or_a_dotdot() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("private");
    write(&outside.join("secret.png"), &png_bytes(64, 64));
    let pack = dir.path().join("team/avatars/crew");
    write(
        &pack.join("pack.yaml"),
        b"faces:\n  - id: viasub\n    file: sub/secret.png\n  - id: dotdot\n    file: ../../../private/secret.png\n  - id: ok\n    file: ok.png\n  - id: nested\n    file: real/inner.png\n",
    );
    write(&pack.join("ok.png"), &png_bytes(64, 64));
    write(&pack.join("real/inner.png"), &png_bytes(64, 64));
    // A folder inside the pack that links out of it.
    std::os::unix::fs::symlink(&outside, pack.join("sub")).unwrap();
    let roots = [team_root(dir.path())];

    assert!(store::find_face(&roots, "crew", "viasub").is_none());
    assert!(store::find_face(&roots, "crew", "dotdot").is_none());
    // A plain subfolder and a plain file are still faces.
    assert!(store::find_face(&roots, "crew", "ok").is_some());
    assert!(store::find_face(&roots, "crew", "nested").is_some());
    // Discovery agrees, so the list never offers what the phone would not be given.
    let found = store::discover(&roots);
    let ids: Vec<_> = found.packs[0].faces.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["ok", "nested"]);
    assert!(found
        .warnings
        .iter()
        .any(|w| w.contains("viasub") && w.contains("link")));
    assert!(found.warnings.iter().any(|w| w.contains("dotdot")));
}

#[cfg(unix)]
#[test]
fn a_manifest_that_is_a_link_a_pipe_a_device_or_huge_is_not_read() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let make = |name: &str| {
        let pack = dir.path().join("team/avatars").join(name);
        write(&pack.join("ok.png"), &png_bytes(64, 64));
        pack
    };
    let manifest = b"faces:\n  - id: ok\n    file: ok.png\n";
    let roots = [team_root(dir.path())];

    // A plain manifest works.
    write(&make("plain").join("pack.yaml"), manifest);
    assert!(store::find_face(&roots, "plain", "ok").is_some());
    // A manifest that is a link, even to a good manifest.
    let linked = make("linked");
    write(&dir.path().join("elsewhere.yaml"), manifest);
    symlink(dir.path().join("elsewhere.yaml"), linked.join("pack.yaml")).unwrap();
    assert!(store::find_face(&roots, "linked", "ok").is_none());
    // A manifest that is a device (never read, so it cannot hang or fill memory).
    let device = make("device");
    symlink("/dev/zero", device.join("pack.yaml")).unwrap();
    assert!(store::find_face(&roots, "device", "ok").is_none());
    // A manifest that is a pipe: reading it would block.
    let pipe = make("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(pipe.join("pack.yaml"))
        .status();
    if made.is_ok_and(|status| status.success()) {
        let started = std::time::Instant::now();
        assert!(store::find_face(&roots, "pipe", "ok").is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
    // A manifest over the cap.
    let big = make("big");
    let mut text = manifest.to_vec();
    text.extend(std::iter::repeat_n(b'#', 70 * 1024));
    write(&big.join("pack.yaml"), &text);
    assert!(store::find_face(&roots, "big", "ok").is_none());
    // A pack folder that is itself a link.
    let real = make("real");
    write(&real.join("pack.yaml"), manifest);
    symlink(&real, dir.path().join("team/avatars/alias")).unwrap();
    assert!(store::find_face(&roots, "alias", "ok").is_none());
}

#[cfg(unix)]
#[test]
fn an_avatars_folder_that_is_a_link_lends_none_of_its_packs() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    // Another user's avatars folder, with a good pack in it.
    let private = dir.path().join("private/avatars/crew");
    write(
        &private.join("pack.yaml"),
        b"faces:\n  - id: ok\n    file: ok.png\n",
    );
    write(&private.join("ok.png"), &png_bytes(64, 64));
    std::fs::create_dir_all(dir.path().join("team")).unwrap();
    symlink(
        dir.path().join("private/avatars"),
        dir.path().join("team/avatars"),
    )
    .unwrap();
    let roots = [team_root(dir.path())];
    assert!(store::find_face(&roots, "crew", "ok").is_none());
    assert!(store::discover(&roots).packs.is_empty());
    // The same folder, not linked, is used.
    let plain = [PackRoot {
        scope: PackScope::Team,
        avatars_dir: dir.path().join("private/avatars"),
    }];
    assert!(store::find_face(&plain, "crew", "ok").is_some());
    assert_eq!(store::discover(&plain).packs.len(), 1);
}

#[cfg(unix)]
#[test]
fn packs_are_trusted_only_from_the_org_root_down_with_no_link_on_the_way() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let manifest = b"faces:\n  - id: ok\n    file: ok.png\n";
    let make_pack = |avatars: &Path| {
        write(&avatars.join("crew/pack.yaml"), manifest);
        write(&avatars.join("crew/ok.png"), &png_bytes(64, 64));
    };
    let team_root = |avatars_dir: std::path::PathBuf| PackRoot {
        scope: PackScope::Team,
        avatars_dir,
    };
    // Another team's real content, which a link must not be able to lend.
    make_pack(&root.join("private/content/avatars"));

    // The plain layout works, and the org root itself may be reached through a link.
    make_pack(&root.join("org/team/content/avatars"));
    let plain = [team_root(root.join("org/team/content/avatars"))];
    assert!(store::find_face(&plain, "crew", "ok").is_some());
    assert_eq!(store::discover(&plain).packs.len(), 1);
    symlink(root.join("org"), root.join("org-link")).unwrap();
    let via_anchor = [team_root(root.join("org-link/team/content/avatars"))];
    assert!(store::find_face(&via_anchor, "crew", "ok").is_some());

    // A link at `content`, at the team folder or at `avatars` is refused by both lookups.
    std::fs::create_dir_all(root.join("a/team")).unwrap();
    symlink(root.join("private/content"), root.join("a/team/content")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::create_dir_all(root.join("private/team-dir")).unwrap();
    symlink(root.join("private"), root.join("b/team")).unwrap();
    std::fs::create_dir_all(root.join("c/team/content")).unwrap();
    symlink(
        root.join("private/content/avatars"),
        root.join("c/team/content/avatars"),
    )
    .unwrap();
    for (name, avatars_dir) in [
        ("linked content", root.join("a/team/content/avatars")),
        ("linked team folder", root.join("b/team/content/avatars")),
        ("linked avatars", root.join("c/team/content/avatars")),
    ] {
        let linked = [team_root(avatars_dir)];
        assert!(store::find_face(&linked, "crew", "ok").is_none(), "{name}");
        assert!(store::discover(&linked).packs.is_empty(), "{name}");
    }
}
