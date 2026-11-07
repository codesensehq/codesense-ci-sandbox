// SPDX-License-Identifier: MIT
//! Finding packs on disk, adding a face to one, and turning a face into
//! something the webview can draw.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;

use crate::context_repo::scratch_space;
use crate::domain::Principal;

use super::manifest::{FaceEntry, Manifest};
use super::{
    face_ref, is_square_ish, is_valid_id, raster, svg, FaceFormat, PackScope, AVATARS_DIR,
    BUILTIN_PREFIX, FACE_ID_MAX, MANIFEST_FILE, MAX_FILE_BYTES, PACK_ID_MAX, SMALL_SIDE,
    SMALL_SUFFIX,
};

/// A folder of packs (`…/content/avatars`) and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRoot {
    pub scope: PackScope,
    pub avatars_dir: PathBuf,
}

/// A face that passed validation.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedFace {
    pub id: String,
    pub label: Option<String>,
    pub format: FaceFormat,
    pub file: PathBuf,
    /// The small PNG next to a raster face, when one was made.
    pub small: Option<PathBuf>,
}

/// A pack and its usable faces.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPack {
    pub id: String,
    pub name: String,
    pub scope: PackScope,
    pub dir: PathBuf,
    pub faces: Vec<LoadedFace>,
    /// A team pack the user also has in their own space under the same id (the
    /// one they shared): the personal copy is not listed separately.
    pub personal_copy: bool,
}

/// Every pack found, and what was skipped along the way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Discovery {
    pub packs: Vec<LoadedPack>,
    pub warnings: Vec<String>,
}

/// The pack a team deals faces from for agents nobody gave one, read from
/// `avatars/settings.yaml` (`default_pack: <id>`). `None` when unset or
/// unreadable: the built-in funny pack then.
pub fn read_default_pack(avatars_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(avatars_dir.join(super::SETTINGS_FILE)).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let pack = value.get("default_pack")?.as_str()?.trim().to_string();
    is_valid_id(&pack, PACK_ID_MAX).then_some(pack)
}

/// Set (or with `None` clear) the team's default pack, keeping any other
/// settings in the file.
pub fn write_default_pack(avatars_dir: &Path, pack: Option<&str>) -> Result<(), String> {
    let path = avatars_dir.join(super::SETTINGS_FILE);
    let mut root = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_yaml::from_str::<serde_yaml::Value>(&text) {
            Ok(serde_yaml::Value::Mapping(map)) => map,
            Ok(serde_yaml::Value::Null) => serde_yaml::Mapping::new(),
            _ => {
                return Err(format!(
                    "{} is not a mapping; fix or delete it first",
                    path.display()
                ))
            }
        },
        Err(_) => serde_yaml::Mapping::new(),
    };
    match pack.map(str::trim) {
        Some(pack) => {
            if !is_valid_id(pack, PACK_ID_MAX) {
                return Err(format!("`{pack}` is not a pack id"));
            }
            root.insert("default_pack".into(), pack.into());
        }
        None => {
            root.remove("default_pack");
        }
    }
    let yaml =
        serde_yaml::to_string(&serde_yaml::Value::Mapping(root)).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(avatars_dir)
        .map_err(|e| format!("cannot create {}: {e}", avatars_dir.display()))?;
    write_atomic(&path, yaml.as_bytes())
}

/// Where `org/team`'s packs are, team first: the team's content, then the
/// user's own scratch space for the org whenever that space exists. A person's
/// own packs show in every team of the org, whether or not the team overlays
/// the rest of their scratch items.
pub fn roots_for_team(home: &Path, principal: &Principal, org: &str, team: &str) -> Vec<PackRoot> {
    let mut roots = Vec::new();
    match crate::agents::roots::team_content_dir(home, principal, org, team) {
        Ok(content) => roots.push(PackRoot {
            scope: if scratch_space::is_scratch_team(team) {
                PackScope::Personal
            } else {
                PackScope::Team
            },
            avatars_dir: content.join(AVATARS_DIR),
        }),
        Err(e) => log::warn!(target: "avatars", "no team content dir for {org}/{team}: {e}"),
    }
    if let Some(space) = scratch_space::org_space(home, principal, org) {
        let dir = space.content_dir().join(AVATARS_DIR);
        if !roots.iter().any(|r| r.avatars_dir == dir) {
            roots.push(PackRoot {
                scope: PackScope::Personal,
                avatars_dir: dir,
            });
        }
    }
    roots
}

