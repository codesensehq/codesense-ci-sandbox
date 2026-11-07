// SPDX-License-Identifier: MIT
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::watch::{team_dir_handler, Coalescer, PaneChange};
use super::*;
use crate::content_engine::{Dispatcher, WatchSetup, Watcher};
use crate::context_repo::path_to_team_panes_dir;

const ORG: &str = "acme/api";
const CHILD: &str = "backend";
const PARENT: &str = "platform";
const OTHER: &str = "design";

fn principal() -> Principal {
    Principal::from(42u64)
}

/// An org where `backend`'s parent is `platform` and `design` stands alone.
fn write_org(home: &Path) -> PathBuf {
    let org_root = path_to_org_root(home, &principal(), ORG).unwrap();
    std::fs::create_dir_all(org_root.join(".git")).unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!("slug: {ORG}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
    )
    .unwrap();
    std::fs::write(
        org_root.join("people.yml"),
        "people:\n  - github_user_id: 42\n    github_username: alice\n",
    )
    .unwrap();
    std::fs::write(
        org_root.join("teams.yml"),
        format!(
            "teams:\n  - name: {CHILD}\n    parent_team: {PARENT}\n  - name: {PARENT}\n  - name: {OTHER}\n"
        ),
    )
    .unwrap();
    for team in [CHILD, PARENT, OTHER] {
        std::fs::create_dir_all(path_to_team_content_dir(&org_root, team)).unwrap();
    }
    std::fs::create_dir_all(org_root.join(PARENT).join("config")).unwrap();
    std::fs::write(
        org_root.join(PARENT).join("config/sharing.yml"),
        "share: ['panes/**']\n",
    )
    .unwrap();
    org_root
}

fn pane(org_root: &Path, team: &str, file: &str, body: &str) -> PathBuf {
    let dir = path_to_team_panes_dir(org_root, team);
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    path
}

fn ids(listing: &PaneListing) -> Vec<(PaneScope, Option<String>, String)> {
    listing
        .panes
        .iter()
        .map(|p| (p.scope, p.team.clone(), p.id.clone()))
        .collect()
}

#[test]
fn pane_ids_are_flat_lowercase_slugs() {
    for ok in ["board", "a", "0", "story-review", "x1-2"] {
        assert!(is_valid_pane_id(ok), "{ok}");
    }
    for bad in ["", "-x", "Board", "a_b", "a.b", "a/b", "é"] {
        assert!(!is_valid_pane_id(bad), "{bad}");
    }
    assert!(is_valid_pane_id(&"a".repeat(MAX_PANE_ID_LEN)));
    assert!(!is_valid_pane_id(&"a".repeat(MAX_PANE_ID_LEN + 1)));
    assert_eq!(pane_id_of_file("board.html"), Some("board"));
    assert_eq!(pane_id_of_file("board.htm"), None);
    assert_eq!(pane_id_of_file("Board.html"), None);
    assert_eq!(pane_id_of_file(".html"), None);
}

#[test]
fn title_is_the_document_title_or_none() {
    assert_eq!(
        pane_title("<html><head><TITLE> Sprint  &amp; risks </TITLE></head>").as_deref(),
        Some("Sprint & risks")
    );
    assert_eq!(pane_title("<title></title>"), None);
    assert_eq!(pane_title("<h1>no title</h1>"), None);
}

