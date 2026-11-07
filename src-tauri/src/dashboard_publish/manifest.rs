// SPDX-License-Identifier: MIT
//! P1: the `dashboard.yml` manifest. Parsing either returns a fully valid
//! [`Manifest`] or a typed [`ManifestError`]; there is no partial result.
//! Every refusal here is decided before a byte of a file is read for upload
//! (SW-PB-22a AC-1, DR-4, registry F4 / F7 / E9 / K8 / K9).

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::catalog::{self, QueryDef, StreamDef, CATALOG, STREAMS};

/// The manifest's file name inside a dashboard folder.
pub const MANIFEST_FILE: &str = "dashboard.yml";

/// The longest title the server accepts for R1, after trimming.
pub const MAX_TITLE_CHARS: usize = 200;

/// K8: at most 50 files per version.
pub const MAX_FILES: usize = 50;

/// K9: the content type for an allowlisted path, or `None` when the extension
/// is not allowed. No SVG: script inside SVG would run on the dash origin.
pub fn content_type_for(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "html" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    })
}

/// F4: `[a-z0-9-]{3,40}`.
pub fn valid_slug(slug: &str) -> bool {
    (3..=40).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `publish.state` in the manifest: `draft` or `published` only (P1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishState {
    Draft,
    Published,
}

impl PublishState {
    pub fn as_str(self) -> &'static str {
        match self {
            PublishState::Draft => "draft",
            PublishState::Published => "published",
        }
    }
}

/// `publish.visibility` in the manifest (P1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Team,
}

impl Visibility {
    pub fn parse(value: &str) -> Option<Visibility> {
        match value {
            "private" => Some(Visibility::Private),
            "team" => Some(Visibility::Team),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Team => "team",
        }
    }
}

/// The most names a `live.queries` list may hold (R1's limit, SW-DP-4 AC-6).
pub const MAX_LIVE_QUERIES: usize = 32;

/// P1 `live.queries` (SW-DP-4b): every catalog query (`all`, now and later), or
/// a list of catalog names. The list is kept sorted and without repeats, so
/// its canonical form and sha do not depend on the order it was written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveQueries {
    All,
    List(Vec<String>),
}

impl LiveQueries {
    /// A list in canonical form: sorted, each name once.
    pub fn list(names: impl IntoIterator<Item = String>) -> LiveQueries {
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        names.dedup();
        LiveQueries::List(names)
    }

    /// True when `query` may be answered under this block.
    pub fn allows(&self, query: &str) -> bool {
        match self {
            LiveQueries::All => true,
            LiveQueries::List(names) => names.iter().any(|n| n == query),
        }
    }