/// The folder an add in `scope` writes into, creating the private space on
/// first use for a personal add.
pub fn avatars_dir_for_add(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
    scope: PackScope,
) -> Result<PathBuf, String> {
    match scope {
        PackScope::Team => crate::agents::roots::team_content_dir(home, principal, org, team)
            .map(|c| c.join(AVATARS_DIR))
            .map_err(|e| e.to_string()),
        PackScope::Personal => scratch_space::ensure_org_space(home, principal, org)
            .map(|(space, _)| space.content_dir().join(AVATARS_DIR))
            .map_err(|e| e.to_string()),
        PackScope::Builtin => Err("the built-in packs cannot be added to".to_string()),
    }
}

/// Every pack under `roots`, in root order then by folder name. A pack id that
/// an earlier root already has is skipped with a warning.
pub fn discover(roots: &[PackRoot]) -> Discovery {
    discover_for(roots, &Default::default())
}

pub(crate) fn discover_for(
    roots: &[PackRoot],
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Discovery {
    let mut out = Discovery::default();
    let mut seen: HashSet<String> = HashSet::new();
    for root in roots {
        if !plain_avatars_root(root) {
            continue;
        }
        let Ok(read) = std::fs::read_dir(&root.avatars_dir) else {
            continue;
        };
        let mut dirs: Vec<(String, PathBuf)> = read
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .filter(|(name, _)| !name.starts_with('.'))
            .collect();
        dirs.sort();
        for (id, dir) in dirs {
            if !scope.allows(&dir.join(MANIFEST_FILE)) {
                continue;
            }
            let at = format!("{} avatars/{id}", scope_word(root.scope));
            if !is_valid_id(&id, PACK_ID_MAX) {
                out.warnings.push(format!(
                    "{at}: a pack folder name is lowercase letters, digits and dashes, at most {PACK_ID_MAX} characters; skipped"
                ));
                continue;
            }
            if id.starts_with(BUILTIN_PREFIX) {
                out.warnings.push(format!(
                    "{at}: names starting `{BUILTIN_PREFIX}` belong to the built-in packs; skipped"
                ));
                continue;
            }
            if seen.contains(&id) {
                // A personal pack the team holds a copy of (one the user shared)
                // is the team's here: listed once, as the team's, and its faces
                // resolve to the team's copy. A different pack that only shares
                // the name is skipped with a warning, as any other clash is.
                let shared = out
                    .packs
                    .iter_mut()
                    .find(|p| p.id == id && p.scope == PackScope::Team)
                    .filter(|p| {
                        root.scope == PackScope::Personal
                            && scope.allows(&p.dir)
                            && scope.allows(&dir)
                            && same_pack(&p.dir, &dir)
                    });
                match shared {
                    Some(team_pack) => team_pack.personal_copy = true,
                    None => out.warnings.push(format!(
                        "{at}: a pack with this name is already listed; rename the folder to use it"
                    )),
                }
                continue;
            }
            let manifest_path = dir.join(MANIFEST_FILE);
            let text = match scope.read_admitted(&manifest_path) {
                Ok(text) => text,
                Err(_) => {
                    out.warnings
                        .push(format!("{at}: no readable {MANIFEST_FILE}; skipped"));
                    continue;
                }
            };
            let manifest = match Manifest::parse(&text) {
                Ok(m) => m,
                Err(e) => {
                    out.warnings
                        .push(format!("{at}/{MANIFEST_FILE}: {e}; skipped"));
                    continue;
                }
            };
            let mut entry_warnings = Vec::new();
            let entries = manifest.entries(&mut entry_warnings);
            let mut faces = Vec::new();
            for entry in entries {
                if !scope.allows(&dir.join(&entry.file)) {
                    continue;
                }
                match check_face(&dir, &entry) {
                    Ok(mut face) => {
                        face.small = face.small.filter(|path| scope.allows(path));
                        faces.push(face);
                    }
                    Err(e) => entry_warnings.push(format!("face `{}`: {e}", entry.id)),
                }
            }
            out.warnings.extend(
                entry_warnings
                    .into_iter()
                    .map(|w| format!("{at}: {w}; skipped")),
            );
            seen.insert(id.clone());
            out.packs.push(LoadedPack {
                name: manifest.name().unwrap_or_else(|| id.clone()),
                id,
                scope: root.scope,
                dir,
                faces,
                personal_copy: false,
            });
        }
    }
    out
}

/// Whether two pack folders hold the same files with the same bytes: a team
/// pack that is a copy of a personal one. Unreadable reads as different.
fn same_pack(a: &Path, b: &Path) -> bool {
    match (pack_fingerprint(a), pack_fingerprint(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A hash of every file under `dir` (folders included, links skipped), by
/// relative path and contents, in path order.
fn pack_fingerprint(dir: &Path) -> std::io::Result<blake3::Hash> {
    fn files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                files(root, &entry.path(), out)?;
            } else if kind.is_file() {
                out.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(&entry.path())
                        .to_path_buf(),
                );
            }
        }
        Ok(())
    }
    let mut paths = Vec::new();
    files(dir, dir, &mut paths)?;
    paths.sort();
    let mut hasher = blake3::Hasher::new();
    for rel in paths {
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(&[0]);
        let bytes = std::fs::read(dir.join(&rel))?;
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(hasher.finalize())
}

fn scope_word(scope: PackScope) -> &'static str {
    match scope {
        PackScope::Team => "team",
        PackScope::Personal => "personal",
        PackScope::Builtin => "built-in",
    }
}

