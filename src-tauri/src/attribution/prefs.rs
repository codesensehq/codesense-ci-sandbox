// SPDX-License-Identifier: MIT
//! Repository capture choices, independent of the checkout's attachment lifecycle.

use crate::domain::Principal;
use crate::shared::json_io::{read_json, write_json_atomic, JsonIoError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttributionMode {
    On,
    Off,
}

/// Where a repo's attribution notes go. Unset means local: they stay on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotesMode {
    /// Notes stay on this machine (`refs/notes/sense`, never pushed).
    Local,
    /// Notes push to this repo's origin with the branch.
    Push,
}

/// A repo's capture choice and where its notes go. Capture is on unless the repo is set off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AttributionPrefs {
    #[serde(default)]
    pub mode: Option<AttributionMode>,
    #[serde(default)]
    pub notes: Option<NotesMode>,
}

impl AttributionPrefs {
    pub(crate) fn notes_mode(&self) -> NotesMode {
        self.notes.unwrap_or(NotesMode::Local)
    }
}

/// A repository's capture choice as the save-source tool names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttributionChoice {
    On,
    Off,
    /// No explicit choice: capture is on.
    Auto,
}

type Store = BTreeMap<String, AttributionPrefs>;
// MCP and local launches share the app process; remote capture is out of scope.
// This process mutex serializes every writer of this machine-local preferences file.
static PREF_WRITES: Mutex<()> = Mutex::new(());

fn file_path(home: &Path, principal: &Principal) -> PathBuf {
    crate::context_repo::path_to_user_home(home, principal).join("attribution-prefs.json")
}

