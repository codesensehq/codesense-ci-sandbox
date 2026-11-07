// SPDX-License-Identifier: MIT
use std::path::Path;

/// How a write changes a note's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyEdit {
    /// Replace the whole body.
    Replace(String),
    /// Add text to the end of the body, on a new line.
    Append(String),
    /// Replace the one occurrence of `find` with `replace`.
    Patch { find: String, replace: String },
    /// Leave the body as it is (a rename only).
    Keep,
    /// Replace the body with `content`, an edit of `base`. When the body on disk
    /// is no longer `base` (someone else changed it meanwhile), the two edits are
    /// merged so neither is lost; where they touch the same lines both are kept.
    ReplaceFrom { base: String, content: String },
}

/// Which side wins where a three-way merge finds both sides changed the same lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeOverlap {
    /// Keep both sides' lines.
    KeepBoth,
    /// Give up: the merge has no answer.
    Refuse,
}

/// Merge `ours` and `theirs`, both edits of `base`, line by line. `None` when they
/// overlap and `overlap` is [`MergeOverlap::Refuse`].
pub(crate) fn merge_text(
    base: &str,
    ours: &str,
    theirs: &str,
    overlap: MergeOverlap,
) -> Result<Option<String>, String> {
    if ours == theirs || theirs == base {
        return Ok(Some(ours.to_string()));
    }
    if ours == base {
        return Ok(Some(theirs.to_string()));
    }
    let err = |what: &'static str| {
        move |e: git2::Error| format!("merge records: {what}: {}", e.message())
    };
    let odb = git2::Odb::new().map_err(err("odb"))?;
    odb.add_new_mempack_backend(1).map_err(err("odb"))?;
    let repo = git2::Repository::from_odb(odb).map_err(err("repo"))?;
    let tree_of = |text: &str| -> Result<git2::Oid, String> {
        let blob = repo.blob(text.as_bytes()).map_err(err("blob"))?;
        let mut builder = repo.treebuilder(None).map_err(err("tree"))?;
        builder
            .insert("note.md", blob, 0o100644)
            .map_err(err("tree"))?;
        builder.write().map_err(err("tree"))
    };
    let find = |oid| repo.find_tree(oid).map_err(err("tree"));
    let (b, o, t) = (
        find(tree_of(base)?)?,
        find(tree_of(ours)?)?,
        find(tree_of(theirs)?)?,
    );
    let mut opts = git2::MergeOptions::new();
    if overlap == MergeOverlap::KeepBoth {
        opts.file_favor(git2::FileFavor::Union);
    }
    let index = repo
        .merge_trees(&b, &o, &t, Some(&opts))
        .map_err(err("merge"))?;
    if index.has_conflicts() {
        return Ok(None);
    }
    let Some(entry) = index.get_path(Path::new("note.md"), 0) else {
        return Ok(Some(String::new()));
    };
    let blob = repo.find_blob(entry.id).map_err(err("blob"))?;
    Ok(Some(String::from_utf8_lossy(blob.content()).into_owned()))
}

/// The body `edit` makes of `body`, or why it cannot apply.
pub(crate) fn apply_edit(body: &str, edit: &BodyEdit) -> Result<String, String> {
    match edit {
        BodyEdit::Replace(content) => Ok(content.clone()),
        BodyEdit::Keep => Ok(body.to_string()),
        BodyEdit::ReplaceFrom { base, content } => {
            merge_text(base, content, body, MergeOverlap::KeepBoth)
                .map(|merged| merged.unwrap_or_else(|| content.clone()))
        }
        BodyEdit::Append(text) => {
            let mut out = body.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(text);
            Ok(out)
        }
        BodyEdit::Patch { find, replace } => {
            if find.is_empty() {
                return Err("patch.find must not be empty".to_string());
            }
            match body.matches(find.as_str()).count() {
                1 => Ok(body.replacen(find.as_str(), replace, 1)),
                0 => Err(
                    "patch.find does not occur in the note; read it again with notes_read"
                        .to_string(),
                ),
                n => Err(format!(
                    "patch.find occurs {n} times; include more surrounding text so it occurs once"
                )),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Overlap {
    pub ours: String,
    pub theirs: String,
    /// One-based line in the base document.
    pub at: usize,
}

pub(crate) fn merge_with_overlaps(
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<(String, Vec<Overlap>), String> {
    if let Some(merged) = merge_text(base, ours, theirs, MergeOverlap::Refuse)? {
        return Ok((merged, vec![]));
    }
    let edits = |next: &str| -> Result<Vec<(usize, usize, String)>, String> {
        let mut opts = git2::DiffOptions::new();
        opts.context_lines(0);
        let patch = git2::Patch::from_buffers(
            base.as_bytes(),
            None,
            next.as_bytes(),
            None,
            Some(&mut opts),
        )
        .map_err(|e| e.to_string())?;
        let lines: Vec<&str> = next.split_inclusive('\n').collect();
        let mut out = vec![];
        for i in 0..patch.num_hunks() {
            let (h, _) = patch.hunk(i).map_err(|e| e.to_string())?;
            let start = if h.old_lines() == 0 {
                h.old_start() as usize
            } else {
                h.old_start().saturating_sub(1) as usize
            };
            let new_start = h.new_start().saturating_sub(1) as usize;
            out.push((
                start,
                start + h.old_lines() as usize,
                lines[new_start..new_start + h.new_lines() as usize].concat(),
            ));
        }
        Ok(out)
    };
    let mut overlaps = vec![];
    for (os, oe, o) in edits(ours)? {
        for (ts, te, t) in edits(theirs)? {
            if os <= te && ts <= oe {
                overlaps.push(Overlap {
                    ours: o.clone(),
                    theirs: t,
                    at: os.max(ts) + 1,
                });
            }
        }
    }
    let merged = merge_text(base, ours, theirs, MergeOverlap::KeepBoth)?
        .ok_or("merge did not keep both sides")?;
    Ok((merged, overlaps))
}

#[derive(Debug, Clone, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct Patch {
    pub find: String,
    pub replace: String,
}

pub(crate) fn requested_edit(
    content: Option<&str>,
    base: Option<&str>,
    append: Option<&str>,
    patch: Option<&Patch>,
    has_fields: bool,
) -> Result<BodyEdit, String> {
    if [content.is_some(), append.is_some(), patch.is_some()]
        .into_iter()
        .filter(|v| *v)
        .count()
        > 1
    {
        return Err("give one of content, append or patch".into());
    }
    Ok(if let Some(content) = content {
        match base {
            Some(base) => BodyEdit::ReplaceFrom {
                base: base.into(),
                content: content.into(),
            },
            None => BodyEdit::Replace(content.into()),
        }
    } else if let Some(text) = append {
        BodyEdit::Append(text.into())
    } else if let Some(patch) = patch {
        BodyEdit::Patch {
            find: patch.find.clone(),
            replace: patch.replace.clone(),
        }
    } else if has_fields {
        BodyEdit::Keep
    } else {
        return Err("give content, append, patch or fields".into());
    })
}