/// A manifest `file` value as a path inside `pack_dir`, or why it is not one. The path may
/// go through no link at any level, and, where it exists, resolves inside the pack folder, so
/// a folder linked into the pack cannot lend an outside image to a face.
fn inside_pack(pack_dir: &Path, file: &str) -> Result<PathBuf, String> {
    let rel = Path::new(file);
    if file.is_empty()
        || rel.is_absolute()
        || rel.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(format!(
            "`{file}` must be a file name inside the pack folder"
        ));
    }
    let path = pack_dir.join(rel);
    let mut at = pack_dir.to_path_buf();
    for part in rel.components() {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("goes through a link, not a plain folder".to_string())
            }
            Ok(_) => {}
            // A missing file is reported by the read that follows.
            Err(_) => break,
        }
    }
    if let (Ok(root), Ok(real)) = (pack_dir.canonicalize(), path.canonicalize()) {
        if !real.starts_with(&root) {
            return Err("resolves outside the pack folder".to_string());
        }
    }
    Ok(path)
}

/// Read a face file, refusing links, non-files and anything over the cap.
fn read_face_file(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "file not found".to_string())?;
    if meta.file_type().is_symlink() {
        return Err("a link, not a file".to_string());
    }
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} KB is over the {} KB limit",
            meta.len() / 1024,
            MAX_FILE_BYTES / 1024
        ));
    }
    std::fs::read(path).map_err(|e| format!("unreadable: {e}"))
}

/// The format of a face file: its extension, confirmed by its bytes.
fn format_of(name: &str, bytes: &[u8]) -> Result<FaceFormat, String> {
    let by_name = FaceFormat::from_file_name(name)
        .ok_or_else(|| "only .png, .webp and .svg faces are supported".to_string())?;
    match FaceFormat::sniff(bytes) {
        Some(actual) if actual == by_name => Ok(by_name),
        Some(actual) => Err(format!(
            "named .{} but is a {} file",
            by_name.extension(),
            actual.extension().to_uppercase()
        )),
        None => Err(format!("not a {} file", by_name.extension().to_uppercase())),
    }
}

