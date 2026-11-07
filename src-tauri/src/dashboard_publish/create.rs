// SPDX-License-Identifier: MIT
//! `create_dashboard` (SW-DP-17): writes a new dashboard folder that already
//! follows the publishing rules, so an agent's first dashboard publishes as it
//! is. The folder gets `dashboard.yml` (draft, private, every written file
//! listed), the pages, a shared script and stylesheet, and the data files.
//!
//! The starter pages work in both places a dashboard is shown:
//! - the CodeSense viewer on this Mac, which serves the folder's files;
//! - the hosted viewer on codesense.ai (SW-PB-22d), where a page runs in a
//!   sandboxed frame with no network: its `fetch` of a listed file is answered
//!   by the shell, links between listed pages are followed by the shell, and
//!   listed scripts, stylesheets and images are inlined.
//!
//! So the pages load data with `fetch("data/<name>.json")` relative to
//! themselves, link to each other by relative file name, reference only files
//! they list, and make no bridge call (publish refuses one, SW-PB-22a AC-4).
//!
//! Every input is checked before anything is written. The written folder is then
//! checked again by the same code `publish_dashboard` runs (manifest load and
//! the published-mode payload build), so a starter that would not publish is
//! never left behind.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use super::catalog::QueryDef;
use super::manifest::{self, LiveFields, LiveQueries, MANIFEST_FILE, MAX_FILES, MAX_TITLE_CHARS};
use super::payload::{self, FolderReader, Mode, MAX_FILE_BYTES, MAX_VERSION_BYTES};

/// The entry page. The hosted viewer opens `index.html` first (the manifest's
/// `publish.entry` is decided, DP-D34, but not built).
pub const ENTRY: &str = "index.html";
/// The shared script every starter page loads.
pub const SCRIPT: &str = "app.js";
/// The shared stylesheet every starter page loads.
pub const STYLE: &str = "style.css";
/// The data file written when the caller names none.
pub const DEFAULT_DATA: &str = "summary";
/// Files the starter always writes besides the extra pages and data files.
const FIXED_FILES: usize = 3;

/// SW-DP-24 toolkit: one source file, shipped in the folder (the hosted frame
/// has no network, so it cannot load a script from anywhere else) and quoted by
/// `get_live_data_contract` as its helper, byte for byte.
pub const TOOLKIT: &str = "cs-live.js";
pub const TOOLKIT_JS: &str = include_str!("toolkit/cs-live.js");
/// Asset release version in the toolkit header; independent of page API wire `v = 1`.
pub const TOOLKIT_VERSION: u32 = 4;
/// SV-INP-29 embed pipe: the page-side forwarder and message-side SDK for live HTML
/// messages inside a published dashboard (`live.embeds`). A dashboard copies it next
/// to `cs-live.js` and lists it in `publish.files`; it makes no network call and holds no token.
pub const EMBED_TOOLKIT: &str = "cs-embed.js";
pub const EMBED_TOOLKIT_JS: &str = include_str!("toolkit/cs-embed.js");
/// The embed toolkit's version comment.
pub const EMBED_TOOLKIT_VERSION: u32 = 1;
/// xterm.js as the app pins it (`@xterm/xterm` in package.json), with its licence.
pub const XTERM_JS_PATH: &str = "vendor/xterm.js";
pub const XTERM_CSS_PATH: &str = "vendor/xterm.css";
pub const XTERM_JS: &str = include_str!("toolkit/vendor/xterm.js");
pub const XTERM_CSS: &str = include_str!("toolkit/vendor/xterm.css");
/// The `@xterm/xterm` version vendored above.
pub const XTERM_VERSION: &str = "6.0.0";

/// SW-DP-21: the Workspaces dashboard (the owner's acceptance test for watching).
const WORKSPACES_HTML: &str = include_str!("templates/workspaces/index.html");
const WORKSPACES_CSS: &str = include_str!("templates/workspaces/style.css");
const WORKSPACES_JS: &str = include_str!("templates/workspaces/app.js");
/// The streams the Workspaces dashboard declares.
pub const WORKSPACES_STREAMS: [&str; 3] = ["workspaces", "terminals", "terminal"];

/// Which folder `create_dashboard` writes (SW-DP-21 `template`). SW-DP-24b's
/// mission control extends `Workspaces` rather than writing a second one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Template {
    /// Pages, data files and an optional live section (SW-DP-17).
    #[default]
    Starter,
    /// Open workspaces, their sessions and a live read-only terminal (SW-DP-21).
    Workspaces,
    /// Agents ordered by attention, with the same terminal view and optional owner control.
    MissionControl,
    BugTriage,
}

impl Template {
    /// `None` (absent) is the starter; an unknown name is refused.
    pub fn parse(name: Option<&str>) -> Result<Template, CreateError> {
        match name {
            None | Some("starter") => Ok(Template::Starter),
            Some("workspaces") => Ok(Template::Workspaces),
            Some("mission-control") => Ok(Template::MissionControl),
            Some("bug-triage") => Ok(Template::BugTriage),
            Some(other) => Err(invalid(format!(
                "template {other:?} is not one of starter, workspaces, mission-control"
            ))),
        }
    }
}

/// What the caller asks for. `folder` must be absolute.
#[derive(Debug, Clone, Default)]
pub struct CreateRequest {
    pub folder: PathBuf,
    pub slug: String,
    pub title: String,
    /// Extra pages besides `index.html`: file names such as `runs.html`.
    pub pages: Vec<String>,
    /// Data files: name (written as `data/<name>.json`) to its first content.
    /// Empty: one `data/summary.json` with a placeholder.
    pub data: BTreeMap<String, Value>,
    /// The manifest `live` block, as `{queries, fields?}`.
    pub live: Option<Value>,
    /// The org and team whose context repo holds the folder (deepest wins), as
    /// the context index reports it; `None` when no team's repo holds it.
    pub context_team: Option<(String, String)>,
    /// Which folder to write (default: the starter).
    pub template: Template,
    /// Opt into owner control for the workspaces or mission-control template.
    pub control: bool,
    pub agent: Option<String>,
}

/// What was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    pub folder: String,
    pub slug: String,
    pub title: String,
    /// `publish.state` in the written manifest.
    pub state: &'static str,
    /// `publish.visibility` in the written manifest.
    pub visibility: &'static str,
    /// The page the hosted viewer opens first.
    pub entry: &'static str,
    /// Every file listed in `publish.files`, in the order listed.
    pub files: Vec<String>,
    /// The pages, entry first.
    pub pages: Vec<String>,
    /// The data files, as listed.
    pub data: Vec<String>,
    /// The written `live` block: `{queries, fields, streams}`, or null.
    pub live: Option<Value>,
    /// The template written: `starter`, `workspaces` or `mission-control`.
    pub template: &'static str,
    /// `{org, team}` of the team context repo holding the folder, or null.
    pub team: Option<Value>,
    /// Things to fix before publishing, such as a folder outside a team's context repo.
    pub warnings: Vec<String>,
    /// Configuration guidance for the generated starter, also returned through next_steps.
    #[serde(skip)]
    starter_next_steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateError {
    /// An input breaks a rule; nothing was written.
    Invalid(String),
    /// Writing failed; what was written is removed again.
    Io(String),
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CreateError::Invalid(m) | CreateError::Io(m) => f.write_str(m),
        }
    }
}

fn invalid(m: impl Into<String>) -> CreateError {
    CreateError::Invalid(m.into())
}

/// An extra page's file name: lower-case letters, digits and `-`, then `.html`,
/// at the folder's top level (so every page links to every other by name).
fn valid_page(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".html") else {
        return false;
    };
    (1..=40).contains(&stem.len())
        && stem.as_bytes()[0].is_ascii_alphanumeric()
        && stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A data name (`data/<name>.json`): lower-case letters, digits, `-` and `_`.
fn valid_data_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The placeholder data a starter page shows until an agent or routine writes
/// real data into the file.
fn placeholder() -> Value {
    serde_json::json!({
        "generated_at": null,
        "note": "Placeholder. An agent or routine rewrites this file with real data; the pages read it with fetch.",
        "items": [],
    })
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// `runs-today.html` → `Runs today`.
fn page_label(page: &str) -> String {
    let stem = page.trim_end_matches(".html").replace('-', " ");
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => stem,
    }
}

/// Check the request and work out the file plan. Writes nothing.
struct Plan {
    title: String,
    pages: Vec<String>,
    data: Vec<(String, Vec<u8>)>,
    live: Option<LivePlan>,
}

/// A `live` block to write: queries, fields, streams and owner control
/// (SW-DP-23: written only when the request's `live` block says `control: owner`).
type LivePlan = manifest::LiveValue;

/// Where to write: the folder with every existing part resolved, and the
/// folders this call must create, outermost first.
struct Target {
    folder: PathBuf,
    missing: Vec<PathBuf>,
}

