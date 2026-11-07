// SPDX-License-Identifier: MIT
//! Create a pipeline story from the app: render a canonical story document from a
//! small draft, validate it with the exact `validate_story` engine agents use, and
//! write it only when nothing blocks.
//!
//! The rendering is pure ([`render_story_markdown`], [`story_file_name`]) so the
//! canonical shape — explicit `type`, a quoted-when-needed scalar for every value,
//! the `## Purpose` / `## Acceptance Criteria` skeleton — is unit-tested without an
//! app. The Tauri commands are thin: resolve the active team's validator inputs,
//! render, validate, and (for `create_story`) write with create-new semantics.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::commands::files::validate_context_path_owned;
use crate::commands::pipeline_format::markdown::render_yaml_scalar;
use crate::mcp::story_authoring::{self, configured_story_repos, unambiguous_story_repos};

/// The fields the New-story form collects. Everything else a story can carry is
/// added later through the metadata band.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoryDraft {
    /// Absolute epic directory the story file is written into.
    pub epic_dir: String,
    pub title: String,
    pub story_id: String,
    pub status: String,
    pub source_repo: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// What the form shows while the author types: where the file would land, the
/// exact bytes that would be written, and the `validate_story` report for them.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewStoryPreview {
    pub path: String,
    pub content: String,
    pub validation: Value,
}

/// Render a draft as a canonical Markdown story. Every scalar goes through the
/// same YAML quoting the metadata editor uses, so a title like `Refunds: retry`
/// is written quoted and re-reads as exactly that string.
pub fn render_story_markdown(draft: &StoryDraft) -> String {
    let title = single_line(&draft.title);
    let mut out = String::with_capacity(256 + title.len());
    out.push_str("---\n");
    push_field(&mut out, "title", &title);
    push_field(&mut out, "type", "story");
    push_field(&mut out, "story_id", draft.story_id.trim());
    push_field(&mut out, "status", draft.status.trim());
    push_field(&mut out, "source_repo", draft.source_repo.trim());
    let deps: Vec<&str> = draft
        .depends_on
        .iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .collect();
    if deps.is_empty() {
        out.push_str("depends_on: []\n");
    } else {
        out.push_str("depends_on:\n");
        for dep in deps {
            out.push_str("  - ");
            out.push_str(&render_yaml_scalar(dep));
            out.push('\n');
        }
    }
    out.push_str("---\n\n");
    out.push_str(&format!("# {title}\n\n"));
    out.push_str("## Purpose\n\nWhy this story matters and who it is for.\n\n");
    out.push_str("## Acceptance Criteria\n\n- **AC-1.** \n");
    out
}

fn push_field(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&render_yaml_scalar(value));
    out.push('\n');
}

/// Collapse any line breaks so a pasted multi-line title stays one scalar.
fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `<STORY_ID>-<title-slug>.md`, the naming convention the tree already uses.
/// The slug keeps ASCII alphanumerics, collapses everything else to single
/// hyphens, and is capped so file names stay readable.
pub fn story_file_name(story_id: &str, title: &str) -> String {
    const MAX_SLUG: usize = 60;
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let mut slug = slug.trim_end_matches('-').to_string();
    if slug.len() > MAX_SLUG {
        slug.truncate(MAX_SLUG);
        slug = slug.trim_end_matches('-').to_string();
    }
    let id: String = story_id
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if slug.is_empty() {
        format!("{id}.md")
    } else {
        format!("{id}-{slug}.md")
    }
}

/// Refuse drafts the form could never produce: an empty title or id, or a
/// control character (a newline would inject extra frontmatter lines).
fn check_draft(draft: &StoryDraft) -> Result<(), String> {
    if draft.title.trim().is_empty() || draft.story_id.trim().is_empty() {
        return Err("a story needs a title and a story id".to_string());
    }
    let has_control = |v: &str| v.chars().any(|c| c.is_control() && !c.is_whitespace());
    let single_line_fields = [&draft.story_id, &draft.status, &draft.source_repo];
    if single_line_fields
        .into_iter()
        .chain(&draft.depends_on)
        .any(|v| v.contains(['\n', '\r']) || has_control(v))
        || has_control(&draft.title)
    {
        return Err("story fields cannot contain line breaks or control characters".to_string());
    }
    Ok(())
}

