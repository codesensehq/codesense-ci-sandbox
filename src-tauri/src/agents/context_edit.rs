// SPDX-License-Identifier: MIT
//! SW-XS-15: one "Add to / Remove from ‹Agent›'s context" edit from the left nav,
//! applied to the agent's STORED definition (so keys a newer build added, on the
//! agent's entries and on its overrides, are carried through untouched).
//!
//! Team-wide edits change `context`; "… for ‹Repo› only" edits change the agent's
//! override for that repo. While the agent has no `context`, a team-wide edit writes
//! the runtime default (`default_context(runtime, false)`) out explicitly, plus the
//! change: the edit makes the agent's context explicit for every launch.
//!
//! `remove` entries are globs; a path written into one has its glob metacharacters
//! escaped, and coverage is checked with the same matcher the resolver uses.

use super::context::{default_context, ContextEntry, RepoOverride};
use super::model::Agent;

/// Which edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextOp {
    Add,
    Remove,
    /// SW-XS-16: write the runtime default out as the agent's own list ("Customize").
    Customize,
    /// SW-XS-16: add several entries in one revision (an accepted suggestion).
    AddEntries,
    /// SW-XS-16: drop one exact `remove` pattern (a glob included) from a repo's override.
    DropRemove,
    /// SW-XS-16: drop one `add` entry from a repo's override, and only that: the repo then
    /// gets what everywhere else gets (never a new `remove`).
    DropAdd,
}

/// What an edit changes, or why it cannot be made here.
#[derive(Debug, Clone, PartialEq)]
pub enum ContextEdit {
    /// The new `context` (team-wide) or the new override for the repo.
    Context(Vec<ContextEntry>),
    Override(RepoOverride),
    /// Nothing to write, with the reason the person is shown.
    Blocked(String),
}