/// Resolve the folder path without trusting it: no `.` or `..` part (the
/// filesystem would resolve `new/../existing` to an existing dashboard), the
/// existing part canonicalised, and the folder itself new or an empty folder.
fn resolve_target(folder: &Path) -> Result<Target, CreateError> {
    use std::path::Component;
    if !folder.is_absolute() {
        return Err(invalid(
            "folder must be the absolute path of the new dashboard folder on this Mac",
        ));
    }
    // Checked on the text: `Path::components` drops an inner `.`.
    let text = folder.to_string_lossy();
    if text.split('/').any(|part| part == "." || part == "..")
        || folder
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid(format!(
            "{} has a . or .. part; give the folder's plain absolute path",
            folder.display()
        )));
    }
    let io = |p: &Path, e: std::io::Error| CreateError::Io(format!("{}: {e}", p.display()));
    match fs::symlink_metadata(folder) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(invalid(format!(
                "{} is a symlink; give the real folder path",
                folder.display()
            )))
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(invalid(format!(
                "{} exists and is not a folder",
                folder.display()
            )))
        }
        Ok(_) => {
            let mut entries = fs::read_dir(folder).map_err(|e| io(folder, e))?;
            if entries.next().is_some() {
                return Err(invalid(format!(
                    "{} already exists and is not empty; create_dashboard only writes a new or empty folder",
                    folder.display()
                )));
            }
            let folder = fs::canonicalize(folder).map_err(|e| io(folder, e))?;
            return Ok(Target {
                folder,
                missing: Vec::new(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(folder, e)),
    }
    // The folder is new: find the deepest part that exists.
    let mut tail = Vec::new();
    let mut existing = folder.to_path_buf();
    loop {
        match fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| invalid("folder has no existing parent"))?
                    .to_os_string();
                tail.push(name);
                existing = existing
                    .parent()
                    .ok_or_else(|| invalid("folder has no existing parent"))?
                    .to_path_buf();
            }
            Err(e) => return Err(io(&existing, e)),
        }
    }
    let mut current = fs::canonicalize(&existing).map_err(|e| io(&existing, e))?;
    if !current.is_dir() {
        return Err(invalid(format!("{} is not a folder", existing.display())));
    }
    let mut missing = Vec::new();
    for name in tail.into_iter().rev() {
        current.push(name);
        missing.push(current.clone());
    }
    Ok(Target {
        folder: current,
        missing,
    })
}

fn plan(req: &CreateRequest, catalog: &[QueryDef]) -> Result<(Target, Plan), CreateError> {
    let target = resolve_target(&req.folder)?;

    if !manifest::valid_slug(&req.slug) {
        return Err(invalid(format!(
            "slug {:?} must be 3 to 40 characters of a-z, 0-9 and -",
            req.slug
        )));
    }
    let title = req.title.trim();
    let title_chars = title.chars().count();
    if title_chars == 0 || title_chars > MAX_TITLE_CHARS {
        return Err(invalid(format!(
            "title must be 1 to {MAX_TITLE_CHARS} characters, not counting spaces at either end"
        )));
    }

    if req.template == Template::Starter && req.control {
        return Err(invalid("control requires template workspaces or mission-control; for starter, declare live.control explicitly"));
    }
    if matches!(
        req.template,
        Template::Workspaces | Template::MissionControl | Template::BugTriage
    ) {
        if !req.pages.is_empty() || !req.data.is_empty() || req.live.is_some() {
            return Err(invalid(
                "the live template writes its own page and live block: leave pages, data and live out",
            ));
        }
        let live = (
            LiveQueries::List(if req.template == Template::BugTriage {
                vec!["queryContext".into()]
            } else if req.template == Template::MissionControl {
                vec!["inbox".into(), "sessions".into()]
            } else {
                Vec::new()
            }),
            if req.template == Template::MissionControl {
                LiveFields::from([("inbox".into(), vec!["text".into()])])
            } else {
                LiveFields::new()
            },
            {
                let mut names: Vec<String> = WORKSPACES_STREAMS
                    .iter()
                    .filter(|name| req.template == Template::Workspaces || **name != "workspaces")
                    .map(|s| s.to_string())
                    .collect();
                if req.template == Template::BugTriage {
                    names.push("actions".into());
                }
                if req.template == Template::MissionControl {
                    names.push("inbox".into());
                }
                names.sort();
                names
            },
            req.control,
            Default::default(),
        );
        return Ok((
            target,
            Plan {
                title: title.to_string(),
                pages: vec![ENTRY.to_string()],
                data: Vec::new(),
                live: Some(live),
            },
        ));
    }

    let mut seen = HashSet::new();
    let mut pages = vec![ENTRY.to_string()];
    for page in &req.pages {
        if page == ENTRY {
            return Err(invalid(
                "pages lists index.html; it is always written, list only the extra pages",
            ));
        }
        if !valid_page(page) {
            return Err(invalid(format!(
                "page {page:?} must be a file name such as runs.html: 1 to 40 characters of a-z, 0-9 and -, then .html, no folder"
            )));
        }
        if !seen.insert(page.clone()) {
            return Err(invalid(format!("page {page:?} is listed more than once")));
        }
        pages.push(page.clone());
    }

    let mut data = Vec::new();
    let mut total: u64 = 0;
    let named: Vec<(String, Value)> = if req.data.is_empty() {
        vec![(DEFAULT_DATA.to_string(), placeholder())]
    } else {
        req.data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    for (name, value) in named {
        if !valid_data_name(&name) {
            return Err(invalid(format!(
                "data name {name:?} must be 1 to 40 characters of a-z, 0-9, - and _ (it is written as data/<name>.json)"
            )));
        }
        let mut bytes =
            serde_json::to_vec_pretty(&value).map_err(|e| invalid(format!("data {name}: {e}")))?;
        bytes.push(b'\n');
        let size = bytes.len() as u64;
        if size > MAX_FILE_BYTES {
            return Err(invalid(format!(
                "data {name} is {size} bytes; the limit is {MAX_FILE_BYTES} per file"
            )));
        }
        total += size;
        data.push((format!("data/{name}.json"), bytes));
    }
    // Pages, script and stylesheet are a few kilobytes; the data dominates.
    if total > MAX_VERSION_BYTES {
        return Err(invalid(format!(
            "the data files are {total} bytes; the limit is {MAX_VERSION_BYTES} for the whole dashboard"
        )));
    }

    let count = pages.len() + data.len() + FIXED_FILES - 1;
    if count > MAX_FILES {
        return Err(invalid(format!(
            "that is {count} files; a dashboard lists at most {MAX_FILES}"
        )));
    }

    let live = match &req.live {
        None | Some(Value::Null) => None,
        Some(value) => {
            let yaml = serde_yaml::to_value(value).map_err(|e| invalid(format!("live: {e}")))?;
            Some(manifest::parse_live_value(yaml, catalog).map_err(|e| invalid(e.to_string()))?)
        }
    };

    Ok((
        target,
        Plan {
            title: title.to_string(),
            pages,
            data,
            live,
        },
    ))
}

/// The `dashboard.yml` text: draft, private, every file listed.
fn manifest_text(
    slug: &str,
    title: &str,
    files: &[String],
    live: Option<&LivePlan>,
) -> Result<String, CreateError> {
    use serde_yaml::{Mapping, Value as Y};
    let mut publish = Mapping::new();
    publish.insert("state".into(), "draft".into());
    publish.insert("visibility".into(), "private".into());
    publish.insert(
        "files".into(),
        Y::Sequence(files.iter().map(|f| Y::from(f.as_str())).collect()),
    );
    let mut doc = Mapping::new();
    doc.insert("slug".into(), slug.into());
    doc.insert("title".into(), title.into());
    doc.insert("publish".into(), Y::Mapping(publish));
    if let Some((queries, fields, streams, control, args)) = live {
        let mut block = Mapping::new();
        // A block of streams alone needs no queries (P1, SW-DP-20).
        if streams.is_empty() || !matches!(queries, LiveQueries::List(q) if q.is_empty()) {
            block.insert(
                "queries".into(),
                serde_yaml::to_value(queries).map_err(|e| CreateError::Io(e.to_string()))?,
            );
        }
        if !streams.is_empty() {
            block.insert(
                "streams".into(),
                Y::Sequence(streams.iter().map(|s| Y::from(s.as_str())).collect()),
            );
        }
        if !fields.is_empty() {
            block.insert(
                "fields".into(),
                serde_yaml::to_value(fields).map_err(|e| CreateError::Io(e.to_string()))?,
            );
        }
        if !args.is_empty() {
            block.insert(
                "args".into(),
                serde_yaml::to_value(args).map_err(|e| CreateError::Io(e.to_string()))?,
            );
        }
        if *control {
            block.insert("control".into(), "owner".into());
        }
        doc.insert("live".into(), Y::Mapping(block));
    }
    let body =
        serde_yaml::to_string(&Y::Mapping(doc)).map_err(|e| CreateError::Io(e.to_string()))?;
    Ok(format!(
        "# CodeSense dashboard. Every file a page uses must be listed under publish.files;\n\
         # publish uploads only those. Publish with publish_dashboard, share with share_dashboard.\n\
         # The guide: get_live_data_contract, section hosted.\n{body}"
    ))
}

/// One page: the shared stylesheet and script, the nav, and a section per data file.
fn page_html(
    title: &str,
    page: &str,
    pages: &[String],
    data: &[String],
    live: Option<&str>,
) -> String {
    let heading = if page == ENTRY {
        escape_html(title)
    } else {
        format!(
            "{} · {}",
            escape_html(&page_label(page)),
            escape_html(title)
        )
    };
    let nav: String = pages
        .iter()
        .map(|p| {
            let label = if p == ENTRY {
                "Overview".to_string()
            } else {
                page_label(p)
            };
            if p == page {
                format!("    <strong>{}</strong>\n", escape_html(&label))
            } else {
                format!("    <a href=\"{p}\">{}</a>\n", escape_html(&label))
            }
        })
        .collect();
    let sections: String = data
        .iter()
        .map(|d| format!("  <section data-src=\"{d}\"></section>\n"))
        .collect();
    let live_section = match live {
        Some(query) if page == ENTRY => format!(
            "  <section id=\"live\" data-query=\"{}\"><h2>Live</h2></section>\n",
            escape_html(query)
        ),
        _ => String::new(),
    };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  \
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  \
         <title>{heading}</title>\n  <link rel=\"stylesheet\" href=\"{STYLE}\">\n</head>\n<body>\n  \
         <h1>{heading}</h1>\n  <nav>\n{nav}  </nav>\n{live_section}{sections}  \
         <script src=\"{SCRIPT}\"></script>\n</body>\n</html>\n"
    )
}