/// Where a draft would be written and the exact bytes, refusing an invalid
/// draft or one whose file already exists.
fn render_draft(draft: &StoryDraft) -> Result<(PathBuf, String), String> {
    check_draft(draft)?;
    let path = draft_target_path(draft);
    if path.exists() {
        return Err(format!("a file already exists at {}", path.display()));
    }
    Ok((path, render_story_markdown(draft)))
}

/// The write half of `create_story`: refuse a blocked report, else write the
/// previewed bytes with create-new semantics.
fn write_previewed_story(preview: &NewStoryPreview) -> Result<(), String> {
    if report_has_blocking(&preview.validation) {
        return Err(format!(
            "story not created — validate_story reported blocking findings: {}",
            blocking_messages(&preview.validation)
        ));
    }
    write_new_file(Path::new(&preview.path), &preview.content)
}

fn draft_target_path(draft: &StoryDraft) -> PathBuf {
    Path::new(&draft.epic_dir).join(story_file_name(&draft.story_id, &draft.title))
}

fn report_has_blocking(report: &Value) -> bool {
    report.get("valid").and_then(Value::as_bool) == Some(false)
}

/// The on-disk inputs `validate_story` checks a file against: the org, the
/// team's status taxonomy and field schema, and the repos of the file's epic.
struct TeamValidatorInputs {
    org: Option<crate::domain::Org>,
    valid_statuses: Vec<String>,
    field_schema: crate::commands::field_schema::FieldSchema,
    epic_repos: Vec<String>,
}

