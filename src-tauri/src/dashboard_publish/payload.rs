// SPDX-License-Identifier: MIT
//! The payload builder: the only value that may leave the Mac for a dashboard
//! (DR-14, "the payload is the only exit"). It reads the allowlisted files and
//! nothing else, runs the bridge-call scan on page code, enforces the K8 size
//! caps, and hands the watcher `{files, manifest_sha}` (SW-PB-22a AC-2, AC-4,
//! AC-5).

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

use super::manifest::{
    content_type_for, normalize_rel, reject_hard_links, reject_links, LiveEmbeds, Manifest, MAX_FILES,
};
use super::state::PublishRecord;

/// K8: 5 MB per file. Read as 5 MiB.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// K8: 25 MB per version. Read as 25 MiB.
pub const MAX_VERSION_BYTES: u64 = 25 * 1024 * 1024;

/// The generated file that carries the page's embed grant (SV-INP-29). It is
/// built from the manifest's `live.embeds`, never read from the folder, and no
/// listed file may take its name. A page with no `live.embeds` gets no such
/// file, and the embed forwarder treats a missing file as "deny everything".
pub const EMBED_GRANT_FILE: &str = "cs-embeds.json";

/// The bytes of [`EMBED_GRANT_FILE`] for `embeds` saved under `sha`, or `None` when
/// they grant nothing. `sha` names the block the grant came from.
fn grant_bytes(embeds: &LiveEmbeds, sha: &str) -> Option<Vec<u8>> {
    if embeds.is_empty() {
        return None;
    }
    let grant = serde_json::json!({
        "v": 1,
        "sha": sha,
        "queries": embeds.queries,
        "streams": embeds.streams,
        "fields": embeds.fields,
        "run": embeds.run,
    });
    Some(serde_json::to_vec(&grant).expect("embed grant serializes"))
}

/// The grant the manifest asks for, as if it were approved. Validation builds use
/// it (they upload nothing); anything that uploads builds with [`build_approved`].
pub fn embed_grant(manifest: &Manifest) -> Option<Vec<u8>> {
    grant_bytes(&manifest.live_embeds, &manifest.live_sha()?)
}

/// Where the generated grant comes from.
enum GrantSource<'a> {
    /// The manifest as written (validation only).
    Manifest,
    /// The block the record saved for this manifest. Without a saved block whose sha
    /// matches the manifest's current one (an unapproved edit), there is no grant file,
    /// and a page with no grant file runs nothing.
    Saved(&'a PublishRecord),
}

/// `draft` only warns on bridge calls. `published` refuses them (AC-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Draft,
    Published,
}

/// How the builder reads allowlisted files. The real implementation reads the
/// folder; tests wrap it to record which files were opened (AC-2).
pub trait FileReader {
    fn len(&self, rel: &str) -> io::Result<u64>;
    fn read(&self, rel: &str) -> io::Result<Vec<u8>>;
}

/// Reads files from a dashboard folder on disk.
pub struct FolderReader<'a> {
    pub folder: &'a Path,
}

impl FolderReader<'_> {
    /// Refuses a link, or a hard-linked file, swapped in after validation.
    fn guard(&self, rel: &str) -> io::Result<()> {
        reject_links(self.folder, rel)
            .and_then(|()| reject_hard_links(self.folder, rel))
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))
    }
}

impl FileReader for FolderReader<'_> {
    fn len(&self, rel: &str) -> io::Result<u64> {
        self.guard(rel)?;
        fs::metadata(self.folder.join(rel)).map(|m| m.len())
    }

    fn read(&self, rel: &str) -> io::Result<Vec<u8>> {
        self.guard(rel)?;
        fs::read(self.folder.join(rel))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// `{files, manifest_sha}`: the value 22e pushes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// Sorted by path.
    pub files: Vec<PayloadFile>,
    /// sha256 over sorted paths and bytes (DR-14).
    pub manifest_sha: String,
}