const STYLE_CSS: &str = r#":root { color-scheme: light dark; }
body {
  margin: 0;
  padding: 16px 20px;
  font: 14px/1.45 var(--cs-font-sans, system-ui, -apple-system, sans-serif);
  color: var(--cs-fg, CanvasText);
  background: var(--cs-bg, Canvas);
}
h1 { font-size: 20px; margin: 0 0 8px; }
h2 { font-size: 15px; margin: 0 0 8px; }
nav { display: flex; flex-wrap: wrap; gap: 12px; margin: 0 0 16px; }
a { color: var(--cs-link, LinkText); }
section {
  border: 1px solid var(--cs-border, rgba(128, 128, 128, 0.35));
  border-radius: 8px;
  padding: 12px;
  margin: 0 0 12px;
  overflow: auto;
}
table { border-collapse: collapse; width: 100%; }
th, td { text-align: left; padding: 4px 8px; border-bottom: 1px solid var(--cs-border, rgba(128, 128, 128, 0.35)); }
pre { margin: 0; white-space: pre-wrap; }
.muted { color: var(--cs-muted-fg, GrayText); }
.error { color: var(--cs-danger, #c0392b); }
"#;

/// The shared script. Data comes from listed files fetched relative to the
/// page; nothing else is reached. No bridge call: publish refuses one.
const APP_JS: &str = r#"// Written by create_dashboard. It works in the CodeSense viewer on this Mac and
// in the hosted viewer on codesense.ai:
// - data comes from files listed in dashboard.yml, fetched relative to the page,
//   for example fetch("data/summary.json");
// - pages link to each other by relative file name (href="runs.html");
// - no bridge calls: a published page cannot use the in-app page SDK.
// The hosted viewer reloads the page when a new version is published, so read
// the data once on load instead of polling.
(function () {
  "use strict";
  function el(tag, text, cls) {
    var n = document.createElement(tag);
    if (text !== undefined) n.textContent = text;
    if (cls) n.className = cls;
    return n;
  }
  function cell(v) {
    if (v === null || v === undefined) return "";
    return typeof v === "object" ? JSON.stringify(v) : String(v);
  }
  // The rows of an answer: a bare array, {items}, or a hosted list answer: exactly one of
  // {runs}, {sessions}, {messages}, {rows} plus only the cut markers (truncated, returned, total).
  // Any other shape is shown as it is.
  var HOSTED = ["runs", "sessions", "messages", "rows"];
  var MARKERS = ["truncated", "returned", "total"];
  function hostedList(value) {
    if (!value || typeof value !== "object" || Array.isArray(value)) return null;
    var keys = Object.keys(value).filter(function (k) { return MARKERS.indexOf(k) < 0; });
    return keys.length === 1 && HOSTED.indexOf(keys[0]) >= 0 && Array.isArray(value[keys[0]]) ? value[keys[0]] : null;
  }
  function render(box, value) {
    var hosted = hostedList(value);
    var rows = hosted || (Array.isArray(value) ? value : value && Array.isArray(value.items) ? value.items : null);
    if (hosted && value.truncated === true) {
      box.appendChild(el("p", "Latest " + rows.length + (typeof value.total === "number" ? " of " + value.total : ", not all shown"), "muted"));
    }
    if (value && value.generated_at) box.appendChild(el("p", "Generated " + value.generated_at, "muted"));
    if (value && value.note) box.appendChild(el("p", String(value.note), "muted"));
    var objects = rows && rows.every(function (r) { return r && typeof r === "object" && !Array.isArray(r); });
    if (rows && rows.length && objects) {
      var cols = [];
      rows.forEach(function (r) { Object.keys(r).forEach(function (k) { if (cols.indexOf(k) < 0) cols.push(k); }); });
      var table = el("table");
      var head = el("tr");
      cols.forEach(function (c) { head.appendChild(el("th", c)); });
      table.appendChild(head);
      rows.forEach(function (r) {
        var tr = el("tr");
        cols.forEach(function (c) { tr.appendChild(el("td", cell(r[c]))); });
        table.appendChild(tr);
      });
      box.appendChild(table);
    } else if (rows && !rows.length) {
      box.appendChild(el("p", "No rows yet.", "muted"));
    } else {
      box.appendChild(el("pre", JSON.stringify(value, null, 2)));
    }
  }
  function load(box) {
    var src = box.getAttribute("data-src");
    box.appendChild(el("h2", src));
    fetch(src)
      .then(function (r) { if (!r.ok) throw new Error("status " + r.status); return r.json(); })
      .then(function (value) { render(box, value); })
      .catch(function (e) { box.appendChild(el("p", "Could not load " + src + ": " + e.message, "error")); });
  }
  Array.prototype.forEach.call(document.querySelectorAll("[data-src]"), load);

  // Live answers (dashboard.yml live block). On codesense.ai the page posts the
  // request to its own window; the viewer forwards it, asks the owner's Mac
  // through codesense.ai, and answers from its parent window. Anywhere else
  // nothing answers, so a timeout says so.
  var liveBox = document.getElementById("live");
  if (!liveBox) return;
  var presence = el("p", "Owner's Mac: unknown", "muted");
  liveBox.appendChild(presence);
  var next = 0;
  var waiting = {};
  addEventListener("message", function (e) {
    var m = e.data;
    if (e.source !== parent || !m || m.cs !== "live") return;
    if (m.op === "presence") { presence.textContent = "Owner's Mac: " + m.mac; return; }
    var done = waiting[m.id];
    if (done) { delete waiting[m.id]; done(m); }
  });
  function ask(query, args) {
    return new Promise(function (ok, fail) {
      var id = "q" + ++next;
      var timer = setTimeout(function () { delete waiting[id]; fail(new Error("no answer")); }, 15000);
      waiting[id] = function (m) { clearTimeout(timer); if (m.ok) ok(m); else fail(new Error(m.error)); };
      // To this page's own window: the hosted viewer's frame script forwards it to the shell.
      postMessage({ cs: "live", v: 1, op: "query", id: id, query: query, args: args || {} }, "*");
    });
  }
  var query = liveBox.getAttribute("data-query");
  if (!query) return;
  ask(query, {})
    .then(function (m) {
      var age = new Date(m.answered_at * 1000).toLocaleString();
      liveBox.appendChild(el("p", (m.source === "cache" ? "Last answer from " : "Answered ") + age + " (" + query + ")", "muted"));
      render(liveBox, m.answer);
    })
    .catch(function (e) {
      liveBox.appendChild(el("p", "No live answer for " + query + " (" + e.message + "). Live answers come only on codesense.ai, from the owner's Mac.", "muted"));
    });
})();
"#;

/// The query the starter's live section asks: one that answers wherever the
/// folder is (`runs`, then `sessionUsage`, then `sessions`), so the example
/// works; `queryContext` only when it is the only one declared. None for
/// presence only.
fn starter_query(queries: &LiveQueries, catalog: &[QueryDef]) -> Option<String> {
    const ANYWHERE: [&str; 3] = ["runs", "sessionUsage", "sessions"];
    ANYWHERE
        .iter()
        .find(|name| queries.allows(name) && catalog.iter().any(|q| q.name == **name))
        .map(|name| name.to_string())
        .or_else(|| match queries {
            LiveQueries::List(names) => names.first().cloned(),
            LiveQueries::All => catalog.first().map(|q| q.name.to_string()),
        })
}

/// Seed the selected starter query from the already validated pin types.
fn starter_app(query: Option<&str>, pins: &manifest::LiveArgs) -> (String, Vec<String>) {
    use manifest::{Pin, Selector};
    let Some(pins) = query
        .and_then(|query| pins.get(query))
        .filter(|pins| !pins.is_empty())
    else {
        return (APP_JS.to_string(), Vec::new());
    };
    let mut args = serde_json::Map::new();
    let mut next_steps = Vec::new();
    for (arg, values) in pins {
        // A list argument (storyTotals.storyIds) is sent as a list of one.
        let list =
            super::catalog::LIST_ARGS.contains(&(query.unwrap_or_default(), String::as_str(arg)));
        let one = |value: serde_json::Value| if list { serde_json::json!([value]) } else { value };
        let literal = values.iter().find_map(|pin| match pin {
            Pin::Selector(_) => None,
            _ => Some(serde_json::to_value(pin).expect("pin literal serializes")),
        });
        if let Some(value) = literal {
            serde_json::Map::insert(&mut args, arg.clone(), one(value));
        } else {
            serde_json::Map::insert(&mut args, arg.clone(), one(serde_json::json!(format!("SET_{arg}"))));
            let scopes: Vec<String> = values
                .iter()
                .filter_map(|pin| match pin {
                    Pin::Selector(Selector::Epic { epic }) => {
                        Some(format!("a story in epic {epic}"))
                    }
                    Pin::Selector(Selector::Team { team }) => {
                        Some(format!("a story in team {team}"))
                    }
                    _ => None,
                })
                .collect();
            next_steps.push(format!("Set {arg} in app.js to {}.", scopes.join(" or ")));
        }
    }
    let args = serde_json::to_string(&args).expect("starter args serialize");
    let mut script = APP_JS.replace("ask(query, {})", &format!("ask(query, {args})"));
    if !next_steps.is_empty() {
        let hint = format!(
            "). {} Live answers come only on codesense.ai, from the owner's Mac.",
            next_steps.join(" ")
        );
        script = script.replace(
            r#""). Live answers come only on codesense.ai, from the owner's Mac.""#,
            &serde_json::to_string(&hint).expect("starter guidance serializes"),
        );
    }
    (script, next_steps)
}

/// Architecture: creation retains baseline writes; unreadable placement outcomes are missing.
pub(crate) fn create_for(
    request: &CreateRequest,
    catalog: &[QueryDef],
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Created, String> {
    let readable = caller.require_preparation_path(&request.folder).is_ok();
    crate::context_repo::caller_scope::readback(
        readable,
        create(request, catalog).map_err(|e| e.to_string()),
    )
}

/// Create the dashboard folder. Refuses (writing nothing) on any bad input.
pub fn create(req: &CreateRequest, catalog: &[QueryDef]) -> Result<Created, CreateError> {
    let (target, plan) = plan(req, catalog)?;
    let data_paths: Vec<String> = plan.data.iter().map(|(p, _)| p.clone()).collect();
    let mut writes: Vec<(String, Vec<u8>)> = Vec::new();
    let mut starter_next_steps = Vec::new();
    match req.template {
        Template::Starter => {
            let live_query = plan
                .live
                .as_ref()
                .map(|(queries, _, _, _, _)| starter_query(queries, catalog).unwrap_or_default());
            for page in &plan.pages {
                let html = page_html(
                    &plan.title,
                    page,
                    &plan.pages,
                    &data_paths,
                    live_query.as_deref(),
                );
                writes.push((page.clone(), html.into_bytes()));
            }
            writes.push((STYLE.to_string(), STYLE_CSS.as_bytes().to_vec()));
            let empty = manifest::LiveArgs::new();
            let pins = plan
                .live
                .as_ref()
                .map(|(_, _, _, _, args)| args)
                .unwrap_or(&empty);
            let (script, configuration) = starter_app(live_query.as_deref(), pins);
            starter_next_steps = configuration;
            writes.push((SCRIPT.to_string(), script.into_bytes()));
            writes.extend(plan.data.iter().cloned());
        }
        Template::Workspaces | Template::MissionControl | Template::BugTriage => {
            // One renderer, stylesheet and writer; mission control changes only its view data.
            let html = if req.template == Template::BugTriage {
                WORKSPACES_HTML.replace(
                    "data-template=\"workspaces\"",
                    if req.agent.is_some() {
                        "data-template=\"bug-triage\" data-work-it=\"true\""
                    } else {
                        "data-template=\"bug-triage\""
                    },
                )
            } else if req.template == Template::MissionControl {
                WORKSPACES_HTML.replace(
                    "data-template=\"workspaces\"",
                    "data-template=\"mission-control\"",
                )
            } else {
                WORKSPACES_HTML.to_string()
            }
            .replace("{{title}}", &escape_html(&plan.title));
            writes.push((ENTRY.to_string(), html.into_bytes()));
            writes.push((STYLE.to_string(), WORKSPACES_CSS.as_bytes().to_vec()));
            writes.push((SCRIPT.to_string(), WORKSPACES_JS.as_bytes().to_vec()));
            writes.extend(toolkit_files());
        }
    }
    let files: Vec<String> = writes.iter().map(|(path, _)| path.clone()).collect();
    // The manifest last: a folder with dashboard.yml is a dashboard.
    let mut yaml = manifest_text(&req.slug, &plan.title, &files, plan.live.as_ref())?;
    if req.template == Template::BugTriage {
        let actions = triage_actions(req.agent.as_deref())?;
        yaml.push_str(
            &serde_yaml::to_string(&serde_json::json!({"actions": actions}))
                .map_err(|e| invalid(e.to_string()))?,
        );
    }
    writes.push((MANIFEST_FILE.to_string(), yaml.into_bytes()));

    let folder = target.folder.clone();
    let mut done = Done::default();
    let result = write_all(&target, &writes, &mut done).and_then(|()| {
        // The same checks publish_dashboard runs, so the starter publishes as it is.
        let m = manifest::load_with(&folder, catalog)
            .map_err(|e| CreateError::Io(format!("the written dashboard.yml did not load: {e}")))?;
        payload::build(&m, Mode::Published, &FolderReader { folder: &folder }).map_err(|e| {
            CreateError::Io(format!("the written dashboard would not publish: {e}"))
        })?;
        Ok(())
    });
    if let Err(e) = result {
        undo(&done);
        return Err(e);
    }

    let live = plan
        .live
        .as_ref()
        .map(|(queries, fields, streams, control, args)| {
            let mut block =
                serde_json::json!({"queries": queries, "fields": fields, "streams": streams});
            if !args.is_empty() {
                block["args"] = serde_json::json!(args);
            }
            if *control {
                block["control"] = serde_json::json!("owner");
            }
            block
        });
    let mut warnings = starter_next_steps.clone();
    if req.template == Template::BugTriage && req.agent.is_none() {
        warnings.push("Work it is omitted. To add it here, declare actions.workIt in dashboard.yml with verb: launchAgent, who: owner, agent: <agent_list id>, prompt: Fix bug {id}, mode: headless, match: {type: bug}, max_per_hour: 5 and max_running: 2; add data-work-it=\"true\" to the body tag in index.html, then publish again.".into());
    }
    if req.context_team.is_none() {
        warnings.push(format!(
            "{} is not inside a team's context repo. Put a dashboard in a team's content folder: CodeSense may not show it there, and a published dashboard's queryContext has no team to read.",
            folder.display()
        ));
        if plan
            .live
            .as_ref()
            .is_some_and(|(q, _, _, _, _)| q.allows("queryContext"))
        {
            warnings.push(
                "live includes queryContext, which answers only for a dashboard inside a team's context repo; here every queryContext answer fails (answer_failed). Move the folder into a team's content folder, or drop queryContext.".into(),
            );
        }
        let agent_streams = plan.live.as_ref().is_some_and(|(_, _, streams, _, _)| {
            streams
                .iter()
                .any(|s| WORKSPACES_STREAMS.contains(&s.as_str()))
        });
        if agent_streams {
            warnings.push(
                "the workspaces, terminals and terminal streams list only the sessions of the team whose context repo holds the dashboard; here they end no_team. Move the folder into a team's content folder.".into(),
            );
        }
    }
    if req.template == Template::Starter
        && plan
            .live
            .as_ref()
            .is_some_and(|(_, _, streams, _, _)| !streams.is_empty())
    {
        warnings.push(format!(
            "the starter page does not use the declared streams; copy the toolkit ({TOOLKIT}, {XTERM_JS_PATH}, {XTERM_CSS_PATH}) into the folder and list them, or use template workspaces"
        ));
    }
    Ok(Created {
        folder: folder.display().to_string(),
        slug: req.slug.clone(),
        title: plan.title,
        state: "draft",
        visibility: "private",
        entry: ENTRY,
        files,
        pages: plan.pages,
        data: data_paths,
        live,
        template: match req.template {
            Template::Starter => "starter",
            Template::Workspaces => "workspaces",
            Template::MissionControl => "mission-control",
            Template::BugTriage => "bug-triage",
        },
        team: req
            .context_team
            .as_ref()
            .map(|(org, team)| serde_json::json!({"org": org, "team": team})),
        warnings,
        starter_next_steps,
    })
}

/// The toolkit and the pinned xterm.js, as a dashboard folder holds them.
pub fn toolkit_files() -> Vec<(String, Vec<u8>)> {
    vec![
        (TOOLKIT.to_string(), TOOLKIT_JS.as_bytes().to_vec()),
        (XTERM_JS_PATH.to_string(), XTERM_JS.as_bytes().to_vec()),
        (XTERM_CSS_PATH.to_string(), XTERM_CSS.as_bytes().to_vec()),
    ]
}

/// What one call created: folders (outermost first) and files. Undo removes
/// only these.
#[derive(Default)]
struct Done {
    dirs: Vec<PathBuf>,
    files: Vec<PathBuf>,
}

fn write_all(
    target: &Target,
    writes: &[(String, Vec<u8>)],
    done: &mut Done,
) -> Result<(), CreateError> {
    use std::io::Write;
    let io = |path: &Path, e: std::io::Error| CreateError::Io(format!("{}: {e}", path.display()));
    let mkdir = |dir: &Path, done: &mut Done| -> Result<(), CreateError> {
        match fs::create_dir(dir) {
            Ok(()) => {
                done.dirs.push(dir.to_path_buf());
                Ok(())
            }
            // Made by this call already, or the (empty) folder given.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
            Err(e) => Err(io(dir, e)),
        }
    };
    for dir in &target.missing {
        mkdir(dir, done)?;
    }
    for (rel, bytes) in writes {
        let path = target.folder.join(rel);
        if let Some(parent) = path.parent() {
            if parent != target.folder {
                mkdir(parent, done)?;
            }
        }
        // Never over an existing file: create_new fails if one appeared.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| io(&path, e))?;
        done.files.push(path.clone());
        file.write_all(bytes).map_err(|e| io(&path, e))?;
    }
    Ok(())
}

/// Best effort: remove only what this call created, files first, then its
/// folders from the innermost out (a folder that is not empty stays).
fn undo(done: &Done) {
    for path in &done.files {
        let _ = fs::remove_file(path);
    }
    for dir in done.dirs.iter().rev() {
        let _ = fs::remove_dir(dir);
    }
}

/// The steps after `create_dashboard`, for the tool's answer.
pub fn next_steps(created: &Created) -> Vec<String> {
    let folder = &created.folder;
    if matches!(created.template, "workspaces" | "mission-control") {
        return vec![
            format!("Publish: publish_dashboard {{\"dashboard\": \"{folder}\", \"visibility\": \"private\"}}. The page is published, but its live queries and streams stay paused until a person confirms them in the CodeSense app (the result says live: pending_confirmation)."),
            format!("Share: share_dashboard {{\"dashboard\": \"{folder}\", \"add\": [\"name@example.com\"]}}. Every invited viewer sees the team's agents and can watch each session's terminal, read-only. Terminal output is redacted (redaction recognises secrets by shape and labels, so it can't promise to hide every one)."),
            if created.live.as_ref().is_some_and(|live| live["control"] == "owner") {
                "Owner control is enabled. Sign in as the owner with a passkey or password and a second factor, unlock this dashboard, choose a terminal, then press Take control in the viewer's top bar. Invited viewers can only watch.".into()
            } else {
                "Owner control is off. To enable it, add control: owner to the live block in dashboard.yml; it is a wider block, so it waits until a person confirms it in the CodeSense app. Invited viewers can only watch.".into()
            },
            "Open it on a phone: it is built for one. In the CodeSense viewer on this Mac the page says live terminals work in the published dashboard.".into(),
            "Change the look in index.html, style.css and app.js; cs-live.js (the toolkit) and vendor/xterm.* stay as written. Rules and the toolkit API: get_live_data_contract, section hosted.".into(),
        ];
    }
    let data = created.data.join(", ");
    let mut steps = vec![
        format!(
            "Put real data in {data}: an agent or a routine rewrites the file (for example from session_usage). Keep the pages reading it with fetch, relative to the page. A new data file must be added to publish.files in dashboard.yml."
        ),
        format!("Check it on this Mac: open {folder}/index.html in CodeSense."),
        format!(
            "Publish: publish_dashboard {{\"dashboard\": \"{folder}\", \"visibility\": \"private\"}}. While CodeSense is open, a change to a listed file is pushed about 60 s later."
        ),
        format!(
            "Share: share_dashboard {{\"dashboard\": \"{folder}\", \"add\": [\"name@example.com\"]}}. Invite emails are reviewed before they are sent."
        ),
    ];
    if created.live.is_some() {
        steps.insert(
            2,
            "publish_dashboard publishes the page, but live data stays paused until a person confirms the live block in the CodeSense app (the result says live: pending_confirmation). After that, a narrower or removed live block in dashboard.yml takes effect within seconds; a wider one asks again while CodeSense is open.".into(),
        );
    }
    steps.splice(2..2, created.starter_next_steps.iter().cloned());
    steps.push("Rules, limits and what a hosted page can and cannot do: get_live_data_contract, section hosted.".into());
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard_publish::catalog::CATALOG;
    use crate::dashboard_publish::manifest::{PublishState, Visibility};

    fn request(folder: &Path) -> CreateRequest {
        CreateRequest {
            folder: folder.to_path_buf(),
            slug: "ai-usage".into(),
            title: "AI usage".into(),
            context_team: Some(("acme".into(), "emea".into())),
            ..Default::default()
        }
    }

    fn new_folder() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("dashboards").join("ai-usage");
        (root, folder)
    }

    fn read(folder: &Path, rel: &str) -> String {
        fs::read_to_string(folder.join(rel)).unwrap()
    }

    /// Every `href`, `src` and `data-src` value in a file.
    fn references(text: &str) -> Vec<String> {
        let re = regex::Regex::new(r#"(?:href|src)="([^"]*)""#).unwrap();
        re.captures_iter(text).map(|c| c[1].to_string()).collect()
    }

    #[test]
    fn bug_triage_declares_actions_and_ships_the_shared_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let req = CreateRequest {
            folder: dir.path().join("triage"),
            slug: "bug-triage".into(),
            title: "Bug triage".into(),
            template: Template::parse(Some("bug-triage")).unwrap(),
            agent: Some("builtin/codex".into()),
            ..Default::default()
        };
        let created = create(&req, super::super::catalog::CATALOG).unwrap();
        let manifest = manifest::load(&req.folder).unwrap();
        assert_eq!(manifest.actions.len(), 3);
        assert!(manifest.live_streams.iter().any(|s| s == "actions"));
        assert!(created.files.iter().any(|s| s == TOOLKIT));
        assert_eq!(read(&req.folder, SCRIPT), WORKSPACES_JS);
        let req = CreateRequest {
            folder: dir.path().join("no-agent"),
            agent: None,
            ..req
        };
        let created = create(&req, super::super::catalog::CATALOG).unwrap();
        assert!(!manifest::load(&req.folder)
            .unwrap()
            .actions
            .contains_key("workIt"));
        assert!(created.warnings.iter().any(|s| s.contains("workIt")));
    }

    #[test]
    fn writes_a_draft_manifest_that_lists_every_written_file() {
        let (_root, folder) = new_folder();
        let created = create(&request(&folder), CATALOG).unwrap();
        let m = manifest::load(&folder).unwrap();
        assert_eq!(m.slug, "ai-usage");
        assert_eq!(m.title, "AI usage");
        assert_eq!(m.state, PublishState::Draft);
        assert_eq!(m.visibility, Visibility::Private);
        assert_eq!(m.live, None);
        assert_eq!(
            m.files,
            vec!["index.html", "style.css", "app.js", "data/summary.json"]
        );
        assert_eq!(created.files, m.files);
        assert_eq!(created.entry, "index.html");
        // Nothing in the folder is left unlisted, except the manifest itself.
        let mut on_disk: Vec<String> = walk(&folder);
        on_disk.retain(|p| p != MANIFEST_FILE);
        on_disk.sort();
        let mut listed = m.files.clone();
        listed.sort();
        assert_eq!(on_disk, listed);
    }

    fn walk(folder: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![folder.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(
                        path.strip_prefix(folder)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        out
    }

    #[test]
    fn the_starter_passes_the_publish_checks() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.pages = vec!["runs.html".into(), "cost-by-day.html".into()];
        req.live = Some(serde_json::json!({"queries": ["sessions", "runs"]}));
        create(&req, CATALOG).unwrap();
        let m = manifest::load(&folder).unwrap();
        // Published mode refuses a bridge call, an over-limit file or an unlisted type.
        let built = payload::build(&m, Mode::Published, &FolderReader { folder: &folder }).unwrap();
        assert!(built.warnings.is_empty());
        for f in [
            "index.html",
            "runs.html",
            "cost-by-day.html",
            "app.js",
            "style.css",
        ] {
            let text = read(&folder, f);
            assert!(
                !text.contains("window.codesense") && !text.contains("codesense.act"),
                "{f} names the bridge"
            );
        }
    }

    #[test]
    fn pages_reference_only_listed_files_by_relative_path() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.pages = vec!["runs.html".into()];
        req.data = BTreeMap::from([
            (
                "runs".to_string(),
                serde_json::json!({"items": [{"id": 1}]}),
            ),
            ("usage".to_string(), serde_json::json!([])),
        ]);
        let created = create(&req, CATALOG).unwrap();
        for page in &created.pages {
            let refs = references(&read(&folder, page));
            assert!(!refs.is_empty());
            for r in refs {
                assert!(
                    created.files.contains(&r),
                    "{page} references {r:?}, which is not a listed file"
                );
            }
        }
        let index = read(&folder, "index.html");
        assert!(index.contains(r#"<a href="runs.html">"#));
        assert!(index.contains(r#"data-src="data/runs.json""#));
        assert!(index.contains(r#"data-src="data/usage.json""#));
        assert!(read(&folder, "runs.html").contains(r#"<a href="index.html">Overview</a>"#));
        let js = read(&folder, "app.js");
        assert!(js.contains("fetch(src)"));
        assert!(!js.contains("XMLHttpRequest"));
        assert!(!js.contains("localStorage"));
        // P13 (SW-DP-5c): a live request goes to the page's own window, never to parent.
        assert!(!js.contains("parent.postMessage"));
        assert!(js.contains(r#"postMessage({ cs: "live", v: 1, op: "query""#));
        assert!(!js.contains("http://") && !js.contains("https://"));
        assert_eq!(
            serde_json::from_str::<Value>(&read(&folder, "data/runs.json")).unwrap(),
            serde_json::json!({"items": [{"id": 1}]})
        );
    }

    #[test]
    fn refuses_a_relative_folder() {
        let mut req = request(Path::new("dashboards/ai-usage"));
        req.folder = PathBuf::from("dashboards/ai-usage");
        assert!(matches!(
            create(&req, CATALOG),
            Err(CreateError::Invalid(_))
        ));
        assert!(!Path::new("dashboards/ai-usage").exists());
    }

    #[test]
    fn refuses_a_folder_that_is_not_empty_and_leaves_it_alone() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("notes.md"), "mine").unwrap();
        let err = create(&request(root.path()), CATALOG).unwrap_err();
        assert!(err.to_string().contains("not empty"), "{err}");
        assert_eq!(walk(root.path()), vec!["notes.md".to_string()]);
    }

    #[test]
    fn accepts_an_existing_empty_folder() {
        let root = tempfile::tempdir().unwrap();
        create(&request(root.path()), CATALOG).unwrap();
        assert!(manifest::load(root.path()).is_ok());
    }

    #[test]
    fn refuses_a_file_or_a_symlink_at_the_folder_path() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("taken");
        fs::write(&file, "x").unwrap();
        assert!(matches!(
            create(&request(&file), CATALOG),
            Err(CreateError::Invalid(_))
        ));
        let target = root.path().join("real");
        fs::create_dir(&target).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            create(&request(&link), CATALOG),
            Err(CreateError::Invalid(_))
        ));
        assert!(fs::read_dir(&target).unwrap().next().is_none());
    }

    #[test]
    fn refuses_a_bad_slug_or_title_and_writes_nothing() {
        let (_root, folder) = new_folder();
        for slug in ["AB", "Has_Upper", "x".repeat(41).as_str()] {
            let mut req = request(&folder);
            req.slug = slug.into();
            assert!(
                matches!(create(&req, CATALOG), Err(CreateError::Invalid(_))),
                "{slug}"
            );
        }
        for title in ["   ", &"x".repeat(MAX_TITLE_CHARS + 1)] {
            let mut req = request(&folder);
            req.title = title.to_string();
            assert!(matches!(
                create(&req, CATALOG),
                Err(CreateError::Invalid(_))
            ));
        }
        assert!(!folder.exists());
    }

    #[test]
    fn a_title_with_yaml_and_html_characters_round_trips() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.title = "  Usage: <team> & \"cost\" # today  ".into();
        let created = create(&req, CATALOG).unwrap();
        assert_eq!(created.title, "Usage: <team> & \"cost\" # today");
        assert_eq!(manifest::load(&folder).unwrap().title, created.title);
        let index = read(&folder, "index.html");
        assert!(index.contains("<h1>Usage: &lt;team&gt; &amp; &quot;cost&quot; # today</h1>"));
    }

    #[test]
    fn refuses_bad_page_names() {
        let (_root, folder) = new_folder();
        for pages in [
            vec!["index.html"],
            vec!["sub/runs.html"],
            vec!["../runs.html"],
            vec!["Runs.html"],
            vec!["runs.htm"],
            vec!["runs.html", "runs.html"],
        ] {
            let mut req = request(&folder);
            req.pages = pages.iter().map(|p| p.to_string()).collect();
            assert!(
                matches!(create(&req, CATALOG), Err(CreateError::Invalid(_))),
                "{pages:?}"
            );
        }
        assert!(!folder.exists());
    }

    #[test]
    fn refuses_bad_data_names() {
        let (_root, folder) = new_folder();
        for name in ["", "../x", "a/b", "Usage", "x.json"] {
            let mut req = request(&folder);
            req.data = BTreeMap::from([(name.to_string(), serde_json::json!({}))]);
            assert!(
                matches!(create(&req, CATALOG), Err(CreateError::Invalid(_))),
                "{name:?}"
            );
        }
        assert!(!folder.exists());
    }

    #[test]
    fn refuses_more_files_than_a_dashboard_may_list() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.pages = (0..30).map(|i| format!("p{i}.html")).collect();
        req.data = (0..20)
            .map(|i| (format!("d{i}"), serde_json::json!({})))
            .collect();
        let err = create(&req, CATALOG).unwrap_err();
        assert!(err.to_string().contains("at most 50"), "{err}");
        // 30 + 17 + index, style, script = 50: allowed.
        req.data = (0..17)
            .map(|i| (format!("d{i}"), serde_json::json!({})))
            .collect();
        assert_eq!(create(&req, CATALOG).unwrap().files.len(), MAX_FILES);
    }

    #[test]
    fn refuses_a_data_file_over_the_size_limit() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        let big = "x".repeat(MAX_FILE_BYTES as usize);
        req.data = BTreeMap::from([("big".to_string(), serde_json::json!(big))]);
        let err = create(&req, CATALOG).unwrap_err();
        assert!(err.to_string().contains("limit"), "{err}");
        assert!(!folder.exists());
    }

    #[test]
    fn writes_a_live_block_checked_against_the_catalog() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.live = Some(serde_json::json!({
            "queries": ["runs", "queryContext"],
            "fields": {"queryContext": ["priority"]},
        }));
        let created = create(&req, CATALOG).unwrap();
        let m = manifest::load(&folder).unwrap();
        assert_eq!(
            m.live,
            Some(LiveQueries::list([
                "queryContext".to_string(),
                "runs".to_string()
            ]))
        );
        assert_eq!(m.live_fields["queryContext"], vec!["priority".to_string()]);
        assert_eq!(
            created.live,
            Some(serde_json::json!({
                "queries": ["queryContext", "runs"],
                "fields": {"queryContext": ["priority"]},
                "streams": [],
            }))
        );
        // The starter asks a query that answers anywhere, not the first by name.
        assert!(read(&folder, "index.html").contains(r#"<section id="live" data-query="runs">"#));
    }

    #[test]
    fn create_dashboard_writes_pins_and_refuses_bad_pins_before_writing() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.live = Some(
            serde_json::json!({"queries":["storyChanges"],"args":{"storyChanges":{"storyId":["SV-CDE-46"]}}}),
        );
        let created = create(&req, CATALOG).unwrap();
        let m = manifest::load(&folder).unwrap();
        assert_eq!(
            created.live.unwrap()["args"],
            serde_json::json!({"storyChanges":{"storyId":["SV-CDE-46"]}})
        );
        assert!(m.live_sha().is_some());
        for args in [
            serde_json::json!({"storyChanges":{"unknown":["a"]}}),
            serde_json::json!({"whyLine":{"repo":["a"]}}),
            serde_json::json!({"storyChanges":{"storyId":[]}}),
        ] {
            let (_root, folder) = new_folder();
            let mut req = request(&folder);
            req.live = Some(serde_json::json!({"queries":["storyChanges"],"args":args}));
            assert!(create(&req, CATALOG).is_err());
            assert!(!folder.exists());
        }
    }

    #[test]
    fn create_dashboard_round_trips_selector_maps() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.live = Some(
            serde_json::json!({"queries":["storyChanges"],"args":{"storyChanges":{"storyId":[{"epic":"SV-CDE"},{"team":"riverside"}]}}}),
        );
        let created = create(&req, CATALOG).unwrap();
        let manifest = manifest::load(&folder).unwrap();
        assert_eq!(
            serde_json::to_value(&manifest.live_args).unwrap(),
            created.live.unwrap()["args"]
        );
        let text = read(&folder, "dashboard.yml");
        assert!(!text.contains("!epic") && !text.contains("!team"));
    }

    fn starter_emitted_args(js: &str) -> Value {
        let call = regex::Regex::new(r"(?m)^  ask\(query, (\{[^\n]*\})\)$").unwrap();
        serde_json::from_str(&call.captures(js).expect("generated starter request")[1]).unwrap()
    }

    #[test]
    fn starter_literal_pin_requests_pass_the_real_pin_gate() {
        for (query, pins, expected) in [
            (
                "runs",
                serde_json::json!({"limit":[5,6]}),
                serde_json::json!({"limit":5}),
            ),
            (
                "whyLine",
                serde_json::json!({"repo":["acme/api"],"start":[5],"rev":["HEAD"]}),
                serde_json::json!({"repo":"acme/api","start":5,"rev":"HEAD"}),
            ),
            (
                "storyChanges",
                serde_json::json!({"storyId":[{"epic":"SV-CDE"},"SV-CDE-46"]}),
                serde_json::json!({"storyId":"SV-CDE-46"}),
            ),
        ] {
            let (_root, folder) = new_folder();
            let mut req = request(&folder);
            req.live = Some(serde_json::json!({"queries":[query],"args":{query:pins}}));
            let created = create(&req, CATALOG).unwrap();
            let args = starter_emitted_args(&read(&folder, SCRIPT));
            assert_eq!(args, expected);
            let manifest = manifest::load(&folder).unwrap();
            assert!(manifest::pin_allows(
                &manifest.live_args,
                query,
                &args,
                |_, _| false
            ));
            assert!(!created.warnings.iter().any(|w| w.starts_with("Set ")));
        }
    }

    #[test]
    fn selector_only_starter_names_placeholder_error_and_next_steps() {
        for (selectors, description) in [
            (
                serde_json::json!([{"epic":"SV-CDE"}]),
                "a story in epic SV-CDE",
            ),
            (serde_json::json!([{"team":"emea"}]), "a story in team emea"),
        ] {
            let (_root, folder) = new_folder();
            let mut req = request(&folder);
            req.live = Some(
                serde_json::json!({"queries":["storyChanges"],"args":{"storyChanges":{"storyId":selectors,"limit":[5]}}}),
            );
            let created = create(&req, CATALOG).unwrap();
            let js = read(&folder, SCRIPT);
            assert_eq!(
                starter_emitted_args(&js),
                serde_json::json!({"storyId":"SET_storyId","limit":5})
            );
            let guidance = format!("Set storyId in app.js to {description}.");
            assert!(created.warnings.contains(&guidance));
            assert!(next_steps(&created).contains(&guidance));
            let rendered_error = js.rsplit(".catch(function (e)").next().unwrap();
            assert!(
                rendered_error.contains(&guidance),
                "the rendered error must name the arg to set"
            );
        }
    }

    #[test]
    fn unpinned_starter_keeps_original_bytes() {
        use sha2::{Digest, Sha256};
        for live in [
            None,
            Some(serde_json::json!({"queries":["runs"]})),
            Some(serde_json::json!({"queries":[]})),
            Some(
                serde_json::json!({"queries":["runs","storyChanges"],"args":{"storyChanges":{"storyId":[{"epic":"SV-CDE"}]}}}),
            ),
        ] {
            let (_root, folder) = new_folder();
            let mut req = request(&folder);
            req.live = live;
            let created = create(&req, CATALOG).unwrap();
            let hash: String = Sha256::digest(read(&folder, SCRIPT).as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(
                hash,
                "2ec6bf722f31e912648ac15839d03b9ec3210b84d08a02a583b016e88966c279"
            );
            assert!(!created.warnings.iter().any(|w| w.starts_with("Set ")));
        }
    }

    #[test]
    fn writes_live_all_and_presence_only() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.live = Some(serde_json::json!({"queries": "all"}));
        create(&req, CATALOG).unwrap();
        assert_eq!(
            manifest::load(&folder).unwrap().live,
            Some(LiveQueries::All)
        );

        let (_root2, folder2) = new_folder();
        let mut req = request(&folder2);
        req.live = Some(serde_json::json!({"queries": []}));
        create(&req, CATALOG).unwrap();
        assert_eq!(
            manifest::load(&folder2).unwrap().live,
            Some(LiveQueries::List(vec![]))
        );
        assert!(read(&folder2, "index.html").contains(r#"<section id="live" data-query="">"#));
    }

    #[test]
    fn refuses_an_unknown_live_query_or_field_and_writes_nothing() {
        let (_root, folder) = new_folder();
        for live in [
            serde_json::json!({"queries": ["secrets"]}),
            serde_json::json!({"queries": "everything"}),
            serde_json::json!({"queries": ["runs"], "fields": {"runs": ["artifact_ref"]}}),
            serde_json::json!({"queries": ["runs"], "fields": {"sessions": ["workspace"]}}),
            serde_json::json!({"fields": {}}),
        ] {
            let mut req = request(&folder);
            req.live = Some(live.clone());
            assert!(
                matches!(create(&req, CATALOG), Err(CreateError::Invalid(_))),
                "{live}"
            );
        }
        assert!(!folder.exists());
    }

    #[test]
    fn without_a_live_block_the_pages_have_no_live_section() {
        let (_root, folder) = new_folder();
        create(&request(&folder), CATALOG).unwrap();
        assert!(!read(&folder, "index.html").contains("id=\"live\""));
        assert!(!read(&folder, MANIFEST_FILE).contains("live:"));
    }

    #[test]
    fn next_steps_name_publish_and_share_with_the_folder() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.live = Some(serde_json::json!({"queries": []}));
        let created = create(&req, CATALOG).unwrap();
        let steps = next_steps(&created).join("\n");
        let shown = created.folder.clone();
        assert!(steps.contains(&format!("publish_dashboard {{\"dashboard\": \"{shown}\"")));
        assert!(steps.contains(&format!("share_dashboard {{\"dashboard\": \"{shown}\"")));
        // SV-PBF-286: publishing no longer turns live data on; a person confirms it.
        assert!(steps.contains("live data stays paused until a person confirms"));
        assert!(steps.contains("pending_confirmation"));
        assert!(steps.contains("get_live_data_contract"));
    }

    #[test]
    fn refuses_a_dot_dot_path_that_leads_to_an_existing_dashboard() {
        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("existing-dashboard");
        create(&request(&existing), CATALOG).unwrap();
        let snapshot = |dir: &Path| -> Vec<(String, String)> {
            let mut files = walk(dir);
            files.sort();
            files
                .into_iter()
                .map(|p| (p.clone(), read(dir, &p)))
                .collect()
        };
        let before = snapshot(&existing);
        for path in [
            root.path()
                .join("new")
                .join("..")
                .join("existing-dashboard"),
            root.path().join(".").join("existing-dashboard"),
            existing.join("..").join("other"),
        ] {
            let mut req = request(&path);
            req.title = "Overwrite".into();
            let err = create(&req, CATALOG).unwrap_err();
            assert!(
                err.to_string().contains(". or .."),
                "{}: {err}",
                path.display()
            );
        }
        assert_eq!(snapshot(&existing), before);
        assert!(!root.path().join("new").exists());
        assert!(!root.path().join("other").exists());
    }

    #[test]
    fn a_new_folder_under_a_symlinked_parent_is_written_at_the_resolved_path() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, root.path().join("alias")).unwrap();
        let created = create(&request(&root.path().join("alias").join("dash")), CATALOG).unwrap();
        let resolved = fs::canonicalize(&real).unwrap().join("dash");
        assert_eq!(created.folder, resolved.display().to_string());
        assert!(manifest::load(&resolved).is_ok());
    }

    #[test]
    fn writing_never_replaces_a_file_and_undo_removes_only_what_it_made() {
        // A file that appears between the check and the write (a race) is
        // neither replaced nor removed.
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("dash");
        fs::create_dir_all(folder.join("data")).unwrap();
        fs::write(folder.join("data/summary.json"), "theirs").unwrap();
        let target = Target {
            folder: folder.clone(),
            missing: vec![],
        };
        let writes = vec![
            ("index.html".to_string(), b"<p>a</p>".to_vec()),
            ("data/summary.json".to_string(), b"{}".to_vec()),
        ];
        let mut done = Done::default();
        let err = write_all(&target, &writes, &mut done).unwrap_err();
        assert!(matches!(err, CreateError::Io(_)), "{err}");
        assert_eq!(done.files, vec![folder.join("index.html")]);
        assert!(done.dirs.is_empty(), "data/ was there already");
        undo(&done);
        assert!(!folder.join("index.html").exists());
        assert_eq!(read(&folder, "data/summary.json"), "theirs");
    }

    #[test]
    fn undo_removes_the_folders_it_made_and_keeps_the_parent() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("a").join("b");
        let target = resolve_target(&folder).unwrap();
        assert_eq!(target.missing.len(), 2);
        let mut done = Done::default();
        write_all(
            &target,
            &[("data/x.json".to_string(), b"{}".to_vec())],
            &mut done,
        )
        .unwrap();
        assert_eq!(done.dirs.len(), 3);
        undo(&done);
        assert!(!root.path().join("a").exists());
        assert!(root.path().exists());
    }

    #[test]
    fn warns_when_the_folder_is_outside_a_team_context_repo() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.context_team = None;
        req.live = Some(serde_json::json!({"queries": ["queryContext", "runs"]}));
        let created = create(&req, CATALOG).unwrap();
        assert_eq!(created.team, None);
        assert_eq!(created.warnings.len(), 2, "{:?}", created.warnings);
        assert!(created.warnings[1].contains("queryContext"));

        let (_root2, folder2) = new_folder();
        let mut req = request(&folder2);
        req.context_team = None;
        let created = create(&req, CATALOG).unwrap();
        assert_eq!(created.warnings.len(), 1);
        assert!(!created.warnings[0].contains("live includes"));

        let (_root3, folder3) = new_folder();
        let mut req = request(&folder3);
        req.live = Some(serde_json::json!({"queries": "all"}));
        let created = create(&req, CATALOG).unwrap();
        assert!(created.warnings.is_empty());
        assert_eq!(
            created.team,
            Some(serde_json::json!({"org": "acme", "team": "emea"}))
        );
    }

    #[test]
    fn the_starter_live_example_uses_a_query_that_answers_anywhere() {
        let list = |names: &[&str]| LiveQueries::list(names.iter().map(|n| n.to_string()));
        assert_eq!(
            starter_query(&LiveQueries::All, CATALOG).as_deref(),
            Some("runs")
        );
        assert_eq!(
            starter_query(&list(&["queryContext", "sessionUsage"]), CATALOG).as_deref(),
            Some("sessionUsage")
        );
        assert_eq!(
            starter_query(&list(&["queryContext"]), CATALOG).as_deref(),
            Some("queryContext")
        );
        assert_eq!(starter_query(&list(&[]), CATALOG), None);
    }

    // ---- SW-DP-21 (template workspaces) and SW-DP-24a (toolkit) ----

    /// SW-DP-24 AC-6: mission control is a publishable extension of Workspaces.
    #[test]
    fn mission_control_passes_publish_checks_without_enabling_control() {
        let (_root, folder) = new_folder();
        let req = CreateRequest {
            template: Template::parse(Some("mission-control")).unwrap(),
            ..request(&folder)
        };
        let created = create(&req, CATALOG).unwrap();
        assert_eq!(created.template, "mission-control");
        let m = manifest::load(&folder).unwrap();
        assert_eq!(
            m.live,
            Some(LiveQueries::List(vec!["inbox".into(), "sessions".into()]))
        );
        assert_eq!(m.live_streams, ["inbox", "terminal", "terminals"]);
        assert_eq!(m.live_fields["inbox"], ["text"]);
        assert!(!read(&folder, MANIFEST_FILE).contains("control:"));
        payload::build(&m, Mode::Published, &FolderReader { folder: &folder }).unwrap();
        assert!(read(&folder, ENTRY).contains("data-template=\"mission-control\""));
        assert_eq!(read(&folder, SCRIPT), WORKSPACES_JS);
        assert_eq!(read(&folder, STYLE), WORKSPACES_CSS);
        assert_eq!(read(&folder, TOOLKIT), TOOLKIT_JS);
        for reference in references(&read(&folder, ENTRY)) {
            assert!(m.files.contains(&reference), "{reference} not listed");
        }
    }

    #[test]
    fn create_dashboard_accepts_explicit_control_for_live_templates() {
        let params = serde_json::json!({
            "folder": "/tmp/mission-control", "slug": "mission-control", "title": "Mission control",
            "template": "mission-control", "control": true
        });
        let got =
            serde_json::from_value::<crate::mcp::handler::CreateDashboardParams>(params.clone())
                .unwrap();
        assert!(got.control);
        for wrong in [
            serde_json::json!("true"),
            serde_json::json!(1),
            serde_json::Value::Null,
        ] {
            let mut bad = params.clone();
            bad["control"] = wrong;
            assert!(
                serde_json::from_value::<crate::mcp::handler::CreateDashboardParams>(bad).is_err()
            );
        }
        let mut omitted = params;
        omitted.as_object_mut().unwrap().remove("control");
        assert!(
            !serde_json::from_value::<crate::mcp::handler::CreateDashboardParams>(omitted)
                .unwrap()
                .control
        );
    }

    #[test]
    fn live_templates_enable_control_only_when_explicitly_requested() {
        for template in [Template::Workspaces, Template::MissionControl] {
            for control in [false, true] {
                let (_root, folder) = new_folder();
                let created = create(
                    &CreateRequest {
                        template,
                        control,
                        ..request(&folder)
                    },
                    CATALOG,
                )
                .unwrap();
                let m = manifest::load(&folder).unwrap();
                assert_eq!(m.live_control, control);
                assert_eq!(
                    read(&folder, MANIFEST_FILE).contains("control: owner"),
                    control
                );
                assert_eq!(
                    created.live.as_ref().unwrap().get("control").is_some(),
                    control
                );
                payload::build(&m, Mode::Published, &FolderReader { folder: &folder }).unwrap();
                assert!(next_steps(&created).iter().any(|s| s.contains(if control {
                    "Take control"
                } else {
                    "control is off"
                })));
            }
        }
    }

    #[test]
    fn live_templates_reject_conflicting_inputs_before_writing() {
        for template in [Template::Workspaces, Template::MissionControl] {
            for field in ["pages", "data", "live"] {
                let (_root, folder) = new_folder();
                let mut req = CreateRequest {
                    template,
                    control: true,
                    ..request(&folder)
                };
                match field {
                    "pages" => req.pages = vec!["extra.html".into()],
                    "data" => {
                        req.data.insert("extra".into(), serde_json::json!({}));
                    }
                    _ => req.live = Some(serde_json::json!({"control": "owner"})),
                }
                assert!(matches!(
                    create(&req, CATALOG),
                    Err(CreateError::Invalid(_))
                ));
                assert!(!folder.exists());
            }
        }
        let (_root, folder) = new_folder();
        assert!(create(
            &CreateRequest {
                control: true,
                ..request(&folder)
            },
            CATALOG
        )
        .is_err());
        assert!(!folder.exists());
    }

    fn workspaces_request(folder: &Path) -> CreateRequest {
        CreateRequest {
            template: Template::Workspaces,
            title: "Team <agents>".into(),
            ..request(folder)
        }
    }

    /// SW-DP-21 AC-11: the Workspaces dashboard passes the publish checks,
    /// declares the three streams (and no queries), lists every file it writes,
    /// ships the toolkit and xterm.js, and references only listed files.
    #[test]
    fn the_workspaces_template_passes_the_publish_checks() {
        let (_root, folder) = new_folder();
        let created = create(&workspaces_request(&folder), CATALOG).unwrap();
        assert_eq!(created.template, "workspaces");
        let m = manifest::load(&folder).unwrap();
        assert_eq!(
            m.files,
            [
                "index.html",
                "style.css",
                "app.js",
                "cs-live.js",
                "vendor/xterm.js",
                "vendor/xterm.css"
            ]
        );
        assert_eq!(m.live, Some(LiveQueries::List(vec![])));
        assert_eq!(m.live_streams, ["terminal", "terminals", "workspaces"]);
        let yml = read(&folder, MANIFEST_FILE);
        assert!(!yml.contains("queries"), "{yml}");
        payload::build(&m, Mode::Published, &FolderReader { folder: &folder }).unwrap();
        let mut on_disk = walk(&folder);
        on_disk.retain(|p| p != MANIFEST_FILE);
        on_disk.sort();
        let mut listed = m.files.clone();
        listed.sort();
        assert_eq!(on_disk, listed);
        let html = read(&folder, "index.html");
        assert!(html.contains("<title>Team &lt;agents&gt;</title>"));
        for r in references(&html) {
            assert!(m.files.contains(&r), "{r} is not listed");
        }
        // xterm.js loads before the toolkit, the toolkit before the page script.
        let at = |s: &str| html.find(s).unwrap();
        assert!(
            at("vendor/xterm.js") < at("cs-live.js") && at("cs-live.js") < at("src=\"app.js\"")
        );
        assert_eq!(read(&folder, "cs-live.js"), TOOLKIT_JS);
        assert!(
            created.live.as_ref().unwrap()["streams"]
                == serde_json::json!(["terminal", "terminals", "workspaces"])
        );
        assert!(next_steps(&created)
            .iter()
            .any(|s| s.contains("Terminal output is redacted")));
    }

    /// SV-PBF-286: the workspaces and mission-control next steps say live data
    /// waits for a person to confirm it, and that adding control asks again.
    #[test]
    fn the_live_templates_next_steps_say_a_person_confirms_live_data() {
        for template in ["workspaces", "mission-control"] {
            let (_root, folder) = new_folder();
            let req = CreateRequest {
                template: Template::parse(Some(template)).unwrap(),
                ..request(&folder)
            };
            let steps = next_steps(&create(&req, CATALOG).unwrap()).join("\n");
            assert!(
                steps.contains("stay paused until a person confirms them"),
                "{template}"
            );
            assert!(steps.contains("live: pending_confirmation"), "{template}");
            assert!(
                steps.contains("it is a wider block, so it waits until a person confirms it"),
                "{template}"
            );
            assert!(!steps.contains("no prompt"), "{template}");
        }
    }

    /// SW-DP-21: the template writes its own page and live block; extra inputs
    /// are refused, as is an unknown template.
    #[test]
    fn the_workspaces_template_takes_no_pages_data_or_live() {
        let (_root, folder) = new_folder();
        let mut req = workspaces_request(&folder);
        req.pages = vec!["more.html".into()];
        assert!(matches!(
            create(&req, CATALOG),
            Err(CreateError::Invalid(_))
        ));
        let mut req = workspaces_request(&folder);
        req.live = Some(serde_json::json!({"queries": []}));
        assert!(matches!(
            create(&req, CATALOG),
            Err(CreateError::Invalid(_))
        ));
        assert!(!folder.exists());
        assert_eq!(Template::parse(None).unwrap(), Template::Starter);
        assert_eq!(
            Template::parse(Some("workspaces")).unwrap(),
            Template::Workspaces
        );
        assert!(Template::parse(Some("unknown")).is_err());
    }

    /// A workspaces dashboard outside a team's context repo is warned about: its streams end no_team.
    #[test]
    fn the_workspaces_template_warns_outside_a_team() {
        let (_root, folder) = new_folder();
        let mut req = workspaces_request(&folder);
        req.context_team = None;
        let created = create(&req, CATALOG).unwrap();
        assert!(created.warnings.iter().any(|w| w.contains("no_team")));
    }

    /// The starter can declare streams too, by the same rules as dashboard.yml.
    #[test]
    fn the_starter_writes_declared_streams() {
        let (_root, folder) = new_folder();
        let mut req = request(&folder);
        req.live = Some(serde_json::json!({"queries": ["runs"], "streams": ["terminals"]}));
        create(&req, CATALOG).unwrap();
        let (_root3, folder3) = new_folder();
        let mut req3 = request(&folder3);
        req3.live = Some(serde_json::json!({"streams": ["terminals"]}));
        let in_team = create(&req3, CATALOG).unwrap();
        assert!(in_team.warnings.iter().any(|w| w.contains(TOOLKIT)));
        assert!(!in_team.warnings.iter().any(|w| w.contains("no_team")));
        let created_no_team = {
            let (_root2, folder2) = new_folder();
            let mut req2 = request(&folder2);
            req2.live = Some(serde_json::json!({"streams": ["terminals"]}));
            req2.context_team = None;
            create(&req2, CATALOG).unwrap()
        };
        let m = manifest::load(&folder).unwrap();
        assert_eq!(m.live_streams, ["terminals"]);
        assert_eq!(m.live, Some(LiveQueries::List(vec!["runs".into()])));
        // Warned: the starter page does not use streams, and agent streams need a team.
        assert!(created_no_team.warnings.iter().any(|w| w.contains(TOOLKIT)));
        assert!(created_no_team
            .warnings
            .iter()
            .any(|w| w.contains("no_team")));
    }

    /// SW-DP-24 AC-1: the contract's helper is the toolkit's text, and
    /// create_dashboard writes the same bytes.
    #[test]
    fn the_contract_helper_is_the_toolkit() {
        let c = crate::mcp::live_data_authoring::local_contract_payload();
        assert_eq!(c["hosted"]["helper"].as_str().unwrap(), TOOLKIT_JS);
        assert!(TOOLKIT_JS.starts_with(&format!("// cs-live {TOOLKIT_VERSION}\n")));
        assert!(
            c["hosted"]["toolkit"]["api"]["CS.watchTerminal(handle, element, opts)"].is_string()
        );
    }

    /// SW-DP-24 AC-2: the toolkit speaks only the page API: no network, no bridge.
    #[test]
    fn the_toolkit_makes_no_network_or_bridge_call() {
        for banned in [
            "fetch(",
            "XMLHttpRequest",
            "WebSocket",
            "codesense.",
            "parent.postMessage",
            "EventSource",
            "localStorage",
        ] {
            assert!(!TOOLKIT_JS.contains(banned), "cs-live.js contains {banned}");
        }
        assert!(TOOLKIT_JS.contains("window.postMessage(msg, \"*\")"));
        for banned in [
            "fetch(",
            "XMLHttpRequest",
            "WebSocket",
            "codesense.act",
            "window.codesense",
        ] {
            assert!(!WORKSPACES_JS.contains(banned), "app.js contains {banned}");
        }
    }

    /// SV-INP-29: the embed toolkit is publishable as it stands: it carries its version
    /// comment, passes the bridge-call scan that refuses pages, and has no network call.
    #[test]
    fn the_embed_toolkit_publishes_clean_and_makes_no_network_call() {
        assert!(EMBED_TOOLKIT_JS.starts_with(&format!("// cs-embed {EMBED_TOOLKIT_VERSION}\n")));
        let hits = payload::scan_bridge_calls(EMBED_TOOLKIT, EMBED_TOOLKIT_JS.as_bytes());
        assert!(hits.is_empty(), "cs-embed.js trips the bridge scan: {hits:?}");
        for banned in ["fetch(", "XMLHttpRequest", "WebSocket", "EventSource", "localStorage", "window.top"] {
            assert!(!EMBED_TOOLKIT_JS.contains(banned), "cs-embed.js contains {banned}");
        }
    }

    /// SW-DP-24 AC-5: the vendored xterm.js is the app's pinned @xterm/xterm, with its licence.
    #[test]
    fn the_vendored_xterm_is_the_apps_pinned_version() {
        let pkg: serde_json::Value =
            serde_json::from_str(include_str!("../../../package.json")).unwrap();
        let pinned = pkg["dependencies"]["@xterm/xterm"]
            .as_str()
            .or_else(|| pkg["devDependencies"]["@xterm/xterm"].as_str())
            .unwrap();
        assert_eq!(pinned, XTERM_VERSION);
        for file in [XTERM_JS, XTERM_CSS] {
            assert!(file.starts_with(&format!("/*! @xterm/xterm {XTERM_VERSION} ")));
            assert!(file.contains("Copyright (c) 2017-2019, The xterm.js authors"));
            assert!(file.contains("Permission is hereby granted, free of charge"));
        }
        // K8: each well under 5 MB.
        assert!((XTERM_JS.len() as u64) < MAX_FILE_BYTES);
        assert!(XTERM_JS.contains("e.Terminal="));
    }
}

fn triage_actions(agent: Option<&str>) -> Result<super::actions::Actions, CreateError> {
    let mut value = serde_json::json!({
        "wontFix": {"verb": "setFields", "who": "owner", "fields": {"status": "wontfix"}, "match": {"type": "bug"}},
        "needsInfo": {"verb": "setFields", "who": "owner", "fields": {"status": "needs-info"}, "match": {"type": "bug"}}
    });
    if let Some(agent) = agent {
        value["workIt"] = serde_json::json!({"verb": "launchAgent", "who": "owner", "agent": agent, "prompt": "Fix bug {id}. Read its file first, then signal_done.", "mode": "headless", "match": {"type": "bug"}, "max_per_hour": 5, "max_running": 2});
    }
    let actions = serde_json::from_value(value).map_err(|e| invalid(format!("actions: {e}")))?;
    super::actions::validate(&actions).map_err(invalid)?;
    Ok(actions)
}
