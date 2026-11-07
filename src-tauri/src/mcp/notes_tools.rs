// SPDX-License-Identifier: MIT
//! The notes MCP tools: `notes_list`, `notes_read`, `notes_create`, `notes_write`,
//! `notes_search`, `notes_history`, `notes_read_at`, `notes_restore`.
//!
//! Notes are the person's private notes for one org, kept on this Mac. The
//! `#[tool]` methods in [`super::handler`] resolve which org's notes and who is
//! calling; the bodies here read and write the notes store, commit each agent
//! change to the notes' local history with the caller credited, and report what
//! they did. Nothing here decides how notes are organised: agents do that.

use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) use super::tool_envelope::ToolEnvelope;
use crate::commands::notes_history::{
    ChangeKind, NoteActor, NoteAtCommit, NoteRevision, NotesLocation,
};
#[cfg(test)]
use crate::commands::scratch::read_index;
use crate::commands::scratch::{Note, NoteEdit};

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotesListParams {
    /// The org (`owner/repo`) whose notes; omit for the org your session works in.
    #[serde(default)]
    pub org: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotesReadParams {
    #[serde(default)]
    pub org: Option<String>,
    /// The note id, from notes_list or notes_search.
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotesCreateParams {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    /// The note's title (blank: "Untitled").
    pub title: String,
    /// Its Markdown body.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    /// Identity comes from the verified connection; a supplied id must name the same caller.
    #[serde(default)]
    pub session_id: Option<String>,
}

pub use crate::records::Patch as NotesPatch;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotesWriteParams {
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub org: Option<String>,
    pub id: String,
    /// The whole new Markdown body.
    #[serde(default)]
    pub content: Option<String>,
    /// Text to add at the end, on a new line.
    #[serde(default)]
    pub append: Option<String>,
    /// Replace one exact passage.
    #[serde(default)]
    pub patch: Option<NotesPatch>,
    /// A new title.
    #[serde(default)]
    pub title: Option<String>,
    /// Identity comes from the verified connection; a supplied id must name the same caller.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotesSearchParams {
    #[serde(default)]
    pub org: Option<String>,
    /// Case-insensitive text to find in titles and bodies.
    pub query: String,
    /// At most this many hits (default 20).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotesHistoryParams {
    #[serde(default)]
    pub org: Option<String>,
    pub id: String,
    /// At most this many revisions, newest first (default 50).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotesReadAtParams {
    #[serde(default)]
    pub org: Option<String>,
    pub id: String,
    /// A commit id from notes_history (full, or at least its first 4 characters).
    pub commit: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotesRestoreParams {
    #[serde(default)]
    pub undo: Option<bool>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    pub id: String,
    /// The commit id (from notes_history) whose text to bring back.
    pub commit: String,
    /// Identity comes from the verified connection; a supplied id must name the same caller.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// `notes_list`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesListed {
    pub org: String,
    pub notes: Vec<crate::records::notes::NoteRow>,
}

/// `notes_read`, `notes_create`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRead {
    pub org: String,
    pub note: Note,
}

pub use crate::records::notes::{NoteHit, NoteWritten};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesFound {
    pub org: String,
    pub hits: Vec<NoteHit>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesHistory {
    pub org: String,
    pub id: String,
    pub revisions: Vec<NoteRevision>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteThen {
    pub org: String,
    #[serde(flatten)]
    pub note: NoteAtCommit,
}

/// What a write did, for the caller to announce.
pub(crate) struct Change {
    pub id: String,
    pub commit: Option<String>,
    pub kind: ChangeKind,
}

pub(crate) type Reply<T> = Result<ToolEnvelope<T>, String>;

fn invalid<T>(message: impl Into<String>) -> Reply<T> {
    Ok(ToolEnvelope::Validation {
        message: message.into(),
    })
}

pub(crate) fn list(loc: &NotesLocation, org: &str) -> Reply<NotesListed> {
    Ok(ToolEnvelope::Ok(NotesListed {
        org: org.into(),
        notes: crate::records::notes::list(loc)?,
    }))
}

pub(crate) fn read(loc: &NotesLocation, org: &str, id: &str) -> Reply<NoteRead> {
    match crate::records::notes::read(loc, id) {
        Ok(note) => Ok(ToolEnvelope::Ok(NoteRead {
            org: org.into(),
            note,
        })),
        Err(message) => invalid(message),
    }
}

pub(crate) fn create(
    loc: &NotesLocation,
    org: &str,
    params: &NotesCreateParams,
    actor: &NoteActor,
) -> Result<(ToolEnvelope<NoteWritten>, Option<Change>), String> {
    let mut result = crate::records::notes::create(
        loc,
        &params.title,
        params.content.as_deref().unwrap_or(""),
        params.pinned.unwrap_or(false),
        actor,
        params.request_id.as_deref(),
    )?;
    result.org = org.into();
    Ok(change_reply(result, ChangeKind::Create))
}

/// The one edit a `notes_write` asks for, or why its arguments do not name one.
pub(crate) fn edit_of(params: &NotesWriteParams) -> Result<NoteEdit, String> {
    crate::records::edit::requested_edit(
        params.content.as_deref(),
        params.base.as_deref(),
        params.append.as_deref(),
        params.patch.as_ref(),
        params.title.is_some() || params.pinned.is_some(),
    )
}

pub(crate) fn write(
    loc: &NotesLocation,
    org: &str,
    params: &NotesWriteParams,
    actor: &NoteActor,
) -> Result<(ToolEnvelope<NoteWritten>, Option<Change>), String> {
    let edit = match edit_of(params) {
        Ok(edit) => edit,
        Err(message) => return Ok((ToolEnvelope::Validation { message }, None)),
    };
    match crate::records::notes::write(
        loc,
        &params.id,
        &edit,
        params.title.as_deref(),
        params.pinned,
        actor,
        params.request_id.as_deref(),
    ) {
        Ok(mut result) => {
            result.org = org.into();
            Ok(change_reply(
                result,
                if edit == NoteEdit::Keep {
                    ChangeKind::Meta
                } else {
                    ChangeKind::Write
                },
            ))
        }
        Err(message) => Ok((ToolEnvelope::Validation { message }, None)),
    }
}

pub(crate) fn search(
    loc: &NotesLocation,
    org: &str,
    query: &str,
    limit: Option<usize>,
) -> Reply<NotesFound> {
    if query.trim().is_empty() {
        return invalid("query must not be blank");
    }
    Ok(ToolEnvelope::Ok(NotesFound {
        org: org.into(),
        hits: crate::records::notes::search(loc, query, limit.unwrap_or(20).clamp(1, 200), None)?,
    }))
}

pub(crate) fn history(
    loc: &NotesLocation,
    org: &str,
    id: &str,
    limit: Option<usize>,
) -> Reply<NotesHistory> {
    match crate::records::notes::store(loc)?.history(id, limit.unwrap_or(50).clamp(1, 500)) {
        Ok(revisions) => Ok(ToolEnvelope::Ok(NotesHistory {
            org: org.into(),
            id: id.into(),
            revisions,
        })),
        Err(message) => invalid(message),
    }
}

pub(crate) fn read_at(loc: &NotesLocation, org: &str, id: &str, commit: &str) -> Reply<NoteThen> {
    match crate::records::notes::read_at(loc, id, commit) {
        Ok(note) => Ok(ToolEnvelope::Ok(NoteThen {
            org: org.into(),
            note,
        })),
        Err(message) => invalid(message),
    }
}

pub(crate) fn restore(
    loc: &NotesLocation,
    org: &str,
    id: &str,
    commit: &str,
    actor: &NoteActor,
    undo: bool,
    request_id: Option<&str>,
) -> Result<(ToolEnvelope<NoteWritten>, Option<Change>), String> {
    match crate::records::notes::restore(loc, id, commit, undo, actor, request_id) {
        Ok(mut result) => {
            result.org = org.into();
            Ok(change_reply(result, ChangeKind::Restore))
        }
        Err(message) => Ok((ToolEnvelope::Validation { message }, None)),
    }
}

fn change_reply(
    result: NoteWritten,
    kind: ChangeKind,
) -> (ToolEnvelope<NoteWritten>, Option<Change>) {
    let change = result.changed.then(|| Change {
        id: result.note.id.clone(),
        commit: result.commit.clone(),
        kind,
    });
    (ToolEnvelope::Ok(result), change)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn loc_for(home: &std::path::Path, org: &str) -> NotesLocation {
        crate::commands::scratch::org_notes_location_unchecked(
            home,
            &crate::domain::Principal::from(42u64),
            org,
        )
        .unwrap()
    }

    fn tidy() -> NoteActor {
        NoteActor::agent("Notes tidy", Some("pty-7".into()), Some("tidy".into()))
    }

    fn write_params(id: &str) -> NotesWriteParams {
        NotesWriteParams {
            request_id: None,
            base: None,
            pinned: None,
            org: None,
            id: id.to_string(),
            content: None,
            append: None,
            patch: None,
            title: None,
            session_id: None,
        }
    }

    fn ok<T>(envelope: ToolEnvelope<T>) -> T {
        match envelope {
            ToolEnvelope::Ok(v) => v,
            _ => panic!("expected ok"),
        }
    }

    #[test]
    fn an_agent_write_updates_the_file_and_index_credits_the_session_and_commits() {
        let tmp = tempdir().unwrap();
        let loc = loc_for(tmp.path(), "acme/ctx");
        let cp = NotesCreateParams {
            request_id: None,
            org: None,
            title: "Plan".into(),
            content: Some("- a\n".into()),
            pinned: None,
            session_id: None,
        };
        let (reply, change) = create(&loc, "acme/ctx", &cp, &tidy()).unwrap();
        let created = ok(reply);
        assert!(created.commit.is_some());
        assert_eq!(change.unwrap().kind, ChangeKind::Create);
        let id = created.note.id;

        let mut params = write_params(&id);
        params.patch = Some(NotesPatch {
            find: "- a".into(),
            replace: "- [x] a".into(),
        });
        let (reply, change) = write(&loc, "acme/ctx", &params, &tidy()).unwrap();
        let written = ok(reply);
        assert!(written.changed);
        assert_eq!(change.unwrap().kind, ChangeKind::Write);
        let sha = written.commit.expect("a commit");

        // File + index.
        assert_eq!(
            crate::records::notes::read(&loc, &id).unwrap().content,
            "- [x] a\n"
        );
        let meta = read_index(&loc.notes_dir())
            .unwrap()
            .into_iter()
            .find(|m| m.id == id)
            .unwrap();
        assert_eq!(meta.updated_by, Some(tidy()));
        assert!(meta.updated_at.is_some());

        // Commit trailer.
        let repo = git2::Repository::open(&loc.repo).unwrap();
        let msg = repo
            .find_commit(git2::Oid::from_str(&sha).unwrap())
            .unwrap()
            .message()
            .unwrap()
            .to_string();
        assert!(msg.contains("Actor-Session: pty-7"), "{msg}");
        assert!(msg.contains("Actor-Name: Notes tidy"), "{msg}");

        // History names the agent.
        let h = ok(history(&loc, "acme/ctx", &id, None).unwrap());
        assert_eq!(h.revisions[0].actor, tidy());
        assert_eq!(h.revisions.len(), 2);

        // Restore the first revision, then read it back.
        let first = h.revisions[1].commit.clone();
        let (reply, change) = restore(&loc, "acme/ctx", &id, &first, &tidy(), false, None).unwrap();
        assert!(ok(reply).changed);
        assert_eq!(change.unwrap().kind, ChangeKind::Restore);
        let note = ok(read(&loc, "acme/ctx", &id).unwrap()).note;
        assert_eq!(note.content, "- a\n");
        let then = ok(read_at(&loc, "acme/ctx", &id, &sha).unwrap());
        assert_eq!(then.note.content, "- [x] a\n");
    }

    #[test]
    fn append_and_a_no_op_write() {
        let tmp = tempdir().unwrap();
        let loc = loc_for(tmp.path(), "acme/ctx");
        let cp = NotesCreateParams {
            request_id: None,
            org: None,
            title: "T".into(),
            content: Some("one".into()),
            pinned: Some(true),
            session_id: None,
        };
        let id = ok(create(&loc, "acme/ctx", &cp, &tidy()).unwrap().0)
            .note
            .id;
        let mut params = write_params(&id);
        params.append = Some("two".into());
        ok(write(&loc, "acme/ctx", &params, &tidy()).unwrap().0);
        assert_eq!(
            ok(read(&loc, "acme/ctx", &id).unwrap()).note.content,
            "one\ntwo"
        );
        let mut same = write_params(&id);
        same.content = Some("one\ntwo".into());
        let (reply, change) = write(&loc, "acme/ctx", &same, &tidy()).unwrap();
        let w = ok(reply);
        assert!(!w.changed && w.commit.is_none() && change.is_none());
    }

    #[test]
    fn write_arguments_must_name_one_edit() {
        let mut p = write_params("abc-1");
        assert!(edit_of(&p).is_err());
        p.content = Some("x".into());
        p.append = Some("y".into());
        assert!(edit_of(&p).is_err());
        let mut p = write_params("abc-1");
        p.title = Some("New".into());
        assert_eq!(edit_of(&p).unwrap(), NoteEdit::Keep);
    }

    #[test]
    fn a_patch_that_does_not_match_once_is_refused() {
        let tmp = tempdir().unwrap();
        let loc = loc_for(tmp.path(), "acme/ctx");
        let cp = NotesCreateParams {
            request_id: None,
            org: None,
            title: "T".into(),
            content: Some("x x".into()),
            pinned: None,
            session_id: None,
        };
        let id = ok(create(&loc, "acme/ctx", &cp, &tidy()).unwrap().0)
            .note
            .id;
        let mut p = write_params(&id);
        p.patch = Some(NotesPatch {
            find: "x".into(),
            replace: "y".into(),
        });
        assert!(matches!(
            write(&loc, "acme/ctx", &p, &tidy()).unwrap().0,
            ToolEnvelope::Validation { .. }
        ));
    }

    #[test]
    fn one_orgs_notes_are_invisible_from_another() {
        let tmp = tempdir().unwrap();
        let a = loc_for(tmp.path(), "acme/ctx");
        let b = loc_for(tmp.path(), "other/ctx");
        let cp = NotesCreateParams {
            request_id: None,
            org: None,
            title: "Secret plan".into(),
            content: Some("launch codes".into()),
            pinned: None,
            session_id: None,
        };
        let id = ok(create(&a, "acme/ctx", &cp, &tidy()).unwrap().0).note.id;

        assert!(matches!(
            read(&b, "other/ctx", &id).unwrap(),
            ToolEnvelope::Validation { .. }
        ));
        let mut p = write_params(&id);
        p.content = Some("overwritten".into());
        assert!(matches!(
            write(&b, "other/ctx", &p, &tidy()).unwrap().0,
            ToolEnvelope::Validation { .. }
        ));
        assert!(ok(search(&b, "other/ctx", "launch", None).unwrap())
            .hits
            .is_empty());
        assert!(ok(list(&b, "other/ctx").unwrap())
            .notes
            .iter()
            .all(|n| n.id != id));
        assert!(ok(history(&b, "other/ctx", &id, None).unwrap())
            .revisions
            .is_empty());
        // Org A still has it, untouched.
        assert_eq!(
            ok(read(&a, "acme/ctx", &id).unwrap()).note.content,
            "launch codes"
        );
    }

    #[test]
    fn search_matches_titles_before_text() {
        let tmp = tempdir().unwrap();
        let loc = loc_for(tmp.path(), "acme/ctx");
        for (title, content) in [("Groceries", "milk"), ("Ideas", "buy milk later")] {
            let p = NotesCreateParams {
                request_id: None,
                org: None,
                title: title.into(),
                content: Some(content.into()),
                pinned: None,
                session_id: None,
            };
            create(&loc, "acme/ctx", &p, &tidy()).unwrap();
        }
        let hits = ok(search(&loc, "acme/ctx", "MILK", None).unwrap()).hits;
        assert_eq!(hits.len(), 2);
        let hits = ok(search(&loc, "acme/ctx", "groc", None).unwrap()).hits;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].matched, "title");
        assert!(matches!(
            search(&loc, "acme/ctx", "  ", None).unwrap(),
            ToolEnvelope::Validation { .. }
        ));
    }
}