/// Is `path` the entry `entry` or inside it?
fn under(path: &str, entry: &str) -> bool {
    path == entry
        || path
            .strip_prefix(entry)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `path` and each folder above it.
fn self_and_ancestors(path: &str) -> impl Iterator<Item = &str> {
    std::iter::once(path).chain(path.match_indices('/').map(|(i, _)| &path[..i]))
}

/// A literal path as a `remove` glob: every glob metacharacter in a class of its own.
pub fn escape_glob(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '*' | '?' | '[' | ']' | '{' | '}' => {
                out.push('[');
                out.push(c);
                out.push(']');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Normalize a nav path: NFC (as stored paths are), no leading `./`, no trailing `/`,
/// no blanks.
pub fn normalize_path(raw: &str) -> Option<String> {
    use unicode_normalization::UnicodeNormalization;
    let nfc: String = raw.nfc().collect();
    let trimmed = nfc.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    if trimmed.is_empty() || trimmed == "." || trimmed.starts_with('/') {
        return None;
    }
    Some(trimmed.to_string())
}

/// The agent's `context` as an edit starts from it.
fn base_context(agent: &Agent) -> Vec<ContextEntry> {
    agent
        .context
        .clone()
        .unwrap_or_else(|| default_context(agent.runtime, false))
}

fn display_name(agent: &Agent) -> &str {
    if agent.name.trim().is_empty() {
        &agent.id
    } else {
        &agent.name
    }
}

/// The startup path that pins `path`, if one does.
fn pinned_by<'a>(agent: &'a Agent, path: &str) -> Option<&'a str> {
    agent
        .startup
        .iter()
        .map(|s| s.trim_end_matches('/'))
        .find(|s| under(path, s))
}

/// `list` after removing `path`: the entry for exactly `path` (whatever its `dest`) and the
/// entries it places the same, keeping nested entries that land elsewhere; with no entry
/// for exactly `path`, every entry inside it.
fn after_remove(list: &[ContextEntry], path: &str) -> Vec<ContextEntry> {
    match list.iter().find(|e| e.path == path) {
        Some(exact) => list
            .iter()
            .filter(|e| e.path != path && !exact.places_like(e))
            .cloned()
            .collect(),
        None => list
            .iter()
            .filter(|e| !under(&e.path, path))
            .cloned()
            .collect(),
    }
}

fn not_a_path_edit() -> String {
    "That edit is not an add or a remove of one path.".into()
}

/// Plan one edit of `path` against the stored `agent` (and, with `repo_override`
/// given as `Some`, the agent's override for that repo: `Some(None)` = none yet).
pub fn plan(
    agent: &Agent,
    op: ContextOp,
    path: &str,
    repo_override: Option<Option<&RepoOverride>>,
) -> ContextEdit {
    let Some(path) = normalize_path(path) else {
        return ContextEdit::Blocked("Pick a file or folder.".into());
    };
    let name = display_name(agent);
    let base = base_context(agent);
    // Coverage is by place (SW-XS-16 r2): an entry covers `path` only when it delivers it
    // where an entry for `path` itself would land it. A folder above that remaps it does
    // not, so a nested entry with its own place is never "already included".
    let new = ContextEntry::new(path.clone());
    let in_base = |p: &ContextEntry| base.iter().find(|e| e.places_like(p));
    match (op, repo_override) {
        // The editor's own operations have their own planners (`plan_customize`, …).
        (
            ContextOp::Customize
            | ContextOp::AddEntries
            | ContextOp::DropRemove
            | ContextOp::DropAdd,
            _,
        ) => ContextEdit::Blocked(not_a_path_edit()),
        (ContextOp::Add, None) => {
            if let Some(e) = in_base(&new) {
                return ContextEdit::Blocked(format!("Already included through {}.", e.path));
            }
            let mut next: Vec<ContextEntry> =
                base.into_iter().filter(|e| keep_beside(e, &new)).collect();
            next.push(new);
            ContextEdit::Context(next)
        }
        (ContextOp::Remove, None) => {
            if let Some(pin) = pinned_by(agent, &path) {
                return ContextEdit::Blocked(format!(
                    "{name} starts with {pin}. Change its startup files in the agent editor."
                ));
            }
            let exact = base.iter().any(|e| e.path == path);
            if let Some(outer) = base
                .iter()
                .find(|e| !exact && under(&path, &e.path) && e.path != path)
            {
                return ContextEdit::Blocked(format!(
                    "Included through {}. Remove that folder, or pick a repo and remove it for \
                     that repo only.",
                    outer.path
                ));
            }
            let next = after_remove(&base, &path);
            if next.len() == base.len() {
                return ContextEdit::Blocked(format!("Not in {name}'s own context."));
            }
            ContextEdit::Context(next)
        }
        (op, Some(current)) => {
            let mut o = current.cloned().unwrap_or_default();
            let globs = o.normalized().map(|n| n.remove_set()).unwrap_or_default();
            let matches = |g: &str, p: &str| {
                RepoOverride {
                    remove: vec![g.to_string()],
                    ..Default::default()
                }
                .remove_set()
                .is_match(p)
            };
            let removed = |p: &str| self_and_ancestors(p).any(|a| globs.is_match(a));
            // Whether a set still brings `p`'s content (by prefix: a `remove` glob drops
            // content wherever it lands) / delivers it where `new` would (by place).
            let brings = |o: &RepoOverride, p: &str| {
                base.iter().chain(o.add.iter()).any(|e| under(p, &e.path))
            };
            let places =
                |o: &RepoOverride| base.iter().chain(o.add.iter()).any(|e| e.places_like(&new));
            let escaped = escape_glob(&path);
            match op {
                ContextOp::Customize
                | ContextOp::AddEntries
                | ContextOp::DropRemove
                | ContextOp::DropAdd => ContextEdit::Blocked(not_a_path_edit()),
                ContextOp::Add => {
                    // Only a removal of exactly this path, or of something inside it,
                    // is this edit's to undo. A broader pattern that matches it (or a
                    // folder above it) would bring back much more than was asked for.
                    let mine = |g: &str| g == escaped || under(g, &escaped);
                    if let Some(broader) = o
                        .remove
                        .iter()
                        .find(|g| !mine(g) && self_and_ancestors(&path).any(|a| matches(g, a)))
                    {
                        return ContextEdit::Blocked(format!(
                            "Removed for this repo through {broader}; edit that pattern in the \
                             agent editor."
                        ));
                    }
                    let before = o.remove.len();
                    o.remove.retain(|g| !mine(g));
                    let dropped = o.remove.len() != before;
                    if !places(&o) {
                        o.add.retain(|e| keep_beside(e, &new));
                        o.add.push(new);
                    } else if !dropped {
                        return ContextEdit::Blocked(format!(
                            "Already in {name}'s context for this repo."
                        ));
                    }
                    ContextEdit::Override(o)
                }
                ContextOp::Remove => {
                    if let Some(pin) = pinned_by(agent, &path) {
                        return ContextEdit::Blocked(format!(
                            "{name} starts with {pin}. Change its startup files in the agent \
                             editor."
                        ));
                    }
                    let before = o.add.len();
                    o.add = after_remove(&o.add, &path);
                    let dropped = o.add.len() != before;
                    let still = brings(&o, &path);
                    let already = removed(&path);
                    if still && !already {
                        o.remove.retain(|g| !under(g, &escaped));
                        o.remove.push(escaped);
                    } else if !dropped {
                        return ContextEdit::Blocked(format!(
                            "Not in {name}'s context for this repo."
                        ));
                    }
                    ContextEdit::Override(o)
                }
            }
        }
    }
}

// ---- SW-XS-16: the agent editor's Context section -------------------------------

/// A typed `dest`: trimmed, no trailing `/`; blank is no remap. The save validates it.
fn clean_dest(dest: Option<&str>) -> Option<String> {
    dest.map(|d| d.trim().trim_end_matches('/'))
        .filter(|d| !d.is_empty())
        .map(str::to_string)
}

/// Where `existing` would sit beside `new`: dropped when `new` delivers it at the same
/// place (it is then redundant), kept otherwise (a nested entry with its own `dest`).
fn keep_beside(existing: &ContextEntry, new: &ContextEntry) -> bool {
    !new.places_like(existing)
}

/// "Add" from the editor. Without a `dest` it is exactly the nav's add ([`plan`]). With
/// one, coverage is judged by place: a folder above the path that lands it elsewhere
/// does not cover it, an entry for the same path with another `dest` is never
/// overwritten (the edit is refused, saying where it lands now), and entries inside the
/// path that the new one would place the same are folded into it.
pub fn plan_add(
    agent: &Agent,
    path: &str,
    dest: Option<&str>,
    repo_override: Option<Option<&RepoOverride>>,
) -> ContextEdit {
    let Some(path) = normalize_path(path) else {
        return ContextEdit::Blocked("Pick a file or folder.".into());
    };
    // No dest, or one equal to the path, is no remap: exactly the nav's add.
    let Some(dest) = clean_dest(dest).filter(|d| *d != path) else {
        return plan(agent, ContextOp::Add, &path, repo_override);
    };
    let new = ContextEntry::with_dest(path.clone(), dest);
    let base = base_context(agent);
    let lands = |e: &ContextEntry| format!("{} already lands at {}.", e.path, e.target());
    let same_path_elsewhere = |list: &[ContextEntry]| {
        list.iter()
            .find(|e| e.path == path && e.target() != new.target())
            .cloned()
    };
    match repo_override {
        None => {
            if let Some(e) = base.iter().find(|e| e.places_like(&new)) {
                return ContextEdit::Blocked(format!("Already included through {}.", e.path));
            }
            if let Some(e) = same_path_elsewhere(&base) {
                return ContextEdit::Blocked(format!("{} Remove it first to move it.", lands(&e)));
            }
            let mut next: Vec<ContextEntry> =
                base.into_iter().filter(|e| keep_beside(e, &new)).collect();
            next.push(new);
            ContextEdit::Context(next)
        }
        Some(current) => {
            let mut o = current.cloned().unwrap_or_default();
            if base.iter().chain(o.add.iter()).any(|e| e.places_like(&new)) {
                return ContextEdit::Blocked(format!(
                    "Already in {}'s context for this repo.",
                    display_name(agent)
                ));
            }
            if let Some(e) = same_path_elsewhere(&o.add) {
                return ContextEdit::Blocked(format!(
                    "{} for this repo. Remove it first to move it.",
                    lands(&e).trim_end_matches('.')
                ));
            }
            // A path the repo leaves out stays out whatever its `dest`.
            if let Some(g) = o.remove.iter().find(|g| {
                RepoOverride {
                    remove: vec![(*g).clone()],
                    ..Default::default()
                }
                .remove_set()
                .is_match(&new.path)
            }) {
                return ContextEdit::Blocked(format!(
                    "Left out for this repo through {g}; stop leaving it out first."
                ));
            }
            o.add.retain(|e| keep_beside(e, &new));
            o.add.push(new);
            ContextEdit::Override(o)
        }
    }
}

/// "Customize": the runtime default, written out as the agent's own `context`.
pub fn plan_customize(agent: &Agent) -> ContextEdit {
    if agent.context.is_some() {
        return ContextEdit::Blocked(format!(
            "{}'s context is already its own list.",
            display_name(agent)
        ));
    }
    ContextEdit::Context(base_context(agent))
}

/// Add `entries` (each keeping its `dest`) to the agent's context in one edit: the runtime
/// default is written out first. An entry is skipped only when one already there delivers
/// it at the same place ([`ContextEntry::places_like`], the test `suggestion_for` applies
/// too, so an accepted suggestion is never offered again); entries inside it that it would
/// place the same are folded into it, and ones placed elsewhere are kept.
pub fn plan_add_entries(agent: &Agent, entries: &[ContextEntry]) -> ContextEdit {
    let mut next = base_context(agent);
    let mut added = 0;
    for entry in entries {
        let Some(path) = normalize_path(&entry.path) else {
            continue;
        };
        let dest = clean_dest(entry.dest.as_deref()).filter(|d| *d != path);
        let new = ContextEntry {
            path,
            dest,
            extra: entry.extra.clone(),
        };
        if next.iter().any(|e| e.places_like(&new)) {
            continue;
        }
        next.retain(|e| keep_beside(e, &new));
        next.push(new);
        added += 1;
    }
    if added == 0 {
        return ContextEdit::Blocked(format!("Already in {}'s context.", display_name(agent)));
    }
    ContextEdit::Context(next)
}

/// A stored pattern or path as the editor names it: trimmed, no trailing `/`.
fn trimmed(raw: &str) -> &str {
    raw.trim().trim_end_matches('/')
}

/// Drop `pattern` from the repo's `remove` list (compared as stored, ignoring a trailing
/// `/`), every other key kept.
pub fn plan_drop_remove(
    agent: &Agent,
    pattern: &str,
    repo_override: Option<&RepoOverride>,
) -> ContextEdit {
    let pattern = trimmed(pattern);
    let not_there = || {
        ContextEdit::Blocked(format!(
            "{pattern} is not left out of {}'s context for this repo.",
            display_name(agent)
        ))
    };
    let Some(current) = repo_override else {
        return not_there();
    };
    let mut o = current.clone();
    let before = o.remove.len();
    o.remove.retain(|g| trimmed(g) != pattern);
    if o.remove.len() == before {
        return not_there();
    }
    ContextEdit::Override(o)
}

/// Drop the `add` entry for `path` from the repo's override, and nothing else: unlike a
/// repo-only remove, it never writes a `remove`, so the repo goes back to what every
/// other workspace gets (which may still include the path).
pub fn plan_drop_add(
    agent: &Agent,
    path: &str,
    repo_override: Option<&RepoOverride>,
) -> ContextEdit {
    let not_there = || {
        ContextEdit::Blocked(format!(
            "{} does not add {} for this repo.",
            display_name(agent),
            trimmed(path)
        ))
    };
    let (Some(path), Some(current)) = (normalize_path(path), repo_override) else {
        return not_there();
    };
    let mut o = current.clone();
    let before = o.add.len();
    o.add.retain(|e| e.path != path);
    if o.add.len() == before {
        return not_there();
    }
    ContextEdit::Override(o)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use codesense_pty_core::agent_cli::AgentCli;

    fn barb(context: Option<Vec<&str>>) -> Agent {
        let mut a = crate::routines::test_support::agent("barb", AgentCli::Claude);
        a.name = "Barb".into();
        a.startup = vec!["CLAUDE.md".into()];
        a.context = context.map(|c| c.into_iter().map(ContextEntry::new).collect());
        a
    }

    fn paths(edit: &ContextEdit) -> Vec<String> {
        match edit {
            ContextEdit::Context(c) => c.iter().map(|e| e.path.clone()).collect(),
            other => panic!("expected a context, got {other:?}"),
        }
    }

    #[test]
    fn a_first_team_wide_add_writes_the_runtime_default_explicitly() {
        let edit = plan(&barb(None), ContextOp::Add, "docs", None);
        assert_eq!(
            paths(&edit),
            [
                "CLAUDE.md",
                ".claude/commands",
                ".claude/hooks",
                ".claude/agents",
                ".claude/skills",
                "docs"
            ]
        );
    }

    #[test]
    fn team_wide_edits_keep_keys_a_newer_build_added() {
        let mut agent = barb(Some(vec!["docs", ".claude/skills"]));
        agent.context.as_mut().unwrap()[1].extra =
            BTreeMap::from([("weight".to_string(), serde_yaml::Value::from(3))]);
        let ContextEdit::Context(next) = plan(&agent, ContextOp::Remove, "docs", None) else {
            panic!("expected a context");
        };
        assert_eq!(next.len(), 1);
        assert_eq!(
            next[0].extra.get("weight"),
            Some(&serde_yaml::Value::from(3))
        );
    }

    #[test]
    fn covered_adds_and_inner_removes_are_refused_with_the_reason() {
        let agent = barb(Some(vec![".claude/skills"]));
        assert_eq!(
            plan(&agent, ContextOp::Add, ".claude/skills/review", None),
            ContextEdit::Blocked("Already included through .claude/skills.".into())
        );
        assert!(matches!(
            plan(&agent, ContextOp::Remove, ".claude/skills/review", None),
            ContextEdit::Blocked(r) if r.starts_with("Included through .claude/skills")
        ));
        assert!(matches!(
            plan(&agent, ContextOp::Remove, "CLAUDE.md", Some(None)),
            ContextEdit::Blocked(r) if r.starts_with("Barb starts with CLAUDE.md")
        ));
    }

    #[test]
    fn a_repo_only_remove_escapes_glob_characters_and_still_matches_the_path() {
        let agent = barb(Some(vec!["docs"]));
        let ContextEdit::Override(o) = plan(
            &agent,
            ContextOp::Remove,
            "docs/[draft] *notes*",
            Some(None),
        ) else {
            panic!("expected an override");
        };
        assert_eq!(o.remove, ["docs/[[]draft[]] [*]notes[*]"]);
        let set = o.normalized().unwrap().remove_set();
        assert!(set.is_match("docs/[draft] *notes*"));
        assert!(
            !set.is_match("docs/d draft-notes-x"),
            "matches only the literal path"
        );
    }

    #[test]
    fn a_repo_only_add_undoes_a_removal_and_respects_an_outer_glob() {
        let agent = barb(Some(vec![".claude/skills"]));
        let current = RepoOverride {
            remove: vec![".claude/skills/legacy".into()],
            extra: BTreeMap::from([("note".to_string(), serde_yaml::Value::from("keep"))]),
            ..Default::default()
        };
        let ContextEdit::Override(o) = plan(
            &agent,
            ContextOp::Add,
            ".claude/skills/legacy",
            Some(Some(&current)),
        ) else {
            panic!("expected an override");
        };
        assert!(o.remove.is_empty() && o.add.is_empty());
        assert_eq!(o.extra.get("note"), Some(&serde_yaml::Value::from("keep")));

        let globbed = RepoOverride {
            remove: vec![".claude/skills/legacy-*".into()],
            ..Default::default()
        };
        assert!(matches!(
            plan(&agent, ContextOp::Add, ".claude/skills/legacy-a/SKILL.md", Some(Some(&globbed))),
            ContextEdit::Blocked(r) if r.contains("through .claude/skills/legacy-*")
        ));
        // A broader glob that matches the path itself is not this edit's to drop:
        // dropping it would bring back every other `legacy-*` folder too.
        assert_eq!(
            plan(
                &agent,
                ContextOp::Add,
                ".claude/skills/legacy-a",
                Some(Some(&globbed))
            ),
            ContextEdit::Blocked(
                "Removed for this repo through .claude/skills/legacy-*; edit that pattern in \
                 the agent editor."
                    .into()
            )
        );
        // An exact (escaped) removal, and removals inside the path, are dropped.
        let exact = RepoOverride {
            remove: vec![
                escape_glob(".claude/skills/[old]"),
                ".claude/skills/[[]old[]]/sub".into(),
                ".claude/skills/other".into(),
            ],
            ..Default::default()
        };
        let ContextEdit::Override(o) = plan(
            &agent,
            ContextOp::Add,
            ".claude/skills/[old]",
            Some(Some(&exact)),
        ) else {
            panic!("expected an override");
        };
        assert_eq!(o.remove, [".claude/skills/other"]);
    }

    #[test]
    fn a_nav_path_is_nfc_like_the_stored_ones() {
        assert_eq!(
            normalize_path("./docs/cafe\u{301}/").as_deref(),
            Some("docs/caf\u{e9}")
        );
    }

    /// The remove-glob cases the nav's TS matcher (`globMatcher`) is checked
    /// against too (`agent-context-model.test.ts`), so the two never drift.
    #[test]
    fn remove_globs_match_the_shared_fixture() {
        #[derive(serde::Deserialize)]
        struct Case {
            glob: String,
            path: String,
            matches: bool,
        }
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("testdata/remove_glob_cases.json")).unwrap();
        assert!(cases.len() >= 10);
        for case in cases {
            let set = RepoOverride {
                remove: vec![case.glob.clone()],
                ..Default::default()
            }
            .normalized()
            .unwrap()
            .remove_set();
            assert_eq!(
                set.is_match(&case.path),
                case.matches,
                "{} vs {}",
                case.glob,
                case.path
            );
        }
    }

    // ---- SW-XS-16: the agent editor's Context section ---------------------------

    #[test]
    fn xs16_customize_writes_the_runtime_default_out_once() {
        assert_eq!(
            paths(&plan_customize(&barb(None))),
            [
                "CLAUDE.md",
                ".claude/commands",
                ".claude/hooks",
                ".claude/agents",
                ".claude/skills"
            ]
        );
        assert!(matches!(
            plan_customize(&barb(Some(vec!["docs"]))),
            ContextEdit::Blocked(r) if r.contains("already")
        ));
    }

    #[test]
    fn xs16_an_add_with_a_dest_keeps_the_remap_team_wide_and_for_a_repo() {
        let agent = barb(Some(vec![".claude/skills"]));
        let ContextEdit::Context(next) =
            plan_add(&agent, "_bmad-output/plans", Some("plans"), None)
        else {
            panic!("expected a context");
        };
        assert_eq!(
            next.last(),
            Some(&ContextEntry::with_dest("_bmad-output/plans", "plans"))
        );
        let ContextEdit::Override(o) =
            plan_add(&agent, "docs/architecture", Some("arch"), Some(None))
        else {
            panic!("expected an override");
        };
        assert_eq!(
            o.add,
            [ContextEntry::with_dest("docs/architecture", "arch")]
        );
        // A blank dest is no remap.
        let ContextEdit::Context(next) = plan_add(&agent, "docs", Some("  "), None) else {
            panic!("expected a context");
        };
        assert_eq!(next.last(), Some(&ContextEntry::new("docs")));
    }

    /// SHOULD-2: coverage with a `dest` is by place. A folder above the path that lands
    /// it elsewhere does not cover it; the same path already landing elsewhere is refused,
    /// never overwritten (team-wide and in a repo's adds); a path the repo leaves out
    /// stays out.
    #[test]
    fn xs16_a_dest_add_is_judged_by_place_and_never_overwrites_a_dest() {
        let agent = barb(Some(vec![".claude/skills"]));
        let ContextEdit::Context(next) = plan_add(&agent, ".claude/skills/foo", Some("foo"), None)
        else {
            panic!("a nested entry landing elsewhere is not covered");
        };
        assert_eq!(
            next,
            [
                ContextEntry::new(".claude/skills"),
                ContextEntry::with_dest(".claude/skills/foo", "foo")
            ]
        );
        assert!(matches!(
            plan_add(&agent, ".claude/skills/foo", Some(".claude/skills/foo"), None),
            ContextEdit::Blocked(r) if r.starts_with("Already included through .claude/skills")
        ));
        let mut remapped = barb(None);
        remapped.context = Some(vec![ContextEntry::with_dest("docs", "kb")]);
        assert!(matches!(
            plan_add(&remapped, "docs", Some("guides"), None),
            ContextEdit::Blocked(r) if r.contains("docs already lands at kb")
        ));
        let current = RepoOverride {
            add: vec![ContextEntry::with_dest("notes", "n")],
            remove: vec!["secret/**".into()],
            ..Default::default()
        };
        assert!(matches!(
            plan_add(&agent, "notes", Some("other"), Some(Some(&current))),
            ContextEdit::Blocked(r) if r.contains("notes already lands at n")
        ));
        assert!(matches!(
            plan_add(&agent, "secret/x", Some("x"), Some(Some(&current))),
            ContextEdit::Blocked(r) if r.starts_with("Left out for this repo through secret/**")
        ));
    }

    /// M1: a suggested entry nested under a default folder but landing elsewhere
    /// (`projections.yml`'s own default `dest` is its last segment) is added, not
    /// skipped as covered; an entry the list already places the same is skipped; and
    /// entries inside a new one that it would place differently are kept.
    #[test]
    fn xs16_adding_entries_appends_what_is_missing_in_one_edit() {
        let agent = barb(None);
        let ContextEdit::Context(next) = plan_add_entries(
            &agent,
            &[
                ContextEntry::new(".claude/skills/review"),
                ContextEntry::with_dest(".claude/skills/foo", "foo"),
                ContextEntry::with_dest("_bmad-output/plans", "plans"),
                ContextEntry::new("docs"),
            ],
        ) else {
            panic!("expected a context");
        };
        let shown: Vec<(&str, Option<&str>)> = next
            .iter()
            .map(|e| (e.path.as_str(), e.dest.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                ("CLAUDE.md", None),
                (".claude/commands", None),
                (".claude/hooks", None),
                (".claude/agents", None),
                (".claude/skills", None),
                (".claude/skills/foo", Some("foo")),
                ("_bmad-output/plans", Some("plans")),
                ("docs", None),
            ],
            "the default is written out first; only an entry placed the same is skipped"
        );
        let mut nested = barb(None);
        nested.context = Some(vec![
            ContextEntry::with_dest("docs/arch", "arch"),
            ContextEntry::new("docs/guide.md"),
        ]);
        let ContextEdit::Context(next) = plan_add_entries(&nested, &[ContextEntry::new("docs")])
        else {
            panic!("expected a context");
        };
        assert_eq!(
            next,
            [
                ContextEntry::with_dest("docs/arch", "arch"),
                ContextEntry::new("docs")
            ],
            "docs/guide.md is folded into docs; docs/arch -> arch is kept"
        );
        assert!(matches!(
            plan_add_entries(&barb(Some(vec!["docs"])), &[ContextEntry::new("docs/guide.md")]),
            ContextEdit::Blocked(r) if r.starts_with("Already")
        ));
        assert!(matches!(
            plan_add_entries(&agent, &[]),
            ContextEdit::Blocked(_)
        ));
    }

    /// SHOULD-1: dropping a repo's add never writes a `remove`, even when the base
    /// context covers the path (a repo-only remove would have).
    #[test]
    fn xs16_dropping_a_repo_add_the_base_covers_writes_no_remove() {
        let agent = barb(Some(vec!["docs"]));
        let current = RepoOverride {
            add: vec![
                ContextEntry::new("docs/architecture"),
                ContextEntry::new("kb"),
            ],
            ..Default::default()
        };
        let ContextEdit::Override(o) = plan_drop_add(&agent, "docs/architecture/", Some(&current))
        else {
            panic!("expected an override");
        };
        assert_eq!(o.add, [ContextEntry::new("kb")]);
        assert!(o.remove.is_empty(), "{:?}", o.remove);
        assert!(matches!(
            plan_drop_add(&agent, "elsewhere", Some(&current)),
            ContextEdit::Blocked(_)
        ));
        // A trailing `/` on either side names the same remove pattern.
        let slashed = RepoOverride {
            remove: vec!["docs/old/".into()],
            ..Default::default()
        };
        assert!(matches!(
            plan_drop_remove(&agent, "docs/old", Some(&slashed)),
            ContextEdit::Override(o) if o.remove.is_empty()
        ));
    }

    /// N1 (r2): the nav's add and remove judge by place too. Removing a nested remap
    /// (one an Accept added) removes just it; removing its parent keeps it; adding a
    /// parent keeps it, team-wide and for a repo.
    #[test]
    fn xs16_nav_add_and_remove_keep_nested_remaps() {
        let mut agent = barb(None);
        agent.context = Some(vec![
            ContextEntry::new(".claude/skills"),
            ContextEntry::new(".claude/skills/review"),
            ContextEntry::with_dest(".claude/skills/foo", "foo"),
        ]);
        // Remove the nested remap after Accept: not "Included through .claude/skills".
        let ContextEdit::Context(next) =
            plan(&agent, ContextOp::Remove, ".claude/skills/foo", None)
        else {
            panic!("an exact entry is removable");
        };
        assert_eq!(
            next,
            [
                ContextEntry::new(".claude/skills"),
                ContextEntry::new(".claude/skills/review")
            ]
        );
        // Remove the parent: it and what it places the same go; the remap stays.
        let ContextEdit::Context(next) = plan(&agent, ContextOp::Remove, ".claude/skills", None)
        else {
            panic!("expected a context");
        };
        assert_eq!(next, [ContextEntry::with_dest(".claude/skills/foo", "foo")]);
        // A path inside an entry with no entry of its own is still refused.
        assert!(matches!(
            plan(&agent, ContextOp::Remove, ".claude/skills/review/SKILL.md", None),
            ContextEdit::Blocked(r) if r.starts_with("Included through")
        ));

        // Add a parent: the remap stays beside it (team-wide)…
        let mut remap = barb(None);
        remap.context = Some(vec![ContextEntry::with_dest("docs/arch", "arch")]);
        let ContextEdit::Context(next) = plan(&remap, ContextOp::Add, "docs", None) else {
            panic!("expected a context");
        };
        assert_eq!(
            next,
            [
                ContextEntry::with_dest("docs/arch", "arch"),
                ContextEntry::new("docs")
            ]
        );
        // …and a nested path the remapping parent lands elsewhere is not "included".
        let mut parent = barb(None);
        parent.context = Some(vec![ContextEntry::with_dest("docs", "kb")]);
        assert!(matches!(
            plan(&parent, ContextOp::Add, "docs/arch", None),
            ContextEdit::Context(_)
        ));
        // …and for a repo.
        let current = RepoOverride {
            add: vec![ContextEntry::with_dest("notes/a", "a")],
            ..Default::default()
        };
        let ContextEdit::Override(o) = plan(
            &barb(Some(vec![".claude/skills"])),
            ContextOp::Add,
            "notes",
            Some(Some(&current)),
        ) else {
            panic!("expected an override");
        };
        assert_eq!(
            o.add,
            [
                ContextEntry::with_dest("notes/a", "a"),
                ContextEntry::new("notes")
            ]
        );
        // A repo remove of the parent add keeps the nested remap.
        let ContextEdit::Override(o) = plan(
            &barb(Some(vec![".claude/skills"])),
            ContextOp::Remove,
            "notes",
            Some(Some(&o)),
        ) else {
            panic!("expected an override");
        };
        assert_eq!(o.add, [ContextEntry::with_dest("notes/a", "a")]);
        assert!(o.remove.is_empty(), "{:?}", o.remove);
        // A dest equal to the path is the plain add.
        assert_eq!(
            plan_add(&remap, "docs", Some("docs/"), None),
            plan(&remap, ContextOp::Add, "docs", None)
        );
    }

    #[test]
    fn xs16_dropping_a_remove_pattern_drops_exactly_that_pattern() {
        let agent = barb(Some(vec![".claude/skills"]));
        let current = RepoOverride {
            remove: vec![".claude/skills/legacy-*".into(), "docs/old".into()],
            extra: BTreeMap::from([("note".to_string(), serde_yaml::Value::from("keep"))]),
            ..Default::default()
        };
        let ContextEdit::Override(o) =
            plan_drop_remove(&agent, ".claude/skills/legacy-*", Some(&current))
        else {
            panic!("expected an override");
        };
        assert_eq!(o.remove, ["docs/old"]);
        assert_eq!(o.extra.get("note"), Some(&serde_yaml::Value::from("keep")));
        assert!(matches!(
            plan_drop_remove(&agent, "docs/new", Some(&current)),
            ContextEdit::Blocked(_)
        ));
        assert!(matches!(
            plan_drop_remove(&agent, "docs/old", None),
            ContextEdit::Blocked(_)
        ));
    }

    #[test]
    fn a_repo_only_add_outside_the_context_lands_in_add() {
        let agent = barb(Some(vec![".claude/skills"]));
        let ContextEdit::Override(o) =
            plan(&agent, ContextOp::Add, "docs/architecture", Some(None))
        else {
            panic!("expected an override");
        };
        assert_eq!(o.add, [ContextEntry::new("docs/architecture")]);
    }
}