fn small_path(file: &Path) -> PathBuf {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    file.with_file_name(format!("{stem}{SMALL_SUFFIX}"))
}

fn not_square(width: f64, height: f64) -> String {
    format!("{width}×{height} is not roughly square (at most 5:4); crop it first")
}

/// Validate one manifest entry against the files in `pack_dir`.
fn check_face(pack_dir: &Path, entry: &FaceEntry) -> Result<LoadedFace, String> {
    let file = inside_pack(pack_dir, &entry.file)?;
    let bytes = read_face_file(&file)?;
    let format = format_of(&entry.file, &bytes)?;
    let mut small = None;
    match format {
        FaceFormat::Png | FaceFormat::Webp => {
            let (w, h) = raster::dimensions(format, &bytes)?;
            if w == 0 || h == 0 || w > super::MAX_SIDE || h > super::MAX_SIDE {
                return Err(format!(
                    "{w}×{h}px is outside 1–{}px per side",
                    super::MAX_SIDE
                ));
            }
            if !is_square_ish(w as f64, h as f64) {
                return Err(not_square(w as f64, h as f64));
            }
            let candidate = small_path(&file);
            if candidate.is_file() {
                small = Some(candidate);
            }
        }
        FaceFormat::Svg => {
            let text = String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())?;
            let cleaned = svg::sanitize(&text)?;
            if let Some((w, h)) = cleaned.size {
                if !is_square_ish(w, h) {
                    return Err(not_square(w, h));
                }
            }
        }
    }
    Ok(LoadedFace {
        id: entry.id.clone(),
        label: entry.label.clone(),
        format,
        file,
        small,
    })
}

/// One face by its `pack` and `id`, found as [`discover`] finds it (the first root
/// holding the pack wins) but reading only that pack's manifest and that face's file.
pub fn find_face(roots: &[PackRoot], pack: &str, id: &str) -> Option<LoadedFace> {
    if !is_valid_id(pack, PACK_ID_MAX)
        || !is_valid_id(id, FACE_ID_MAX)
        || pack.starts_with(BUILTIN_PREFIX)
    {
        return None;
    }
    let dir = roots
        .iter()
        .filter(|root| plain_avatars_root(root))
        .map(|root| root.avatars_dir.join(pack))
        .find(|dir| std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()))?;
    let text = read_manifest(&dir)?;
    let manifest = Manifest::parse(&text).ok()?;
    let entry = manifest
        .entries(&mut Vec::new())
        .into_iter()
        .find(|entry| entry.id == id)?;
    check_face(&dir, &entry).ok()
}

/// Whether `root.avatars_dir` really is where it claims to be, with no link along the way.
///
/// Packs live at `<org root>/<team>/content/avatars`, where the org root (the context-repo
/// clone, or the user's private space parent) is the trusted anchor and may itself be reached
/// through a link. For that layout, `<team>`, `content` and `avatars` must each be a real
/// folder (not a link) and the whole path must resolve to exactly that place under the
/// canonical org root, so a link at any level cannot point the team's packs at another
/// team's or user's folder. Any other layout (a folder given directly, as the tests do)
/// is checked below its own parent: a real folder that resolves to `<parent>/<name>`.
fn plain_avatars_root(root: &PackRoot) -> bool {
    let dir = &root.avatars_dir;
    let parts: Vec<&std::ffi::OsStr> = dir
        .components()
        .rev()
        .take(3)
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let canonical_layout = parts.len() == 3
        && parts[0] == AVATARS_DIR
        && parts[1] == crate::context_repo::TEAM_CONTENT_DIR;
    let depth = if canonical_layout { 3 } else { 1 };
    let Some(anchor) = dir.ancestors().nth(depth) else {
        return false;
    };
    let Ok(below) = dir.strip_prefix(anchor) else {
        return false;
    };
    let mut at = anchor.to_path_buf();
    for part in below.components() {
        at.push(part);
        let plain = std::fs::symlink_metadata(&at)
            .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink());
        if !plain {
            return false;
        }
    }
    matches!(
        (anchor.canonicalize(), dir.canonicalize()),
        (Ok(anchor), Ok(real)) if real == anchor.join(below)
    )
}