#[test]
fn icon_is_the_codesense_icon_meta_as_written() {
    assert_eq!(
        pane_icon(r#"<head><META content="git-pull-request" Name='codesense:icon'></head>"#)
            .as_deref(),
        Some("git-pull-request")
    );
    assert_eq!(
        pane_icon(r#"<meta charset="utf-8"><meta name=codesense:icon content=NotAnIcon/>"#)
            .as_deref(),
        Some("NotAnIcon"),
        "an unknown name is kept for the app to show as unknown"
    );
    assert_eq!(pane_icon(r#"<meta name="description" content="x">"#), None);
    assert_eq!(
        pane_icon(r#"<meta data-name="codesense:icon" content="x">"#),
        None
    );
    assert_eq!(
        pane_icon(r#"<meta name="codesense:icon" content="  ">"#),
        None
    );
    assert_eq!(pane_icon("<title>no meta</title>"), None);
}

#[test]
fn surfaces_are_declarations_in_every_scope() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    let marked = r#"<title>Inbox</title><META content='human-inbox' NAME="codesense:surface">"#;
    pane(&org_root, CHILD, "inbox.html", marked);
    let (space, _) = scratch_space::ensure_org_space(home.path(), &principal(), ORG).unwrap();
    std::fs::create_dir_all(space.panes_dir()).unwrap();
    std::fs::write(space.panes_dir().join("inbox.html"), marked).unwrap();
    std::fs::write(space.panes_dir().join("plain.html"), "<title>Plain</title>").unwrap();
    std::fs::write(
        space.panes_dir().join("other.html"),
        r#"<meta name="codesense:surface" content="dashboard">"#,
    )
    .unwrap();

    let listing = list_panes(home.path(), &principal(), ORG, CHILD).unwrap();
    for entry in listing.panes {
        let json = serde_json::to_value(&entry).unwrap();
        if entry.id == "inbox" {
            assert_eq!(json["surface"], "human-inbox");
        } else if entry.id == "other" {
            assert_eq!(json["surface"], "dashboard");
        } else {
            assert!(json.get("surface").is_none());
        }
    }
}

/// A team's panes are listed for it and its child, never for an unrelated team;
/// the same id in two layers is two entries; assets and odd names are not panes.
#[test]
fn listing_walks_the_chain_nearest_first_then_private() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    pane(
        &org_root,
        PARENT,
        "risks.html",
        "<title>Platform risks</title>",
    );
    pane(&org_root, PARENT, "board.html", "x");
    pane(
        &org_root,
        CHILD,
        "board.html",
        "<title>Backend board</title>",
    );
    pane(&org_root, CHILD, "lib/chart.js", "x");
    pane(&org_root, CHILD, "README.md", "x");
    pane(&org_root, CHILD, "Bad Name.html", "x");
    let (space, _) = scratch_space::ensure_org_space(home.path(), &principal(), ORG).unwrap();
    std::fs::create_dir_all(space.panes_dir()).unwrap();
    std::fs::write(
        space.panes_dir().join("board.html"),
        r#"<title>Mine</title><meta name="codesense:icon" content="kanban">"#,
    )
    .unwrap();

    let child = list_panes(home.path(), &principal(), ORG, CHILD).unwrap();
    assert_eq!(
        ids(&child),
        vec![
            (PaneScope::Team, Some(CHILD.into()), "board".into()),
            (PaneScope::Team, Some(PARENT.into()), "board".into()),
            (PaneScope::Team, Some(PARENT.into()), "risks".into()),
            (PaneScope::Private, None, "board".into()),
            (PaneScope::Private, None, "agent-list".into()),
            (PaneScope::Private, None, "files-tree".into()),
            (PaneScope::Private, None, "inbox".into()),
            (PaneScope::Private, None, "story-trail".into()),
        ]
    );
    assert_eq!(child.panes[0].title, "Backend board");
    assert_eq!(
        child.panes[1].title, "board",
        "no <title> falls back to the id"
    );
    assert_eq!(child.panes[0].panel_id, "pane:acme/api:backend/board");
    assert_eq!(child.panes[3].panel_id, "pane:private:acme/api/board");
    assert!(child.panes[0].modified.is_some());
    assert_eq!(child.panes[0].icon, None);
    assert_eq!(child.panes[3].icon.as_deref(), Some("kanban"));
    assert_eq!(child.chain, vec![CHILD.to_string(), PARENT.to_string()]);
    assert_eq!(
        PathBuf::from(&child.content_root),
        path_to_team_content_dir(&org_root, CHILD)
    );

    let other = list_panes(home.path(), &principal(), ORG, OTHER).unwrap();
    assert_eq!(
        ids(&other),
        vec![
            (PaneScope::Private, None, "board".into()),
            (PaneScope::Private, None, "agent-list".into()),
            (PaneScope::Private, None, "files-tree".into()),
            (PaneScope::Private, None, "inbox".into()),
            (PaneScope::Private, None, "story-trail".into()),
        ],
        "an unrelated team sees none of platform's panes"
    );
}

fn builtin_inbox(listing: &PaneListing) -> Option<&PaneEntry> {
    listing
        .panes
        .iter()
        .find(|p| p.dashboard_ref.scope == DashboardScope::BuiltIn && p.id == "inbox")
}

/// A team with no inbox of its own lists the bundled inbox last, as the built-in
/// of its org, served from a machine-local `panes/` dir with the fixture's bytes.
#[test]
fn a_team_without_an_inbox_lists_the_builtin_inbox_last_for_its_slot() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    pane(&org_root, CHILD, "board.html", "<title>Board</title>");

    let listing = list_panes(home.path(), &principal(), ORG, CHILD).unwrap();
    let inbox = builtin_inbox(&listing).expect("built-in inbox listed");
    assert_eq!(
        listing.panes.iter().filter(|pane| pane.declaration.slots.iter().any(|slot| slot == "right-pane")).last(),
        Some(inbox),
        "listed last for its slot"
    );
    assert_eq!(
        inbox.dashboard_ref.key(),
        "dashboard:built-in:acme/api/inbox"
    );
    assert_eq!(inbox.panel_id, "pane:private:acme/api/inbox");
    assert_eq!(
        inbox.status,
        PaneStatus::Ready,
        "{:?}",
        inbox.rejected_reason
    );
    assert_eq!(inbox.surface.as_deref(), Some("human-inbox"));
    assert_eq!(inbox.title, "Inbox");
    let commands: Vec<_> = inbox
        .declaration
        .commands
        .iter()
        .map(|c| (c.id.as_str(), c.scope, c.default_chord.as_deref()))
        .collect();
    assert_eq!(commands.len(), 9, "{commands:?}");
    assert!(commands.iter().all(|c| c.1 == CommandScope::SlotFocused));
    assert_eq!(
        commands[8],
        (
            "toggle-list",
            CommandScope::SlotFocused,
            Some("CmdOrCtrl+B")
        )
    );
    assert!(commands[..8].iter().all(|c| c.2.is_none()));
    assert!(builtin::is_builtin_pane_path(Path::new(&inbox.path)));
    assert_eq!(
        std::fs::read_to_string(&inbox.path).unwrap(),
        include_str!("../../../src/features/shell/right-pane/__fixtures__/inbox-pane.html")
    );
    assert_eq!(
        builtin_at(Path::new(&inbox.path), ORG).as_ref(),
        Some(inbox),
        "a frame of the file resolves to the same built-in"
    );

    // A fresh install's personal space lists it too.
    let scratch = list_panes(
        home.path(),
        &principal(),
        ORG,
        crate::context_repo::scratch_space::SCRATCH_TEAM,
    )
    .unwrap();
    assert!(builtin_inbox(&scratch).is_some());
}

/// Recovery gets its own descriptor and binds through the same content-root resolver as mint_frame.
#[test]
fn bundled_recovery_binds_each_requested_team_without_space_discovery() {
    use crate::context_query::{index::IndexRoot, model::Layer};
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    pane(
        &org_root,
        CHILD,
        "inbox.html",
        "<title>Team override</title>",
    );
    let roots: Vec<_> = [CHILD, OTHER]
        .into_iter()
        .map(|team| IndexRoot {
            dir: path_to_team_content_dir(&org_root, team),
            org: Some(ORG.into()),
            team: Some(team.into()),
            layer: Layer::Team,
        })
        .collect();
    let mut path = None;
    for team in [CHILD, OTHER] {
        let recovery = list_builtin_panes(home.path(), &principal(), ORG, team).unwrap();
        let inbox = recovery.builtin_inbox.as_ref().unwrap();
        assert_eq!(inbox.dashboard_ref.scope, DashboardScope::BuiltIn);
        assert!(recovery.panes.contains(inbox));
        assert!(recovery.panes.iter().any(|p| p.id == "files-tree"));
        let location =
            crate::commands::pane_bridge::doc_context(&roots, &recovery.content_root).unwrap();
        assert_eq!(location.org.as_deref(), Some(ORG));
        assert_eq!(location.team.as_deref(), Some(team));
        assert!(builtin_at(Path::new(&inbox.path), location.org.as_deref().unwrap()).is_some());
        if let Some(previous) = &path {
            assert_eq!(previous, &inbox.path);
        }
        path = Some(inbox.path.clone());
    }
}

#[test]
fn bundled_discovery_works_after_an_attached_checkout_goes_away() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("state");
    let checkout =
        crate::context_repo::context_attachments::tests::checkout(temp.path(), "volume", ORG);
    crate::context_repo::context_attachments::attach(&home, &principal(), &checkout).unwrap();
    std::fs::rename(&checkout, temp.path().join("unmounted-volume")).unwrap();
    assert!(list_panes(&home, &principal(), ORG, "default").is_err());
    let recovery = list_builtin_panes(&home, &principal(), ORG, "default").unwrap();
    let inbox = recovery.builtin_inbox.unwrap();
    assert_eq!(inbox.dashboard_ref.scope, DashboardScope::BuiltIn);
    assert!(Path::new(&inbox.path).is_file());
    assert!(Path::new(&recovery.content_root).is_dir());
    assert!(!Path::new(&recovery.content_root).starts_with(checkout));
}

/// A team's or the private space's own `inbox` wins: the built-in is not listed.
#[test]
fn a_team_or_private_inbox_hides_the_builtin() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    pane(&org_root, PARENT, "inbox.html", "<title>Team inbox</title>");
    let child = list_panes(home.path(), &principal(), ORG, CHILD).unwrap();
    assert!(builtin_inbox(&child).is_none(), "{child:?}");

    let other = list_panes(home.path(), &principal(), ORG, OTHER).unwrap();
    assert!(builtin_inbox(&other).is_some());
    let (space, _) = scratch_space::ensure_org_space(home.path(), &principal(), ORG).unwrap();
    write_pane(&space.panes_dir(), "inbox", "<title>Mine</title>").unwrap();
    let other = list_panes(home.path(), &principal(), ORG, OTHER).unwrap();
    assert!(builtin_inbox(&other).is_none());
    let fallback = other
        .builtin_inbox
        .as_ref()
        .expect("shadowed Inbox keeps its bundled fallback");
    assert_eq!(fallback.dashboard_ref.scope, DashboardScope::BuiltIn);
    assert!(builtin::is_builtin_pane_path(Path::new(&fallback.path)));
    assert!(!other.panes.iter().any(|pane| pane.path == fallback.path));
    assert_eq!(
        ids(&other),
        vec![
            (PaneScope::Private, None, "inbox".into()),
            (PaneScope::Private, None, "agent-list".into()),
            (PaneScope::Private, None, "files-tree".into()),
            (PaneScope::Private, None, "story-trail".into()),
        ],
        "the private inbox fork replaces only the built-in Inbox"
    );
}

/// The built-in is read-only: no write target reaches its dir, and stray edits
/// to its file are put back from the binary on the next listing.
#[test]
fn the_builtin_inbox_is_never_a_write_target() {
    let home = tempfile::tempdir().unwrap();
    write_org(home.path());
    let listing = list_panes(home.path(), &principal(), ORG, CHILD).unwrap();
    let path = PathBuf::from(&builtin_inbox(&listing).unwrap().path);
    for target in [
        PaneTarget::Team {
            org: ORG.into(),
            team: CHILD.into(),
        },
        PaneTarget::Private { org: ORG.into() },
    ] {
        let dir = target.panes_dir(home.path(), &principal()).unwrap();
        assert_ne!(Some(dir.as_path()), path.parent(), "{target:?}");
        assert!(!builtin::is_builtin_pane_path(&dir.join("inbox.html")));
    }
    std::fs::write(&path, "edited").unwrap();
    list_panes(home.path(), &principal(), ORG, OTHER).unwrap();
    assert_ne!(std::fs::read_to_string(&path).unwrap(), "edited");
}

/// A team name, or a parent named in the synced teams.yml, that is not one
/// directory never steers discovery out of the org.
#[test]
fn listing_never_leaves_the_org_through_a_team_name() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    std::fs::write(
        org_root.join("teams.yml"),
        format!("teams:\n  - name: {CHILD}\n    parent_team: ../outside\n"),
    )
    .unwrap();
    let outside = org_root.parent().unwrap().join("outside/panes");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("leak.html"), "x").unwrap();

    let child = list_panes(home.path(), &principal(), ORG, CHILD).unwrap();
    assert!(child.panes.iter().all(|p| p.id != "leak"), "{child:?}");
    assert_eq!(child.chain, vec![CHILD.to_string()]);
    assert!(list_panes(home.path(), &principal(), ORG, "../outside").is_err());
}

