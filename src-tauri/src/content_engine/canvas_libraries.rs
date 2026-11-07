// SPDX-License-Identifier: MIT
//! Canvas wireframe-library storage — SW-SC-5, collapsed to one team tier
//! by SW-SC-8.
//!
//! The shared canvas uses Excalidraw's own **native** library panel for all
//! management (browse, drag-to-canvas, save-selection, "Load from file"
//! import, per-item delete). CodeSense's only job is to persist that panel
//! to a single canonical file on the **active team** tier, git-synced so it
//! provisions to every member with no extra pipeline:
//!
//! - [`team_libraries_dir`] resolves `_team/{team}/libraries/` — a sibling
//!   of `install/`, never inside it, so the resolver/wave never materialize
//!   packs into source repos (see [`crate::context_repo::path`] "Canvas
//!   wireframe-library path helpers").
//! - [`read_team_library`] / [`write_team_library_at`] load and atomically
//!   store the one [`TEAM_LIBRARY_FILENAME`] bag — the whole library, v2.
//! - [`seed_library_if_absent`] merges a bundled starter pack into that bag
//!   exactly once, guarded by a marker file so deleting seeded shapes never
//!   brings them back (delete-sticky).
//!
//! The persisted bag is always our own v2 envelope (`libraryItems`), and the
//! bundled starter is v2 too, so the seed merge only ever parses JSON we
//! control. Imported v1 packs are normalized by Excalidraw's own loader on
//! the frontend before they are ever serialized back here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use log::warn;
use serde_json::{json, Value};

use crate::context_repo::{path_to_org_root, path_to_team_libraries_dir};

use super::atomic_io::write_tmp_fsync_rename;
use super::layers::{validate_org_slug, validate_path_segment, ResolverContext};
use super::Error;

/// Canonical filename for the one team-tier library bag. The native panel's
/// entire contents are serialized here as a v2 `.excalidrawlib`.
pub const TEAM_LIBRARY_FILENAME: &str = "team.excalidrawlib";

/// Resolve the active team's `libraries/` dir (team-only — the personal and
/// org tiers were dropped in SW-SC-8). `None` when there's no active team,
/// or when a defensive validation of `ctx.org_slug` / `ctx.team_name` fails
/// — the same guards [`super::layers::content_layer_root`] applies before any
/// `PathBuf::join`, so a hostile context can't escape `codesense_home`.
///
/// Pure: no filesystem touch.
pub fn team_libraries_dir(ctx: &ResolverContext) -> Option<PathBuf> {
    if validate_org_slug(&ctx.org_slug).is_some() {
        warn!(
            target: "content_engine::canvas_libraries",
            "rejected unsafe ctx.org_slug {:?}", ctx.org_slug
        );
        return None;
    }
    let team = ctx.team_name.as_deref()?;
    if validate_path_segment(team).is_some() {
        warn!(
            target: "content_engine::canvas_libraries",
            "rejected unsafe ctx.team_name {:?}", team
        );
        return None;
    }
    let org_root = path_to_org_root(&ctx.codesense_home, &ctx.principal(), &ctx.org_slug).ok()?;
    Some(path_to_team_libraries_dir(&org_root, team))
}

/// Read the library bag directly from `dir` — the dir-based read seam, mirror
/// of [`write_team_library_at`]. `None` when the bag doesn't exist yet (a fresh
/// tier before any save/seed) or is present-but-unreadable (logged and treated
/// as absent, so a single bad read never throws into the canvas-open path).
/// Used for both the active-team bag ([`read_team_library`]) and the personal
/// overlay bag, which lives under a tier that isn't the active team.
pub fn read_library_at(dir: &Path) -> Option<String> {
    let path = dir.join(TEAM_LIBRARY_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!(
                target: "content_engine::canvas_libraries",
                "skipping unreadable library {}: {}", path.display(), e
            );
            None
        }
    }
}

/// Read the canonical team library bag, or `None` when it doesn't exist yet
/// (a fresh team before any save/seed) or the team can't be resolved. Thin
/// active-team caller of [`read_library_at`].
pub fn read_team_library(ctx: &ResolverContext) -> Option<String> {
    read_library_at(&team_libraries_dir(ctx)?)
}

