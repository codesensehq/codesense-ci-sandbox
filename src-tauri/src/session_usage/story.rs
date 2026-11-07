// SPDX-License-Identifier: MIT
//! Which story a usage session was spent on.
//!
//! The primary source is the managed worktree records (the `story` rows `list_worktrees`
//! lists): a session whose working folder is a story worktree, or a folder inside one, worked
//! on that story. Both paths are canonicalised, so a symlinked path still matches. The link is
//! stored with the session in the usage store when its usage is recorded (and on a report that
//! finds it live), so a session keeps its story after the worktree is removed. A session with
//! no such folder falls back to the story its launch was opened for, as the sessions registry
//! recorded it (`open_workspace`'s story, keyed by the launch handle the usage store attributes
//! the session to). The worktree wins when both are known.
//!
//! The story's title, status and epic come from the story file's frontmatter, read with the
//! reader the context index uses. One [`StoryResolver`] serves one report: the worktree
//! records are read once, and each story file at most once.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::commands::pipeline_format;
use crate::install::worktree_index::WorktreeStoryRecord;

/// The most of a story file read for its frontmatter, as the context index reads it.
const HEAD_CAP: u64 = 64 * 1024;

/// The story a session was spent on. `title`, `status` and `epic` are `null` when the story
/// file does not say, or is not known.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UsageStory {
    /// The story id, e.g. `SV-5`.
    pub id: String,
    pub title: Option<String>,
    pub status: Option<String>,
    /// The parent epic (`parent_epic` or `epic` frontmatter).
    pub epic: Option<String>,
    /// `associated` when only a context worktree's `<team>/<epic>-<n>` name ties the session
    /// to the story; absent when the link is exact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<&'static str>,
}

/// A session's link to a story worktree's story: what the usage store keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryLink {
    pub story_id: String,
    /// The story file; empty when an explicit launch story has no readable source.
    pub source: PathBuf,
    pub org: String,
}

/// One story worktree: where it is and which story file it works.
struct StoryWorktree {
    /// Canonical folder (the recorded path when it cannot be canonicalised).
    root: PathBuf,
    /// The recorded folder.
    recorded: PathBuf,
    link: StoryLink,
}

/// Ties sessions to stories for one report.
#[derive(Default)]
pub struct StoryResolver {
    scope: crate::context_repo::caller_scope::CallerScope,
    worktrees: Vec<StoryWorktree>,
    /// A launch handle → the story it was launched for (the sessions registry).
    launch_stories: HashMap<String, String>,
    /// Story files already read, by path.
    files: HashMap<PathBuf, UsageStory>,
    /// The CodeSense home, when the resolver can look stories up by id in the org roots.
    home: Option<PathBuf>,
    /// Org roots under the home with their slugs, listed on first use.
    roots: std::cell::OnceCell<Vec<(String, PathBuf)>>,
    /// Story file lookups by (story id, org), so each is resolved once per resolver.
    sources: std::cell::RefCell<HashMap<(String, String), Option<PathBuf>>>,
    /// Context worktree name (`org/team/slug`) → the story it names, once looked up.
    named: HashMap<String, Option<(String, PathBuf)>>,
    /// The frontmatter index to resolve named worktrees from, instead of the app's shared one.
    index: Option<std::sync::Arc<crate::install::frontmatter_cache::FrontmatterCache>>,
    /// The cache generation `index` stands for, instead of the process-wide count.
    index_generation: u64,
}

/// What a remembered answer stands on: the index at a cache generation, or an org walk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Basis {
    Index(u64),
    Walk(u64),
}

/// Process-wide memo of `org root/team/name` → (what the answer stands on, the story it
/// names or `None`), shared by every resolver so a batch of computes looks each name up once.
type NamedMemo = HashMap<String, (Basis, Option<(PathBuf, String)>)>;
static NAMED: std::sync::Mutex<Option<NamedMemo>> = std::sync::Mutex::new(None);