/// The most a `pack.yaml` read here takes (a manifest lists a few hundred faces at most).
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// A pack's `pack.yaml` as text: a plain file inside the pack folder (no link at any level,
/// not a device or pipe) of at most [`MAX_MANIFEST_BYTES`], read through a bounded reader so
/// a file that lies about its size cannot run the read away.
fn read_manifest(pack_dir: &Path) -> Option<String> {
    use std::io::Read as _;
    let path = inside_pack(pack_dir, MANIFEST_FILE).ok()?;
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(&path)
        .ok()?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_MANIFEST_BYTES).then_some(text)
}

/// The source of `face` for drawing elsewhere: a raster face's small PNG when it has
/// one, else its file, and an SVG's file (to be cleaned by whoever draws it).
pub fn source_bytes(face: &LoadedFace) -> Result<(FaceFormat, Vec<u8>), String> {
    if face.format != FaceFormat::Svg {
        let small = face
            .small
            .as_deref()
            .and_then(|path| read_face_file(path).ok())
            .filter(|bytes| FaceFormat::sniff(bytes) == Some(FaceFormat::Png));
        if let Some(bytes) = small {
            return Ok((FaceFormat::Png, bytes));
        }
    }
    Ok((face.format, read_face_file(&face.file)?))
}

/// A `data:` URL the webview can draw for `face`: its small variant when it
/// has one, else the file itself; an SVG is always cleaned first.
pub fn display_source(face: &LoadedFace) -> Result<String, String> {
    let (mime, bytes) = match face.format {
        FaceFormat::Svg => {
            let text = String::from_utf8(read_face_file(&face.file)?)
                .map_err(|_| "not UTF-8 text".to_string())?;
            ("image/svg+xml", svg::sanitize(&text)?.svg.into_bytes())
        }
        FaceFormat::Png | FaceFormat::Webp => {
            let small = face
                .small
                .as_deref()
                .and_then(|p| read_face_file(p).ok())
                .filter(|b| FaceFormat::sniff(b) == Some(FaceFormat::Png));
            match small {
                Some(bytes) => ("image/png", bytes),
                None => (face.format.mime(), read_face_file(&face.file)?),
            }
        }
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// What to add.
#[derive(Debug, Clone)]
pub struct AddRequest {
    pub pack: String,
    pub id: String,
    /// The image: a path inside the pack folder (used where it is), or any
    /// other path (copied in as `<id>.<ext>`). A relative path is read from the
    /// pack folder.
    pub file: PathBuf,
    pub label: Option<String>,
    /// The pack's display name, for a new pack.
    pub pack_name: Option<String>,
}

/// What an add did.
#[derive(Debug, Clone, PartialEq)]
pub struct AddOutcome {
    pub reference: String,
    pub pack_dir: PathBuf,
    pub file: PathBuf,
    pub small_file: Option<PathBuf>,
    pub manifest: PathBuf,
    /// A face with this id was already listed and is now replaced.
    pub replaced: bool,
    pub warnings: Vec<String>,
}

/// Validate a face and list it in `avatars_dir/<pack>/pack.yaml`, writing its
/// small variant (raster) or its cleaned copy (SVG) beside it. Nothing is
/// written unless every check passes.
pub fn add_face(avatars_dir: &Path, req: &AddRequest) -> Result<AddOutcome, String> {
    add_face_for(avatars_dir, req, &Default::default())
}

pub(crate) fn add_face_for(
    avatars_dir: &Path,
    req: &AddRequest,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<AddOutcome, String> {
    let pack = req.pack.trim();
    let id = req.id.trim();
    if !is_valid_id(pack, PACK_ID_MAX) {
        return Err(format!(
            "pack `{pack}`: use lowercase letters, digits and dashes, at most {PACK_ID_MAX} characters"
        ));
    }
    if pack.starts_with(BUILTIN_PREFIX) {
        return Err(format!(
            "pack names starting `{BUILTIN_PREFIX}` belong to the built-in packs"
        ));
    }
    if !is_valid_id(id, FACE_ID_MAX) {
        return Err(format!(
            "id `{id}`: use lowercase letters, digits and dashes, at most {FACE_ID_MAX} characters"
        ));
    }
    let pack_dir = avatars_dir.join(pack);
    let source = if req.file.is_absolute() {
        req.file.clone()
    } else {
        pack_dir.join(&req.file)
    };
    if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        scope.existing(&source)?;
    }
    let source_name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| "`file` names no file".to_string())?;
    if source_name == MANIFEST_FILE || source_name.ends_with(SMALL_SUFFIX) {
        return Err(format!("`{source_name}` is not a face image"));
    }
    let bytes = read_face_file(&source)?;
    let format = format_of(&source_name, &bytes)?;
    let mut warnings = Vec::new();

    // Used where it is when it already sits in the pack folder; copied in otherwise.
    let in_place = match (source.parent(), std::fs::canonicalize(&pack_dir)) {
        (Some(parent), Ok(pack_real)) => std::fs::canonicalize(parent)
            .map(|p| p == pack_real)
            .unwrap_or(false),
        _ => false,
    };
    let file_name = if in_place {
        source_name.clone()
    } else {
        format!("{id}.{}", format.extension())
    };
    let target = pack_dir.join(&file_name);

    let (face_bytes, small_bytes) = match format {
        FaceFormat::Png | FaceFormat::Webp => {
            let image = raster::decode(format, &bytes)?;
            if !is_square_ish(image.width as f64, image.height as f64) {
                return Err(not_square(image.width as f64, image.height as f64));
            }
            if image.width.min(image.height) < SMALL_SIDE {
                warnings.push(format!(
                    "{}×{}px is small; it may look soft in the list (at least {SMALL_SIDE}px is best)",
                    image.width, image.height
                ));
            }
            let small = raster::encode_png(&raster::small_square(&image, SMALL_SIDE))?;
            (bytes, Some(small))
        }
        FaceFormat::Svg => {
            let text = String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())?;
            let cleaned = svg::sanitize(&text)?;
            match cleaned.size {
                Some((w, h)) if !is_square_ish(w, h) => return Err(not_square(w, h)),
                Some(_) => {}
                None => warnings.push(
                    "the SVG has no viewBox or size; give it a square viewBox so it scales cleanly"
                        .to_string(),
                ),
            }
            if cleaned.changed() {
                warnings.push(format!(
                    "removed from the SVG: {}",
                    cleaned.removed.join(", ")
                ));
                (cleaned.svg.into_bytes(), None)
            } else {
                (text.into_bytes(), None)
            }
        }
    };

    let manifest_path = pack_dir.join(MANIFEST_FILE);
    let mut manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => Manifest::parse(&text).map_err(|e| {
            format!(
                "{MANIFEST_FILE} in `{pack}` is unreadable ({e}); fix or delete it, then add again"
            )
        })?,
        Err(_) => Manifest::default(),
    };
    let display_name = req
        .pack_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| title_case(pack));
    manifest.set_name_if_missing(&display_name);
    let label = req
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string);
    let replaced = manifest.upsert(&FaceEntry {
        id: id.to_string(),
        file: file_name.clone(),
        label,
    });
    if replaced {
        warnings.push(format!(
            "replaced the face `{id}` that was already in `{pack}`"
        ));
    }
    let yaml = manifest.to_yaml()?;

    std::fs::create_dir_all(&pack_dir)
        .map_err(|e| format!("cannot create {}: {e}", pack_dir.display()))?;
    if std::fs::read(&target).ok().as_deref() != Some(face_bytes.as_slice()) {
        write_atomic(&target, &face_bytes)?;
    }
    let small_file = match small_bytes {
        Some(small) => {
            let path = small_path(&target);
            write_atomic(&path, &small)?;
            Some(path)
        }
        None => None,
    };
    write_atomic(&manifest_path, yaml.as_bytes())?;

    Ok(AddOutcome {
        reference: face_ref(pack, id),
        pack_dir,
        file: target,
        small_file,
        manifest: manifest_path,
        replaced,
        warnings,
    })
}