/// Convert a `serde_json` failure into the engine's I/O error variant.
fn json_err(e: serde_json::Error) -> Error {
    Error::Io(std::io::Error::other(e))
}

/// Parse a v2 `.excalidrawlib` bag and return its `libraryItems`. Errors when
/// `content` isn't a JSON object carrying an **array** `libraryItems`. Used to
/// validate before persisting and before merging, so a malformed bag is never
/// silently dropped or written to the git-synced team file.
fn parse_bag_items(content: &str) -> Result<Vec<Value>, Error> {
    let bag: Value = serde_json::from_str(content).map_err(json_err)?;
    match bag.get("libraryItems") {
        Some(Value::Array(arr)) => Ok(arr.clone()),
        _ => Err(Error::Io(std::io::Error::other(
            "canvas library bag is missing an array `libraryItems`",
        ))),
    }
}

/// Parse an **optional** bag string into its items: a `None`/blank input is a
/// fresh tier with no items; a present-but-malformed bag is an error. The one
/// "base bag might not exist yet" reader shared by the seed, the overlay, and
/// the write-split.
fn parse_optional_bag_items(bag: Option<&str>) -> Result<Vec<Value>, Error> {
    match bag.filter(|s| !s.trim().is_empty()) {
        Some(text) => parse_bag_items(text),
        None => Ok(Vec::new()),
    }
}

/// Atomically write the whole library bag to `dir/`[`TEAM_LIBRARY_FILENAME`],
/// creating the `libraries/` dir on first write. The caller serializes the
/// entire native panel here (SW-SC-8 D3), so an empty panel persists as an
/// empty bag — a native per-item delete sticks rather than being re-injected.
/// The `libraries/` tree is unwatched, so the plain atomic writer is correct
/// (no watcher suppression needed).
///
/// `content` is validated as a v2 envelope first: the bag is git-synced to
/// every team member, so a malformed write (a frontend serialize bug, a
/// truncated payload) must not become the canonical file and break everyone's
/// load + seed.
pub fn write_team_library_at(dir: &Path, content: &str) -> Result<(), Error> {
    parse_bag_items(content)?;
    std::fs::create_dir_all(dir)?;
    write_tmp_fsync_rename(&dir.join(TEAM_LIBRARY_FILENAME), content.as_bytes())
}

/// Serialize `items` as our canonical v2 `.excalidrawlib` envelope — the single
/// place a bag is written back to JSON, so every producer (seed, overlay, delta)
/// emits byte-identical framing.
fn serialize_bag(items: &[Value]) -> Result<String, Error> {
    serde_json::to_string(&json!({
        "type": "excalidrawlib",
        "version": 2,
        "libraryItems": items,
    }))
    .map_err(json_err)
}

/// Union two v2 bags' items into `base ++ (overlay-only)`, dedup by item `id`,
/// **base order preserved**: every base item is kept in place, then each overlay
/// item whose `id` isn't already in base is appended in overlay order. The
/// **base always wins** an `id` collision. Items lacking an `id` can't be deduped
/// and are always kept.
///
/// One merge, base-authoritative both ways — the overlay is purely *additive*:
/// - starter seed ([`seed_library_if_absent`]): base = the user's existing bag,
///   overlay = the bundled starter → existing shapes win, starter-only appended.
/// - personal overlay (`read_canvas_library`, AC-1): base = the shared team bag,
///   overlay = your personal bag → team stencils are authoritative, your private
///   ones appended. This makes the read the exact inverse of the write-split
///   ([`bag_delta`] subtracts team ids), so no personal item can shadow a team id
///   on read only to be dropped on write — overriding a team stencil is out of
///   scope (like hiding/deleting one).
fn merge_bags(base: Vec<Value>, overlay: Vec<Value>) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::with_capacity(base.len() + overlay.len());
    let mut seen: HashSet<String> = HashSet::new();
    for item in base {
        if let Some(id) = item.get("id").and_then(Value::as_str) {
            seen.insert(id.to_string());
        }
        result.push(item);
    }
    for item in overlay {
        match item.get("id").and_then(Value::as_str) {
            Some(id) if seen.contains(id) => continue, // base already won this id
            _ => result.push(item),
        }
    }
    result
}