/// One walk of an org's story files: `(path, team, id)` of every pipeline file with an id.
/// It serves the stories the index cannot (no `source_repo`, or an org it does not cover),
/// so a name lookup never walks.
struct OrgWalk {
    serial: u64,
    /// The org fingerprint before the walk.
    stamp: u64,
    /// The cache generation the walk was taken at; it matters only to an org the index covers.
    generation: u64,
    covered: bool,
    built: std::time::Instant,
    stories: Vec<(PathBuf, String, String)>,
}

static WALK_SERIALS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The walk held for each org root, with when its fingerprint was last checked.
static ORG_WALKS: std::sync::Mutex<
    Option<HashMap<PathBuf, (std::sync::Arc<OrgWalk>, std::time::Instant)>>,
> = std::sync::Mutex::new(None);

/// One lock per org root, so parallel computes share a single walk.
static WALK_GATES: std::sync::Mutex<
    Option<HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<()>>>>,
> = std::sync::Mutex::new(None);

/// How long a walk is trusted before the org is walked again, for a subfolder add nothing
/// else shows.
const WALK_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// How long the fingerprint is trusted between checks.
const STAMP_CHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// A cheap fingerprint of an org's story folders: the modified time of each team's `content`
/// folder, the folders the index reads (`.git`, `_`-prefixed and `worktrees` entries are not
/// among them). A story file added or removed directly there moves it.
fn org_stamp(root: &Path) -> u64 {
    let mtime = |dir: &Path| -> u64 {
        let modified = std::fs::metadata(dir).and_then(|m| std::fs::Metadata::modified(&m));
        match modified {
            Ok(at) => match std::time::SystemTime::duration_since(&at, std::time::UNIX_EPOCH) {
                Ok(since) => std::time::Duration::as_nanos(&since) as u64,
                Err(_) => 0,
            },
            Err(_) => 0,
        }
    };
    let mut stamp = 0u64;
    crate::install::frontmatter_cache::for_each_indexable_dir(root, |_, content| {
        stamp = u64::wrapping_add(stamp, u64::rotate_left(mtime(content), 3) | 1);
    });
    stamp
}

/// The held walk of `root` when it still stands: within its lifetime, at this generation when
/// the index covers the org, and with the org fingerprint unmoved (checked at most once a second).
fn held_walk(root: &Path, generation: u64) -> Option<std::sync::Arc<OrgWalk>> {
    let mut walks = std::sync::Mutex::lock(&ORG_WALKS).unwrap_or_else(|p| p.into_inner());
    let (walk, checked) = HashMap::get_mut(Option::as_mut(&mut *walks)?, root)?;
    if std::time::Instant::elapsed(&walk.built) > WALK_TTL
        || (walk.covered && walk.generation != generation)
    {
        return None;
    }
    if std::time::Instant::elapsed(checked) > STAMP_CHECK {
        if org_stamp(root) != walk.stamp {
            return None;
        }
        *checked = std::time::Instant::now();
    }
    Some(std::sync::Arc::clone(walk))
}