/// One bridge-call hit: the owner sees the file and the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub payload: Payload,
    /// Bridge-call hits in `draft` (in `published` they are an error).
    pub warnings: Vec<Finding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    Unreadable { path: String, reason: String },
    TooLarge { path: String, size: u64 },
    VersionTooLarge { size: u64 },
    BridgeCalls(Vec<Finding>),
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PayloadError::Unreadable { path, reason } => {
                write!(
                    f,
                    "file {path:?} could not be read: {reason}; nothing was published"
                )
            }
            PayloadError::TooLarge { path, size } => write!(
                f,
                "file {path:?} is {size} bytes; the limit is {MAX_FILE_BYTES} per file"
            ),
            PayloadError::VersionTooLarge { size } => write!(
                f,
                "the version is {size} bytes; the limit is {MAX_VERSION_BYTES} per version"
            ),
            PayloadError::BridgeCalls(hits) => {
                write!(
                    f,
                    "published pages call the codesense bridge; remove these calls:"
                )?;
                for h in hits {
                    write!(f, " {}:{}", h.path, h.line)?;
                }
                Ok(())
            }
        }
    }
}

/// Build the payload for a validated manifest.
pub fn build(
    manifest: &Manifest,
    mode: Mode,
    reader: &dyn FileReader,
) -> Result<Built, PayloadError> {
    build_from(manifest, mode, reader, GrantSource::Manifest)
}

/// The payload to upload: like [`build`], but the generated `cs-embeds.json` comes
/// from the live block `record` saved for this manifest, only while its sha matches
/// the manifest's (`approval::approved_block`). An edit that is not approved yet
/// widens nothing: the page keeps no grant until the approval follows the edit.
pub fn build_approved(
    manifest: &Manifest,
    mode: Mode,
    reader: &dyn FileReader,
    record: &PublishRecord,
) -> Result<Built, PayloadError> {
    build_from(manifest, mode, reader, GrantSource::Saved(record))
}

fn build_from(
    manifest: &Manifest,
    mode: Mode,
    reader: &dyn FileReader,
    grant: GrantSource<'_>,
) -> Result<Built, PayloadError> {
    let mut paths: Vec<&str> = manifest.files.iter().map(String::as_str).collect();
    paths.sort_unstable();
    if paths.contains(&EMBED_GRANT_FILE) {
        return Err(PayloadError::Unreadable {
            path: EMBED_GRANT_FILE.into(),
            reason: "the name is reserved for the generated embed grant".into(),
        });
    }

    let mut files = Vec::with_capacity(paths.len());
    let mut warnings = Vec::new();
    let mut total: u64 = 0;
    for rel in paths {
        // The manifest already refused unknown types; this is the same check
        // kept at the exit so the payload cannot carry an unlisted type.
        let content_type = content_type_for(rel).ok_or_else(|| PayloadError::Unreadable {
            path: rel.into(),
            reason: "type not allowed".into(),
        })?;
        // Same normal-path rule as the manifest, kept at the exit: a hand-built
        // manifest with `./x` or `a//b` is refused here, not read.
        normalize_rel(rel).map_err(|reason| PayloadError::Unreadable {
            path: rel.into(),
            reason: format!("not a normal path: {reason}"),
        })?;

        // The listed file is uploaded as it is on disk, under the size cap.
        let size = reader.len(rel).map_err(|e| unreadable(rel, &e))?;
        if size > MAX_FILE_BYTES {
            return Err(PayloadError::TooLarge {
                path: rel.into(),
                size,
            });
        }
        let bytes = reader.read(rel).map_err(|e| unreadable(rel, &e))?;

        let size = bytes.len() as u64;
        if size > MAX_FILE_BYTES {
            return Err(PayloadError::TooLarge {
                path: rel.into(),
                size,
            });
        }
        total += size;
        if total > MAX_VERSION_BYTES {
            return Err(PayloadError::VersionTooLarge { size: total });
        }

        if matches!(extension(rel), Some("html" | "js" | "css")) {
            warnings.extend(scan_bridge_calls(rel, &bytes));
        }

        files.push(PayloadFile {
            path: rel.into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            size,
            content_type,
            bytes,
        });
    }

    if mode == Mode::Published && !warnings.is_empty() {
        return Err(PayloadError::BridgeCalls(warnings));
    }

    // The generated file counts toward the version's file limit, approved or not.
    if !manifest.live_embeds.is_empty() && files.len() + 1 > MAX_FILES {
        return Err(PayloadError::Unreadable {
            path: EMBED_GRANT_FILE.into(),
            reason: format!(
                "live.embeds adds {EMBED_GRANT_FILE} to the version, so publish.files may list at most {} files",
                MAX_FILES - 1
            ),
        });
    }
    let generated = match grant {
        GrantSource::Manifest => embed_grant(manifest),
        GrantSource::Saved(record) => super::approval::approved_block(record, manifest)
            .and_then(|approved| grant_bytes(&approved.embeds, &approved.sha)),
    };
    if let Some(bytes) = generated {
        let size = bytes.len() as u64;
        total += size;
        if total > MAX_VERSION_BYTES {
            return Err(PayloadError::VersionTooLarge { size: total });
        }
        files.push(PayloadFile {
            path: EMBED_GRANT_FILE.into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            size,
            content_type: "application/json",
            bytes,
        });
        files.sort_by(|a, b| a.path.cmp(&b.path));
    }

    let manifest_sha = manifest_sha(&files);
    Ok(Built {
        payload: Payload {
            files,
            manifest_sha,
        },
        warnings,
    })
}