/// Merge `starter`'s v2 `libraryItems` into `existing` (also v2), dedup by
/// item `id` with the **existing** bag winning a collision. `existing =
/// None`/blank yields the starter's items alone. Returns the serialized
/// merged v2 bag. A present-but-malformed `existing` bag is an **error**
/// (propagated so the caller aborts the seed and leaves the bag untouched),
/// never silently discarded into a starter-only bag.
///
/// Only ever parses our own v2 envelopes (the persisted bag and the bundled
/// starter), so it never has to normalize the v1 format — that's the
/// frontend loader's job on import. Thin caller of [`merge_bags`] (existing bag
/// is the base, so it wins; the starter is the additive overlay).
fn merge_starter_into_bag(existing: Option<&str>, starter: &[u8]) -> Result<String, Error> {
    let base = parse_optional_bag_items(existing)?;
    let starter: Value = serde_json::from_slice(starter).map_err(json_err)?;
    let overlay = match starter.get("libraryItems") {
        Some(Value::Array(arr)) => arr.clone(),
        _ => Vec::new(), // a starter without items leaves the base bag alone
    };
    serialize_bag(&merge_bags(base, overlay))
}

/// Overlay the `personal` bag on top of `team` (AC-1). Personal absent, blank,
/// or carrying **no items** → `team` verbatim (byte-identical fast path — no
/// re-serialization, no churn; covers the "deleted all personal stencils" case).
/// Otherwise the id-deduped union with the **team bag authoritative** (your
/// personal stencils are additive; they never shadow a team id — see
/// [`merge_bags`]). A malformed team OR personal bag makes the overlay fall back
/// to `team` verbatim so the canvas always opens — a bad bag never throws into
/// the open path (AC-6 read).
pub fn read_overlay(team: Option<String>, personal: Option<String>) -> Option<String> {
    let Some(personal) = personal.filter(|s| !s.trim().is_empty()) else {
        return team;
    };
    match merge_overlay_str(team.as_deref(), &personal) {
        Ok(Some(merged)) => Some(merged),
        Ok(None) => team, // personal parsed but empty → team verbatim
        Err(e) => {
            warn!(
                target: "content_engine::canvas_libraries",
                "canvas scratch library skipped (malformed bag): {e}"
            );
            team
        }
    }
}

/// Merge personal-wins, or `Ok(None)` when personal has no items (so the caller
/// keeps `team` verbatim). Propagates a parse error so [`read_overlay`] can fall
/// back to team-only.
fn merge_overlay_str(team: Option<&str>, personal: &str) -> Result<Option<String>, Error> {
    let overlay = parse_bag_items(personal)?;
    if overlay.is_empty() {
        return Ok(None);
    }
    let base = parse_optional_bag_items(team)?;
    Ok(Some(serialize_bag(&merge_bags(base, overlay))?))
}

/// The **delta** to persist to the personal bag (AC-2): the items of `whole`
/// (the entire native panel the FE serializes on every change) whose `id` is
/// **not** in `base` (the shared team bag). Items lacking an `id` can't be in
/// the team base, so they're kept as personal. A malformed `whole` or `base`
/// is an **error** — the caller then writes nothing, never overwriting a
/// git-synced bag with a mis-computed split (AC-6 write). A `None`/blank base
/// (no team bag yet) means an empty base-id set → the delta is the whole panel.
pub fn bag_delta(whole: &str, base: Option<&str>) -> Result<String, Error> {
    let base_ids: HashSet<String> = parse_optional_bag_items(base)?
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let delta: Vec<Value> = parse_bag_items(whole)?
        .into_iter()
        .filter(|i| match i.get("id").and_then(Value::as_str) {
            Some(id) => !base_ids.contains(id),
            None => true,
        })
        .collect();
    serialize_bag(&delta)
}