/// A pane is never content: the file search over a team's content root does
/// not surface its `panes/` sibling.
#[test]
fn a_pane_never_appears_in_the_content_file_search() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    pane(&org_root, CHILD, "x.html", "<title>x</title>");
    std::fs::write(
        path_to_team_content_dir(&org_root, CHILD).join("doc.html"),
        "<title>doc</title>",
    )
    .unwrap();
    let content = path_to_team_content_dir(&org_root, CHILD);
    // The list is served from the index, so index the content root plus the
    // `panes/` sibling (as the footprint walk would) and list through the core.
    let record = |p: std::path::PathBuf| {
        std::sync::Arc::new(crate::context_query::record::ContextRecord {
            headings: Vec::new(),
            path: p.to_string_lossy().replace('\\', "/"),
            rel_path: String::new(),
            file_name: p.file_name().unwrap().to_string_lossy().into_owned(),
            org: None,
            team: None,
            layer: crate::context_query::model::Layer::Team,
            kind: crate::context_query::model::Kind::Doc,
            mtime_ms: None,
            title: None,
            fields: Default::default(),
            is_pipeline_doc: false,
            metadata_truncated: false,
            metadata_valid: true,
            summary: None,
        })
    };
    let snapshot = std::sync::Arc::new(crate::context_query::index::Snapshot::build(
        vec![
            record(content.join("doc.html")),
            record(content.parent().unwrap().join("panes/x.html")),
        ],
        vec![crate::context_query::index::IndexRoot {
            dir: content.clone(),
            org: None,
            team: None,
            layer: crate::context_query::model::Layer::Team,
        }],
        1,
        true,
    ));
    let found = crate::context_query::file_list::FileListMemo::new()
        .list(&snapshot, &content.to_string_lossy())
        .unwrap();
    let names: Vec<_> = found.iter().map(|f| f.relative_path.as_str()).collect();
    assert_eq!(names, vec!["doc.html"]);
    assert!(found.iter().all(|f| !f.full_path.contains("/panes/")));
}

#[test]
fn write_pane_overwrites_through_a_dot_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("panes");
    write_pane(&dir, "board", "one").unwrap();
    let path = write_pane(&dir, "board", "two").unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "two");
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec!["board.html"], "no staging file left behind");
    assert!(write_pane(&dir, "Bad", "x").is_err());
}

#[test]
fn concurrent_writes_of_one_pane_each_land_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("panes");
    let bodies: Vec<String> = (0..8).map(|i| format!("{i}").repeat(64 * 1024)).collect();
    std::thread::scope(|scope| {
        for body in &bodies {
            let dir = &dir;
            scope.spawn(move || write_pane(dir, "board", body).unwrap());
        }
    });
    let landed = std::fs::read_to_string(dir.join("board.html")).unwrap();
    assert!(
        bodies.contains(&landed),
        "one writer's whole document, never a mix"
    );
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(left.len(), 1, "no staging file left behind");
}