/// An advisory for copied toolkits; never a publish refusal. Only listed files are inspected.
pub fn toolkit_notes(manifest: &Manifest, reader: &dyn FileReader) -> Vec<String> {
    fn version(bytes: &[u8]) -> Option<u64> {
        std::str::from_utf8(bytes)
            .ok()?
            .lines()
            .next()?
            .trim()
            .strip_prefix("// cs-live ")?
            .parse()
            .ok()
    }
    let Some(current) = version(super::create::TOOLKIT_JS.as_bytes()) else {
        return Vec::new();
    };
    manifest.files.iter().filter_map(|path| {
        if normalize_rel(path).is_err() || Path::new(path).file_name()?.to_str()? != "cs-live.js" {
            return None;
        }
        if reader.len(path).ok()? > MAX_FILE_BYTES { return None; }
        let bytes = reader.read(path).ok()?;
        (version(&bytes)? < current).then(|| format!("{path} is older than this app's cs-live.js; recreate a live starter to get the current toolkit. Publishing is still allowed."))
    }).collect()
}

fn unreadable(rel: &str, e: &io::Error) -> PayloadError {
    PayloadError::Unreadable {
        path: rel.into(),
        reason: e.to_string(),
    }
}

fn extension(rel: &str) -> Option<&str> {
    Path::new(rel).extension()?.to_str()
}

/// `window.codesense`, `codesense.act` and `codesense.<name>(` are bridge
/// calls. A plain mention such as `codesense.ai` is not.
fn bridge_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"window\.codesense|\bcodesense\.act\b|\bcodesense\.\w+\s*\(")
            .expect("bridge pattern is a valid regex")
    })
}

pub(crate) fn scan_bridge_calls(path: &str, bytes: &[u8]) -> Vec<Finding> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .enumerate()
        .filter(|(_, line)| bridge_pattern().is_match(line))
        .map(|(i, line)| Finding {
            path: path.into(),
            line: i + 1,
            text: line.trim().chars().take(160).collect(),
        })
        .collect()
}