/// Seed `starter_bytes` into the team bag ([`TEAM_LIBRARY_FILENAME`]) under
/// `libraries_dir`, exactly once, guarded by `libraries_dir/marker_name`.
///
/// Returns `Ok(true)` when it seeded, `Ok(false)` when the marker already
/// existed (so a user who deleted the seeded shapes never gets them back —
/// delete-sticky). The merge keeps any item the user already has on an `id`
/// collision. The marker is written **after** the bag so a crash between the
/// two re-seeds on the next call rather than leaving a marker with no seed.
pub fn seed_library_if_absent(
    libraries_dir: &Path,
    marker_name: &str,
    starter_bytes: &[u8],
) -> Result<bool, Error> {
    let marker = libraries_dir.join(marker_name);
    if marker.exists() {
        return Ok(false);
    }
    let bag_path = libraries_dir.join(TEAM_LIBRARY_FILENAME);
    // Only a genuinely-absent bag merges to "starter alone". A bag that exists
    // but can't be read is propagated, never silently overwritten with a
    // starter-only bag (which would lose the user's shapes).
    let existing = match std::fs::read_to_string(&bag_path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let merged = merge_starter_into_bag(existing.as_deref(), starter_bytes)?;

    std::fs::create_dir_all(libraries_dir)?;
    write_tmp_fsync_rename(&bag_path, merged.as_bytes())?;
    write_tmp_fsync_rename(&marker, b"")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(home: &Path, team: Option<&str>) -> ResolverContext {
        ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: team.map(String::from),
        }
    }

    #[test]
    fn team_libraries_dir_resolves_under_team_shared() {
        let home = Path::new("/home/dev/.codesense");
        assert_eq!(
            team_libraries_dir(&ctx(home, Some("default"))).unwrap(),
            PathBuf::from("/home/dev/.codesense/users/42/orgs/acme/api/default/libraries"),
        );
    }

    #[test]
    fn team_libraries_dir_none_without_team() {
        let home = Path::new("/home/dev/.codesense");
        assert!(team_libraries_dir(&ctx(home, None)).is_none());
    }

    #[test]
    fn team_libraries_dir_rejects_unsafe_context() {
        let home = Path::new("/home/dev/.codesense");
        let bad_org = ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: 42,
            org_slug: "../escape".to_string(),
            team_name: Some("default".to_string()),
        };
        assert!(team_libraries_dir(&bad_org).is_none());

        let bad_team = ResolverContext {
            context_root: None,
            codesense_home: home.to_path_buf(),
            user_id: 42,
            org_slug: "acme/api".to_string(),
            team_name: Some("ev/il".to_string()),
        };
        assert!(team_libraries_dir(&bad_team).is_none());
    }

    #[test]
    fn write_then_read_round_trips_the_whole_bag() {
        let tmp = tempfile::TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let dir = team_libraries_dir(&c).unwrap();

        let bag = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"a"}]}"#;
        write_team_library_at(&dir, bag).unwrap();
        assert_eq!(read_team_library(&c).as_deref(), Some(bag));
    }

    #[test]
    fn read_is_none_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(read_team_library(&ctx(tmp.path(), Some("default"))).is_none());
    }

    #[test]
    fn empty_bag_round_trips_as_empty_not_deleted() {
        // A native "delete the last item" persists an empty bag — it must
        // survive a round-trip (delete-sticky), not be treated as absent.
        let tmp = tempfile::TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let dir = team_libraries_dir(&c).unwrap();
        let empty = r#"{"type":"excalidrawlib","version":2,"libraryItems":[]}"#;
        write_team_library_at(&dir, empty).unwrap();
        assert_eq!(read_team_library(&c).as_deref(), Some(empty));
    }

    #[test]
    fn write_rejects_a_malformed_bag_and_writes_nothing() {
        // The bag is git-synced to every member, so a malformed write must be
        // rejected before it becomes the canonical file.
        let tmp = tempfile::TempDir::new().unwrap();
        let c = ctx(tmp.path(), Some("default"));
        let dir = team_libraries_dir(&c).unwrap();
        assert!(write_team_library_at(&dir, "not json").is_err());
        assert!(write_team_library_at(&dir, r#"{"type":"excalidrawlib","version":2}"#).is_err());
        assert!(write_team_library_at(&dir, r#"{"libraryItems":{}}"#).is_err());
        assert!(read_team_library(&c).is_none());
    }

    fn starter(ids: &[&str]) -> Vec<u8> {
        let items: Vec<Value> = ids.iter().map(|id| json!({ "id": id })).collect();
        serde_json::to_vec(&json!({
            "type": "excalidrawlib", "version": 2, "libraryItems": items
        }))
        .unwrap()
    }

    fn item_ids(bag: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(bag).unwrap();
        v["libraryItems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn seed_merges_into_empty_then_is_a_noop_when_marker_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("default/libraries");
        let marker = ".seeded-starter";

        // First call seeds the starter as the whole bag.
        let seeded = seed_library_if_absent(&dir, marker, &starter(&["s1", "s2"])).unwrap();
        assert!(seeded);
        assert!(dir.join(marker).exists());
        assert_eq!(
            item_ids(&std::fs::read_to_string(dir.join(TEAM_LIBRARY_FILENAME)).unwrap()),
            ["s1", "s2"],
        );

        // Second call is a no-op (marker present).
        let seeded = seed_library_if_absent(&dir, marker, &starter(&["s3"])).unwrap();
        assert!(!seeded);
        assert_eq!(
            item_ids(&std::fs::read_to_string(dir.join(TEAM_LIBRARY_FILENAME)).unwrap()),
            ["s1", "s2"],
        );
    }

    #[test]
    fn seed_merges_starter_into_existing_bag_existing_wins_on_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("libraries");
        std::fs::create_dir_all(&dir).unwrap();
        // The user already has their own bag with one shape sharing id "s1".
        let existing =
            r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"s1","name":"mine"}]}"#;
        std::fs::write(dir.join(TEAM_LIBRARY_FILENAME), existing).unwrap();

        let seeded = seed_library_if_absent(&dir, ".m", &starter(&["s1", "s2"])).unwrap();
        assert!(seeded);
        let bag = std::fs::read_to_string(dir.join(TEAM_LIBRARY_FILENAME)).unwrap();
        // Existing "s1" kept (the user's, not the starter's), "s2" appended.
        assert_eq!(item_ids(&bag), ["s1", "s2"]);
        let v: Value = serde_json::from_str(&bag).unwrap();
        assert_eq!(v["libraryItems"][0]["name"], "mine");
    }

    #[test]
    fn seed_errors_on_malformed_existing_bag_and_leaves_it_untouched() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("libraries");
        std::fs::create_dir_all(&dir).unwrap();
        // A present-but-malformed bag must abort the seed (propagate), never be
        // clobbered with a starter-only bag (which would drop the user's shapes).
        let garbage = r#"{"libraryItems":"not-an-array"}"#;
        std::fs::write(dir.join(TEAM_LIBRARY_FILENAME), garbage).unwrap();

        assert!(seed_library_if_absent(&dir, ".m", &starter(&["s1"])).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join(TEAM_LIBRARY_FILENAME)).unwrap(),
            garbage,
        );
        // Marker not written, so a later fixed bag can still seed.
        assert!(!dir.join(".m").exists());
    }

    #[test]
    fn deleting_seeded_items_then_reseed_attempt_stays_deleted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("libraries");
        let marker = ".seeded-starter";
        seed_library_if_absent(&dir, marker, &starter(&["s1"])).unwrap();

        // User deletes everything → an empty bag is persisted (whole-bag write).
        write_team_library_at(
            &dir,
            r#"{"type":"excalidrawlib","version":2,"libraryItems":[]}"#,
        )
        .unwrap();

        // A later open re-runs seeding: marker present → no-op, stays empty.
        let seeded = seed_library_if_absent(&dir, marker, &starter(&["s1"])).unwrap();
        assert!(!seeded);
        assert!(
            item_ids(&std::fs::read_to_string(dir.join(TEAM_LIBRARY_FILENAME)).unwrap()).is_empty()
        );
    }

    // ---- merge_bags (AC-3) ----

    #[test]
    fn merge_bags_base_wins_and_appends_overlay_only_in_order() {
        // Base kept in position, colliding `b` NOT re-added, overlay-only `c`
        // appended — the one additive union both callers use.
        let base = vec![json!({"id":"a","v":"base"}), json!({"id":"b","v":"base"})];
        let overlay = vec![
            json!({"id":"b","v":"overlay"}),
            json!({"id":"c","v":"overlay"}),
        ];
        let merged = merge_bags(base, overlay);
        let ids: Vec<_> = merged.iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(merged[1]["v"], "base"); // base won the `b` collision
        assert_eq!(merged[2]["v"], "overlay");
    }

    #[test]
    fn merge_bags_keeps_id_less_items_from_both() {
        let base = vec![json!({"id":"a"}), json!({"name":"no-id-base"})];
        let overlay = vec![json!({"name":"no-id-overlay"})];
        let merged = merge_bags(base, overlay);
        assert_eq!(merged.len(), 3); // nothing deduped away
    }

    // ---- read_overlay (AC-1 + AC-6 read) ----

    const TEAM_BAG: &str = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"t1"}]}"#;

    #[test]
    fn read_overlay_no_personal_returns_team_verbatim() {
        assert_eq!(
            read_overlay(Some(TEAM_BAG.to_string()), None).as_deref(),
            Some(TEAM_BAG),
        );
    }

    #[test]
    fn read_overlay_empty_personal_returns_team_verbatim_byte_identical() {
        // Deleted all personal stencils → empty-items bag must NOT re-serialize
        // the team bag (byte-identity fast path).
        let empty = r#"{"type":"excalidrawlib","version":2,"libraryItems":[]}"#;
        let out = read_overlay(Some(TEAM_BAG.to_string()), Some(empty.to_string()));
        assert_eq!(out.as_deref(), Some(TEAM_BAG));
    }

    #[test]
    fn read_overlay_unions_team_and_personal_team_authoritative() {
        // Team bag is authoritative: a colliding personal `t1` does NOT shadow
        // the team item; personal-only `p1` is appended. This keeps the read the
        // exact inverse of the write-split.
        let personal = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"t1","v":"mine"},{"id":"p1"}]}"#;
        let out = read_overlay(Some(TEAM_BAG.to_string()), Some(personal.to_string())).unwrap();
        assert_eq!(item_ids(&out), ["t1", "p1"]); // union, deduped
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["libraryItems"][0].get("v").is_none()); // team's t1 kept, not personal's
    }

    #[test]
    fn read_overlay_malformed_personal_falls_back_to_team() {
        // A corrupt personal bag must never brick the canvas-open path.
        let out = read_overlay(Some(TEAM_BAG.to_string()), Some("not json".to_string()));
        assert_eq!(out.as_deref(), Some(TEAM_BAG));
    }

    #[test]
    fn read_overlay_personal_only_when_no_team() {
        let personal = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"p1"}]}"#;
        let out = read_overlay(None, Some(personal.to_string())).unwrap();
        assert_eq!(item_ids(&out), ["p1"]);
    }

    // ---- bag_delta (AC-2) ----

    #[test]
    fn bag_delta_keeps_only_items_absent_from_base() {
        let whole =
            r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"t1"},{"id":"p1"}]}"#;
        let out = bag_delta(whole, Some(TEAM_BAG)).unwrap();
        assert_eq!(item_ids(&out), ["p1"]); // t1 (team) subtracted
    }

    #[test]
    fn bag_delta_no_base_keeps_everything() {
        let whole = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"p1"}]}"#;
        assert_eq!(item_ids(&bag_delta(whole, None).unwrap()), ["p1"]);
    }

    #[test]
    fn bag_delta_malformed_input_is_error() {
        assert!(bag_delta("not json", Some(TEAM_BAG)).is_err());
        assert!(bag_delta(TEAM_BAG, Some("not json")).is_err());
    }

    #[test]
    fn overlay_round_trip_drops_personal_item_colliding_with_team_id() {
        // A personal item sharing a team `id` is team-authoritative-dropped on
        // read AND on write — the read is the exact inverse of the write-split, so
        // a colliding personal item can never survive on one side only.
        let team =
            r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"t1","v":"team"}]}"#;
        let personal = r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"t1","v":"mine"},{"id":"p1"}]}"#;

        // Read: team wins t1, p1 appended.
        let merged = read_overlay(Some(team.to_string()), Some(personal.to_string())).unwrap();
        assert_eq!(item_ids(&merged), ["t1", "p1"]);
        let v: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["libraryItems"][0]["v"], "team"); // not "mine"

        // Write the merged panel back: the delta excludes t1 (in team base), so the
        // personal bag keeps only p1 — never a colliding t1.
        let delta = bag_delta(&merged, Some(team)).unwrap();
        assert_eq!(item_ids(&delta), ["p1"]);
    }

    // ---- read_library_at (dir-based read seam) ----

    #[test]
    fn read_library_at_absent_is_none_present_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("libraries");
        assert!(read_library_at(&dir).is_none());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(TEAM_LIBRARY_FILENAME), TEAM_BAG).unwrap();
        assert_eq!(read_library_at(&dir).as_deref(), Some(TEAM_BAG));
    }
}