/// The org's story files, walked once and again only when the held walk no longer stands.
/// Parallel callers wait on one walk.
fn org_walk(root: &Path, generation: u64, covered: bool) -> std::sync::Arc<OrgWalk> {
    if let Some(walk) = held_walk(root, generation) {
        return walk;
    }
    let gate = {
        let mut gates = std::sync::Mutex::lock(&WALK_GATES).unwrap_or_else(|p| p.into_inner());
        std::sync::Arc::clone(
            Option::get_or_insert_with(&mut *gates, HashMap::new)
                .entry(root.to_path_buf())
                .or_default(),
        )
    };
    let _walking = std::sync::Mutex::lock(&gate).unwrap_or_else(|p| p.into_inner());
    if let Some(walk) = held_walk(root, generation) {
        return walk;
    }
    #[cfg(test)]
    {
        let mut walks = std::sync::Mutex::lock(&ORG_SCANS).unwrap_or_else(|p| p.into_inner());
        *Option::get_or_insert_with(&mut *walks, HashMap::new)
            .entry(root.to_path_buf())
            .or_insert(0) += 1;
    }
    let stamp = org_stamp(root);
    let walk = std::sync::Arc::new(OrgWalk {
        serial: std::sync::atomic::AtomicU64::fetch_add(
            &WALK_SERIALS,
            1,
            std::sync::atomic::Ordering::AcqRel,
        ),
        stamp,
        generation,
        covered,
        built: std::time::Instant::now(),
        stories: crate::install::frontmatter_cache::FrontmatterCache::scan_org_story_ids(root),
    });
    let mut walks = std::sync::Mutex::lock(&ORG_WALKS).unwrap_or_else(|p| p.into_inner());
    HashMap::insert(
        Option::get_or_insert_with(&mut *walks, HashMap::new),
        root.to_path_buf(),
        (std::sync::Arc::clone(&walk), std::time::Instant::now()),
    );
    walk
}

/// Disk scans of an org root made for named worktrees, by root.
#[cfg(test)]
static ORG_SCANS: std::sync::Mutex<Option<HashMap<PathBuf, u32>>> = std::sync::Mutex::new(None);

/// The story (file, id) in `root` that worktree `name` of `team` stands for. Decided from the
/// frontmatter `index` when it covers the org and names the story; otherwise from the org's
/// walked story files (stories the index skips, orgs it does not cover). `generation` is the
/// cache generation read before `index` was taken. Hits and misses are both remembered.
fn named_in_org(
    root: &Path,
    team: &str,
    name: &str,
    index: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    generation: u64,
) -> Option<(PathBuf, String)> {
    let key = format!("{}/{team}/{name}", Path::display(root));
    let pick = |dir_team: &str, id: &str| dir_team == team && convention_names(name, id);
    found_in_org(root, key, pick, index, generation)
}

/// The story file in `root` whose frontmatter id is exactly `id`, through the same index, org
/// walk and memo as a named worktree: once the index is built a lookup reads no disk, and a
/// story the index skips costs at most the org's one shared walk.
fn file_in_org(
    root: &Path,
    id: &str,
    index: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    generation: u64,
) -> Option<PathBuf> {
    let key = format!("{}#id/{id}", Path::display(root));
    let pick = |_: &str, found: &str| found == id;
    Option::map(
        found_in_org(root, key, pick, index, generation),
        |(path, _)| path,
    )
}

/// [`file_in_org`] against the app's shared frontmatter index, for callers without a resolver.
pub(crate) fn story_file_in_org(root: &Path, id: &str) -> Option<PathBuf> {
    let (generation, index) = shared_index();
    file_in_org(root, id, Option::as_deref(&index), generation)
}

/// The cache generation, read before the app's shared index snapshot, so an answer is never
/// stamped newer than the index it was read from.
fn shared_index() -> (
    u64,
    Option<std::sync::Arc<crate::install::frontmatter_cache::FrontmatterCache>>,
) {
    (
        crate::install::frontmatter_cache::FrontmatterCache::publish_generation(),
        crate::install::frontmatter_cache::FrontmatterCache::global_snapshot(),
    )
}