    /// The canonical sha of the block (SHA-256 hex of `"all"` or of the sorted
    /// JSON list). The block saved on the record is tied to it (P2 `live_approved.sha`).
    pub fn sha(&self) -> String {
        use sha2::{Digest, Sha256};
        let canonical = serde_json::to_string(self).expect("live queries serialize");
        Sha256::digest(canonical.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// `"all"` or a JSON list of names: the R1 `live_queries` form and the P2 form.
impl Serialize for LiveQueries {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            LiveQueries::All => s.serialize_str("all"),
            LiveQueries::List(names) => names.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for LiveQueries {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            List(Vec<String>),
        }
        match Raw::deserialize(d)? {
            Raw::Word(w) if w == "all" => Ok(LiveQueries::All),
            Raw::Word(w) => Err(serde::de::Error::custom(format!(
                "live queries must be \"all\" or a list, not {w:?}"
            ))),
            Raw::List(names) => Ok(LiveQueries::list(names)),
        }
    }
}

/// P1 `live.fields` (SW-DP-5a review S1): per query, the fields the dashboard
/// shares beyond the query's default set (for `queryContext`, the frontmatter
/// keys it shares). Sorted and without repeats, so the block's sha is stable.
pub type LiveFields = BTreeMap<String, Vec<String>>;

/// Literal or scoped selector pins, by query then argument name.
pub type LiveArgs = BTreeMap<String, BTreeMap<String, Vec<Pin>>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Pin {
    String(String),
    Integer(i64),
    Unsigned(u64),
    Selector(Selector),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Selector {
    Epic { epic: String },
    Team { team: String },
}

impl Pin {
    fn allows(
        &self,
        value: &serde_json::Value,
        resolve: &mut impl FnMut(&Selector, &str) -> bool,
    ) -> bool {
        match self {
            Self::String(s) => value.as_str() == Some(s),
            Self::Integer(n) => value.as_i64() == Some(*n),
            Self::Unsigned(n) => value.as_u64() == Some(*n),
            Self::Selector(selector) => value.as_str().is_some_and(|id| resolve(selector, id)),
        }
    }
}

/// Every pinned argument must be present and match a literal or a resolved selector. A list
/// argument (`catalog::LIST_ARGS`) must be a non-empty list whose every item matches.
pub fn pin_allows(
    pins: &LiveArgs,
    query: &str,
    value: &serde_json::Value,
    mut resolve: impl FnMut(&Selector, &str) -> bool,
) -> bool {
    pins.get(query).is_none_or(|args| {
        args.iter().all(|(key, values)| {
            let list = catalog::LIST_ARGS.contains(&(query, String::as_str(key)));
            serde_json::Value::get(value, key).is_some_and(|v| match v.as_array() {
                Some(items) if list => {
                    !<[serde_json::Value]>::is_empty(items)
                        && Iterator::all(&mut items.iter(), |item| {
                            values.iter().any(|pin| Pin::allows(pin, item, &mut resolve))
                        })
                }
                _ => !list && values.iter().any(|pin| Pin::allows(pin, v, &mut resolve)),
            })
        })
    })
}

/// Scope-dependent validation at publish and watcher follow, after shape validation.
pub fn validate_pin_scope(
    pins: &LiveArgs,
    scope: Option<&super::state::Scope>,
) -> Result<(), ManifestError> {
    for args in pins.values() {
        for values in args.values() {
            for pin in values {
                if let Pin::Selector(selector) = pin {
                    let scope = scope.ok_or_else(|| {
                        ManifestError::BadLiveArgs(
                            "selectors need the dashboard's saved team scope".into(),
                        )
                    })?;
                    if let Selector::Team { team } = selector {
                        if team != &scope.team {
                            return Err(ManifestError::BadLiveArgs(format!(
                                "team selector {team} must equal the dashboard's saved team {}",
                                scope.team
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// The missing viewer-required pin, shared by answer and share checks.
pub fn missing_pin(queries: &LiveQueries, pins: &LiveArgs) -> Option<(&'static str, &'static str)> {
    catalog::PIN_REQUIRED_ARGS
        .iter()
        .copied()
        .find(|(query, arg)| {
            queries.allows(query)
                && !pins
                    .get(*query)
                    .and_then(|p| p.get(*arg))
                    .is_some_and(|v| !v.is_empty())
        })
}

/// P1 `live.embeds` (SV-INP-29): what content embedded in the page (an HTML
/// message in a sandboxed frame) may ask for through the page's live pipe. The
/// page's publisher grants it; the content never does. Every name is declared
/// in the page's own `live` block and every field is one the page itself
/// shares, so an embed never sees more than the page. `fields` is resolved at
/// parse time: one list per embed query, the query's defaults when the
/// manifest names none. Empty means no embed access at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveEmbeds {
    pub queries: Vec<String>,
    pub streams: Vec<String>,
    pub fields: LiveFields,
    /// Who sees live messages run (`live.embeds.run`). `None` (the default)
    /// keeps every message scriptless for everyone, whatever else is granted.
    #[serde(default)]
    pub run: EmbedRun,
}

/// `live.embeds.run`: for whom a message's own scripts run on this dashboard.
/// With `Owner` or `Everyone`, agent-authored scripts run on those people's
/// devices with the same trust as on the Mac: they can read what `live.embeds`
/// grants and, through WebRTC, reveal the device's IP. `Everyone` means every
/// invited viewer; the publisher vouches for their own agents' messages.
/// Viewers never act in either mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbedRun {
    #[default]
    None,
    Owner,
    Everyone,
}

impl LiveEmbeds {
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty() && self.streams.is_empty() && self.run == EmbedRun::None
    }
}

/// The most names a `live.streams` list may hold (R1's limit, SW-DP-20).
pub const MAX_LIVE_STREAMS: usize = 16;

/// The canonical sha of a `live` block: its queries alone (as in SW-DP-4b)
/// when it names no fields and no streams; else the queries, the fields and,
/// when there are any, the streams (SW-DP-20) together. A block saved before
/// streams existed keeps its sha. SW-DP-23: `control: owner` joins the block
/// only when it is set, so a block without it keeps the sha it had before.
pub fn live_sha(
    queries: &LiveQueries,
    fields: &LiveFields,
    streams: &[String],
    control: bool,
) -> String {
    if fields.is_empty() && streams.is_empty() && !control {
        return queries.sha();
    }
    use sha2::{Digest, Sha256};
    let block = if control {
        serde_json::json!({"queries": queries, "fields": fields, "streams": streams, "control": "owner"})
    } else if streams.is_empty() {
        serde_json::json!({"queries": queries, "fields": fields})
    } else {
        serde_json::json!({"queries": queries, "fields": fields, "streams": streams})
    };
    let canonical = serde_json::to_string(&block).expect("live block serializes");
    Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Chrome {
    /// The hosted viewer draws nothing over the page (SV-INP-27); state reaches the page through the SDK.
    #[default]
    None,
    Full,
    Minimal,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ThemeColor {
    pub light: String,
    pub dark: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Install {
    pub name: String,
    pub short_name: Option<String>,
    pub icon: String,
    pub theme_color: Option<ThemeColor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Presentation {
    pub install: Option<Install>,
    pub chrome: Chrome,
}

/// A manifest that passed every check in AC-1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub slug: String,
    pub title: String,
    pub state: PublishState,
    pub visibility: Visibility,
    pub retain: u8,
    /// The allowlist, in the order the manifest lists it.
    pub files: Vec<String>,
    /// P1 `live.queries`. `None`: no `live` block, so no live and no presence (DP-D5).
    pub live: Option<LiveQueries>,
    /// P1 `live.fields`. Empty without a `live` block.
    pub live_fields: LiveFields,
    pub live_args: LiveArgs,
    /// P1 `live.streams` (SW-DP-20): names from `STREAMS`, sorted, each once.
    /// Empty without a `live` block or without the key.
    pub live_streams: Vec<String>,
    /// P1 `live.control: owner` (SW-DP-23, DP-D48): the dashboard's owner may
    /// type into its terminals. Needs `terminal` in `live.streams`.
    pub live_control: bool,
    /// P1 `live.embeds` (SV-INP-29). Empty without the key.
    pub live_embeds: LiveEmbeds,
    pub actions: super::actions::Actions,
    pub session_days: u16,
    pub install: Option<Install>,
    pub chrome: Chrome,
}

impl Manifest {
    pub fn presentation(&self) -> Option<Presentation> {
        if self.install.is_none() && self.chrome == Chrome::None {
            return None;
        }
        Some(Presentation {
            install: self.install.clone(),
            chrome: self.chrome,
        })
    }

    /// The sha the saved live block is tied to, when there is a `live` block.
    pub fn live_sha(&self) -> Option<String> {
        self.live.as_ref().map(|q| {
            let base = live_sha(q, &self.live_fields, &self.live_streams, self.live_control);
            let base = if self.live_args.is_empty() {
                base
            } else {
                use sha2::{Digest, Sha256};
                let text = serde_json::to_vec(&(base, &self.live_args)).expect("pins serialize");
                Sha256::digest(text)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect()
            };
            if self.actions.is_empty() && self.live_embeds.is_empty() {
                return base;
            }
            use sha2::{Digest, Sha256};
            // Embeds join only when set, so a manifest without them keeps its sha.
            let text = if self.live_embeds.is_empty() {
                serde_json::to_vec(&(base, &self.actions))
            } else {
                serde_json::to_vec(&(base, &self.actions, &self.live_embeds))
            }
            .expect("action manifest serializes");
            Sha256::digest(text)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        })
    }
}

/// Why a manifest was refused. Each variant names what to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Unreadable(String),
    Yaml(String),
    MissingField(&'static str),
    BadSlug(String),
    UnknownValue {
        field: &'static str,
        value: String,
    },
    EmptyFiles,
    TooManyFiles(usize),
    BadPath {
        path: String,
        reason: &'static str,
    },
    PathOutsideFolder(String),
    Symlink(String),
    /// The file has more than one name (a hard link), so it may be a file that
    /// is not listed, uploaded under a listed name.
    HardLink(String),
    MissingFile(String),
    TypeNotAllowed(String),
    /// The title, trimmed, must be 1 to 200 characters (the server's rule for R1).
    BadTitle,
    /// `live.queries` names a query that is not in the catalog (P14).
    UnknownLiveQuery(String),
    /// The `live` block is not `queries: all` or `queries: [names]`.
    BadLive(&'static str),
    /// A `live.fields` entry the query does not allow.
    BadLiveField(String),
    BadLiveArgs(String),
    /// `live.streams` names a stream that is not in `STREAMS` (P17).
    UnknownLiveStream(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Unreadable(m) => write!(f, "manifest unreadable: {m}"),
            ManifestError::Yaml(m) => write!(f, "manifest is not valid YAML: {m}"),
            ManifestError::MissingField(field) => write!(f, "manifest is missing {field}"),
            ManifestError::BadSlug(slug) => {
                write!(
                    f,
                    "slug {slug:?} must match [a-z0-9-] and be 3 to 40 characters"
                )
            }
            ManifestError::UnknownValue { field, value } => {
                write!(f, "{field} has unknown value {value:?}")
            }
            ManifestError::EmptyFiles => write!(f, "publish.files must list at least one file"),
            ManifestError::TooManyFiles(n) => {
                write!(f, "publish.files lists {n} files; the limit is {MAX_FILES}")
            }
            ManifestError::BadPath { path, reason } => write!(f, "file {path:?}: {reason}"),
            ManifestError::PathOutsideFolder(path) => {
                write!(f, "file {path:?} resolves outside the dashboard folder")
            }
            ManifestError::Symlink(path) => {
                write!(
                    f,
                    "file {path:?} goes through a symlink; symlinks are not allowed in a dashboard"
                )
            }
            ManifestError::HardLink(path) => write!(
                f,
                "file {path:?} is a hard link (it has more than one name); not allowed in a dashboard"
            ),
            ManifestError::MissingFile(path) => write!(f, "file {path:?} does not exist"),
            ManifestError::TypeNotAllowed(path) => {
                write!(f, "file {path:?} has a type that is not allowed to publish")
            }
            ManifestError::BadTitle => {
                write!(f, "title must be 1 to {MAX_TITLE_CHARS} characters, not counting spaces at either end")
            }
            ManifestError::UnknownLiveQuery(name) => write!(f, "unknown live query {name}"),
            ManifestError::BadLive(why) => write!(f, "live: {why}"),
            ManifestError::BadLiveArgs(why) => write!(f, "live.args: {why}"),
            ManifestError::BadLiveField(why) => write!(f, "live.fields: {why}"),
            ManifestError::UnknownLiveStream(name) => write!(f, "unknown live stream {name}"),
        }
    }
}

#[derive(Deserialize)]
struct RawManifest {
    install: Option<serde_yaml::Value>,
    chrome: Option<String>,
    slug: Option<String>,
    title: Option<String>,
    publish: Option<RawPublish>,
    live: Option<RawLive>,
    viewers: Option<RawViewers>,
    #[serde(default)]
    actions: super::actions::Actions,
}

#[derive(Deserialize)]
struct RawViewers {
    session_days: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLive {
    args: Option<serde_yaml::Value>,
    queries: Option<serde_yaml::Value>,
    fields: Option<BTreeMap<String, Vec<String>>>,
    streams: Option<serde_yaml::Value>,
    control: Option<serde_yaml::Value>,
    embeds: Option<RawEmbeds>,
}

/// `live.embeds` as written. An unknown key is refused, so a misspelt grant
/// cannot silently grant nothing (or a typo'd narrowing grant more).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEmbeds {
    queries: Option<Vec<String>>,
    streams: Option<Vec<String>>,
    fields: Option<BTreeMap<String, Vec<String>>>,
    run: Option<String>,
}

/// A parsed `live` block.
struct LiveBlock {
    args: LiveArgs,
    queries: LiveQueries,
    fields: LiveFields,
    streams: Vec<String>,
    control: bool,
    embeds: LiveEmbeds,
}

/// A `live` block given as a value (`create_dashboard`): queries, fields,
/// streams, owner control and query pins.
pub type LiveValue = (LiveQueries, LiveFields, Vec<String>, bool, LiveArgs);

fn default_retain() -> serde_yaml::Value {
    serde_yaml::Value::Number(5.into())
}

#[derive(Deserialize)]
struct RawPublish {
    #[serde(default = "default_retain")]
    retain: serde_yaml::Value,
    state: Option<String>,
    visibility: Option<String>,
    files: Option<Vec<String>>,
}

/// Read and validate `<folder>/dashboard.yml`.
pub fn load(folder: &Path) -> Result<Manifest, ManifestError> {
    load_with(folder, CATALOG)
}

/// `load` against a given query catalog (the live link's, which the tests set).
pub fn load_with(folder: &Path, catalog: &[QueryDef]) -> Result<Manifest, ManifestError> {
    let text = fs::read_to_string(folder.join(MANIFEST_FILE))
        .map_err(|e| ManifestError::Unreadable(e.to_string()))?;
    parse_with(&text, folder, catalog)
}

/// Read only `slug` from `<folder>/dashboard.yml`, for takedown. The allowlist
/// and the other fields are not checked, so a bad file cannot block unpublish.
/// The slug is still F4-checked, because it names the local record file.
pub fn load_slug(folder: &Path) -> Result<String, ManifestError> {
    #[derive(Deserialize)]
    struct SlugOnly {
        slug: Option<String>,
    }
    let text = fs::read_to_string(folder.join(MANIFEST_FILE))
        .map_err(|e| ManifestError::Unreadable(e.to_string()))?;
    let raw: SlugOnly =
        serde_yaml::from_str(&text).map_err(|e| ManifestError::Yaml(e.to_string()))?;
    let slug = raw.slug.ok_or(ManifestError::MissingField("slug"))?;
    if !valid_slug(&slug) {
        return Err(ManifestError::BadSlug(slug));
    }
    Ok(slug)
}

/// Normalise one allowlist entry. The entry must be a plain path: `/`-separated
/// segments that are each a real name. Refused: an empty entry, a backslash,
/// an empty segment (`a//b`, a leading or trailing `/`), `.` and `..`.
/// Absolute paths and prefixes are refused by the same rules (their first
/// segment is empty or a drive name). Returns the entry unchanged when it passes,
/// so the stored form is the one every later check compares against.
pub fn normalize_rel(rel: &str) -> Result<String, &'static str> {
    if rel.is_empty() {
        return Err("must not be empty");
    }
    if rel.contains('\\') {
        return Err("must use \"/\" and no backslash");
    }
    for segment in rel.split('/') {
        match segment {
            "" => return Err("must not have an empty segment, a leading or a trailing \"/\""),
            "." | ".." => return Err("must not contain \".\" or \"..\""),
            _ => {}
        }
    }
    Ok(rel.to_string())
}

/// Refuse a symlink at any segment of `rel` under `folder`. A link is a second
/// name for some bytes, so the filter and the allowlist could be bypassed through
/// it. Also used by the payload reader, so a link swapped in after validation is
/// refused at read time too.
pub fn reject_links(folder: &Path, rel: &str) -> Result<(), ManifestError> {
    let mut current = folder.to_path_buf();
    for segment in rel.split('/') {
        current.push(segment);
        let meta = fs::symlink_metadata(&current).map_err(|e| missing_or_unreadable(e, rel))?;
        if meta.file_type().is_symlink() {
            return Err(ManifestError::Symlink(rel.to_string()));
        }
    }
    Ok(())
}

/// Validate manifest text against the folder it describes (file checks need
/// the folder, so a bare string cannot be checked alone).
pub fn parse(text: &str, folder: &Path) -> Result<Manifest, ManifestError> {
    parse_with(text, folder, CATALOG)
}

/// `parse` against a given query catalog (the tests pass a fixture catalog).
pub fn parse_with(
    text: &str,
    folder: &Path,
    catalog: &[QueryDef],
) -> Result<Manifest, ManifestError> {
    let raw: RawManifest =
        serde_yaml::from_str(text).map_err(|e| ManifestError::Yaml(e.to_string()))?;

    let slug = raw.slug.ok_or(ManifestError::MissingField("slug"))?;
    if !valid_slug(&slug) {
        return Err(ManifestError::BadSlug(slug));
    }
    let title = raw.title.ok_or(ManifestError::MissingField("title"))?;
    let title_chars = title.trim().chars().count();
    if title_chars == 0 || title_chars > MAX_TITLE_CHARS {
        return Err(ManifestError::BadTitle);
    }
    let publish = raw.publish.ok_or(ManifestError::MissingField("publish"))?;

    let retain = match publish.retain {
        serde_yaml::Value::String(ref value) if value == "current" => 1,
        ref value => value
            .as_u64()
            .filter(|n| (1..=5).contains(n))
            .map(|n| n as u8)
            .ok_or_else(|| ManifestError::UnknownValue {
                field: "publish.retain",
                value: format!("{value:?}"),
            })?,
    };

    let state_raw = publish
        .state
        .ok_or(ManifestError::MissingField("publish.state"))?;
    let state = match state_raw.as_str() {
        "draft" => PublishState::Draft,
        "published" => PublishState::Published,
        _ => {
            return Err(ManifestError::UnknownValue {
                field: "publish.state",
                value: state_raw,
            })
        }
    };

    let visibility_raw = publish
        .visibility
        .ok_or(ManifestError::MissingField("publish.visibility"))?;
    let visibility = Visibility::parse(&visibility_raw).ok_or(ManifestError::UnknownValue {
        field: "publish.visibility",
        value: visibility_raw,
    })?;

    let files = publish
        .files
        .ok_or(ManifestError::MissingField("publish.files"))?;
    if files.is_empty() {
        return Err(ManifestError::EmptyFiles);
    }
    if files.len() > MAX_FILES {
        return Err(ManifestError::TooManyFiles(files.len()));
    }
    let folder_canon =
        fs::canonicalize(folder).map_err(|e| ManifestError::Unreadable(e.to_string()))?;
    let mut seen = HashSet::new();
    for rel in &files {
        check_path(folder, &folder_canon, rel)?;
        // Case-folded, because on the default macOS filesystem `Data/Report.json`
        // and `data/report.json` are the same file.
        if !seen.insert(rel.to_ascii_lowercase()) {
            return Err(ManifestError::BadPath {
                path: rel.clone(),
                reason: "is listed more than once",
            });
        }
    }

    let session_days = raw.viewers.and_then(|v| v.session_days).unwrap_or(90);
    if !(1..=365).contains(&session_days) {
        return Err(ManifestError::Yaml(
            "viewers.session_days must be an integer from 1 to 365".into(),
        ));
    }
    let chrome = match raw.chrome.as_deref().unwrap_or("none") {
        "none" => Chrome::None,
        "full" => Chrome::Full,
        "minimal" => Chrome::Minimal,
        value => {
            return Err(ManifestError::UnknownValue {
                field: "chrome",
                value: value.into(),
            })
        }
    };
    let install = raw
        .install
        .map(|value| -> Result<Install, ManifestError> {
            let install: Install = serde_yaml::from_value(value)
                .map_err(|e| ManifestError::Yaml(format!("install: {e}")))?;
            if install.name.trim().is_empty() || install.name.chars().count() > 64 {
                return Err(ManifestError::Yaml(
                    "install.name must be 1 to 64 characters".into(),
                ));
            }
            if install
                .short_name
                .as_ref()
                .is_some_and(|s| s.chars().count() > 30)
            {
                return Err(ManifestError::Yaml(
                    "install.short_name must be at most 30 characters".into(),
                ));
            }
            if !install.icon.ends_with(".png") || !files.contains(&install.icon) {
                return Err(ManifestError::Yaml(
                    "install.icon must be a .png path listed in publish.files".into(),
                ));
            }
            if let Some(theme) = &install.theme_color {
                for (field, value) in [
                    ("install.theme_color.light", &theme.light),
                    ("install.theme_color.dark", &theme.dark),
                ] {
                    if value.len() != 7
                        || !value.starts_with('#')
                        || !value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
                    {
                        return Err(ManifestError::UnknownValue {
                            field,
                            value: value.clone(),
                        });
                    }
                }
            }
            Ok(install)
        })
        .transpose()?;
    super::actions::validate(&raw.actions).map_err(ManifestError::Yaml)?;
    let (mut live, live_fields, live_streams, live_control, live_embeds, live_args) = match raw.live
    {
        None => (
            None,
            LiveFields::new(),
            Vec::new(),
            false,
            LiveEmbeds::default(),
            LiveArgs::new(),
        ),
        Some(live) => {
            let block = parse_live(live, catalog, STREAMS)?;
            (
                Some(block.queries),
                block.fields,
                block.streams,
                block.control,
                block.embeds,
                block.args,
            )
        }
    };

    if !raw.actions.is_empty() && live.is_none() {
        live = Some(LiveQueries::List(vec![]));
    }
    Ok(Manifest {
        slug,
        title,
        state,
        visibility,
        retain,
        files,
        live,
        live_fields,
        live_args,
        live_streams,
        live_control,
        live_embeds,
        actions: raw.actions,
        session_days,
        install,
        chrome,
    })
}

/// A `live` block given as a value rather than as manifest text (the
/// `create_dashboard` tool's input), checked by the same rules as the manifest,
/// streams included (SW-DP-21: the `workspaces` template declares them).
pub fn parse_live_value(
    value: serde_yaml::Value,
    catalog: &[QueryDef],
) -> Result<LiveValue, ManifestError> {
    let raw: RawLive =
        serde_yaml::from_value(value).map_err(|e| ManifestError::Yaml(format!("live: {e}")))?;
    let block = parse_live(raw, catalog, STREAMS)?;
    Ok((
        block.queries,
        block.fields,
        block.streams,
        block.control,
        block.args,
    ))
}

/// P1 `live` (SW-DP-4 AC-7): `queries: all`, `queries: []` (presence only) or a
/// list of catalog names. A name outside the catalog is refused. SW-DP-20:
/// `streams: [names]` from `streams`; a block that declares streams may leave
/// `queries` out (no queries).
fn parse_live(
    live: RawLive,
    catalog: &[QueryDef],
    streams: &[StreamDef],
) -> Result<LiveBlock, ManifestError> {
    let raw_fields = live.fields.unwrap_or_default();
    let declared_streams = match live.streams {
        None => Vec::new(),
        Some(value) => parse_live_streams(value, streams)?,
    };
    // SW-DP-23 (DP-D48): `control: owner` lets the dashboard's owner type into
    // its terminals; only with the `terminal` stream declared.
    let control = match live.control {
        None => false,
        Some(serde_yaml::Value::String(word)) if word == "owner" => {
            if !declared_streams.iter().any(|s| s == "terminal") {
                return Err(ManifestError::BadLive(
                    "control: owner needs the terminal stream in live.streams",
                ));
            }
            true
        }
        Some(_) => return Err(ManifestError::BadLive("control must be owner")),
    };
    let queries = match live.queries {
        Some(queries) => parse_live_queries(queries, catalog)?,
        None if !declared_streams.is_empty() => LiveQueries::List(Vec::new()),
        None => return Err(ManifestError::MissingField("live.queries")),
    };
    let mut fields = LiveFields::new();
    for (query, list) in raw_fields {
        let def = catalog::find(catalog, &query)
            .filter(|_| {
                queries.allows(&query)
                    || (query == "inbox" && declared_streams.iter().any(|s| s == "inbox"))
            })
            .ok_or_else(|| {
                ManifestError::BadLiveField(format!("{query} is not a declared query"))
            })?;
        if list.len() > MAX_LIVE_QUERIES {
            return Err(ManifestError::BadLiveField(format!(
                "{query} lists more than 32 fields"
            )));
        }
        for field in &list {
            if let Some(why) = catalog::field_refusal(def, field) {
                return Err(ManifestError::BadLiveField(format!(
                    "{query}: {field} {why}"
                )));
            }
        }
        let mut list = list;
        list.sort();
        list.dedup();
        fields.insert(query, list);
    }
    let mut args = LiveArgs::new();
    if let Some(raw) = live.args {
        let raw: BTreeMap<String, BTreeMap<String, Vec<Pin>>> =
            serde_yaml::from_value(raw).map_err(|e| ManifestError::BadLiveArgs(e.to_string()))?;
        for (query, pins) in raw {
            let def = catalog::find(catalog, &query)
                .filter(|_| queries.allows(&query))
                .ok_or_else(|| {
                    ManifestError::BadLiveArgs(format!("{query} is not a declared query"))
                })?;
            let mut normalized = BTreeMap::new();
            for (key, mut values) in pins {
                if !def.args.contains(&key.as_str()) {
                    return Err(ManifestError::BadLiveArgs(format!(
                        "{query}: unknown argument {key}"
                    )));
                }
                if values.is_empty() || values.len() > 64 {
                    return Err(ManifestError::BadLiveArgs(format!(
                        "{query}.{key} needs 1–64 literals or selectors"
                    )));
                }
                for pin in &values {
                    if let Pin::Selector(selector) = pin {
                        let token = match selector {
                            Selector::Epic { epic: s } | Selector::Team { team: s } => s,
                        };
                        if !catalog::SELECTOR_ARGS.contains(&(query.as_str(), key.as_str()))
                            || token.trim().is_empty()
                        {
                            return Err(ManifestError::BadLiveArgs(format!(
                                "{query}.{key}: selector is empty or not allowed on this argument"
                            )));
                        }
                    }
                }
                values
                    .sort_by_cached_key(|v| serde_json::to_string(v).expect("literal serializes"));
                values.dedup();
                normalized.insert(key, values);
            }
            if !normalized.is_empty() {
                args.insert(query, normalized);
            }
        }
    }
    let embeds = match live.embeds {
        None => LiveEmbeds::default(),
        Some(raw) => parse_live_embeds(raw, catalog, &queries, &fields, &declared_streams)?,
    };
    Ok(LiveBlock {
        args,
        queries,
        fields,
        streams: declared_streams,
        control,
        embeds,
    })
}

/// `live.embeds` against the page's own block (SV-INP-29): each query and
/// stream must be declared by the page, and each field one the page shares for
/// that query. A query with no field list gets its defaults, narrowed to what
/// the page shares.
fn parse_live_embeds(
    raw: RawEmbeds,
    catalog: &[QueryDef],
    queries: &LiveQueries,
    page_fields: &LiveFields,
    page_streams: &[String],
) -> Result<LiveEmbeds, ManifestError> {
    let mut names = raw.queries.unwrap_or_default();
    names.sort();
    names.dedup();
    let mut streams = raw.streams.unwrap_or_default();
    streams.sort();
    streams.dedup();
    if names.len() > MAX_LIVE_QUERIES || streams.len() > MAX_LIVE_STREAMS {
        return Err(ManifestError::BadLive("embeds lists too many names"));
    }
    for stream in &streams {
        if !page_streams.contains(stream) {
            return Err(ManifestError::BadLiveField(format!(
                "embeds.streams {stream} is not in live.streams"
            )));
        }
    }
    let run = match raw.run.as_deref() {
        None | Some("none") => EmbedRun::None,
        Some("owner") => EmbedRun::Owner,
        Some("everyone") => EmbedRun::Everyone,
        Some(_) => {
            return Err(ManifestError::BadLive(
                "embeds.run must be none, owner or everyone",
            ))
        }
    };
    if run != EmbedRun::None && names.is_empty() && streams.is_empty() {
        return Err(ManifestError::BadLive(
            "embeds.run needs at least one query or stream in embeds",
        ));
    }
    let mut named = raw.fields.unwrap_or_default();
    if let Some(query) = named.keys().find(|q| !names.contains(q)) {
        return Err(ManifestError::BadLiveField(format!(
            "embeds.fields {query} is not in embeds.queries"
        )));
    }
    let mut fields = LiveFields::new();
    for query in &names {
        let def = catalog::find(catalog, query)
            .filter(|_| {
                queries.allows(query) || (query == "inbox" && page_streams.iter().any(|s| s == "inbox"))
            })
            .ok_or_else(|| {
                ManifestError::BadLiveField(format!("embeds.queries {query} is not a declared query"))
            })?;
        let shared = catalog::effective_fields(def, page_fields.get(query));
        let mut list = match named.remove(query) {
            Some(list) => list,
            None => catalog::effective_fields(def, None)
                .into_iter()
                .filter(|f| shared.contains(f))
                .collect(),
        };
        if list.len() > MAX_LIVE_QUERIES {
            return Err(ManifestError::BadLiveField(format!(
                "embeds.fields {query} lists more than 32 fields"
            )));
        }
        if let Some(field) = list.iter().find(|f| !shared.contains(f)) {
            return Err(ManifestError::BadLiveField(format!(
                "embeds.fields {query}: {field} is not shared by the page (live.fields)"
            )));
        }
        list.sort();
        list.dedup();
        fields.insert(query.clone(), list);
    }
    Ok(LiveEmbeds {
        queries: names,
        streams,
        fields,
        run,
    })
}

/// `live.streams` alone (SW-DP-20): a list of `STREAMS` names, sorted, each once.
fn parse_live_streams(
    value: serde_yaml::Value,
    streams: &[StreamDef],
) -> Result<Vec<String>, ManifestError> {
    let serde_yaml::Value::Sequence(items) = value else {
        return Err(ManifestError::BadLive(
            "streams must be a list of stream names",
        ));
    };
    if items.len() > MAX_LIVE_STREAMS {
        return Err(ManifestError::BadLive("streams lists more than 16 names"));
    }
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        let serde_yaml::Value::String(name) = item else {
            return Err(ManifestError::BadLive("each stream name must be text"));
        };
        if catalog::find_stream(streams, &name).is_none() {
            return Err(ManifestError::UnknownLiveStream(name));
        }
        names.push(name);
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// `live.queries` alone.
fn parse_live_queries(
    queries: serde_yaml::Value,
    catalog: &[QueryDef],
) -> Result<LiveQueries, ManifestError> {
    let queries = match queries {
        serde_yaml::Value::String(word) if word == "all" => return Ok(LiveQueries::All),
        serde_yaml::Value::Sequence(items) => items,
        _ => {
            return Err(ManifestError::BadLive(
                "queries must be the word all or a list of query names",
            ))
        }
    };
    if queries.len() > MAX_LIVE_QUERIES {
        return Err(ManifestError::BadLive("queries lists more than 32 names"));
    }
    let mut names = Vec::with_capacity(queries.len());
    for item in queries {
        let serde_yaml::Value::String(name) = item else {
            return Err(ManifestError::BadLive("each query name must be text"));
        };
        if catalog::find(catalog, &name).is_none() {
            return Err(ManifestError::UnknownLiveQuery(name));
        }
        names.push(name);
    }
    Ok(LiveQueries::list(names))
}

/// F7 for one allowlist entry: a normal relative path (see `normalize_rel`), no
/// symlink at any segment, an allowed extension, and an existing regular file
/// that canonicalises inside the folder.
fn check_path(folder: &Path, folder_canon: &Path, rel: &str) -> Result<(), ManifestError> {
    normalize_rel(rel).map_err(|reason| ManifestError::BadPath {
        path: rel.to_string(),
        reason,
    })?;
    if content_type_for(rel).is_none() {
        return Err(ManifestError::TypeNotAllowed(rel.to_string()));
    }

    reject_links(folder, rel)?;
    let full = folder.join(rel);
    let canon = fs::canonicalize(&full).map_err(|e| missing_or_unreadable(e, rel))?;
    if !canon.starts_with(folder_canon) {
        return Err(ManifestError::PathOutsideFolder(rel.to_string()));
    }
    if !canon.is_file() {
        return Err(ManifestError::BadPath {
            path: rel.to_string(),
            reason: "is not a regular file",
        });
    }
    reject_hard_links(folder, rel)
}

/// Refuse a regular file that has more than one name. A hard link could
/// otherwise upload a file that is not listed (or lives outside the folder)
/// under a listed name. Checked when the manifest is validated and again when
/// the payload reads the file.
pub fn reject_hard_links(folder: &Path, rel: &str) -> Result<(), ManifestError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(folder.join(rel)).map_err(|e| missing_or_unreadable(e, rel))?;
        if meta.nlink() > 1 {
            return Err(ManifestError::HardLink(rel.to_string()));
        }
    }
    #[cfg(not(unix))]
    let _ = (folder, rel);
    Ok(())
}

fn missing_or_unreadable(e: io::Error, rel: &str) -> ManifestError {
    if e.kind() == io::ErrorKind::NotFound {
        ManifestError::MissingFile(rel.to_string())
    } else {
        ManifestError::Unreadable(format!("{rel}: {e}"))
    }
}

/// Rewrite `publish.state` (and `publish.visibility` when given) in
/// `<folder>/dashboard.yml`. Every other key is kept. Comments are not kept,
/// because the file is re-serialised. Callers validate the manifest first.
pub fn write_publish(
    folder: &Path,
    state: PublishState,
    visibility: Option<Visibility>,
) -> Result<(), ManifestError> {
    let path = folder.join(MANIFEST_FILE);
    let text = fs::read_to_string(&path).map_err(|e| ManifestError::Unreadable(e.to_string()))?;
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| ManifestError::Yaml(e.to_string()))?;
    let publish = doc
        .get_mut("publish")
        .and_then(|p| p.as_mapping_mut())
        .ok_or(ManifestError::MissingField("publish"))?;
    publish.insert("state".into(), state.as_str().into());
    if let Some(v) = visibility {
        publish.insert("visibility".into(), v.as_str().into());
    }
    let out = serde_yaml::to_string(&doc).map_err(|e| ManifestError::Yaml(e.to_string()))?;
    let tmp = folder.join(format!("{MANIFEST_FILE}.tmp"));
    fs::write(&tmp, out).map_err(|e| ManifestError::Unreadable(e.to_string()))?;
    fs::rename(&tmp, &path).map_err(|e| ManifestError::Unreadable(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder_with(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        dir
    }

    fn yaml(slug: &str, state: &str, visibility: &str, files: &str) -> String {
        format!(
            "slug: \"{slug}\"\ntitle: AI usage\npublish:\n  state: {state}\n  visibility: {visibility}\n  files:\n{files}"
        )
    }

    const GOOD_FILES: &str = "    - index.html\n    - data/usage.json\n";

    #[test]
    fn publish_retain_values() {
        let dir = folder_with(&["index.html", "data/usage.json"]);
        let base = yaml("ai-usage", "draft", "private", GOOD_FILES);
        for (value, expected) in [
            ("current", 1),
            ("1", 1),
            ("2", 2),
            ("3", 3),
            ("4", 4),
            ("5", 5),
        ] {
            let text = base.replace("publish:", &format!("publish:\n  retain: {value}"));
            assert_eq!(parse(&text, dir.path()).unwrap().retain, expected);
        }
        assert_eq!(parse(&base, dir.path()).unwrap().retain, 5);
        for value in [
            "0", "6", "-1", "true", "1.5", "all", "[]", "{}", "null", "'2'",
        ] {
            let text = base.replace("publish:", &format!("publish:\n  retain: {value}"));
            // Explicit null is invalid too, rather than the absent default.
            assert!(
                parse(&text, dir.path())
                    .unwrap_err()
                    .to_string()
                    .contains("publish.retain"),
                "{value}"
            );
        }
    }

    #[test]
    fn valid_manifest_parses_to_the_typed_object() {
        let dir = folder_with(&["index.html", "data/usage.json"]);
        let m = parse(
            &yaml("ai-usage", "draft", "private", GOOD_FILES),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            m,
            Manifest {
                slug: "ai-usage".into(),
                title: "AI usage".into(),
                state: PublishState::Draft,
                visibility: Visibility::Private,
                retain: 5,
                files: vec!["index.html".into(), "data/usage.json".into()],
                live: None,
                live_fields: LiveFields::new(),
                live_args: LiveArgs::new(),
                live_streams: Vec::new(),
                live_control: false,
                live_embeds: Default::default(),
                actions: Default::default(),
                session_days: 90,
                install: None,
                chrome: Chrome::None,
            }
        );
    }

    /// SV-INP-29: `live.embeds` is a subset of the page's own block, its fields are
    /// resolved, `run` defaults to none, and every part is in the sha.
    #[test]
    fn live_embeds_are_a_subset_of_the_page_and_part_of_the_sha() {
        let dir = folder_with(&["index.html"]);
        let base = "live:\n  queries: [queryContext]\n  fields:\n    queryContext: [status, title, sprint, priority, type]\n";
        let plain = parse(&with_live(base), dir.path()).unwrap();
        assert!(plain.live_embeds.is_empty());
        let embeds = format!(
            "{base}  embeds:\n    queries: [queryContext]\n    fields:\n      queryContext: [status, title, sprint, priority, type]\n"
        );
        let off = parse(&with_live(&embeds), dir.path()).unwrap();
        assert_eq!(off.live_embeds.run, EmbedRun::None, "run defaults to none");
        assert_eq!(off.live_embeds.queries, vec!["queryContext".to_string()]);
        assert_eq!(
            off.live_embeds.fields["queryContext"],
            vec!["priority", "sprint", "status", "title", "type"]
        );
        assert_ne!(off.live_sha(), plain.live_sha(), "embeds are part of the sha");
        let owner = parse(&with_live(&format!("{embeds}    run: owner\n")), dir.path()).unwrap();
        let everyone =
            parse(&with_live(&format!("{embeds}    run: everyone\n")), dir.path()).unwrap();
        assert_eq!(owner.live_embeds.run, EmbedRun::Owner);
        assert_eq!(everyone.live_embeds.run, EmbedRun::Everyone);
        assert_ne!(owner.live_sha(), off.live_sha(), "run is part of the sha");
        assert_ne!(owner.live_sha(), everyone.live_sha());
        // A query with no field list gets the defaults the page shares.
        let defaults = parse(
            &with_live("live:\n  queries: [queryContext]\n  embeds:\n    queries: [queryContext]\n"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            defaults.live_embeds.fields["queryContext"],
            vec!["owner", "status", "title", "type", "updated"]
        );
        for (bad, why) in [
            (
                "live:\n  queries: [sessions]\n  embeds:\n    queries: [queryContext]\n",
                "not a declared query",
            ),
            (
                "live:\n  queries: [queryContext]\n  embeds:\n    queries: [queryContext]\n    streams: [terminal]\n",
                "not in live.streams",
            ),
            (
                "live:\n  queries: [queryContext]\n  fields:\n    queryContext: [status]\n  embeds:\n    queries: [queryContext]\n    fields:\n      queryContext: [status, sprint]\n",
                "not shared by the page",
            ),
            (
                "live:\n  queries: [queryContext]\n  embeds:\n    queries: [queryContext]\n    fields:\n      sessions: [x]\n",
                "not in embeds.queries",
            ),
        ] {
            match parse(&with_live(bad), dir.path()) {
                Err(ManifestError::BadLiveField(msg)) => assert!(msg.contains(why), "{bad}: {msg}"),
                other => panic!("{bad}: {other:?}"),
            }
        }
        for bad in [
            "live:\n  queries: [queryContext]\n  embeds:\n    queries: [queryContext]\n    run: viewers\n",
            "live:\n  queries: [queryContext]\n  embeds:\n    run: owner\n",
        ] {
            assert!(
                matches!(parse(&with_live(bad), dir.path()), Err(ManifestError::BadLive(_))),
                "{bad}"
            );
        }
        // A misspelt key is refused, not ignored.
        assert!(parse(
            &with_live("live:\n  queries: [queryContext]\n  embeds:\n    queries: [queryContext]\n    trust: owner\n"),
            dir.path()
        )
        .is_err());
    }

    #[test]
    fn viewer_session_days_defaults_and_bounds() {
        let dir = folder_with(&["index.html", "data/usage.json"]);
        let base = yaml("ai-usage", "draft", "private", GOOD_FILES);
        assert_eq!(parse(&base, dir.path()).unwrap().session_days, 90);
        for days in [1, 90, 365] {
            assert_eq!(
                parse(
                    &format!("{base}viewers: {{ session_days: {days} }}\n"),
                    dir.path()
                )
                .unwrap()
                .session_days,
                days
            );
        }
        for days in ["0", "366", "-1", "1.5", "forever"] {
            assert!(parse(
                &format!("{base}viewers: {{ session_days: {days} }}\n"),
                dir.path()
            )
            .is_err());
        }
    }

    #[test]
    fn install_and_chrome_are_validated_and_default_to_no_presentation() {
        let dir = folder_with(&["index.html", "icon.png"]);
        let base = yaml(
            "ai-usage",
            "draft",
            "private",
            "    - index.html\n    - icon.png\n",
        );
        assert!(parse(&base, dir.path()).unwrap().presentation().is_none());
        assert_eq!(parse(&base, dir.path()).unwrap().chrome, Chrome::None);
        assert!(parse(&format!("{base}chrome: none\n"), dir.path())
            .unwrap()
            .presentation()
            .is_none());
        assert_eq!(
            serde_json::to_value(
                parse(&format!("{base}chrome: full\n"), dir.path())
                    .unwrap()
                    .presentation()
            )
            .unwrap(),
            serde_json::json!({"install": null, "chrome": "full"})
        );
        let block = "install: {name: Family brief, short_name: Brief, icon: icon.png, theme_color: {light: '#f7f6f3', dark: '#201e1c'}}\nchrome: minimal\n";
        let m = parse(&format!("{base}{block}"), dir.path()).unwrap();
        let presentation = serde_json::to_value(m.presentation()).unwrap();
        assert_eq!(presentation["chrome"], "minimal");
        assert_eq!(presentation["install"]["name"], "Family brief");
        assert_eq!(presentation["install"]["theme_color"]["dark"], "#201e1c");
        for (block, field) in [
            ("install: {name: '', icon: icon.png}", "install.name"),
            ("install: {name: Brief, icon: missing.png}", "install.icon"),
            ("install: {name: Brief, icon: index.html}", "install.icon"),
            ("install: {name: Brief, icon: icon.png, theme_color: {light: red, dark: '#201e1c'}}", "install.theme_color.light"),
            ("install: {name: Brief, icon: icon.png, theme_color: {light: '#f7f6f3', dark: '#abc'}}", "install.theme_color.dark"),
            ("chrome: menu", "chrome"),
        ] {
            let err = parse(&format!("{base}{block}\n"), dir.path()).unwrap_err();
            assert!(err.to_string().contains(field), "{err}: expected {field}");
        }
        for (key, count, field) in [
            ("name", 65, "install.name"),
            ("short_name", 31, "install.short_name"),
        ] {
            let block = if key == "name" {
                format!("install: {{name: '{}', icon: icon.png}}", "x".repeat(count))
            } else {
                format!(
                    "install: {{name: Brief, short_name: '{}', icon: icon.png}}",
                    "x".repeat(count)
                )
            };
            assert!(parse(&format!("{base}{block}\n"), dir.path())
                .unwrap_err()
                .to_string()
                .contains(field));
        }
        assert_eq!(
            serde_json::to_value(
                parse(&format!("{base}chrome: minimal\n"), dir.path())
                    .unwrap()
                    .presentation()
            )
            .unwrap(),
            serde_json::json!({"install": null, "chrome": "minimal"})
        );
    }

    #[test]
    fn refuses_a_blank_or_too_long_title() {
        let dir = folder_with(&["index.html"]);
        let too_long = "x".repeat(MAX_TITLE_CHARS + 1);
        for title in ["\"   \"", too_long.as_str()] {
            let text = format!(
                "slug: ai-usage\ntitle: {title}\npublish:\n  state: draft\n  visibility: private\n  files:\n    - index.html\n"
            );
            fs::write(dir.path().join(MANIFEST_FILE), text).unwrap();
            assert_eq!(
                load(dir.path()).unwrap_err(),
                ManifestError::BadTitle,
                "{title}"
            );
        }
        let text = format!(
            "slug: ai-usage\ntitle: \"{}\"\npublish:\n  state: draft\n  visibility: private\n  files:\n    - index.html\n",
            "x".repeat(MAX_TITLE_CHARS)
        );
        fs::write(dir.path().join(MANIFEST_FILE), text).unwrap();
        assert!(
            load(dir.path()).is_ok(),
            "a title of exactly the limit is refused"
        );
    }

    #[test]
    fn refuses_a_bad_slug() {
        let dir = folder_with(&["index.html"]);
        for slug in [
            "AB",
            "Has_Upper",
            "a",
            "-",
            "x".repeat(41).as_str(),
            "sp ace",
        ] {
            let err = parse(
                &yaml(slug, "draft", "private", "    - index.html\n"),
                dir.path(),
            )
            .unwrap_err();
            assert!(
                matches!(err, ManifestError::BadSlug(_)),
                "slug {slug:?}: {err:?}"
            );
        }
    }

    #[test]
    fn refuses_an_unknown_state() {
        let dir = folder_with(&["index.html"]);
        let err = parse(
            &yaml("ai-usage", "live", "private", "    - index.html\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::UnknownValue {
                field: "publish.state",
                ..
            }
        ));
    }

    #[test]
    fn refuses_an_unknown_visibility() {
        let dir = folder_with(&["index.html"]);
        let err = parse(
            &yaml("ai-usage", "draft", "public", "    - index.html\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::UnknownValue {
                field: "publish.visibility",
                ..
            }
        ));
    }

    #[test]
    fn refuses_an_absolute_path() {
        let dir = folder_with(&["index.html"]);
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - /etc/passwd.html\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadPath { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_path_with_dot_dot() {
        let dir = folder_with(&["index.html"]);
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - ../index.html\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadPath { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_directory_symlink_that_resolves_outside_the_folder() {
        let outside = folder_with(&["secret.html"]);
        let dir = folder_with(&[]);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - linked/secret.html\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Symlink(_)), "{err:?}");
    }

    #[test]
    fn refuses_a_file_symlink_that_leaves_the_folder() {
        let outside = folder_with(&["secret.html"]);
        let dir = folder_with(&[]);
        std::os::unix::fs::symlink(
            outside.path().join("secret.html"),
            dir.path().join("index.html"),
        )
        .unwrap();
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - index.html\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Symlink(_)), "{err:?}");
    }

    #[test]
    fn refuses_a_symlink_even_when_it_stays_inside_the_folder() {
        // Refused by rule, not by target: a second name for a file is not allowed.
        let dir = folder_with(&["real.html"]);
        std::os::unix::fs::symlink(dir.path().join("real.html"), dir.path().join("alias.html"))
            .unwrap();
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - alias.html\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Symlink(_)), "{err:?}");
    }

    #[test]
    fn refuses_a_symlink_alias_of_a_listed_file() {
        // Every check keys on the normalised path, so an alias must never reach
        // a listed file through a second name.
        let dir = folder_with(&["data/usage.json"]);
        std::os::unix::fs::symlink(
            dir.path().join("data/usage.json"),
            dir.path().join("alias.json"),
        )
        .unwrap();
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - alias.json\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Symlink(_)), "{err:?}");
    }

    #[test]
    fn refuses_a_hard_link_to_a_listed_file() {
        // A hard link is a second name for the same inode: a file that is not
        // listed would upload under an allowlisted name, so it is refused.
        let dir = folder_with(&["data/usage.json"]);
        fs::hard_link(
            dir.path().join("data/usage.json"),
            dir.path().join("report.json"),
        )
        .unwrap();
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - report.json\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::HardLink(_)), "{err:?}");
    }

    #[test]
    fn refuses_a_current_dir_alias_of_a_listed_file() {
        let dir = folder_with(&["data/usage.json"]);
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - ./data/usage.json\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadPath { .. }), "{err:?}");
    }

    #[test]
    fn refuses_a_doubled_slash_alias_of_a_listed_file() {
        let dir = folder_with(&["data/usage.json"]);
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - data//usage.json\n"),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadPath { .. }), "{err:?}");
    }

    #[test]
    fn refuses_the_same_file_listed_twice() {
        let dir = folder_with(&["index.html"]);
        let err = parse(
            &yaml(
                "ai-usage",
                "draft",
                "private",
                "    - index.html\n    - INDEX.html\n",
            ),
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadPath { .. }), "{err:?}");
    }

    #[test]
    fn load_slug_reads_only_the_slug_of_a_broken_manifest() {
        // A bad state and a missing file would fail full validation; takedown must not care.
        let dir = folder_with(&[]);
        fs::write(
            dir.path().join(MANIFEST_FILE),
            "slug: ai-usage\npublish:\n  state: live\n  files:\n    - gone.html\n",
        )
        .unwrap();
        assert_eq!(load_slug(dir.path()).unwrap(), "ai-usage");
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn load_slug_still_refuses_a_bad_slug() {
        let dir = folder_with(&[]);
        fs::write(
            dir.path().join(MANIFEST_FILE),
            "slug: ../escape\npublish:\n  state: draft\n",
        )
        .unwrap();
        assert!(matches!(
            load_slug(dir.path()),
            Err(ManifestError::BadSlug(_))
        ));
    }

    #[test]
    fn refuses_a_missing_allowlisted_file() {
        let dir = folder_with(&["index.html"]);
        let err = parse(
            &yaml("ai-usage", "draft", "private", GOOD_FILES),
            dir.path(),
        )
        .unwrap_err();
        assert_eq!(err, ManifestError::MissingFile("data/usage.json".into()));
    }

    #[test]
    fn refuses_more_than_fifty_files() {
        let names: Vec<String> = (0..51).map(|i| format!("    - f{i}.txt\n")).collect();
        let dir = folder_with(&[]);
        let err = parse(
            &yaml("ai-usage", "draft", "private", &names.concat()),
            dir.path(),
        )
        .unwrap_err();
        assert_eq!(err, ManifestError::TooManyFiles(51));
    }

    #[test]
    fn refuses_svg_and_other_types_outside_k9() {
        let dir = folder_with(&["logo.svg"]);
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    - logo.svg\n"),
            dir.path(),
        )
        .unwrap_err();
        assert_eq!(err, ManifestError::TypeNotAllowed("logo.svg".into()));
    }

    #[test]
    fn refuses_an_empty_allowlist() {
        let dir = folder_with(&[]);
        let err = parse(
            &yaml("ai-usage", "draft", "private", "    []\n"),
            dir.path(),
        )
        .unwrap_err();
        assert_eq!(err, ManifestError::EmptyFiles);
    }

    #[test]
    fn write_publish_changes_only_publish_keys() {
        let dir = folder_with(&["index.html"]);
        fs::write(
            dir.path().join(MANIFEST_FILE),
            yaml("ai-usage", "draft", "private", "    - index.html\n"),
        )
        .unwrap();
        write_publish(dir.path(), PublishState::Published, Some(Visibility::Team)).unwrap();
        let m = load(dir.path()).unwrap();
        assert_eq!(m.state, PublishState::Published);
        assert_eq!(m.visibility, Visibility::Team);
        assert_eq!(m.title, "AI usage");
        assert_eq!(m.files, vec!["index.html".to_string()]);
    }

    /// A fixture catalog: the app's is empty until SW-DP-5.
    const FIXTURE_CATALOG: &[QueryDef] = &[
        QueryDef {
            name: "sessionUsage",
            args: &["sinceMs", "untilMs", "sessionId", "cwd", "story", "limit", "cursor"],
            describe: "Token use per session",
            shares: "the fixture answer",
            fields: crate::dashboard_publish::catalog::Fields::Named(&["note"]),
            run: crate::dashboard_publish::catalog::tests::fixture_run,
        },
        QueryDef {
            name: "runs",
            args: &["limit"],
            describe: "Agent runs",
            shares: "the fixture answer",
            fields: crate::dashboard_publish::catalog::Fields::Named(&["note"]),
            run: crate::dashboard_publish::catalog::tests::fixture_run,
        },
    ];

    fn with_live(live: &str) -> String {
        format!(
            "{}{live}",
            yaml("ai-usage", "draft", "private", "    - index.html\n")
        )
    }

    /// SW-DP-4 AC-7: the `live` block as absent, `[]`, a list or `all`; an
    /// unknown name is refused; the sha does not follow the list order.
    #[test]
    fn the_live_block_parses_in_its_four_forms() {
        let dir = folder_with(&["index.html"]);
        let cases: &[(&str, Result<Option<LiveQueries>, ManifestError>)] = &[
            ("", Ok(None)),
            (
                "live:\n  queries: []\n",
                Ok(Some(LiveQueries::List(vec![]))),
            ),
            (
                "live:\n  queries: [sessionUsage, runs]\n",
                Ok(Some(LiveQueries::List(vec![
                    "runs".into(),
                    "sessionUsage".into(),
                ]))),
            ),
            ("live:\n  queries: all\n", Ok(Some(LiveQueries::All))),
            (
                "live:\n  queries: [runs, secrets]\n",
                Err(ManifestError::UnknownLiveQuery("secrets".into())),
            ),
            (
                "live:\n  queries: everything\n",
                Err(ManifestError::BadLive(
                    "queries must be the word all or a list of query names",
                )),
            ),
            (
                "live:\n  fields: {}\n",
                Err(ManifestError::MissingField("live.queries")),
            ),
            (
                "live:\n  queries: [1]\n",
                Err(ManifestError::BadLive("each query name must be text")),
            ),
        ];
        for (block, want) in cases {
            let got = parse_with(&with_live(block), dir.path(), FIXTURE_CATALOG).map(|m| m.live);
            assert_eq!(&got, want, "block {block:?}");
        }
    }

    #[test]
    fn the_apps_catalog_names_are_accepted_and_others_refused() {
        let dir = folder_with(&["index.html"]);
        let m = parse(
            &with_live("live:\n  queries: [sessionUsage, runs, sessions, queryContext]\n"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            m.live,
            Some(LiveQueries::list(
                ["sessionUsage", "runs", "sessions", "queryContext"].map(String::from)
            ))
        );
        let err = parse(&with_live("live:\n  queries: [nope]\n"), dir.path()).unwrap_err();
        assert_eq!(err.to_string(), "unknown live query nope");
    }

    #[test]
    fn the_live_sha_follows_the_set_of_names_not_their_order() {
        let a = LiveQueries::list(["sessionUsage".to_string(), "runs".to_string()]);
        let b = LiveQueries::list([
            "runs".to_string(),
            "sessionUsage".to_string(),
            "runs".to_string(),
        ]);
        assert_eq!(a.sha(), b.sha());
        assert_ne!(a.sha(), LiveQueries::list(["runs".to_string()]).sha());
        assert_ne!(a.sha(), LiveQueries::All.sha());
        assert_ne!(LiveQueries::List(vec![]).sha(), LiveQueries::All.sha());
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            r#"["runs","sessionUsage"]"#
        );
        assert_eq!(
            serde_json::to_string(&LiveQueries::All).unwrap(),
            r#""all""#
        );
        let back: LiveQueries = serde_json::from_str(r#"["sessionUsage","runs"]"#).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn more_than_32_live_queries_are_refused() {
        let dir = folder_with(&["index.html"]);
        let names: Vec<String> = (0..33).map(|_| "runs".to_string()).collect();
        let block = format!("live:\n  queries: [{}]\n", names.join(", "));
        assert!(matches!(
            parse_with(&with_live(&block), dir.path(), FIXTURE_CATALOG),
            Err(ManifestError::BadLive(_))
        ));
    }

    #[test]
    fn write_publish_keeps_the_live_block() {
        let dir = folder_with(&["index.html"]);
        fs::write(
            dir.path().join(MANIFEST_FILE),
            with_live("live:\n  queries: []\n"),
        )
        .unwrap();
        write_publish(dir.path(), PublishState::Published, None).unwrap();
        assert_eq!(
            load(dir.path()).unwrap().live,
            Some(LiveQueries::List(vec![]))
        );
    }

    /// SW-DP-20 AC-4: `live.streams` lists `STREAMS` names (the test build
    /// holds `tick`); they are part of the sha, and a block without streams
    /// keeps the sha it had before streams existed.
    #[test]
    fn live_streams_are_checked_and_part_of_the_sha() {
        let dir = folder_with(&["index.html"]);
        let m = parse(
            &with_live("live:\n  streams: [tickValue, tick, tick]\n"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            m.live_streams,
            vec!["tick".to_string(), "tickValue".to_string()]
        );
        assert_eq!(
            m.live,
            Some(LiveQueries::List(vec![])),
            "streams alone: no queries"
        );
        let presence = parse(&with_live("live:\n  queries: []\n"), dir.path()).unwrap();
        assert!(presence.live_streams.is_empty());
        assert_eq!(presence.live_sha(), Some(LiveQueries::List(vec![]).sha()));
        assert_ne!(m.live_sha(), presence.live_sha());
        let both = parse(
            &with_live("live:\n  queries: [runs]\n  streams: [tick]\n"),
            dir.path(),
        )
        .unwrap();
        assert_ne!(both.live_sha(), both.live.as_ref().map(LiveQueries::sha));

        assert_eq!(
            parse(&with_live("live:\n  streams: [nope]\n"), dir.path()).unwrap_err(),
            ManifestError::UnknownLiveStream("nope".into())
        );
        let many = format!("live:\n  streams: [{}]\n", vec!["tick"; 17].join(", "));
        for bad in [
            "live:\n  streams: tick\n",
            "live:\n  streams: [1]\n",
            many.as_str(),
        ] {
            assert!(
                matches!(
                    parse(&with_live(bad), dir.path()),
                    Err(ManifestError::BadLive(_))
                ),
                "{bad}"
            );
        }
        // create_dashboard takes streams by the same rules (SW-DP-21).
        let value: serde_yaml::Value =
            serde_yaml::from_str("{queries: [], streams: [tick, terminal]}").unwrap();
        let (_, _, streams, _, _) = parse_live_value(value, CATALOG).unwrap();
        assert_eq!(streams, ["terminal", "tick"]);
        let value: serde_yaml::Value = serde_yaml::from_str("{streams: [nope]}").unwrap();
        assert_eq!(
            parse_live_value(value, CATALOG).unwrap_err(),
            ManifestError::UnknownLiveStream("nope".into())
        );
    }

    /// SW-DP-23 (P1, DP-D48): `live.control: owner` turns owner control on. It
    /// must be the word `owner` and needs the `terminal` stream; it joins the sha
    /// only when set, so a block without it keeps the sha it had before.
    #[test]
    fn live_control_is_owner_only_with_the_terminal_stream_and_part_of_the_sha() {
        let dir = folder_with(&["index.html"]);
        let plain = parse(
            &with_live("live:\n  streams: [terminal, tick]\n"),
            dir.path(),
        )
        .unwrap();
        assert!(!plain.live_control);
        let before = live_sha(
            plain.live.as_ref().unwrap(),
            &plain.live_fields,
            &plain.live_streams,
            false,
        );
        assert_eq!(
            plain.live_sha(),
            Some(before.clone()),
            "no control: the sha is unchanged"
        );
        let on = parse(
            &with_live("live:\n  streams: [terminal, tick]\n  control: owner\n"),
            dir.path(),
        )
        .unwrap();
        assert!(on.live_control);
        assert_eq!(on.live_streams, plain.live_streams);
        assert_ne!(on.live_sha(), Some(before), "control is part of the sha");
        for (bad, why) in [
            (
                "live:\n  streams: [tick]\n  control: owner\n",
                "needs the terminal stream",
            ),
            (
                "live:\n  queries: []\n  control: owner\n",
                "needs the terminal stream",
            ),
            (
                "live:\n  streams: [terminal]\n  control: viewers\n",
                "control must be owner",
            ),
            (
                "live:\n  streams: [terminal]\n  control: true\n",
                "control must be owner",
            ),
        ] {
            match parse(&with_live(bad), dir.path()) {
                Err(ManifestError::BadLive(msg)) => assert!(msg.contains(why), "{bad}: {msg}"),
                other => panic!("{bad}: {other:?}"),
            }
        }
        // create_dashboard takes it by the same rules.
        let value: serde_yaml::Value =
            serde_yaml::from_str("{streams: [terminal], control: owner}").unwrap();
        assert!(parse_live_value(value, CATALOG).unwrap().3);
        let value: serde_yaml::Value =
            serde_yaml::from_str("{streams: [tick], control: owner}").unwrap();
        assert!(matches!(
            parse_live_value(value, CATALOG),
            Err(ManifestError::BadLive(_))
        ));
    }

    /// SW-DP-25 AC-1: actions cannot silently become arbitrary commands or args.
    #[test]
    fn action_manifest_rejects_unsafe_declarations() {
        let dir = folder_with(&["index.html"]);
        for declaration in [
            "{verb: shell, who: owner}",
            "{verb: setFields, who: owner, fields: {status: done}, args: {text: string}}",
            "{verb: launchAgent, who: owner, agent: fixer, prompt: 'Fix {path}', mode: headless}",
            "{verb: setFields, who: owner, fields: {status: done}, max_per_hour: 31}",
            "{verb: launchAgent, who: owner, agent: fixer, prompt: 'Fix {id}', mode: headless, max_running: 5}",
        ] {
            let text = with_live(&format!("actions:\n  fix: {declaration}\n"));
            assert!(parse(&text, dir.path()).is_err(), "accepted {declaration}");
        }
        let long = "x".repeat(501);
        assert!(parse(&with_live(&format!("actions:\n  fix: {{verb: launchAgent, who: owner, agent: fixer, mode: headless, prompt: '{long}'}}\n")), dir.path()).is_err());
    }

    /// R1 holds the same 16-name ceiling (the site worker's dashboards module).
    #[cfg(cs_site)]
    #[test]
    fn the_stream_list_ceiling_matches_r1() {
        const DASHBOARDS: &str = include_str!("../../../site/worker/lib/dashboards.ts");
        let re = regex::Regex::new(r"export const MAX_LIVE_STREAMS = (\d+);").unwrap();
        let r1: usize = re.captures(DASHBOARDS).expect("MAX_LIVE_STREAMS")[1]
            .parse()
            .unwrap();
        assert_eq!(r1, MAX_LIVE_STREAMS);
    }

    #[test]
    fn selector_entries_parse_canonically_and_reject_wrong_arguments() {
        let parse = |block: &str| parse_live_value(serde_yaml::from_str(block).unwrap(), CATALOG);
        let a = parse("{queries: [storyChanges], args: {storyChanges: {storyId: [SV-CDE-46, {epic: SV-CDE}, {team: riverside}, {epic: SV-CDE}]}}}").unwrap();
        let b = parse("{queries: [storyChanges], args: {storyChanges: {storyId: [{team: riverside}, {epic: SV-CDE}, SV-CDE-46]}}}").unwrap();
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
        for block in [
            "{queries: [storyChanges], args: {storyChanges: {storyId: [{unknown: X}]}}}",
            "{queries: [whyLine], args: {whyLine: {repo: [{epic: X}]}}}",
            "{queries: [storyChanges], args: {storyChanges: {storyId: [{epic: ''}]}}}",
            "{queries: [storyChanges], args: {storyChanges: {storyId: [{team: ''}]}}}",
            "{queries: [storyChanges], args: {storyChanges: {storyId: [{epic: X, team: riverside}]}}}",
        ] { assert!(parse(block).is_err(), "{block}"); }
    }

    #[test]
    fn live_args_are_canonical_and_keep_old_shas_when_empty() {
        let dir = folder_with(&["index.html"]);
        let load = |block: &str| parse_with(&with_live(block), dir.path(), CATALOG).unwrap();
        let plain = load("live: {queries: [whyLine]}\n");
        let empty = load("live: {queries: [whyLine], args: {}}\n");
        assert_eq!(plain.live_sha(), empty.live_sha());
        assert_eq!(plain.live_sha(), plain.live.as_ref().map(LiveQueries::sha));
        let a = load(
            "live: {queries: [whyLine], args: {whyLine: {repo: [b, a, a], start: [5, '5']}}}\n",
        );
        let b =
            load("live: {queries: [whyLine], args: {whyLine: {start: ['5', 5], repo: [a, b]}}}\n");
        assert_eq!(a.live_sha(), b.live_sha());
        assert_ne!(plain.live_sha(), a.live_sha());
        let parsed = parse_live_value(
            serde_yaml::from_str("{queries: [whyLine], args: {whyLine: {repo: [b, a, a]}}}")
                .unwrap(),
            CATALOG,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap()[4]["whyLine"]["repo"],
            serde_json::json!(["a", "b"])
        );
        let value =
            serde_yaml::from_str("{queries: [whyLine], args: {whyLine: {repo: [a]}}}").unwrap();
        assert_eq!(
            serde_json::to_value(parse_live_value(value, CATALOG).unwrap()).unwrap()[4]["whyLine"]
                ["repo"],
            serde_json::json!(["a"])
        );
    }

    #[test]
    fn live_args_refusals_apply_to_manifest_and_create() {
        let dir = folder_with(&["index.html"]);
        for block in [
            "{queries: all, args: {unknown: {repo: [a]}}}",
            "{queries: [runs], args: {whyLine: {repo: [a]}}}",
            "{queries: all, streams: [terminal], args: {terminal: {handle: [a]}}}",
            "{queries: [whyLine], args: {whyLine: {unknown: [a]}}}",
            "{queries: [whyLine], args: {whyLine: {repo: a}}}",
            "{queries: [whyLine], args: {whyLine: {repo: []}}}",
            "{queries: [whyLine], args: {whyLine: {repo: [[a]]}}}",
            "{queries: [whyLine], args: {whyLine: {repo: [null]}}}",
            "{queries: [whyLine], args: {whyLine: {repo: [true]}}}",
            "{queries: [whyLine], args: {whyLine: {repo: [1.5]}}}",
            "{queries: [whyLine], arg: {whyLine: {repo: [a]}}}",
        ]
        .into_iter()
        .map(String::from)
        .chain(std::iter::once(format!(
            "{{queries: [whyLine], args: {{whyLine: {{repo: [{}]}}}}}}",
            vec!["a"; 65].join(",")
        ))) {
            assert!(
                parse_with(&with_live(&format!("live: {block}\n")), dir.path(), CATALOG).is_err(),
                "{block}"
            );
            assert!(
                parse_live_value(serde_yaml::from_str(&block).unwrap(), CATALOG).is_err(),
                "{block}"
            );
        }
    }

    /// Review S1: `live.fields` names extra fields per declared query; the
    /// query decides which; the fields are part of the block's sha.
    #[test]
    fn live_fields_are_checked_and_part_of_the_sha() {
        let dir = folder_with(&["index.html"]);
        let ok = parse(
            &with_live("live:\n  queries: [queryContext, runs]\n  fields:\n    queryContext: [priority, status, status]\n    runs: [summary]\n"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            ok.live_fields["queryContext"],
            vec!["priority".to_string(), "status".to_string()]
        );
        assert_ne!(ok.live_sha(), ok.live.as_ref().map(LiveQueries::sha));
        let plain = parse(
            &with_live("live:\n  queries: [queryContext, runs]\n"),
            dir.path(),
        )
        .unwrap();
        assert_eq!(plain.live_sha(), plain.live.as_ref().map(LiveQueries::sha));
        for bad in [
            "live:\n  queries: [runs]\n  fields:\n    queryContext: [status]\n",
            "live:\n  queries: [queryContext]\n  fields:\n    queryContext: [summary]\n",
            "live:\n  queries: [runs]\n  fields:\n    runs: [artifact_ref]\n",
            "live:\n  queries: all\n  fields:\n    nope: [x]\n",
        ] {
            assert!(
                matches!(
                    parse(&with_live(bad), dir.path()),
                    Err(ManifestError::BadLiveField(_))
                ),
                "{bad}"
            );
        }
    }
}