#[test]
fn changes_classify_to_a_pane_an_asset_or_noise() {
    let dir = Path::new("/o/acme/api/backend/panes");
    let change = |rel: &str| watch::team_pane_change(dir, ORG, CHILD, &dir.join(rel));
    assert_eq!(change("board.html").unwrap().id.as_deref(), Some("board"));
    assert_eq!(change("lib/chart.js").unwrap().id, None);
    assert_eq!(change("notes.txt").unwrap().id, None);
    for noise in [
        ".board.html.writing",
        ".board.html.4242.7.writing",
        "board.html.swp",
        ".DS_Store",
        "board.html~",
    ] {
        assert_eq!(change(noise), None, "{noise}");
    }
    assert_eq!(
        watch::team_pane_change(
            dir,
            ORG,
            CHILD,
            Path::new("/o/acme/api/backend/content/x.md")
        ),
        None
    );
    let scratch = Path::new("/h/users/42/scratch");
    assert_eq!(
        watch::scratch_pane_change(
            scratch,
            None,
            &scratch.join("orgs/acme/api/panes/mine.html")
        ),
        Some(PaneChange {
            scope: PaneScope::Private,
            org: ORG.into(),
            team: None,
            id: Some("mine".into()),
            declarations: true,
            installed: None,
        })
    );
    assert_eq!(
        watch::scratch_pane_change(scratch, None, &scratch.join("orgs/acme/api/content/x.md")),
        None
    );
}

fn collecting_coalescer() -> (Arc<Coalescer>, mpsc::Receiver<PaneChange>) {
    collecting_coalescer_with(watch::QUIET, watch::MAX_GATHER)
}

fn collecting_coalescer_with(
    quiet: Duration,
    max: Duration,
) -> (Arc<Coalescer>, mpsc::Receiver<PaneChange>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let coalescer = Arc::new(Coalescer::new(
        quiet,
        max,
        Arc::new(move |change| {
            let _ = tx.lock().unwrap().send(change);
        }),
    ));
    (coalescer, rx)
}

fn drain(rx: &mpsc::Receiver<PaneChange>, settle: Duration) -> Vec<PaneChange> {
    let mut got = Vec::new();
    while let Ok(change) = rx.recv_timeout(settle) {
        got.push(change);
    }
    got
}

/// SW-XS-12: receive into `got` until `done(got)` holds or the watch deadline
/// passes; whether it held.
fn recv_until(
    rx: &mpsc::Receiver<PaneChange>,
    got: &mut Vec<PaneChange>,
    done: impl Fn(&[PaneChange]) -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + crate::content_engine::test_support::WATCH_DEADLINE;
    while !done(got) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        if let Ok(change) = rx.recv_timeout(left.min(Duration::from_millis(100))) {
            got.push(change);
        }
    }
    true
}

fn change(team: &str, id: Option<&str>) -> PaneChange {
    PaneChange {
        scope: PaneScope::Team,
        org: ORG.into(),
        team: Some(team.into()),
        id: id.map(str::to_string),
        declarations: id.is_some(),
        installed: None,
    }
}

/// SW-XS-12: the coalescing contract, driven by an injected event source (the
/// team-dir handler fed the paths a `git pull` produces) instead of the
/// kernel watcher, so it holds under any machine load: fifty writes across a
/// pane and its assets reload that pane once and the layer's panes once, and
/// a write outside `panes/` goes to the team dir's other handler. The quiet
/// window is long enough that no scheduling delay splits the burst.
#[test]
fn an_injected_burst_coalesces_to_one_change_per_pane() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    let (coalescer, rx) =
        collecting_coalescer_with(Duration::from_secs(2), Duration::from_secs(60));
    let (rest_tx, rest_rx) = mpsc::channel::<PathBuf>();
    let rest_tx = Mutex::new(rest_tx);
    let rest: crate::content_engine::DispatchHandler = Arc::new(move |p: &Path| {
        let _ = rest_tx.lock().unwrap().send(p.to_path_buf());
    });
    let team_dir = org_root.join(CHILD);
    let handler = team_dir_handler(team_dir.clone(), ORG.to_string(), coalescer, rest);
    let panes = path_to_team_panes_dir(&org_root, CHILD);
    for i in 0..25 {
        handler(&panes.join("board.html"));
        handler(&panes.join(format!("lib/part-{i}.js")));
    }
    handler(&panes.join(".board.html.swp"));
    handler(&path_to_team_content_dir(&org_root, CHILD).join("story.md"));

    let mut got = Vec::new();
    assert!(recv_until(&rx, &mut got, |got| got.len() >= 2), "{got:?}");
    // Nothing else is fed, so anything more would already be pending: wait
    // out one more quiet window and require silence.
    got.extend(drain(&rx, Duration::from_millis(2_500)));
    got.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(
        got,
        vec![change(CHILD, None), change(CHILD, Some("board"))],
        "fifty writes reload the board once and the layer's panes once; a swap file is noise"
    );
    let routed: Vec<PathBuf> = rest_rx.try_iter().collect();
    assert_eq!(routed.len(), 1, "{routed:?}");
    assert!(routed[0].ends_with("story.md"));
}