/// The suffix of the hidden folder a share copies a pack into before it is
/// published: `.<pack><SHARING>-<pid>`.
const SHARING: &str = ".sharing";

/// Copy the pack `pack` from `from_dir` (the user's own `avatars/` for the org)
/// into `to_dir` (the team's `avatars/`), so every teammate gets it with the
/// team's content and its `pack:id` faces resolve to the team's copy. The
/// personal pack stays, for the org's other teams; in this team it is listed
/// once, as the team's ([`discover`]).
///
/// Refused, changing nothing, when the team already has a pack with that id or
/// the pack holds a link (a face is only ever read from a real file in its
/// pack). The pack is copied under a hidden name and published with a rename
/// that never replaces anything, so a failed share leaves no pack behind, and
/// one that lost a race to a same-named pack leaves that pack as it was. A
/// hidden copy a crashed share left is swept first. Returns the team's copy.
pub fn share_pack(from_dir: &Path, to_dir: &Path, pack: &str) -> Result<PathBuf, String> {
    let pack = pack.trim();
    if !is_valid_id(pack, PACK_ID_MAX) || pack.starts_with(BUILTIN_PREFIX) {
        return Err(format!("`{pack}` is not one of your packs"));
    }
    let source = from_dir.join(pack);
    let is_dir = std::fs::symlink_metadata(&source).is_ok_and(|m| m.is_dir());
    if !is_dir || !source.join(MANIFEST_FILE).is_file() {
        return Err(format!("you have no pack `{pack}` to share"));
    }
    if let Some(link) = first_link(&source).map_err(|e| format!("cannot read `{pack}`: {e}"))? {
        return Err(format!(
            "`{pack}` holds a link ({}); replace it with the file itself and share again",
            link.display()
        ));
    }
    let target = to_dir.join(pack);
    let taken = || {
        format!(
            "the team already has a pack named `{pack}`; rename your pack's folder and share it again"
        )
    };
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(taken());
    }
    std::fs::create_dir_all(to_dir)
        .map_err(|e| format!("cannot create {}: {e}", to_dir.display()))?;
    sweep_stale_shares(to_dir, pack);
    let staging = to_dir.join(format!(".{pack}{SHARING}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let published = copy_pack_tree(&source, &staging)
        .map_err(|e| format!("couldn't share `{pack}`; nothing was changed: {e}"))
        .and_then(|()| {
            publish_new(&staging, &target).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => taken(),
                _ => format!("couldn't share `{pack}`; nothing was changed: {e}"),
            })
        });
    if published.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    published.map(|()| target)
}

/// The first link anywhere in `dir`, if any.
fn first_link(dir: &Path) -> std::io::Result<Option<PathBuf>> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Ok(Some(entry.path()));
        }
        if kind.is_dir() {
            if let Some(link) = first_link(&entry.path())? {
                return Ok(Some(link));
            }
        }
    }
    Ok(None)
}

/// Remove the hidden copies of `pack` that a share interrupted part-way left
/// in `to_dir`, so none lingers to be committed with the team's content.
fn sweep_stale_shares(to_dir: &Path, pack: &str) {
    let prefix = format!(".{pack}{SHARING}-");
    let Ok(entries) = std::fs::read_dir(to_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Copy a pack folder's files and subfolders (the caller has refused links).
fn copy_pack_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_pack_tree(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

/// Rename `staged` to `destination` only when nothing is there, not even an
/// empty folder made since it was checked.
pub(super) fn publish_new(staged: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            staged,
            rustix::fs::CWD,
            destination,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
    }
    // Windows refuses to rename over an existing folder.
    #[cfg(windows)]
    {
        std::fs::rename(staged, destination)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::content_engine::atomic_io::write_tmp_fsync_rename(path, bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// `office-legends` → `Office Legends`.
fn title_case(id: &str) -> String {
    id.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