fn load(home: &Path, principal: &Principal) -> Result<Store, String> {
    match read_json(&file_path(home, principal)) {
        Ok(store) => Ok(store),
        Err(JsonIoError::Io(error))
            if std::io::Error::kind(&error) == std::io::ErrorKind::NotFound =>
        {
            Ok(BTreeMap::new())
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn read(
    home: &Path,
    principal: &Principal,
    key: &str,
) -> Result<AttributionPrefs, String> {
    let _write = PREF_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let store = load(home, principal)?;
    Ok(store.get(key).copied().unwrap_or_default())
}

/// The only writer: re-read under the lock, change one identity, then replace atomically.
/// Entries outlive attachments, so replacing or removing a clone cannot erase a choice.
pub(crate) fn update(
    home: &Path,
    principal: &Principal,
    key: &str,
    change: impl FnOnce(&mut AttributionPrefs),
) -> Result<(), String> {
    let _write = PREF_WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load(home, principal)?;
    change(store.entry(key.to_owned()).or_default());
    write_json_atomic(&file_path(home, principal), &store).map_err(|e| e.to_string())?;
    drop(_write);
    super::watcher::refresh(home);
    Ok(())
}

/// Inspect the checkout's current origin, using the same identity normalization as attach.
/// Never use a cached attachment URL for a launch or a notes-push destination.
pub(crate) fn repo_key_for_checkout(checkout: &Path) -> Result<(String, Option<String>), String> {
    let attachment = crate::commands::attachments::existing_checkout_attachment(checkout)?;
    let key = if attachment.remote_url.is_some() {
        crate::commands::attachments::attachment_key(&attachment)
    } else {
        // Local-only linked worktrees share their main checkout's repository identity.
        crate::operations::git_remote::derive_repository_identity(checkout)
            .map(|identity| crate::domain::SourceRepoSlug::remote_key(&identity))
    }
    .ok_or_else(|| "cannot resolve repository identity".to_owned())?;
    Ok((key, attachment.remote_url))
}

/// Save-source's optional parameters modify the current origin's entry, not its clone row.
/// Pushing notes needs an origin to push them to: without one it is refused (`no_remote`).
pub(crate) fn set_for_checkout(
    home: &Path,
    principal: &Principal,
    checkout: &str,
    choice: Option<AttributionChoice>,
    notes: Option<NotesMode>,
) -> Result<(), String> {
    check_notes(checkout, notes)?;
    let (key, _) = repo_key_for_checkout(Path::new(checkout))?;
    update(home, principal, &key, |prefs| {
        match choice {
            Some(AttributionChoice::On) => prefs.mode = Some(AttributionMode::On),
            Some(AttributionChoice::Off) => prefs.mode = Some(AttributionMode::Off),
            Some(AttributionChoice::Auto) => prefs.mode = None,
            None => {}
        }
        if let Some(notes) = notes {
            prefs.notes = Some(notes);
        }
    })
}

/// Every local principal's preferences in `home`: the managed git-ai config is one per home,
/// so every profile's push choice counts.
fn all_stores(home: &Path) -> Vec<(Principal, Store)> {
    crate::context_repo::org_enumeration::principals(home)
        .into_iter()
        .filter_map(|principal| {
            let store = {
                let _write = PREF_WRITES.lock().unwrap_or_else(|e| e.into_inner());
                Result::ok(load(home, &principal))?
            };
            Some((principal, store))
        })
        .collect()
}

/// The repos set `notes: push` by any local profile, by repository key.
pub(crate) fn push_repos(home: &Path) -> Vec<String> {
    let mut repos = std::collections::BTreeSet::new();
    for (_, store) in all_stores(home) {
        for (key, prefs) in store {
            if prefs.notes == Some(NotesMode::Push) {
                repos.insert(key);
            }
        }
    }
    repos.into_iter().collect()
}

/// The destinations git-ai may push notes to: the current origin of every attached checkout of
/// a repo set `push`, as it is (without credentials) and in its https and ssh forms. Sorted, so
/// the managed config's stamp is stable.
pub(crate) fn push_destinations(home: &Path) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for (principal, store) in all_stores(home) {
        // Every launch applies the config: with nothing set to push, no checkout is opened.
        if !std::iter::Iterator::any(&mut store.values(), |prefs| {
            prefs.notes == Some(NotesMode::Push)
        }) {
            continue;
        }
        for checkout in attached_checkouts(home, &principal) {
            #[cfg(test)]
            opened::count();
            // The origin as the checkout has it now, never the attachment's cached URL.
            let Ok((key, Some(url))) = repo_key_for_checkout(&checkout) else {
                continue;
            };
            if store.get(&key).map(AttributionPrefs::notes_mode) != Some(NotesMode::Push) {
                continue;
            }
            out.insert(without_credentials(&url));
            let Ok(origin) = crate::context_repo::normalize_source_repo_url(&url) else {
                continue;
            };
            let host = Option::unwrap_or_default(std::iter::Iterator::next(&mut str::split(
                &origin.normalized_url,
                '/',
            )));
            out.insert(format!(
                "https://{host}/{}/{}.git",
                origin.owner, origin.repo
            ));
            out.insert(format!("git@{host}:{}/{}.git", origin.owner, origin.repo));
        }
    }
    out.into_iter().collect()
}

/// Checkouts `push_destinations` opened on this thread, for tests.
#[cfg(test)]
pub(crate) mod opened {
    use std::cell::Cell;
    thread_local! {
        static OPENED: Cell<usize> = const { Cell::new(0) };
    }
    pub(crate) fn count() {
        OPENED.with(|n| n.set(n.get() + 1));
    }
    pub(crate) fn take() -> usize {
        OPENED.with(|n| n.replace(0))
    }
}

/// The checkouts `principal` attached in `home`.
pub(crate) fn attached_checkouts(home: &Path, principal: &Principal) -> Vec<PathBuf> {
    Result::map_or_else(
        crate::context_repo::attachments_io::read_attachments(
            &crate::context_repo::path_to_user_attachments_file(home, principal),
        ),
        |error| {
            log::warn!(target: "attribution", "attached checkouts unreadable: {error}");
            Vec::new()
        },
        |attachments| {
            attachments
                .attachments
                .into_iter()
                .map(|row| PathBuf::from(row.repo_path))
                .collect()
        },
    )
}

/// Every checkout any local profile attached in `home`.
pub(crate) fn all_attached_checkouts(home: &Path) -> Vec<PathBuf> {
    let principals = crate::context_repo::org_enumeration::principals(home);
    std::iter::Iterator::collect(std::iter::Iterator::flat_map(
        principals.iter(),
        |principal| attached_checkouts(home, principal),
    ))
}

/// `url` without the userinfo of an `http(s)://user[:token]@host/...` URL, which can hold a
/// token. An ssh user (`ssh://git@…`, `git@host:path`) names an account, not a secret, and stays.
fn without_credentials(url: &str) -> String {
    let url = str::trim(url);
    let Some((scheme, rest)) = str::split_once(url, "://") else {
        return url.to_owned();
    };
    if !str::eq_ignore_ascii_case(scheme, "https") && !str::eq_ignore_ascii_case(scheme, "http") {
        return url.to_owned();
    }
    let (authority, path) = Option::unwrap_or(str::split_once(rest, '/'), (rest, ""));
    let host = Option::map_or(str::rsplit_once(authority, '@'), authority, |(_, host)| {
        host
    });
    format!("{scheme}://{host}/{path}")
}

/// A notes choice the checkout can take: pushing needs an origin. Checked before anything is
/// saved, so a refused request changes nothing.
pub(crate) fn check_notes(checkout: &str, notes: Option<NotesMode>) -> Result<(), String> {
    if notes != Some(NotesMode::Push) {
        return Ok(());
    }
    match repo_key_for_checkout(Path::new(checkout))? {
        (_, Some(_)) => Ok(()),
        (_, None) => {
            Err("no_remote: notes can push only to an origin; the repo stays local".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::resolve::tests::alice;
    use crate::attribution::testing::Launch;
    use codesense_pty_core::agent_cli::AgentCli;

    #[test]
    fn save_choices_keep_omitted_fields_and_auto_clears_only_the_mode() {
        let fixture = Launch::new(AgentCli::Claude);
        let home = fixture.home();
        let path = fixture.cwd.to_string_lossy();
        set_for_checkout(home, &alice(), &path, None, Some(NotesMode::Push)).unwrap();
        let (key, _) = repo_key_for_checkout(&fixture.cwd).unwrap();
        let pushed = read(home, &alice(), &key).unwrap();
        assert_eq!(pushed.mode, Some(AttributionMode::On));
        assert_eq!(pushed.notes, Some(NotesMode::Push));
        set_for_checkout(home, &alice(), &path, Some(AttributionChoice::Off), None).unwrap();
        let off = read(home, &alice(), &key).unwrap();
        assert_eq!(off.mode, Some(AttributionMode::Off));
        assert_eq!(off.notes, Some(NotesMode::Push));
        set_for_checkout(home, &alice(), &path, Some(AttributionChoice::Auto), None).unwrap();
        let automatic = read(home, &alice(), &key).unwrap();
        assert_eq!(automatic.mode, None);
        assert_eq!(automatic.notes, Some(NotesMode::Push));
        let wire: serde_json::Value = read_json(&file_path(home, &alice())).unwrap();
        assert!(wire[&key]["mode"].is_null());
        assert_eq!(wire[&key]["notes"], "push");
        set_for_checkout(home, &alice(), &path, None, Some(NotesMode::Local)).unwrap();
        let local = read(home, &alice(), &key).unwrap();
        assert_eq!(AttributionPrefs::notes_mode(&local), NotesMode::Local);
        assert_eq!(
            AttributionPrefs::notes_mode(&AttributionPrefs::default()),
            NotesMode::Local
        );
    }
}