fn load_team_validator_inputs(
    org_slug: String,
    org_root: &Path,
    team: &str,
    story_path: Option<&Path>,
) -> TeamValidatorInputs {
    load_team_validator_inputs_for(org_slug, org_root, team, story_path, &Default::default())
}
fn load_team_validator_inputs_for(
    org_slug: String,
    org_root: &Path,
    team: &str,
    story_path: Option<&Path>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> TeamValidatorInputs {
    let org = crate::domain::load_org(org_slug, org_root, None).org;
    let content_dir = crate::context_repo::path_to_team_content_dir(org_root, team);
    let valid_statuses =
        crate::commands::status_taxonomy::load_status_taxonomy_from_for(&content_dir, scope)
            .states
            .into_iter()
            .map(|s| s.id)
            .collect();
    let field_schema =
        crate::commands::field_schema::load_field_schema_from_for(&content_dir, scope);
    let epic_repos = story_path
        .map(|path| epic_repos_for_story_for(path, scope))
        .unwrap_or_default();
    TeamValidatorInputs {
        org,
        valid_statuses,
        field_schema,
        epic_repos,
    }
}

/// Run `validate_story` for `text` against the ACTIVE team's configuration —
/// configured source repos, status taxonomy, field schema, org-wide story_id
/// index and roster. Shared by the MCP tool and the app's New-story form and
/// metadata band so all three give the same verdict for the same bytes.
pub(crate) async fn validate_story_in_active_context(
    app: &AppHandle,
    text: &str,
    path: Option<&Path>,
) -> Result<Value, String> {
    validate_story_in_context_for(app, text, path, &Default::default()).await
}

pub(crate) async fn validate_story_in_context_for<R: tauri::Runtime>(
    app: &AppHandle<R>,
    text: &str,
    path: Option<&Path>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Value, String> {
    let ctx = match scope {
        crate::context_repo::caller_scope::CallerScope::Team(view) => {
            match crate::commands::route_to_context::derive_context_for(
                app,
                view.org(),
                view.reader_team(),
            )
            .await?
            {
                crate::commands::route_to_context::ContextResolution::Ok(ctx) => ctx,
                _ => return Err(crate::context_repo::caller_scope::MISSING.into()),
            }
        }
        _ => crate::commands::route_to_context::derive_active_context(app).await?,
    };
    let org_root = crate::context_repo::path_to_org_root(
        &ctx.codesense_home,
        &ctx.principal(),
        &ctx.org_slug,
    )?;
    let team = ctx.team_name.clone().unwrap_or_default();

    let TeamValidatorInputs {
        org,
        valid_statuses,
        field_schema,
        epic_repos,
    } = {
        let org_slug = ctx.org_slug.clone();
        let org_root = org_root.clone();
        let story_path = path.map(Path::to_path_buf);
        let scope = scope.clone();
        crate::commands::await_blocking("validate_story:team_config", move || {
            Ok(load_team_validator_inputs_for(
                org_slug,
                &org_root,
                &team,
                story_path.as_deref(),
                &scope,
            ))
        })
        .await?
    };

    let teams: &[crate::domain::Team] = org.as_ref().map(|o| o.teams.as_slice()).unwrap_or(&[]);
    let configured_repos = configured_story_repos(teams);
    let valid_teams: Vec<String> = teams.iter().map(|t| t.name.clone()).collect();
    let active_team_candidates: Vec<String> = teams
        .iter()
        .find(|t| Some(&t.name) == ctx.team_name.as_ref())
        .map(unambiguous_story_repos)
        .unwrap_or_default();

    // Effective summary headings, so the blank-card advisory matches the scanner.
    let summary_headings = crate::commands::system::read_configured_summary_headings(app);

    // Lock-free snapshot of the frontmatter cache backs the O(1) duplicate
    // story_id lookup; no cache yet → the uniqueness check is skipped, never a
    // false green.
    let cache_snapshot = app
        .try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
        .map(|cache| crate::install::frontmatter_cache::FrontmatterCache::snapshot(&cache));
    let raw_story_id_index: &dyn story_authoring::StoryIdIndex = match &cache_snapshot {
        Some(snap) => &**snap,
        None => &story_authoring::NoStoryIdIndex,
    };
    struct ScopedIndex<'a>(
        &'a dyn story_authoring::StoryIdIndex,
        &'a crate::context_repo::caller_scope::CallerScope,
    );
    impl story_authoring::StoryIdIndex for ScopedIndex<'_> {
        fn paths_for_story_id(&self, id: &str) -> Vec<PathBuf> {
            self.0
                .paths_for_story_id(id)
                .into_iter()
                .filter(|path| self.1.allows(path))
                .collect()
        }
    }
    let scoped_index = ScopedIndex(raw_story_id_index, scope);
    let story_id_index: &dyn story_authoring::StoryIdIndex = &scoped_index;
    let assignee_resolver: &dyn story_authoring::AssigneeResolver = match org.as_ref() {
        Some(o) => o,
        None => &story_authoring::NoAssigneeResolver,
    };

    let cfg = story_authoring::ValidatorConfig {
        configured_repos: &configured_repos,
        valid_teams: &valid_teams,
        active_team_candidates: &active_team_candidates,
        epic_repos: &epic_repos,
        summary_headings: &summary_headings,
        valid_statuses: &valid_statuses,
        story_id_index,
        assignee_resolver,
        schema: &field_schema,
    };
    Ok(story_authoring::validate_candidate_for(
        text, path, &cfg, scope,
    ))
}

/// Epic main-doc file names that can carry `repos:`, in preference order
/// (matched case-insensitively).
const EPIC_DOC_NAMES: &[&str] = &["readme.md", "index.md"];

/// Bound on how many directories (the story's own included) the `repos:`
/// lookup climbs — deeper than any real epic nesting.
const MAX_EPIC_DEPTH: usize = 8;