/// The seam: through the real kernel watcher and dispatcher, with the team-dir
/// handler every team of the org is watched by, a pane saved in a parent team
/// reloads, a burst of writes across a pane and its assets reloads that pane and
/// the layer's assets, a deletion is reported so the listing can drop it, and
/// content changes still reach the team dir's other handler.
///
/// SW-XS-12: outcome-based, not timing-based. Every stimulus is waited for by
/// its own event (a late-attaching stream is covered by repeating the first
/// write), there is no wall-clock bound, and the exact one-change-per-pane
/// count lives in [`an_injected_burst_coalesces_to_one_change_per_pane`],
/// because a loaded machine may deliver one burst as two kernel batches.
#[test]
fn real_watcher_emits_one_debounced_change_per_pane() {
    let home = tempfile::tempdir().unwrap();
    let org_root = write_org(home.path());
    let dispatcher = Arc::new(Dispatcher::new(50, 10_000, 60_000, 500));
    let watcher = Watcher::new(WatchSetup { dirs: vec![] }, Arc::clone(&dispatcher)).unwrap();
    let (coalescer, rx) = collecting_coalescer();
    let (rest_tx, rest_rx) = mpsc::channel::<PathBuf>();
    let rest_tx = Mutex::new(rest_tx);
    let rest: crate::content_engine::DispatchHandler = Arc::new(move |p: &Path| {
        let _ = rest_tx.lock().unwrap().send(p.to_path_buf());
    });
    for team in [CHILD, PARENT] {
        let team_dir = org_root.join(team);
        crate::commands::watch::attach_recursive_watch(
            &watcher,
            &dispatcher,
            &team_dir,
            team_dir_handler(
                team_dir.clone(),
                ORG.to_string(),
                Arc::clone(&coalescer),
                Arc::clone(&rest),
            ),
            "test",
        );
    }
    assert!(
        !path_to_team_panes_dir(&org_root, PARENT).exists(),
        "watching never creates a panes/ dir"
    );

    // Creating the first pane also creates `panes/` itself (a layer-wide
    // change), so wait for the pane's own event among whatever arrives; the
    // save is repeated until the stream (attached moments ago) reports it.
    let risks = change(PARENT, Some("risks"));
    let mut first = Vec::new();
    let mut version = 0;
    assert!(
        crate::content_engine::test_support::act_until(
            Duration::from_millis(500),
            || {
                version += 1;
                pane(
                    &org_root,
                    PARENT,
                    "risks.html",
                    &format!("<title>v{version}</title>"),
                );
            },
            || {
                first.extend(rx.try_iter());
                first.contains(&risks)
            },
        ),
        "a reload: {first:?}"
    );
    let _ = drain(&rx, Duration::from_millis(400));

    // The burst, repeated while unobserved: on a loaded machine FSEvents may
    // coalesce a burst into one directory-level event that names no pane.
    let mut got = Vec::new();
    let mut round = 0;
    assert!(
        crate::content_engine::test_support::act_until(
            Duration::from_secs(2),
            || {
                round += 1;
                for i in 0..25 {
                    pane(&org_root, CHILD, "board.html", &format!("r{round} v{i}"));
                    pane(
                        &org_root,
                        CHILD,
                        &format!("lib/part-{i}.js"),
                        &format!("r{round}"),
                    );
                }
            },
            || {
                got.extend(rx.try_iter());
                got.contains(&change(CHILD, Some("board"))) && got.contains(&change(CHILD, None))
            },
        ),
        "the burst reloads the board and the layer's panes: {got:?}"
    );

    // The delete is reported (as the pane, or as a coalesced layer-wide
    // reload, which also drops it). Only events received strictly after the
    // delete count: a retry first re-creates the pane, waits for that create
    // to be reported and drains it until quiet, and only then deletes again,
    // so a re-create event can never satisfy the delete assertion.
    let risks_path = path_to_team_panes_dir(&org_root, PARENT).join("risks.html");
    let layer = change(PARENT, None);
    let _ = drain(&rx, Duration::from_millis(400));
    let deadline = std::time::Instant::now() + crate::content_engine::test_support::WATCH_DEADLINE;
    let mut attempts = Vec::new();
    let deleted = loop {
        if std::time::Instant::now() >= deadline {
            break false;
        }
        if !risks_path.exists() {
            let mut created = Vec::new();
            let mut version = 0;
            assert!(
                crate::content_engine::test_support::act_until(
                    Duration::from_millis(500),
                    || {
                        version += 1;
                        pane(
                            &org_root,
                            PARENT,
                            "risks.html",
                            &format!("<title>again {version}</title>"),
                        );
                    },
                    || {
                        created.extend(rx.try_iter());
                        created.contains(&risks) || created.contains(&layer)
                    },
                ),
                "the re-created pane is reported before it is deleted again: {created:?}"
            );
            let _ = drain(&rx, Duration::from_millis(400));
        }
        std::fs::remove_file(&risks_path).unwrap();
        let mut after_delete = Vec::new();
        let seen = {
            let window = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                if after_delete.contains(&risks) || after_delete.contains(&layer) {
                    break true;
                }
                let left = window.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    break false;
                }
                if let Ok(c) = rx.recv_timeout(left.min(Duration::from_millis(100))) {
                    after_delete.push(c);
                }
            }
        };
        attempts.push(after_delete);
        if seen {
            break true;
        }
    };
    assert!(deleted, "a delete, events after each delete: {attempts:?}");

    assert!(
        rest_rx
            .try_iter()
            .all(|p| !p.to_string_lossy().contains("/panes/")),
        "pane paths never reach the team dir's other handler"
    );
    std::fs::write(
        path_to_team_content_dir(&org_root, CHILD).join("story.md"),
        "x",
    )
    .unwrap();
    let deadline = std::time::Instant::now() + crate::content_engine::test_support::WATCH_DEADLINE;
    let routed = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let path = rest_rx.recv_timeout(left).expect("content routed on");
        if path.ends_with("story.md") {
            break path;
        }
        assert!(
            !path.to_string_lossy().contains("/panes/"),
            "pane paths never reach the team dir's other handler: {path:?}"
        );
    };
    assert!(routed.ends_with("story.md"));
}

#[test]
fn builtin_inbox_mark_unread_is_admitted_in_open_and_team_safe() {
    use crate::verbs::{capabilities, Policy, Preset, Registry};
    let home = tempfile::tempdir().unwrap();
    write_org(home.path());
    let listing = list_panes(home.path(), &principal(), ORG, CHILD).unwrap();
    let inbox = builtin_inbox(&listing).unwrap();
    let registry = Registry::default();
    let verb = registry.get("markUnread").unwrap();
    assert_eq!(verb.grant, "inbox:write");
    assert_eq!(verb.mcp_twin.as_deref(), Some("mark_unread"));
    assert!(!verb.confirmable);
    for preset in [Preset::Open, Preset::TeamSafe] {
        let policy = Policy {
            preset,
            ..Policy::default()
        };
        let granted = policy.effective_grant(
            &inbox.dashboard_ref.key(),
            "right-pane",
            "builtin",
            &inbox.declaration.capabilities,
            &capabilities::full(),
        );
        assert!(granted.contains(&verb.grant), "{preset:?}: {granted:?}");
    }
}