/// sha256 over each file's path and bytes, in path order (DR-14).
fn manifest_sha(files: &[PayloadFile]) -> String {
    let mut h = Sha256::new();
    for f in files {
        h.update(f.path.as_bytes());
        h.update([0u8]);
        h.update(&f.bytes);
        h.update([0u8]);
    }
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::super::manifest;
    use super::*;
    use std::cell::RefCell;

    /// Wraps the folder reader and records every file it opens (AC-2).
    struct RecordingReader<'a> {
        inner: FolderReader<'a>,
        reads: RefCell<Vec<String>>,
        fail_on: Option<&'static str>,
    }

    impl FileReader for RecordingReader<'_> {
        fn len(&self, rel: &str) -> io::Result<u64> {
            self.inner.len(rel)
        }

        fn read(&self, rel: &str) -> io::Result<Vec<u8>> {
            self.reads.borrow_mut().push(rel.into());
            if self.fail_on == Some(rel) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
            }
            self.inner.read(rel)
        }
    }

    fn folder(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            let p = dir.path().join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        dir
    }

    fn manifest_for(dir: &Path, files: &[&str]) -> Manifest {
        let list: String = files.iter().map(|f| format!("    - {f}\n")).collect();
        let text = format!(
            "slug: ai-usage\ntitle: AI usage\npublish:\n  state: draft\n  visibility: private\n  files:\n{list}"
        );
        manifest::parse(&text, dir).unwrap()
    }

    fn recorder<'a>(dir: &'a Path, fail_on: Option<&'static str>) -> RecordingReader<'a> {
        RecordingReader {
            inner: FolderReader { folder: dir },
            reads: RefCell::new(vec![]),
            fail_on,
        }
    }

    fn live_manifest(dir: &Path, live: &str, files: &[&str]) -> Manifest {
        let list: String = files.iter().map(|f| format!("    - {f}\n")).collect();
        let text = format!(
            "slug: ai-usage\ntitle: AI usage\npublish:\n  state: draft\n  visibility: private\n  files:\n{list}{live}"
        );
        manifest::parse(&text, dir).unwrap()
    }

    /// SV-INP-29: the grant file exists only when the manifest grants embeds, is
    /// generated from the manifest, carries `run`, and its name cannot be listed.
    #[test]
    fn the_embed_grant_is_generated_from_the_manifest_or_absent() {
        let dir = folder(&[("index.html", b"<p>Inbox</p>")]);
        let none = live_manifest(dir.path(), "live:\n  queries: [queryContext]\n", &["index.html"]);
        assert!(embed_grant(&none).is_none());
        let built = build(&none, Mode::Published, &recorder(dir.path(), None)).unwrap();
        assert!(built.payload.files.iter().all(|f| f.path != EMBED_GRANT_FILE));

        let granted = live_manifest(
            dir.path(),
            "live:\n  queries: [queryContext]\n  fields:\n    queryContext: [status, title]\n  embeds:\n    queries: [queryContext]\n    fields:\n      queryContext: [status]\n    run: owner\n",
            &["index.html"],
        );
        let built = build(&granted, Mode::Published, &recorder(dir.path(), None)).unwrap();
        let file = built.payload.files.iter().find(|f| f.path == EMBED_GRANT_FILE).expect("grant file");
        assert_eq!(file.content_type, "application/json");
        let grant: serde_json::Value = serde_json::from_slice(&file.bytes).unwrap();
        assert_eq!(grant["v"], 1);
        assert_eq!(grant["queries"], serde_json::json!(["queryContext"]));
        assert_eq!(grant["fields"], serde_json::json!({"queryContext": ["status"]}));
        assert_eq!(grant["run"], "owner");
        assert_eq!(grant["sha"], serde_json::json!(granted.live_sha()));
        // Sorted by path, and the version sha covers the generated file.
        let paths: Vec<_> = built.payload.files.iter().map(|f| f.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted);
        let again = live_manifest(
            dir.path(),
            "live:\n  queries: [queryContext]\n  fields:\n    queryContext: [status, title]\n  embeds:\n    queries: [queryContext]\n    fields:\n      queryContext: [status]\n    run: everyone\n",
            &["index.html"],
        );
        let other = build(&again, Mode::Published, &recorder(dir.path(), None)).unwrap();
        assert_ne!(other.payload.manifest_sha, built.payload.manifest_sha, "run changes the payload");

        // The name is reserved: a folder file cannot stand in for the grant.
        let dir = folder(&[("index.html", b"<p>x</p>"), (EMBED_GRANT_FILE, b"{}")]);
        let sneaky = live_manifest(dir.path(), "", &["index.html", EMBED_GRANT_FILE]);
        assert!(matches!(
            build(&sneaky, Mode::Published, &recorder(dir.path(), None)),
            Err(PayloadError::Unreadable { .. })
        ));
    }

    fn saved_for(dir: &Path, m: &Manifest) -> PublishRecord {
        let mut record = PublishRecord::new("ai-usage");
        record.state = super::super::state::LocalState::Published;
        record.folder = Some(super::super::state::folder_key(dir));
        record.live_approved = super::super::approval::decide(m, dir, None, None).approved;
        record
    }

    /// SV-INP-29 (review r1): what is uploaded is generated from the block the record
    /// saved for this manifest, never from the manifest alone. An edit that is not
    /// approved yet (the sha differs) widens nothing: there is no grant file at all.
    #[test]
    fn the_uploaded_grant_comes_from_the_approved_block_and_only_while_its_sha_matches() {
        let dir = folder(&[("index.html", b"<p>Inbox</p>")]);
        let block = |run: &str| {
            format!("live:\n  queries: [queryContext]\n  embeds:\n    queries: [queryContext]\n    run: {run}\n")
        };
        let owner = live_manifest(dir.path(), &block("owner"), &["index.html"]);
        let record = saved_for(dir.path(), &owner);
        let grant_of = |m: &Manifest, r: &PublishRecord| {
            let built = build_approved(m, Mode::Published, &recorder(dir.path(), None), r).unwrap();
            built
                .payload
                .files
                .iter()
                .find(|f| f.path == EMBED_GRANT_FILE)
                .map(|f| serde_json::from_slice::<serde_json::Value>(&f.bytes).unwrap())
        };
        // Approved and unchanged: the grant is the saved block.
        let grant = grant_of(&owner, &record).expect("grant for the approved block");
        assert_eq!(grant["run"], "owner");
        assert_eq!(grant["sha"], serde_json::json!(owner.live_sha()));
        // The manifest is edited wider (or narrower) and not approved yet: no grant file.
        let everyone = live_manifest(dir.path(), &block("everyone"), &["index.html"]);
        assert!(grant_of(&everyone, &record).is_none(), "an unapproved edit widened the grant");
        let none = live_manifest(dir.path(), &block("none"), &["index.html"]);
        assert!(grant_of(&none, &record).is_none());
        // Once the approval follows the edit, the grant follows it.
        let followed = saved_for(dir.path(), &everyone);
        assert_eq!(grant_of(&everyone, &followed).unwrap()["run"], "everyone");
        // No saved block, a block saved for another folder or another slug: no grant.
        let mut unsaved = record.clone();
        unsaved.live_approved = None;
        assert!(grant_of(&owner, &unsaved).is_none());
        let other = tempfile::tempdir().unwrap();
        let mut elsewhere = record.clone();
        elsewhere.folder = Some(super::super::state::folder_key(other.path()));
        assert!(grant_of(&owner, &elsewhere).is_none());
        // The file limit still counts the slot, approved or not.
        let names: Vec<String> = (0..MAX_FILES).map(|i| format!("f{i}.txt")).collect();
        let items: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"x" as &[u8])).collect();
        let full_dir = folder(&items);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let full = live_manifest(full_dir.path(), &block("owner"), &refs);
        assert!(matches!(
            build_approved(&full, Mode::Published, &recorder(full_dir.path(), None), &unsaved),
            Err(PayloadError::Unreadable { .. })
        ));
    }

    /// The generated grant file counts toward the 50-file version limit, so a
    /// manifest at the limit cannot also carry embeds (the hosted side would refuse it).
    #[test]
    fn the_embed_grant_counts_toward_the_file_limit() {
        let names: Vec<String> = (0..MAX_FILES).map(|i| format!("f{i}.txt")).collect();
        let items: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"x" as &[u8])).collect();
        let dir = folder(&items);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let live = "live:\n  queries: [queryContext]\n  embeds:\n    queries: [queryContext]\n    run: owner\n";
        let full = live_manifest(dir.path(), live, &refs);
        match build(&full, Mode::Published, &recorder(dir.path(), None)) {
            Err(PayloadError::Unreadable { path, reason }) => {
                assert_eq!(path, EMBED_GRANT_FILE);
                assert!(reason.contains("at most 49"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        let room = live_manifest(dir.path(), live, &refs[..MAX_FILES - 1]);
        assert_eq!(
            build(&room, Mode::Published, &recorder(dir.path(), None)).unwrap().payload.files.len(),
            MAX_FILES
        );
    }

    #[test]
    fn older_toolkit_copies_get_a_publish_note_without_refusing_publish() {
        let version = super::super::create::TOOLKIT_VERSION;
        for (header, old) in [
            (format!("// cs-live {}", version - 1), true),
            (format!("// cs-live {version}"), false),
            (format!("// cs-live {}", version + 1), false),
            ("// cs-live nope".to_owned(), false),
            ("// another helper".to_owned(), false),
        ] {
            let dir = folder(&[
                ("index.html", b"<p>Agents</p>"),
                ("assets/cs-live.js", header.as_bytes()),
                ("cs-live.js", b"// cs-live 0"),
            ]);
            let m = manifest_for(dir.path(), &["index.html", "assets/cs-live.js"]);
            let r = recorder(dir.path(), None);
            let notes = toolkit_notes(&m, &r);
            assert_eq!(notes.len(), usize::from(old), "{header}");
            if old {
                assert!(notes[0].contains("cs-live.js is older than this app's"));
            }
            assert!(!r.reads.borrow().iter().any(|p| p == "cs-live.js"));
            assert!(build(&m, Mode::Published, &r).is_ok());
        }
    }

    #[test]
    fn only_allowlisted_files_are_opened_and_uploaded() {
        // AC-2: secret.txt sits next to the allowlisted index.html.
        let dir = folder(&[
            ("index.html", b"<p>hello</p>"),
            ("secret.txt", b"do not send"),
            ("data/usage.json", b"{\"prompt\":\"SECRET PROMPT\"}"),
        ]);
        let m = manifest_for(dir.path(), &["index.html", "data/usage.json"]);
        let r = recorder(dir.path(), None);
        let built = build(&m, Mode::Draft, &r).unwrap();

        let paths: Vec<&str> = built
            .payload
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(paths, vec!["data/usage.json", "index.html"]);
        assert_eq!(
            *r.reads.borrow(),
            vec!["data/usage.json".to_string(), "index.html".to_string()]
        );
        assert!(!r.reads.borrow().iter().any(|p| p == "secret.txt"));
    }

    #[test]
    fn a_listed_data_file_is_uploaded_as_it_is_on_disk() {
        let body = b"{\"runs\": 3}";
        let dir = folder(&[("data/usage.json", body)]);
        let m = manifest_for(dir.path(), &["data/usage.json"]);
        let built = build(&m, Mode::Draft, &FolderReader { folder: dir.path() }).unwrap();
        assert_eq!(built.payload.files[0].bytes, body.to_vec());
    }

    #[test]
    fn a_page_with_a_bridge_call_is_refused_when_published() {
        // AC-4: the owner sees the file and the line.
        let dir = folder(&[(
            "index.html",
            b"<script>\nwindow.codesense.sessions()\n</script>",
        )]);
        let m = manifest_for(dir.path(), &["index.html"]);
        let err = build(&m, Mode::Published, &FolderReader { folder: dir.path() }).unwrap_err();
        match err {
            PayloadError::BridgeCalls(hits) => {
                assert_eq!(hits.len(), 1);
                assert_eq!(hits[0].path, "index.html");
                assert_eq!(hits[0].line, 2);
            }
            other => panic!("expected BridgeCalls, got {other:?}"),
        }
    }

    #[test]
    fn the_same_page_is_a_warning_in_draft() {
        let dir = folder(&[(
            "index.html",
            b"<script>window.codesense.sessions()</script>",
        )]);
        let m = manifest_for(dir.path(), &["index.html"]);
        let built = build(&m, Mode::Draft, &FolderReader { folder: dir.path() }).unwrap();
        assert_eq!(built.warnings.len(), 1);
        assert_eq!(built.warnings[0].line, 1);
    }

    #[test]
    fn codesense_act_and_bare_calls_are_caught_but_a_domain_is_not() {
        let dir = folder(&[(
            "app.js",
            b"codesense.act('open')\ncodesense.runPrompt(x)\nsee https://codesense.ai/docs\n",
        )]);
        let m = manifest_for(dir.path(), &["app.js"]);
        let built = build(&m, Mode::Draft, &FolderReader { folder: dir.path() }).unwrap();
        let lines: Vec<usize> = built.warnings.iter().map(|f| f.line).collect();
        assert_eq!(lines, vec![1, 2]);
    }

    #[test]
    fn a_file_over_five_mb_is_refused() {
        let big = vec![b'a'; (MAX_FILE_BYTES + 1) as usize];
        let dir = folder(&[("index.html", &big)]);
        let m = manifest_for(dir.path(), &["index.html"]);
        let err = build(&m, Mode::Draft, &FolderReader { folder: dir.path() }).unwrap_err();
        assert!(matches!(err, PayloadError::TooLarge { .. }), "{err:?}");
    }

    #[test]
    fn a_version_over_twenty_five_mb_is_refused() {
        let five = vec![b'a'; MAX_FILE_BYTES as usize];
        let names: Vec<String> = (0..6).map(|i| format!("p{i}.html")).collect();
        let files: Vec<(&str, &[u8])> = names
            .iter()
            .map(|n| (n.as_str(), five.as_slice()))
            .collect();
        let dir = folder(&files);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let m = manifest_for(dir.path(), &refs);
        let err = build(&m, Mode::Draft, &FolderReader { folder: dir.path() }).unwrap_err();
        assert!(
            matches!(err, PayloadError::VersionTooLarge { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn an_unreadable_allowlisted_file_refuses_the_whole_payload() {
        let dir = folder(&[("index.html", b"<p>a</p>"), ("app.js", b"1")]);
        let m = manifest_for(dir.path(), &["index.html", "app.js"]);
        let r = recorder(dir.path(), Some("app.js"));
        let err = build(&m, Mode::Draft, &r).unwrap_err();
        assert!(matches!(err, PayloadError::Unreadable { ref path, .. } if path == "app.js"));
    }

    #[test]
    fn manifest_sha_is_stable_and_follows_the_bytes() {
        let dir = folder(&[("index.html", b"<p>a</p>")]);
        let m = manifest_for(dir.path(), &["index.html"]);
        let r = FolderReader { folder: dir.path() };
        let a = build(&m, Mode::Draft, &r).unwrap();
        let b = build(&m, Mode::Draft, &r).unwrap();
        assert_eq!(a.payload.manifest_sha, b.payload.manifest_sha);
        fs::write(dir.path().join("index.html"), b"<p>b</p>").unwrap();
        let c = build(&m, Mode::Draft, &r).unwrap();
        assert_ne!(a.payload.manifest_sha, c.payload.manifest_sha);
        assert_eq!(c.payload.files[0].size, 8);
        assert_eq!(c.payload.files[0].content_type, "text/html");
    }

    #[test]
    fn a_non_normal_path_is_refused_at_the_payload_exit() {
        let dir = folder(&[("data/usage.json", b"{}")]);
        let m = Manifest {
            slug: "ai-usage".into(),
            title: "AI usage".into(),
            state: manifest::PublishState::Draft,
            visibility: manifest::Visibility::Private,
            retain: 5,
            files: vec!["./data/usage.json".into()],
            live: None,
            live_fields: Default::default(),
            live_args: Default::default(),
            live_streams: Vec::new(),
            live_control: false,
            live_embeds: Default::default(),
            actions: Default::default(),
            session_days: 90,
            install: None,
            chrome: Default::default(),
        };
        let r = recorder(dir.path(), None);
        let err = build(&m, Mode::Draft, &r).unwrap_err();
        assert!(
            matches!(err, PayloadError::Unreadable { ref path, .. } if path == "./data/usage.json"),
            "{err:?}"
        );
        assert!(r.reads.borrow().is_empty());
    }
}