/// The story `pick(dir_team, story_id)` chooses in `root`, remembered under `key`: from the
/// frontmatter `index` when it covers the org and has the story, otherwise from the org's walk.
fn found_in_org(
    root: &Path,
    key: String,
    pick: impl Fn(&str, &str) -> bool,
    index: Option<&crate::install::frontmatter_cache::FrontmatterCache>,
    generation: u64,
) -> Option<(PathBuf, String)> {
    let known = {
        let memo = std::sync::Mutex::lock(&NAMED).unwrap_or_else(|p| p.into_inner());
        memo.as_ref().and_then(|m| m.get(&key)).cloned()
    };
    if let Some((basis, found)) = known {
        let stands = match basis {
            Basis::Index(at) => at == generation,
            Basis::Walk(serial) => {
                Option::is_some_and(held_walk(root, generation), |walk| walk.serial == serial)
            }
        };
        if stands {
            return found;
        }
    }
    let indexed = Option::and_then(index, |index| index.named_story_under(root, &pick));
    let (basis, found) = match indexed {
        Some(Some(found)) => (Basis::Index(generation), Some(found)),
        covered => {
            let walk = org_walk(root, generation, covered.is_some());
            let found = Iterator::find(&mut walk.stories.iter(), |(_, dir_team, id)| {
                pick(dir_team, id)
            })
            .map(|(path, _, id)| (path.clone(), id.clone()));
            (Basis::Walk(walk.serial), found)
        }
    };
    let mut memo = std::sync::Mutex::lock(&NAMED).unwrap_or_else(|p| p.into_inner());
    HashMap::insert(
        Option::get_or_insert_with(&mut *memo, HashMap::new),
        key,
        (basis, found.clone()),
    );
    found
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

impl StoryResolver {
    /// A resolver over these worktree records and launch stories.
    pub fn new(records: Vec<WorktreeStoryRecord>, launch_stories: HashMap<String, String>) -> Self {
        let worktrees = records
            .into_iter()
            .filter(|r| !r.story_id.trim().is_empty())
            .map(|r| StoryWorktree {
                root: canonical(&r.code_repo_path),
                recorded: r.code_repo_path,
                link: StoryLink {
                    story_id: r.story_id,
                    source: r.story_source_path,
                    org: r.org_slug,
                },
            })
            .collect();
        Self {
            scope: Default::default(),
            worktrees,
            launch_stories: launch_stories
                .into_iter()
                .filter(|(_, story)| !story.trim().is_empty())
                .collect(),
            files: HashMap::new(),
            home: None,
            roots: std::cell::OnceCell::new(),
            sources: std::cell::RefCell::new(HashMap::new()),
            named: HashMap::new(),
            index: None,
            index_generation: 0,
        }
    }

    /// A resolver over every worktree record under the CodeSense home.
    pub fn for_home(home: &Path, launch_stories: HashMap<String, String>) -> Self {
        Self {
            home: Some(home.to_path_buf()),
            ..Self::new(
                crate::install::worktree_index::all_records(home),
                launch_stories,
            )
        }
    }

    /// The story file for `id` in `org`'s context, for a story no worktree record names.
    /// Resolved through the frontmatter index, once per resolver.
    pub fn source_in_org(&self, id: &str, org: Option<&str>) -> Option<PathBuf> {
        let org = org?;
        let key = (id.to_owned(), org.to_owned());
        if let Some(known) = self.sources.borrow().get(&key) {
            return known.clone();
        }
        let root = Iterator::find(&mut self.org_roots().iter(), |(slug, _)| *slug == org);
        let found = Option::and_then(root, |(_, root)| {
            let (generation, index) = Self::index_at(self);
            let path = file_in_org(root, id, Option::as_deref(&index), generation)?;
            bool::then_some(self.scope.allows(&path), path)
        });
        self.sources.borrow_mut().insert(key, found.clone());
        found
    }

    /// Every org root under the home with its slug, listed once per resolver.
    fn org_roots(&self) -> &[(String, PathBuf)] {
        self.roots.get_or_init(|| {
            let Some(home) = self.home.as_deref() else {
                return Vec::new();
            };
            crate::context_repo::org_enumeration::enumerate_all_org_roots(home)
                .into_iter()
                .filter_map(|root| {
                    let slug = crate::context_repo::org_slug_from_org_root(
                        &std::path::Path::to_string_lossy(&root),
                    )?
                    .to_owned();
                    Some((slug, root))
                })
                .collect()
        })
    }

    /// The story a context worktree's `<team>/<epic>-<n>` name points at (its folder is
    /// `.../context-worktrees/<org>/<team>/<epic>-<n>`), with its story file: an association,
    /// not an exact link. A `-<k>` collision suffix after the number is ignored.
    pub fn named_story(&mut self, cwd: &str) -> Option<(String, PathBuf)> {
        let rest = cwd.split_once("/context-worktrees/")?.1;
        // The org slug may itself hold a `/`, so the org is the one whose slug prefixes `rest`.
        let (root, after) = Iterator::find_map(&mut self.org_roots().iter(), |(org, root)| {
            let after = rest.strip_prefix(&format!("{org}/"))?;
            Some((root.clone(), after.to_owned()))
        })?;
        let mut parts = after.split('/');
        let (team, slug) = (parts.next()?, parts.next()?);
        let key = format!("{}/{team}/{slug}", std::path::Path::display(&root));
        if let Some(known) = self.named.get(&key) {
            return known.clone();
        }
        let name = convention_tail(slug);
        let (generation, index) = Self::index_at(self);
        let found = named_in_org(&root, team, &name, index.as_deref(), generation)
            .and_then(|(path, id)| self.scope.allows(&path).then_some((id, path)));
        self.named.insert(key, found.clone());
        found
    }

    /// The frontmatter index this resolver reads, with the cache generation it stands for.
    fn index_at(
        &self,
    ) -> (
        u64,
        Option<std::sync::Arc<crate::install::frontmatter_cache::FrontmatterCache>>,
    ) {
        match self.index.clone() {
            Some(index) => (self.index_generation, Some(index)),
            None => shared_index(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_index(
        mut self,
        index: std::sync::Arc<crate::install::frontmatter_cache::FrontmatterCache>,
        generation: u64,
    ) -> Self {
        self.index = Some(index);
        self.index_generation = generation;
        self
    }

    pub(crate) fn with_scope(
        mut self,
        scope: crate::context_repo::caller_scope::CallerScope,
    ) -> Self {
        self.scope = scope;
        self
    }

    /// The story worktree holding `cwd` (the innermost, when worktrees nest).
    pub fn worktree_link(&self, cwd: &str) -> Option<StoryLink> {
        let cwd = Path::new(cwd);
        if self.worktrees.is_empty() || !cwd.is_absolute() {
            return None;
        }
        let canon = canonical(cwd);
        self.worktrees
            .iter()
            .filter_map(|w| {
                // The depth of the folder that matched, canonical or as recorded.
                let matched = if canon.starts_with(&w.root) {
                    &w.root
                } else if cwd.starts_with(&w.recorded) {
                    &w.recorded
                } else {
                    return None;
                };
                Some((matched.components().count(), w))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, w)| w.link.clone())
    }

    /// The story the launch `handle` was opened for, with its story file when a worktree
    /// record of the same org names that story.
    pub fn launch_story(
        &self,
        handle: &str,
        org: Option<&str>,
    ) -> Option<(String, Option<PathBuf>)> {
        let id = self.launch_stories.get(handle)?.trim().to_string();
        let source = org
            .and_then(|org| {
                self.worktrees
                    .iter()
                    .find(|w| w.link.org == org && w.link.story_id.eq_ignore_ascii_case(&id))
                    .map(|w| w.link.source.clone())
            })
            .or_else(|| self.source_in_org(&id, org));
        Some((id, source))
    }

    /// The story `id`, with title, status and epic from its story file when there is one.
    pub fn story(&mut self, id: &str, source: Option<&Path>) -> UsageStory {
        let scope = &self.scope;
        let mut story = match source {
            Some(source) => self
                .files
                .entry(source.to_path_buf())
                .or_insert_with(|| read_story(source, scope))
                .clone(),
            None => UsageStory::default(),
        };
        story.id = id.to_string();
        story
    }
}

/// The `<EPIC>-<n>` a context worktree or branch name `<epic>-<n>` stands for, upper-cased,
/// with a `-<k>` collision suffix after the number dropped.
pub(crate) fn convention_tail(slug: &str) -> String {
    let mut name = slug.to_ascii_uppercase();
    let numbers = name
        .rsplit('-')
        .take(2)
        .filter(|s| Iterator::all(&mut str::bytes(s), |b| u8::is_ascii_digit(&b)))
        .count();
    if numbers == 2 {
        name.truncate(name.rfind('-').unwrap_or(name.len()));
    }
    name
}

/// Whether story `id` is the one a name `<epic>-<n>` stands for (`SV-CDE-63` for `cde-63`).
pub(crate) fn convention_names(name: &str, id: &str) -> bool {
    let id = id.to_ascii_uppercase();
    name.contains('-') && (id == name || id.ends_with(&format!("-{name}")))
}

/// A frontmatter value as text: a string or a number, trimmed; `None` when blank.
fn text(fields: &std::collections::BTreeMap<String, Value>, key: &str) -> Option<String> {
    let value = match fields.get(key)? {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!value.is_empty()).then_some(value)
}

/// The first `HEAD_CAP` bytes of a story file as text.
fn read_head(source: &Path) -> Option<String> {
    let mut head = Vec::new();
    std::fs::File::open(source)
        .and_then(|f| f.take(HEAD_CAP).read_to_end(&mut head))
        .ok()?;
    Some(String::from_utf8_lossy(&head).into_owned())
}

/// The story file's title: its frontmatter `title`, else its first heading.
fn title_of(
    format: pipeline_format::PipelineFileFormat,
    fields: &std::collections::BTreeMap<String, Value>,
    head: &str,
) -> Option<String> {
    text(fields, "title").or_else(|| {
        pipeline_format::extract_title(format, head)
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    })
}

/// Title, status and epic from a story file's frontmatter. A missing or unreadable file
/// leaves them `None`. The title falls back to the file's first heading.
fn read_story(source: &Path, scope: &crate::context_repo::caller_scope::CallerScope) -> UsageStory {
    let mut story = UsageStory::default();
    if !scope.allows(source) {
        return story;
    }
    let Some(head) = read_head(source) else {
        return story;
    };
    let format = pipeline_format::format_of(source);
    let fields = crate::commands::frontmatter_editor::frontmatter_json(format, &head);
    story.title = title_of(format, &fields, &head);
    story.status = text(&fields, "status");
    story.epic = text(&fields, "parent_epic").or_else(|| text(&fields, "epic"));
    story
}

/// What a story file says about itself, for a task record: its title, its `type`
/// (`story`, `bug` or `spike`) and the first paragraph of its Purpose section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryDetail {
    pub title: Option<String>,
    pub kind: Option<String>,
    pub purpose: Option<String>,
}

pub fn story_detail(source: &Path) -> StoryDetail {
    let Some(head) = read_head(source) else {
        return StoryDetail::default();
    };
    let format = pipeline_format::format_of(source);
    let fields = crate::commands::frontmatter_editor::frontmatter_json(format, &head);
    let mut paragraph = Vec::new();
    let mut lines = head
        .lines()
        .skip_while(|l| l.trim() != "## Purpose")
        .skip(1);
    for line in lines.by_ref() {
        let line = line.trim();
        if line.starts_with("## ") || (line.is_empty() && !paragraph.is_empty()) {
            break;
        }
        if !line.is_empty() {
            paragraph.push(line);
        }
    }
    StoryDetail {
        title: title_of(format, &fields, &head),
        kind: text(&fields, "type"),
        purpose: Some(paragraph.join(" ")).filter(|p| !p.is_empty()),
    }
}

/// Disk scans of `root` made for named worktrees so far in this process.
#[cfg(test)]
pub(crate) fn org_scans(root: &Path) -> u32 {
    let scans = std::sync::Mutex::lock(&ORG_SCANS).unwrap_or_else(|p| p.into_inner());
    scans
        .as_ref()
        .and_then(|m| m.get(root))
        .copied()
        .unwrap_or(0)
}