#[test]
fn inherited_panes_discovery_only_opens_shared_panes() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::write(root.join("parent/panes/secret.html"), "parent-secret-bytes").unwrap();
    let target = PaneTarget::Team {
        org: "acme/ctx".into(),
        team: "child".into(),
    };
    let me = Principal::from(1u64);
    let opened = serde_json::to_value(
        crate::mcp::pane_tools::open(
            home.path(),
            &me,
            &target,
            "parent",
            Some(("acme/ctx", "child")),
            None,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(opened["status"], "ok");
    assert_eq!(
        opened["data"]["path"],
        root.join("parent/panes/parent.html")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(opened["data"]["placed"], true);
    let denied = serde_json::to_value(
        crate::mcp::pane_tools::open(home.path(), &me, &target, "secret", None, None).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(root.join("parent/panes/secret.html")).unwrap();
    let missing = serde_json::to_value(
        crate::mcp::pane_tools::open(home.path(), &me, &target, "secret", None, None).unwrap(),
    )
    .unwrap();
    assert_eq!(denied, missing);
    let listed = serde_json::to_value(
        crate::mcp::pane_tools::list(home.path(), &me, "acme/ctx", "child").unwrap(),
    )
    .unwrap();
    assert!(!listed.to_string().contains("secret.html"));
    let listing = list_panes(home.path(), &Principal::from(1u64), "acme/ctx", "child").unwrap();
    assert!(listing
        .panes
        .iter()
        .any(|p| p.path == root.join("parent/panes/parent.html").to_string_lossy()));
    assert!(listing
        .panes
        .iter()
        .any(|p| p.path == root.join("child/panes/child.html").to_string_lossy()));
    assert!(!format!("{listing:?}").contains("secret.html"));
    assert!(!listing
        .panes
        .iter()
        .any(|p| p.path == root.join("grand/panes/grand.html").to_string_lossy()));
}

#[cfg(unix)]
#[test]
fn own_pane_and_manifest_aliases_cannot_disclose_private_ancestor_metadata() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    let me = Principal::from(1u64);
    let target = PaneTarget::Team {
        org: "acme/ctx".into(),
        team: "child".into(),
    };
    std::fs::write(root.join("parent/panes/secret.html"), r#"<title>PRIVATE_TITLE</title><meta name="codesense:icon" content="PRIVATE_ICON"><meta name="codesense:surface" content="PRIVATE_SURFACE">"#).unwrap();
    std::fs::write(
        root.join("parent/panes/private-manifest.yaml"),
        "version: 1\npanes:\n  - id: child\n    html: child.html\n    title: PRIVATE_OWN_OVERLAY\n",
    )
    .unwrap();
    std::fs::write(root.join("grand/panes/private-manifest.yaml"), "version: 1\npanes:\n  - id: parent\n    html: parent.html\n    title: PRIVATE_INHERITED_OVERLAY\n").unwrap();
    let alias = root.join("child/panes/linked.html");
    let own_manifest = root.join("child/panes/pane-manifest.yaml");
    let inherited_manifest = root.join("parent/panes/pane-manifest.yaml");
    std::os::unix::fs::symlink("../../parent/panes/secret.html", &alias).unwrap();
    std::os::unix::fs::symlink("../../parent/panes/private-manifest.yaml", &own_manifest).unwrap();
    std::os::unix::fs::symlink(
        "../../grand/panes/private-manifest.yaml",
        &inherited_manifest,
    )
    .unwrap();
    let direct = || {
        serde_json::to_value(list_panes(home.path(), &me, "acme/ctx", "child").unwrap()).unwrap()
    };
    let listed = || {
        serde_json::to_value(
            crate::mcp::pane_tools::list(home.path(), &me, "acme/ctx", "child").unwrap(),
        )
        .unwrap()
    };
    let open_alias = || {
        serde_json::to_value(
            crate::mcp::pane_tools::open(home.path(), &me, &target, "linked", None, None).unwrap(),
        )
        .unwrap()
    };
    let before = direct();
    let before_mcp = listed();
    let denied = open_alias();
    for shown in [&before, &before_mcp, &denied] {
        assert!(!shown.to_string().contains("PRIVATE_"), "{shown}");
    }
    assert!(before["panes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "child"));
    assert!(before["panes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "parent"));
    assert!(!before["panes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "linked"));
    for path in [&alias, &own_manifest, &inherited_manifest] {
        std::fs::remove_file(path).unwrap();
    }
    assert_eq!(direct(), before);
    assert_eq!(listed(), before_mcp);
    assert_eq!(open_alias(), denied);
}

#[cfg(unix)]
#[test]
fn own_team_parentless_and_private_pane_aliases_keep_working() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    let me = Principal::from(1u64);
    for team in ["child", "parent", "sibling"] {
        let dir = root.join(team).join("panes");
        std::fs::write(dir.join("local-manifest.yaml"), format!("version: 1\npanes:\n  - id: alias\n    html: alias.html\n    title: Own overlay {team}\n")).unwrap();
        std::os::unix::fs::symlink(format!("{team}.html"), dir.join("alias.html")).unwrap();
        std::os::unix::fs::symlink("local-manifest.yaml", dir.join("pane-manifest.yaml")).unwrap();
        let direct = list_panes(home.path(), &me, "acme/ctx", team).unwrap();
        let alias = direct
            .panes
            .iter()
            .find(|p| p.id == "alias" && p.team.as_deref() == Some(team))
            .unwrap();
        assert_eq!(alias.title, format!("Own overlay {team}"));
        let listed = serde_json::to_value(
            crate::mcp::pane_tools::list(home.path(), &me, "acme/ctx", team).unwrap(),
        )
        .unwrap();
        assert!(listed.to_string().contains(&format!("Own overlay {team}")));
        let target = PaneTarget::Team {
            org: "acme/ctx".into(),
            team: team.into(),
        };
        let opened = serde_json::to_value(
            crate::mcp::pane_tools::open(home.path(), &me, &target, "alias", None, None).unwrap(),
        )
        .unwrap();
        assert_eq!(opened["status"], "ok", "{opened}");
    }
    let (space, _) =
        crate::context_repo::scratch_space::ensure_org_space(home.path(), &me, "acme/ctx").unwrap();
    let dir = space.panes_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("own.html"), "<title>Private own pane</title>").unwrap();
    std::os::unix::fs::symlink("own.html", dir.join("private-alias.html")).unwrap();
    let listed = list_panes(home.path(), &me, "acme/ctx", "child").unwrap();
    assert!(listed
        .panes
        .iter()
        .any(|p| p.id == "private-alias" && p.scope == PaneScope::Private));
    let target = PaneTarget::Private {
        org: "acme/ctx".into(),
    };
    let opened = serde_json::to_value(
        crate::mcp::pane_tools::open(home.path(), &me, &target, "private-alias", None, None)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(opened["status"], "ok", "{opened}");
}

#[cfg(unix)]
#[test]
fn pane_alias_owner_matrix_gates_list_and_mcp_open_before_metadata() {
    let fixture = crate::context_repo::inheritance::alias_fixture();
    let me = Principal::from(1u64);
    let listed = || {
        serde_json::to_value(list_panes(fixture.home.path(), &me, "acme/ctx", "child").unwrap())
            .unwrap()
    };
    let mcp_list = || {
        serde_json::to_value(
            crate::mcp::pane_tools::list(fixture.home.path(), &me, "acme/ctx", "child").unwrap(),
        )
        .unwrap()
    };
    let shown = listed();
    let mcp_shown = mcp_list();
    for snapshot in [&shown, &mcp_shown] {
        assert!(!snapshot.to_string().contains("UNSHARED_ANCESTOR_SENTINEL"));
        assert!(!snapshot.to_string().contains("UNSHARED_ANCESTOR_ICON"));
    }
    for case in &fixture.cases {
        let target = if case.origin == "private" {
            PaneTarget::Private {
                org: "acme/ctx".into(),
            }
        } else {
            PaneTarget::Team {
                org: "acme/ctx".into(),
                team: "child".into(),
            }
        };
        let open = || {
            serde_json::to_value(
                crate::mcp::pane_tools::open(
                    fixture.home.path(),
                    &me,
                    &target,
                    &case.id,
                    Some(("acme/ctx", "child")),
                    None,
                )
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            shown["panes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == case.id),
            case.admitted,
            "{}",
            case.id
        );
        let opened = open();
        assert_eq!(
            opened["status"] == "ok",
            case.admitted,
            "{}: {opened}",
            case.id
        );
        if !case.admitted {
            std::fs::remove_file(&case.pane).unwrap();
            assert_eq!(open(), opened, "{}", case.id);
            assert_eq!(listed(), shown, "{}", case.id);
            assert_eq!(mcp_list(), mcp_shown, "{}", case.id);
        }
    }
}

#[cfg(unix)]
#[test]
fn private_companion_alias_cannot_export_an_unshared_ancestor_overlay() {
    let fixture = crate::context_repo::inheritance::alias_fixture();
    let me = Principal::from(1u64);
    let source = fixture.root.join("parent/panes/private-overlay.yaml");
    std::fs::write(&source, "version: 1\npanes:\n  - id: own\n    html: own.html\n    title: UNSHARED_COMPANION_SENTINEL\n    icon: UNSHARED_COMPANION_ICON\n").unwrap();
    let alias = fixture.private.join("panes/pane-manifest.yaml");
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let listed = || {
        serde_json::to_value(list_panes(fixture.home.path(), &me, "acme/ctx", "child").unwrap())
            .unwrap()
    };
    let mcp = || {
        serde_json::to_value(
            crate::mcp::pane_tools::list(fixture.home.path(), &me, "acme/ctx", "child").unwrap(),
        )
        .unwrap()
    };
    let before = listed();
    let before_mcp = mcp();
    assert!(!before.to_string().contains("UNSHARED_COMPANION"));
    assert!(!before_mcp.to_string().contains("UNSHARED_COMPANION"));
    std::fs::remove_file(alias).unwrap();
    assert_eq!(listed(), before);
    assert_eq!(mcp(), before_mcp);
}

#[cfg(unix)]
#[test]
fn inherited_companion_alias_stays_in_its_owners_panes_and_filters_rows() {
    for (target, admitted) in [
        ("parent/panes/local-manifest.yaml", true),
        ("parent/config/private.yaml", false),
        ("parent/panes/nested/manifest.yaml", false),
        ("grand/panes/manifest.yaml", false),
    ] {
        let (home, root) = crate::context_repo::inheritance::test_fixture();
        let me = Principal::from(1u64);
        let path = root.join(target);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "version: 1\npanes:\n  - id: parent\n    html: parent.html\n    title: Admitted overlay\n    icon: book-open\n    showsOn: ['*']\n    priority: 42\n  - id: secret\n    html: secret.html\n    title: UNSHARED_OVERLAY_SENTINEL\n").unwrap();
        std::fs::write(
            root.join("parent/panes/secret.html"),
            "<title>UNSHARED_HTML_SENTINEL</title>",
        )
        .unwrap();
        std::os::unix::fs::symlink(&path, root.join("parent/panes/pane-manifest.yaml")).unwrap();
        let listing = list_panes(home.path(), &me, "acme/ctx", "child").unwrap();
        let pane = listing.panes.iter().find(|p| p.id == "parent").unwrap();
        assert_eq!(pane.title == "Admitted overlay", admitted, "{target}");
        let shown = serde_json::to_value(&listing).unwrap().to_string();
        let mcp = serde_json::to_value(
            crate::mcp::pane_tools::list(home.path(), &me, "acme/ctx", "child").unwrap(),
        )
        .unwrap()
        .to_string();
        for snapshot in [shown, mcp] {
            assert!(!snapshot.contains("UNSHARED_"), "{target}: {snapshot}");
        }
        if admitted {
            assert_eq!(pane.icon.as_deref(), Some("book-open"));
            assert!(listing.manifests.iter().any(|l| l
                .manifest
                .panes
                .iter()
                .any(|p| p.id == "parent" && p.priority == Some(42))));
            let own = list_panes(home.path(), &me, "acme/ctx", "parent").unwrap();
            assert_eq!(
                own.panes.iter().find(|p| p.id == "parent").unwrap().title,
                "Admitted overlay"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn registered_external_checkout_preserves_private_and_installed_pane_consumers() {
    let (home, managed) = crate::context_repo::inheritance::test_fixture();
    let external = tempfile::tempdir().unwrap();
    let root = external.path().join("checkout");
    std::fs::rename(&managed, &root).unwrap();
    git2::Repository::init(&root).unwrap();
    let me = Principal::from(1u64);
    crate::context_repo::context_attachments::attach(home.path(), &me, &root).unwrap();
    assert_eq!(
        crate::context_repo::path_to_org_root(home.path(), &me, "acme/ctx").unwrap(),
        root.canonicalize().unwrap()
    );
    let (space, _) =
        crate::context_repo::scratch_space::ensure_org_space(home.path(), &me, "acme/ctx").unwrap();
    let private = space.panes_dir();
    std::fs::create_dir_all(&private).unwrap();
    std::fs::write(private.join("local.html"), "<title>Local private</title>").unwrap();
    std::os::unix::fs::symlink("local.html", private.join("local-alias.html")).unwrap();
    std::fs::write(private.join("local-manifest.yaml"), "version: 1\npanes:\n  - id: local\n    html: local.html\n    title: Local private overlay\n").unwrap();
    std::os::unix::fs::symlink("local-manifest.yaml", private.join("pane-manifest.yaml")).unwrap();
    let first = list_panes(home.path(), &me, "acme/ctx", "child").unwrap();
    let source = first
        .panes
        .iter()
        .find(|p| p.id == "child" && p.scope == PaneScope::Team)
        .unwrap();
    let installed = install::choose(home.path(), &me, source, true, true).unwrap();
    let listed = || list_panes(home.path(), &me, "acme/ctx", "child").unwrap();
    let mcp = || {
        serde_json::to_value(
            crate::mcp::pane_tools::list(home.path(), &me, "acme/ctx", "child").unwrap(),
        )
        .unwrap()
    };
    let target = PaneTarget::Private {
        org: "acme/ctx".into(),
    };
    let open = |id: &str| {
        serde_json::to_value(
            crate::mcp::pane_tools::open(
                home.path(),
                &me,
                &target,
                id,
                Some(("acme/ctx", "child")),
                None,
            )
            .unwrap(),
        )
        .unwrap()
    };
    let listing = listed();
    assert_eq!(
        listing
            .panes
            .iter()
            .find(|p| p.id == "local")
            .unwrap()
            .title,
        "Local private overlay"
    );
    assert!(listing
        .panes
        .iter()
        .any(|p| p.dashboard_ref == installed && p.install_source_version.is_some()));
    assert!(mcp().to_string().contains("Local private overlay"));
    for id in ["local", "local-alias", installed.id.as_str()] {
        assert_eq!(open(id)["status"], "ok", "{id}");
    }
    std::fs::write(
        root.join("parent/panes/secret.html"),
        "<title>UNSHARED_EXTERNAL_ANCESTOR_SENTINEL</title>",
    )
    .unwrap();
    let alias = private.join("forbidden.html");
    std::os::unix::fs::symlink(root.join("parent/panes/secret.html"), &alias).unwrap();
    let before = serde_json::to_value(listed()).unwrap();
    let before_mcp = mcp();
    let denied = open("forbidden");
    assert!(!before.to_string().contains("UNSHARED_EXTERNAL"));
    assert!(!before_mcp.to_string().contains("UNSHARED_EXTERNAL"));
    std::fs::remove_file(alias).unwrap();
    assert_eq!(open("forbidden"), denied);
    assert_eq!(serde_json::to_value(listed()).unwrap(), before);
    assert_eq!(mcp(), before_mcp);
}

#[cfg(unix)]
#[test]
fn unavailable_attached_checkout_preserves_local_private_listing_and_mcp_open() {
    let (home, managed) = crate::context_repo::inheritance::test_fixture();
    let external = tempfile::tempdir().unwrap();
    let root = external.path().join("checkout");
    std::fs::rename(&managed, &root).unwrap();
    git2::Repository::init(&root).unwrap();
    let me = Principal::from(1u64);
    crate::context_repo::context_attachments::attach(home.path(), &me, &root).unwrap();
    let (space, _) =
        crate::context_repo::scratch_space::ensure_org_space(home.path(), &me, "acme/ctx").unwrap();
    std::fs::create_dir_all(space.panes_dir()).unwrap();
    std::fs::write(
        space.panes_dir().join("local.html"),
        "<title>Local offline</title>",
    )
    .unwrap();
    let first = list_panes(home.path(), &me, "acme/ctx", "child").unwrap();
    let installed = install::choose(
        home.path(),
        &me,
        first
            .panes
            .iter()
            .find(|p| p.id == "child" && p.scope == PaneScope::Team)
            .unwrap(),
        true,
        true,
    )
    .unwrap();
    let git = crate::agents::prepare::step3::tests::git;
    std::fs::write(
        root.join("parent/panes/secret.html"),
        "<title>OFFLINE_ANCESTOR_SECRET</title>",
    )
    .unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "retained baseline"]);
    let retained = external.path().join("retained");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            "child/offline",
            retained.to_str().unwrap(),
            "HEAD",
        ],
    );
    let alias = space.panes_dir().join("forbidden.html");
    std::os::unix::fs::symlink(retained.join("parent/panes/secret.html"), &alias).unwrap();
    std::fs::rename(&root, external.path().join("moved-by-owner")).unwrap();
    assert!(list_panes(home.path(), &me, "acme/ctx", "child")
        .unwrap_err()
        .contains("unavailable"));
    let listing = list_panes(
        home.path(),
        &me,
        "acme/ctx",
        crate::context_repo::scratch_space::SCRATCH_TEAM,
    )
    .unwrap();
    for id in ["local", installed.id.as_str()] {
        assert!(listing
            .panes
            .iter()
            .any(|p| p.id == id && p.scope == PaneScope::Private));
        let opened = crate::mcp::pane_tools::open(
            home.path(),
            &me,
            &PaneTarget::Private {
                org: "acme/ctx".into(),
            },
            id,
            Some(("acme/ctx", "child")),
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(opened).unwrap()["status"],
            "ok",
            "{id}"
        );
    }
    let scoped =
        || serde_json::to_value(list_private_panes(home.path(), &me, "acme/ctx", "child")).unwrap();
    let open = |id: &str| {
        serde_json::to_value(
            crate::mcp::pane_tools::open(
                home.path(),
                &me,
                &PaneTarget::Private {
                    org: "acme/ctx".into(),
                },
                id,
                Some(("acme/ctx", "child")),
                None,
            )
            .unwrap(),
        )
        .unwrap()
    };
    let denied = open("forbidden");
    let before = scoped();
    assert_eq!(denied["status"], "validation");
    assert!(!before.to_string().contains("OFFLINE_ANCESTOR_SECRET"));
    assert!(!before["panes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "forbidden"));
    std::fs::write(
        retained.join("teams.yml"),
        "teams:\n  - name: parent\n    parent_team: grand\n  - name: grand\n  - name: sibling\n",
    )
    .unwrap();
    assert_eq!(
        open("forbidden"),
        denied,
        "offline older topology cannot place the reader, so same-org non-own targets fail closed"
    );
    assert_eq!(scoped(), before);
    assert_eq!(open("local")["status"], "ok");
    let own_alias = space.panes_dir().join("own-alias.html");
    std::os::unix::fs::symlink(retained.join("child/panes/child.html"), &own_alias).unwrap();
    assert_eq!(open("own-alias")["status"], "ok");
    std::fs::remove_file(own_alias).unwrap();
    std::fs::remove_file(alias).unwrap();
    assert_eq!(
        open("forbidden"),
        denied,
        "unprovable ancestor shares answer exactly as missing"
    );
    assert_eq!(scoped(), before);
    assert!(
        !root.exists(),
        "private discovery never replaces the unavailable attachment"
    );
    assert!(!crate::context_repo::path_to_managed_org_root(home.path(), &me, "acme/ctx").exists());
}