/// The directory's epic doc, README before index so the pick never depends on
/// directory listing order.
fn epic_doc_in_for(
    dir: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Option<PathBuf> {
    if !scope.visible(dir) {
        return None;
    }
    let files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| scope.allows(p))
        .filter(|p| p.is_file())
        .collect();
    EPIC_DOC_NAMES.iter().find_map(|want| {
        files
            .iter()
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(want))
            })
            .cloned()
    })
}

/// The `repos:` of the epic a file belongs to: the nearest enclosing epic doc
/// (README/index) that declares any, climbing through parent epics until a
/// directory has no epic doc. Empty when no enclosing epic names repos. Passing
/// an epic's own doc resolves that epic.
pub(crate) fn epic_repos_for_story(story_path: &Path) -> Vec<String> {
    epic_repos_for_story_for(story_path, &Default::default())
}
fn epic_repos_for_story_for(
    story_path: &Path,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Vec<String> {
    let Some(start) = story_path.parent() else {
        return Vec::new();
    };
    for dir in start.ancestors().take(MAX_EPIC_DEPTH) {
        let Some(doc) = epic_doc_in_for(dir, scope) else {
            break;
        };
        if !scope.allows(&doc) {
            break;
        }
        match crate::commands::pipeline_scanner::read_epic_repo_slugs(&doc) {
            Ok(repos) if !repos.is_empty() => return repos,
            _ => {}
        }
    }
    Vec::new()
}

/// The repos a story's `source_repo` should come from: its epic's `repos:`
/// (empty when the epic names none; the caller then offers the team's repos).
#[tauri::command]
pub async fn story_repo_choices(app: AppHandle, path: String) -> Result<Vec<String>, String> {
    let file_path = validate_context_path_owned(&path, &app)?;
    crate::commands::await_blocking("story_repo_choices", move || {
        Ok(epic_repos_for_story(&file_path))
    })
    .await
}

/// Render + validate a draft without writing anything (the form's live preview).
#[tauri::command]
pub async fn preview_new_story(
    app: AppHandle,
    draft: StoryDraft,
) -> Result<NewStoryPreview, String> {
    validate_context_path_owned(&draft.epic_dir, &app)?;
    let (path, content) = render_draft(&draft)?;
    let validation = validate_story_in_active_context(&app, &content, Some(&path)).await?;
    Ok(NewStoryPreview {
        path: path.to_string_lossy().into_owned(),
        content,
        validation,
    })
}

/// Render, validate and write a new story. Refuses (writing nothing) when the
/// report has a blocking finding or the target file already exists, so a story
/// created here can never be silently dropped by the scanner.
#[tauri::command]
pub async fn create_story(app: AppHandle, draft: StoryDraft) -> Result<NewStoryPreview, String> {
    crate::commands::files::validate_context_write_path_owned(&draft.epic_dir, &app)?;
    let preview = preview_new_story(app, draft).await?;
    crate::commands::await_blocking("create_story:write", move || {
        write_previewed_story(&preview).map(|()| preview)
    })
    .await
}

/// Validate an on-disk file (the metadata band's findings strip).
#[tauri::command]
pub async fn validate_story_file(app: AppHandle, path: String) -> Result<Value, String> {
    let file_path = validate_context_path_owned(&path, &app)?;
    let read_path = file_path.clone();
    let text = crate::commands::await_blocking("validate_story_file:read", move || {
        std::fs::read_to_string(&read_path)
            .map_err(|e| format!("failed to read {}: {e}", read_path.display()))
    })
    .await?;
    validate_story_in_active_context(&app, &text, Some(&file_path)).await
}

fn blocking_messages(report: &Value) -> String {
    report
        .get("findings")
        .and_then(Value::as_array)
        .map(|findings| {
            findings
                .iter()
                .filter(|f| f.get("severity").and_then(Value::as_str) == Some("blocking"))
                .filter_map(|f| f.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

/// Create-new write: never overwrites an existing story.
fn write_new_file(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => {
                format!("a file already exists at {}", path.display())
            }
            _ => format!("failed to create {}: {e}", path.display()),
        })?;
    file.write_all(content.as_bytes()).map_err(|e| {
        // Never leave a truncated story behind to block every retry.
        let _ = std::fs::remove_file(path);
        format!("failed to write {}: {e}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::pipeline_format::markdown::extract_meta_fields;

    fn draft(title: &str) -> StoryDraft {
        StoryDraft {
            epic_dir: "/ctx/checkout".into(),
            title: title.into(),
            story_id: "LL-7".into(),
            status: "planning".into(),
            source_repo: "acme/api".into(),
            depends_on: vec!["LL-3".into(), " ".into(), "LL-4".into()],
        }
    }

    #[test]
    fn renders_canonical_story_frontmatter_and_skeleton() {
        let doc = render_story_markdown(&draft("Refund endpoint"));
        assert!(doc.starts_with("---\ntitle: Refund endpoint\ntype: story\nstory_id: LL-7\n"));
        assert!(doc.contains("status: planning\nsource_repo: acme/api\n"));
        assert!(doc.contains("depends_on:\n  - LL-3\n  - LL-4\n---\n"));
        assert!(doc.contains("\n## Purpose\n"));
        assert!(doc.contains("\n## Acceptance Criteria\n"));

        let fields = extract_meta_fields(&doc);
        assert!(fields.is_pipeline_story());
        assert_eq!(fields.story_id.as_deref(), Some("LL-7"));
        assert_eq!(fields.source_repo.as_deref(), Some("acme/api"));
        assert_eq!(
            fields.depends_on,
            Some(vec!["LL-3".to_string(), "LL-4".to_string()])
        );
    }

    #[test]
    fn a_colon_space_value_is_quoted_and_round_trips() {
        let doc = render_story_markdown(&draft("Refunds: retry with idempotency keys"));
        assert!(doc.contains("title: \"Refunds: retry with idempotency keys\"\n"));
        let fields = extract_meta_fields(&doc);
        // The block still parses — the story is not dropped by the scanner.
        assert!(fields.is_pipeline_story());
        assert_eq!(fields.story_id.as_deref(), Some("LL-7"));
    }

    #[test]
    fn empty_depends_on_is_an_explicit_empty_list() {
        let mut d = draft("x");
        d.depends_on.clear();
        let doc = render_story_markdown(&d);
        assert!(doc.contains("depends_on: []\n"));
        assert_eq!(extract_meta_fields(&doc).depends_on, Some(vec![]));
    }

    #[test]
    fn multi_line_title_collapses_to_one_line() {
        let doc = render_story_markdown(&draft("Line one\nline two"));
        assert!(doc.contains("title: Line one line two\n"));
        assert!(doc.contains("# Line one line two\n"));
    }

    #[test]
    fn file_name_is_id_plus_capped_slug() {
        assert_eq!(
            story_file_name("LL-7", "Refunds: retry w/ keys!"),
            "LL-7-refunds-retry-w-keys.md"
        );
        assert_eq!(story_file_name("LL-7", "  "), "LL-7.md");
        assert_eq!(story_file_name("LL/7", "a"), "LL-7-a.md");
        let long = "word ".repeat(40);
        let name = story_file_name("LL-7", &long);
        assert!(name.len() <= "LL-7-".len() + 60 + ".md".len());
        assert!(!name.contains("-.md"));
    }

    #[test]
    fn create_new_write_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("epic").join("LL-7.md");
        write_new_file(&path, "one").unwrap();
        let err = write_new_file(&path, "two").unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");
    }

    fn validator_cfg<'a>(repos: &'a [String]) -> story_authoring::ValidatorConfig<'a> {
        static HEADINGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        static STATUSES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        static SCHEMA: std::sync::OnceLock<crate::commands::field_schema::FieldSchema> =
            std::sync::OnceLock::new();
        story_authoring::ValidatorConfig {
            configured_repos: repos,
            valid_teams: &[],
            active_team_candidates: repos,
            epic_repos: &[],
            summary_headings: HEADINGS.get_or_init(|| vec!["Purpose".into(), "Story".into()]),
            valid_statuses: STATUSES.get_or_init(|| {
                crate::commands::pipeline_scanner::VALID_STATUSES
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            }),
            story_id_index: &story_authoring::NoStoryIdIndex,
            assignee_resolver: &story_authoring::NoAssigneeResolver,
            schema: SCHEMA.get_or_init(crate::commands::field_schema::default_schema),
        }
    }

    /// The create path end to end against the real validator: render → validate
    /// → gate → write.
    fn create_with(draft: &StoryDraft, repos: &[String]) -> Result<NewStoryPreview, String> {
        let (path, content) = render_draft(draft)?;
        let validation =
            story_authoring::validate_candidate(&content, Some(&path), &validator_cfg(repos));
        let preview = NewStoryPreview {
            path: path.to_string_lossy().into_owned(),
            content,
            validation,
        };
        write_previewed_story(&preview).map(|()| preview)
    }

    fn draft_in(dir: &Path, title: &str, repo: &str) -> StoryDraft {
        StoryDraft {
            epic_dir: dir.to_string_lossy().into_owned(),
            title: title.into(),
            story_id: "LL-7".into(),
            status: "planning".into(),
            source_repo: repo.into(),
            depends_on: vec![],
        }
    }

    #[test]
    fn a_clean_draft_is_written_and_reads_back_as_a_valid_story() {
        let dir = tempfile::tempdir().unwrap();
        let repos = vec!["acme/api".to_string()];
        let preview = create_with(&draft_in(dir.path(), "Refunds: retry", "acme/api"), &repos)
            .expect("clean draft is created");
        let on_disk = std::fs::read_to_string(&preview.path).unwrap();
        assert_eq!(on_disk, preview.content);
        let reread = story_authoring::validate_candidate(
            &on_disk,
            Some(Path::new(&preview.path)),
            &validator_cfg(&repos),
        );
        assert_eq!(reread["detectedKind"], "pipeline_story");
        assert_eq!(reread["valid"], true, "{reread}");
    }

    #[test]
    fn a_blocked_draft_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let repos = vec!["acme/api".to_string()];
        let err =
            create_with(&draft_in(dir.path(), "Refunds", "acme/unknown"), &repos).unwrap_err();
        assert!(err.contains("blocking"), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_existing_file_and_malformed_fields_are_refused_up_front() {
        let dir = tempfile::tempdir().unwrap();
        let draft = draft_in(dir.path(), "Refunds", "acme/api");
        std::fs::write(draft_target_path(&draft), "mine").unwrap();
        assert!(render_draft(&draft).unwrap_err().contains("already exists"));

        let mut bad = draft_in(dir.path(), "Other", "acme/api");
        bad.story_id = "LL-8\nstatus: done".into();
        assert!(render_draft(&bad).is_err());
        bad.story_id = "LL-8".into();
        bad.title = "  ".into();
        assert!(render_draft(&bad).is_err());
        bad.title = "bell\u{7}".into();
        assert!(render_draft(&bad).is_err());
    }

    #[test]
    fn blocking_report_is_detected_and_summarized() {
        let report = serde_json::json!({
            "valid": false,
            "findings": [
                {"severity": "blocking", "message": "no type"},
                {"severity": "advisory", "message": "meh"}
            ]
        });
        assert!(report_has_blocking(&report));
        assert_eq!(blocking_messages(&report), "no type");
        assert!(!report_has_blocking(&serde_json::json!({"valid": true})));
    }

    fn validator_cfg_with_epic<'a>(
        repos: &'a [String],
        epic_repos: &'a [String],
    ) -> story_authoring::ValidatorConfig<'a> {
        story_authoring::ValidatorConfig {
            epic_repos,
            ..validator_cfg(repos)
        }
    }

    fn finding_codes_of(report: &Value) -> Vec<String> {
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap().to_string())
            .collect()
    }

    /// Validate the on-disk file the way `validate_story_file` does: its bytes,
    /// its path, and the repos of the epic it sits in.
    fn validate_on_disk(path: &Path, repos: &[String]) -> Value {
        let text = std::fs::read_to_string(path).unwrap();
        let epic_repos = load_team_validator_inputs(
            "acme".into(),
            Path::new("/nonexistent-org"),
            "core",
            Some(path),
        )
        .epic_repos;
        story_authoring::validate_candidate(
            &text,
            Some(path),
            &validator_cfg_with_epic(repos, &epic_repos),
        )
    }

    fn scanned_story<'t>(
        tree: &'t crate::commands::pipeline_scanner::DerivedPipelineTree,
        path: &Path,
    ) -> Option<&'t crate::commands::pipeline_scanner::StoryRef> {
        let want = path.to_string_lossy();
        tree.stories
            .iter()
            .find(|s| s.file_path == want)
            .or_else(|| tree.sub_epics.iter().find_map(|e| scanned_story(e, path)))
    }

    fn write_epic(dir: &Path, repos: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        let list: String = repos.iter().map(|r| format!("  - {r}\n")).collect();
        std::fs::write(
            dir.join("README.md"),
            format!("---\ntype: epic\nrepos:\n{list}---\n\n# Epic\n\n## Purpose\n\nx\n"),
        )
        .unwrap();
    }

    #[test]
    fn epic_repos_come_from_the_nearest_epic_that_names_any() {
        let root = tempfile::tempdir().unwrap();
        let epic = root.path().join("checkout");
        write_epic(&epic, &["acme/api", "acme/web"]);
        let sub = epic.join("refunds");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("README.md"), "---\ntype: epic\n---\n\n# Refunds\n").unwrap();

        assert_eq!(
            epic_repos_for_story(&sub.join("LL-1.md")),
            vec!["acme/api".to_string(), "acme/web".to_string()],
            "a sub-epic with no repos inherits its parent epic's"
        );
        assert_eq!(
            epic_repos_for_story(&epic.join("README.md")),
            vec!["acme/api".to_string(), "acme/web".to_string()],
            "an epic's own doc resolves that epic (the new-story form's lookup)"
        );
        std::fs::write(sub.join("index.md"), "---\nrepos:\n  - acme/other\n---\n").unwrap();
        assert_eq!(
            epic_repos_for_story(&sub.join("LL-1.md")),
            vec!["acme/api".to_string(), "acme/web".to_string()],
            "README wins over index regardless of listing order"
        );
        let loose = root.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert!(epic_repos_for_story(&loose.join("LL-2.md")).is_empty());
    }

    #[test]
    fn source_repo_findings_through_the_scanner_seam() {
        let content = tempfile::tempdir().unwrap();
        let epic = content.path().join("checkout");
        write_epic(&epic, &["acme/api"]);
        let repos = vec!["acme/api".to_string(), "acme/web".to_string()];
        let story = |id: &str, repo: Option<&str>| {
            let repo_line = repo
                .map(|r| format!("source_repo: {r}\n"))
                .unwrap_or_default();
            format!(
                "---\ntitle: {id}\ntype: story\nstory_id: {id}\nstatus: todo\n{repo_line}---\n\n# {id}\n\n## Purpose\n\nx\n"
            )
        };
        let missing = epic.join("LL-1.md");
        let off_epic = epic.join("LL-2.md");
        let fine = epic.join("LL-3.md");
        std::fs::write(&missing, story("LL-1", None)).unwrap();
        std::fs::write(&off_epic, story("LL-2", Some("acme/web"))).unwrap();
        std::fs::write(&fine, story("LL-3", Some("acme/api"))).unwrap();

        let tree =
            crate::commands::pipeline_scanner::scan_pipeline_tree_impl(content.path(), "", "", "")
                .unwrap();
        for path in [&missing, &off_epic, &fine] {
            assert!(
                scanned_story(&tree, path).is_some(),
                "{} scanned",
                path.display()
            );
        }

        let codes = finding_codes_of(&validate_on_disk(&missing, &repos));
        assert!(
            codes.contains(&"MISSING_SOURCE_REPO".to_string()),
            "{codes:?}"
        );
        let codes = finding_codes_of(&validate_on_disk(&off_epic, &repos));
        assert!(
            codes.contains(&"SOURCE_REPO_NOT_IN_EPIC".to_string()),
            "{codes:?}"
        );
        assert!(!codes.contains(&"MISSING_SOURCE_REPO".to_string()));
        assert!(finding_codes_of(&validate_on_disk(&fine, &repos)).is_empty());
    }

    /// No silent path: an orphan story on disk is skipped by the index but stays
    /// on the board (the scan places it with no repo, which the "needs a repo"
    /// badge counts) and is reported by `validate_story`; setting
    /// its repo through the fix writer makes it indexed, placed and launchable.
    #[test]
    fn an_orphan_story_is_reported_then_fixed_into_the_index() {
        use crate::install::frontmatter_cache::FrontmatterCache;

        let content = tempfile::tempdir().unwrap();
        let epic = content.path().join("checkout");
        write_epic(&epic, &["acme/api"]);
        let orphan = epic.join("LL-9-refunds.md");
        std::fs::write(
            &orphan,
            "---\ntitle: \"Refunds: retry\"\ntype: story\nstory_id: LL-9\nstatus: todo\n---\n\n# Refunds\n\n## Purpose\n\nx\n",
        )
        .unwrap();
        let repos = vec!["acme/api".to_string()];

        // Skipped by the index...
        let cache = FrontmatterCache::build_from_content_root(content.path(), "default");
        assert!(!cache.indexes_path(&orphan));
        // ...but still on the board: the scan places it with no repo, which is what
        // the "needs a repo" badge counts (the board does not drop it)...
        let tree =
            crate::commands::pipeline_scanner::scan_pipeline_tree_impl(content.path(), "", "", "")
                .unwrap();
        let before = scanned_story(&tree, &orphan).expect("orphan is on the board");
        assert!(before.source_repo.is_none());
        // ...and by validate_story.
        let codes = finding_codes_of(&validate_on_disk(&orphan, &repos));
        assert!(
            codes.contains(&"MISSING_SOURCE_REPO".to_string()),
            "{codes:?}"
        );

        // The fix action offers the epic's repos and writes the chosen one.
        let choices = epic_repos_for_story(&orphan);
        assert_eq!(choices, repos);
        let text = std::fs::read_to_string(&orphan).unwrap();
        std::fs::write(
            &orphan,
            crate::commands::pipeline_format::markdown::rewrite_field(
                &text,
                "source_repo",
                &choices[0],
            ),
        )
        .unwrap();

        // The watcher refresh now indexes it under its repo (launchable)...
        let entry = FrontmatterCache::prepare_refresh(&orphan, "default").expect("now indexed");
        assert_eq!(entry.source_repo(), "acme/api");
        let rebuilt = FrontmatterCache::build_from_content_root(content.path(), "default");
        assert!(rebuilt.indexes_path(&orphan));
        assert_eq!(rebuilt.by_story_id("LL-9").len(), 1);
        // ...the scan places it with its repo, and it validates clean.
        let tree =
            crate::commands::pipeline_scanner::scan_pipeline_tree_impl(content.path(), "", "", "")
                .unwrap();
        let after = scanned_story(&tree, &orphan).expect("still on the board");
        assert_eq!(after.source_repo.as_deref(), Some("acme/api"));
        assert_eq!(after.story_id.as_deref(), Some("LL-9"));
        assert!(finding_codes_of(&validate_on_disk(&orphan, &repos)).is_empty());
        // The quoted title survived the frontmatter rewrite.
        let text = std::fs::read_to_string(&orphan).unwrap();
        assert!(text.contains("title: \"Refunds: retry\"\n"), "{text}");
    }
}
