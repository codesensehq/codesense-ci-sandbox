// SPDX-License-Identifier: MIT
//! User-owned context checkouts. Registration never changes the checkout.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::domain::{Principal, RawOrgYml};
use crate::shared::yaml_io::{read_yaml_or_default, write_yaml_value_atomic};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextAttachment {
    pub slug: String,
    pub display_name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    attachments: BTreeMap<String, ContextAttachment>,
}

#[derive(Clone, PartialEq, Eq)]
struct Fingerprint {
    modified: SystemTime,
    len: u64,
}

#[derive(Clone)]
struct CachedRegistry {
    fingerprint: Option<Fingerprint>,
    value: Result<Registry, String>,
}

type RegistryCache = BTreeMap<PathBuf, CachedRegistry>;

fn cache() -> &'static Mutex<RegistryCache> {
    static CACHE: OnceLock<Mutex<RegistryCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Every attached checkout root seen in a registry → that registry file. Kept apart
/// from the bounded registry cache so evicting a cached registry never makes a
/// path-only lookup ([`location_for_known_path`]) forget which home a checkout is
/// registered under; it holds paths only (a registry is always re-read for truth).
fn known_roots() -> &'static Mutex<BTreeMap<PathBuf, PathBuf>> {
    static ROOTS: OnceLock<Mutex<BTreeMap<PathBuf, PathBuf>>> = OnceLock::new();
    ROOTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn remember_roots(file: &Path, registry: &Registry) {
    if let Ok(mut roots) = known_roots().lock() {
        roots.retain(|_, registered| registered != file);
        for attachment in registry.attachments.values() {
            roots.insert(attachment.root.clone(), file.to_path_buf());
        }
    }
}

/// Shared admission lock spans registration and managed creation, including rollback.
pub(crate) fn admission(
    home: &Path,
    principal: &Principal,
    slug: &str,
) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<
        crate::commands::weak_registry::WeakArcRegistry<tokio::sync::Mutex<()>>,
    > = OnceLock::new();
    let key = format!("{}:{}:{slug}", home.display(), principal.as_str());
    LOCKS
        .get_or_init(Default::default)
        .get_or_create(&key, || std::sync::Arc::new(tokio::sync::Mutex::new(())))
}

fn scoped_registry(home: &Path, path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(home.join(super::USERS_DIR)).ok()?;
    let mut parts = relative.components();
    let principal = parts.next()?.as_os_str().to_str()?;
    if parts.next()?.as_os_str() != super::ORGS_DIR {
        return None;
    }
    Some(registry_path(home, &Principal::from(principal)))
}

pub fn registry_path(home: &Path, principal: &Principal) -> PathBuf {
    super::path_to_user_home(home, principal).join("context-attachments.yml")
}

fn fingerprint(path: &Path) -> Result<Option<Fingerprint>, String> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some(Fingerprint {
            modified: meta.modified().map_err(|e| e.to_string())?,
            len: meta.len(),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Cannot inspect {}: {e}", path.display())),
    }
}

fn read_locked(path: &Path, cache: &mut RegistryCache) -> Result<Registry, String> {
    let stamp = fingerprint(path)?;
    if let Some(cached) = cache.get(path).filter(|c| c.fingerprint == stamp) {
        return cached.value.clone();
    }
    let value = read_yaml_or_default::<Registry>(path)
        .map_err(|e| format!("Cannot read context attachment registry {}: {e}. Repair the file before continuing.", path.display()))
        .and_then(|registry| {
            for (slug, attachment) in &registry.attachments {
                if slug != &attachment.slug || super::org_slug::validate_org_slug(slug).is_some()
                    || !attachment.root.is_absolute() || attachment.root.parent().is_none()
                    || attachment.root.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir))
                {
                    return Err(format!("Invalid context attachment in {} for {slug:?}; repair the registry before continuing", path.display()));
                }
            }
            let mut roots = std::collections::BTreeSet::new();
            if registry.attachments.values().any(|a| !roots.insert(&a.root)) {
                return Err(format!("Duplicate context roots in {}; repair the registry before continuing", path.display()));
            }
            Ok(registry)
        });
    if let Ok(registry) = &value {
        remember_roots(path, registry);
    }
    // The process normally uses one namespace. Bound retention for tests and
    // long-running sessions that switch between many principals: evict ONE other
    // entry, never the whole cache (known roots survive either way).
    if cache.len() >= 64 && !cache.contains_key(path) {
        if let Some(evict) = cache.keys().find(|k| k.as_path() != path).cloned() {
            cache.remove(&evict);
        }
    }
    cache.insert(
        path.to_path_buf(),
        CachedRegistry {
            fingerprint: stamp,
            value: value.clone(),
        },
    );
    value
}

pub fn list(home: &Path, principal: &Principal) -> Result<Vec<ContextAttachment>, String> {
    let mut cache = cache()
        .lock()
        .map_err(|_| "Context attachment registry lock unavailable")?;
    Ok(read_locked(&registry_path(home, principal), &mut cache)?
        .attachments
        .into_values()
        .collect())
}

pub fn registered(
    home: &Path,
    principal: &Principal,
    slug: &str,
) -> Result<Option<ContextAttachment>, String> {
    let mut cache = cache()
        .lock()
        .map_err(|_| "Context attachment registry lock unavailable")?;
    Ok(read_locked(&registry_path(home, principal), &mut cache)?
        .attachments
        .get(slug)
        .cloned())
}

pub fn available_root(attachment: &ContextAttachment) -> Result<PathBuf, String> {
    let current = attachment.root.canonicalize().map_err(|e| {
        format!(
            "Attached context {} is unavailable at {}: {e}. Restore the checkout or detach it.",
            attachment.slug,
            attachment.root.display()
        )
    })?;
    if current != attachment.root || !current.join("org.yml").is_file() {
        return Err(format!("Attached context {} has moved or is no longer valid at {}. Restore the checkout or detach it.", attachment.slug, attachment.root.display()));
    }
    // Metadata-invalidated and bounded: hot resolution does not repeatedly open
    // libgit2 or parse YAML. Worktree gitfiles and Git config changes invalidate it.
    type ValidationCache =
        BTreeMap<(PathBuf, String), (Vec<Option<Fingerprint>>, Result<(), String>)>;
    static VALIDATIONS: OnceLock<Mutex<ValidationCache>> = OnceLock::new();
    let stamps = ["org.yml", ".git", ".git/config"]
        .iter()
        .map(|name| {
            if *name == ".git/config" && !current.join(".git").is_dir() {
                Ok(None)
            } else {
                fingerprint(&current.join(name))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut validations = VALIDATIONS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| "Context validation cache unavailable")?;
    let key = (current.clone(), attachment.slug.clone());
    let valid = if let Some((_, result)) = validations
        .get(&key)
        .filter(|(previous, _)| previous == &stamps)
    {
        result.clone()
    } else {
        let result = (|| -> Result<(), String> {
            let repo = git2::Repository::open(&current).map_err(|e| e.to_string())?;
            if repo.workdir().and_then(|p| p.canonicalize().ok()).as_ref() != Some(&current) {
                return Err("the registered folder is no longer a Git checkout root".into());
            }
            let raw: RawOrgYml = serde_yaml::from_slice(
                &std::fs::read(current.join("org.yml")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if raw.slug != attachment.slug {
                return Err(format!("declared identity changed to {}", raw.slug));
            }
            Ok(())
        })();
        if validations.len() >= 64 {
            validations.clear();
        }
        validations.insert(key, (stamps, result.clone()));
        result
    };
    drop(validations);
    valid.map_err(|e| {
        format!(
            "Attached context {} is unavailable at {}: {e}. Restore the checkout or detach it.",
            attachment.slug,
            current.display()
        )
    })?;
    Ok(current)
}

pub fn attach(
    home: &Path,
    principal: &Principal,
    selected: &Path,
) -> Result<ContextAttachment, String> {
    attach_impl(home, principal, selected, false)
}

/// Caller holds the identity admission lock through creation and registration.
///
/// **Lock contract (SW-CR-5):** when `admitted == true`, the caller MUST hold
/// an admission mutex keyed to the `org.yml` slug of `selected` — the same key
/// `admission(home, principal, &raw.slug)` would produce. The non-admitted
/// path takes that mutex internally; the admitted path trusts the caller to
/// hold the slug-matched key through creation and registration so no concurrent
/// attach for the same slug can race. `create_at_with` → `create_with_attach`
/// is the only current caller; it takes `admission(..., &slug)` on the
/// `sanitize_name` slug (equal to `raw.slug` by scaffold contract) before
/// handing control to `attach_admitted`.
pub(crate) fn attach_admitted(
    home: &Path,
    principal: &Principal,
    selected: &Path,
) -> Result<ContextAttachment, String> {
    attach_impl(home, principal, selected, true)
}

fn attach_impl(
    home: &Path,
    principal: &Principal,
    selected: &Path,
    admitted: bool,
) -> Result<ContextAttachment, String> {
    if !selected.is_absolute() {
        return Err("Select an absolute context checkout path".into());
    }
    let root = selected.canonicalize().map_err(|e| {
        format!(
            "Cannot access selected context checkout {}: {e}",
            selected.display()
        )
    })?;
    let repository = git2::Repository::open(&root)
        .map_err(|e| format!("Selected folder is not a Git checkout: {e}"))?;
    if repository
        .workdir()
        .and_then(|p| p.canonicalize().ok())
        .as_ref()
        != Some(&root)
    {
        return Err("Select the checkout root, not a subdirectory or bare repository".into());
    }
    let raw: RawOrgYml = serde_yaml::from_slice(
        &std::fs::read(root.join("org.yml"))
            .map_err(|e| format!("Cannot read selected context's org.yml: {e}"))?,
    )
    .map_err(|e| format!("Invalid context org.yml: {e}"))?;
    if let Some(reason) = super::org_slug::validate_org_slug(&raw.slug) {
        return Err(format!(
            "Invalid declared context identity {}: {reason}",
            raw.slug
        ));
    }
    // SW-CR-1: serialize the collision pre-check under the identity admission
    // lock so two concurrent attach calls for the same slug + different roots
    // see each other's registration (the later admission-held cross-registry
    // and duplicate checks still catch anything that slips through, but the
    // nicer "edit {new}/org.yml" guidance below only fires when this scope
    // holds the lock). Dropped at the end of this fn on every exit, including
    // error returns.
    let _admission =
        (!admitted).then(|| admission(home, principal, &raw.slug).blocking_lock_owned());
    // Honest slug collision — the slug lives in `org.yml` and travels with the
    // repo, so a user who hits a collision resolves it by editing that file at
    // the folder they're attaching, not by renaming the already-attached one.
    // Different-root collisions name BOTH roots and point at the new folder's
    // `org.yml`. The actionable guidance IS the error — rendered as-is to the
    // user (no FE classifier case is needed).
    if let Some(existing) = registered(home, principal, &raw.slug)? {
        if existing.root != root {
            return Err(format!(
                "Slug {} is already attached at {}; edit {}/org.yml to pick a different slug for this folder, then re-attach.",
                raw.slug,
                existing.root.display(),
                root.display(),
            ));
        }
        // Same root → fall through; the re-attach path below handles
        // idempotency and follows a `display_name` rename.
    }
    if root.parent().is_none() {
        return Err("A filesystem root cannot be attached as a context checkout".into());
    }
    if root.to_str().is_none() {
        return Err("Select a checkout whose path is valid UTF-8".into());
    }
    // SW-CR-5: case-insensitive (ASCII) match on the reserved owner so a
    // mixed-case `_Personal/<x>` slug can't slip past this two-segment guard
    // on a case-folding filesystem.
    if raw
        .slug
        .split('/')
        .next()
        .map(|f| f.eq_ignore_ascii_case(super::scoped_key::PERSONAL_OWNER))
        .unwrap_or(false)
    {
        return Err(format!(
            "The {} owner is reserved for personal keys; choose a separately named context",
            super::scoped_key::PERSONAL_OWNER
        ));
    }
    let loaded = crate::domain::load_org(raw.slug.clone(), &root, None);
    if loaded.org.is_none() {
        let joined = loaded
            .file_errors
            .iter()
            .map(|e| format!("{}: {}", e.path.display(), e.message))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("Invalid context registry: {joined}"));
    }
    let attachment = ContextAttachment {
        slug: raw.slug,
        display_name: raw.display_name,
        root,
    };
    let path = registry_path(home, principal);
    let mut cache = cache()
        .lock()
        .map_err(|_| "Context attachment registry lock unavailable")?;
    let mut registry = read_locked(&path, &mut cache)?;
    for other in registry_files(home)? {
        if other != path
            && read_locked(&other, &mut cache)?
                .attachments
                .values()
                .any(|a| a.root == attachment.root)
        {
            return Err("This checkout is already attached under another principal; detach that registration first".into());
        }
    }
    if let Some(existing) = registry.attachments.get(&attachment.slug) {
        if existing.root == attachment.root {
            // Same checkout again (every launch re-attaches the bundled docs): keep the
            // registration, but follow a rename in org.yml so the stored name never goes stale.
            if existing.display_name == attachment.display_name {
                return Ok(existing.clone());
            }
            registry
                .attachments
                .insert(attachment.slug.clone(), attachment.clone());
            write_yaml_value_atomic(&path, &registry)
                .map_err(|e| format!("Cannot save context attachment: {e}"))?;
            cache.remove(&path);
            read_locked(&path, &mut cache)?;
            return Ok(attachment);
        }
        return Err(format!(
            "Context {} is already attached at {}; detach that registration first",
            attachment.slug,
            existing.root.display()
        ));
    }
    if registry
        .attachments
        .values()
        .any(|a| a.root == attachment.root)
    {
        return Err("This checkout is already attached under another identity".into());
    }
    let managed = super::path_to_user_home(home, principal)
        .join(super::ORGS_DIR)
        .join(&attachment.slug);
    if managed.exists() {
        return Err(format!(
            "Context {} already exists in managed storage at {}; its data has been preserved",
            attachment.slug,
            managed.display()
        ));
    }
    registry
        .attachments
        .insert(attachment.slug.clone(), attachment.clone());
    write_yaml_value_atomic(&path, &registry)
        .map_err(|e| format!("Cannot save context attachment: {e}"))?;
    cache.remove(&path);
    read_locked(&path, &mut cache)?;
    Ok(attachment)
}

pub fn detach(home: &Path, principal: &Principal, slug: &str) -> Result<(), String> {
    let path = registry_path(home, principal);
    let mut cache = cache()
        .lock()
        .map_err(|_| "Context attachment registry lock unavailable")?;
    let mut registry = read_locked(&path, &mut cache)?;
    if registry.attachments.remove(slug).is_some() {
        write_yaml_value_atomic(&path, &registry)
            .map_err(|e| format!("Cannot detach context: {e}"))?;
        cache.remove(&path);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::context_repo::path_to_org_root;

    pub(crate) fn checkout(parent: &Path, folder: &str, slug: &str) -> PathBuf {
        let root = parent.join(folder);
        git2::Repository::init(&root).unwrap();
        std::fs::write(
            root.join("org.yml"),
            format!("slug: {slug}\ndisplay_name: My context\ncreated_at: 2026-10-01T00:00:00Z\n"),
        )
        .unwrap();
        std::fs::write(root.join("people.yml"), "people: []\n").unwrap();
        std::fs::write(root.join("teams.yml"), "teams:\n  - name: default\n").unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn attaches_dirty_checkout_by_declared_identity_without_origin_or_binding() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = checkout(temp.path(), "arbitrary-folder", "acme/context");
        let guest = Principal::local_guest();
        let data = b"saved edits and untracked data\n";
        std::fs::write(root.join("notes.md"), data).unwrap();
        let config = std::fs::read(root.join(".git/config")).unwrap();
        let attachment = attach(&home, &guest, &root).unwrap();
        assert_eq!(attachment.slug, "acme/context");
        assert_eq!(
            path_to_org_root(&home, &guest, &attachment.slug).unwrap(),
            root
        );
        assert_eq!(std::fs::read(root.join("notes.md")).unwrap(), data);
        assert_eq!(std::fs::read(root.join(".git/config")).unwrap(), config);
        assert!(!super::super::path_to_user_org_bindings_file(&home, &guest).exists());
        assert!(!super::super::path_to_managed_org_root(&home, &guest, &attachment.slug).exists());
        let loaded = crate::domain::load_org(attachment.slug, &root, None);
        assert!(loaded.org.is_some(), "{:?}", loaded.file_errors);
        assert_eq!(loaded.root, root);
    }

    #[test]
    fn restart_and_repeat_are_idempotent_and_detach_preserves_every_file() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = checkout(temp.path(), "checkout", "acme/context");
        let who = Principal::from(42_u64);
        let attached = attach(&home, &who, &root).unwrap();
        assert_eq!(attach(&home, &who, &root).unwrap(), attached);
        cache().lock().unwrap().remove(&registry_path(&home, &who));
        assert_eq!(list(&home, &who).unwrap(), vec![attached]);
        detach(&home, &who, "acme/context").unwrap();
        detach(&home, &who, "acme/context").unwrap();
        assert!(root.join("org.yml").is_file());
        assert!(root.join(".git/config").is_file());
        assert!(list(&home, &who).unwrap().is_empty());
    }

    #[test]
    fn reattaching_the_same_checkout_follows_a_display_name_rename() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = checkout(temp.path(), "checkout", "acme/context");
        let who = Principal::from(42_u64);
        assert_eq!(
            attach(&home, &who, &root).unwrap().display_name,
            "My context"
        );
        std::fs::write(
            root.join("org.yml"),
            "slug: acme/context\ndisplay_name: Renamed context\ncreated_at: 2026-10-01T00:00:00Z\n",
        )
        .unwrap();
        let again = attach(&home, &who, &root).unwrap();
        assert_eq!(again.display_name, "Renamed context");
        assert_eq!(again.root, root);
        cache().lock().unwrap().remove(&registry_path(&home, &who));
        assert_eq!(list(&home, &who).unwrap(), vec![again]);
    }

    #[test]
    fn unavailable_attachment_remains_discoverable_and_never_falls_back() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = checkout(temp.path(), "checkout", "acme/context");
        let who = Principal::local_guest();
        attach(&home, &who, &root).unwrap();
        let moved = temp.path().join("moved-by-owner");
        std::fs::rename(&root, &moved).unwrap();
        assert!(path_to_org_root(&home, &who, "acme/context")
            .unwrap_err()
            .contains("unavailable"));
        let found = crate::commands::org::discover_local_orgs_impl(&home, &who).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].is_attached);
        assert!(found[0].availability_error.is_some());
        detach(&home, &who, "acme/context").unwrap();
        assert!(moved.join("org.yml").exists());
        assert!(!root.exists());
    }

    #[test]
    fn collisions_and_corrupt_registry_preserve_existing_state() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let first = checkout(temp.path(), "first", "acme/context");
        let second = checkout(temp.path(), "second", "acme/context");
        let who = Principal::local_guest();
        attach(&home, &who, &first).unwrap();
        let path = registry_path(&home, &who);
        let before = std::fs::read(&path).unwrap();
        assert!(attach(&home, &who, &second).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let corrupt = b"attachments: [[[";
        std::fs::write(&path, corrupt).unwrap();
        assert!(path_to_org_root(&home, &who, "acme/context").is_err());
        assert!(crate::commands::org::discover_local_orgs_impl(&home, &who).is_err());
        assert!(attach(&home, &who, &second).is_err());
        assert!(detach(&home, &who, "acme/context").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }

    // SW-CR-1: slug collision across two different roots fails with an error
    // naming BOTH roots and points the user at the new folder's `org.yml`.
    #[test]
    fn attach_rejects_slug_collision_from_different_root() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let first = checkout(temp.path(), "first", "notes");
        let second = checkout(temp.path(), "second", "notes");
        let who = Principal::local_guest();
        attach(&home, &who, &first).unwrap();
        let error = attach(&home, &who, &second).unwrap_err();
        assert!(error.contains(first.to_str().unwrap()), "{error}");
        assert!(error.contains(second.to_str().unwrap()), "{error}");
        assert!(error.contains("org.yml"), "{error}");
        // The slug renders plain (not debug-quoted) so the surfaced error
        // reads naturally — matches the existing "Context <slug> is already"
        // style in this file.
        assert!(error.contains("Slug notes "), "{error}");
        // Still exactly one registered attachment under `notes`.
        cache().lock().unwrap().remove(&registry_path(&home, &who));
        let listed = list(&home, &who).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].slug, "notes");
        assert_eq!(listed[0].root, first);
    }

    // SW-CR-1: attaching the same root twice succeeds without a duplicate entry.
    #[test]
    fn attach_is_idempotent_for_same_root_same_slug() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = checkout(temp.path(), "checkout", "notes");
        let who = Principal::local_guest();
        let first = attach(&home, &who, &root).unwrap();
        let second = attach(&home, &who, &root).unwrap();
        assert_eq!(first, second);
        cache().lock().unwrap().remove(&registry_path(&home, &who));
        let listed = list(&home, &who).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], first);
    }

    // SW-CR-5 (CR-1 R2 follow-up): the identity admission lock serializes two
    // concurrent attach calls for the same slug + different roots so one
    // succeeds and the other returns the actionable "edit {new}/org.yml"
    // collision guidance. Without the admission ordering both threads could
    // see no existing registration under the slug and race on write.
    #[test]
    fn attach_rejects_concurrent_same_slug_different_root() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let first = checkout(temp.path(), "first", "notes");
        let second = checkout(temp.path(), "second", "notes");
        let who = Principal::local_guest();
        let barrier = std::sync::Barrier::new(2);
        let (a, b) = std::thread::scope(|scope| {
            let t1 = {
                let home = &home;
                let who = &who;
                let first = &first;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    attach(home, who, first)
                })
            };
            let t2 = {
                let home = &home;
                let who = &who;
                let second = &second;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    attach(home, who, second)
                })
            };
            (t1.join().unwrap(), t2.join().unwrap())
        });
        let (ok_count, err_count) = (
            [&a, &b].iter().filter(|r| r.is_ok()).count(),
            [&a, &b].iter().filter(|r| r.is_err()).count(),
        );
        assert_eq!(ok_count, 1, "expected exactly one Ok; got a={a:?} b={b:?}");
        assert_eq!(
            err_count, 1,
            "expected exactly one Err; got a={a:?} b={b:?}"
        );
        let err = a.err().or(b.err()).unwrap();
        assert!(err.contains("edit "), "{err}");
        assert!(err.contains("/org.yml to pick a different slug"), "{err}");
        cache().lock().unwrap().remove(&registry_path(&home, &who));
        let listed = list(&home, &who).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].slug, "notes");
    }

    #[test]
    fn concurrent_registrations_do_not_lose_updates() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let roots: Vec<_> = (0..8)
            .map(|id| {
                checkout(
                    temp.path(),
                    &format!("checkout-{id}"),
                    &format!("acme/context-{id}"),
                )
            })
            .collect();
        std::thread::scope(|scope| {
            for root in roots {
                let home = &home;
                scope.spawn(move || attach(home, &Principal::local_guest(), &root).unwrap());
            }
        });
        assert_eq!(list(&home, &Principal::local_guest()).unwrap().len(), 8);
    }
}

/// Reverse identity for legacy consumers that receive a path rather than an org.
#[derive(Clone, Debug)]
pub(crate) struct RegisteredLocation {
    pub home: PathBuf,
    pub principal: Principal,
    pub attachment: ContextAttachment,
}

fn registry_files(home: &Path) -> Result<Vec<PathBuf>, String> {
    type Inventory = BTreeMap<PathBuf, (Option<Fingerprint>, Vec<PathBuf>)>;
    static INVENTORIES: OnceLock<Mutex<Inventory>> = OnceLock::new();
    let users = home.join(super::USERS_DIR);
    let stamp = fingerprint(&users)?;
    let mut inventories = INVENTORIES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| "Context inventory lock unavailable")?;
    if let Some((previous, files)) = inventories
        .get(&users)
        .filter(|(previous, _)| previous == &stamp)
    {
        let _ = previous;
        return Ok(files.clone());
    }
    let mut files = Vec::new();
    if stamp.is_some() {
        for user in std::fs::read_dir(&users).map_err(|e| e.to_string())? {
            let user = user.map_err(|e| e.to_string())?;
            if user.file_type().map_err(|e| e.to_string())?.is_dir() {
                files.push(user.path().join("context-attachments.yml"));
            }
        }
    }
    if inventories.len() >= 64 {
        inventories.clear();
    }
    inventories.insert(users, (stamp, files.clone()));
    Ok(files)
}

/// Uses persisted registrations on a cold process, and metadata-invalidated caches
/// thereafter. Never selects an arbitrary principal when ownership is ambiguous.
pub(crate) fn location_for_path(
    home: &Path,
    path: &Path,
) -> Result<Option<RegisteredLocation>, String> {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("Context path contains parent traversal".into());
    }
    let mut cache = cache()
        .lock()
        .map_err(|_| "Context attachment registry lock unavailable")?;
    let mut found: Option<RegisteredLocation> = None;
    let files = scoped_registry(home, path)
        .map(|file| vec![file])
        .map(Ok)
        .unwrap_or_else(|| registry_files(home))?;
    for file in files {
        let registry = read_locked(&file, &mut cache)?;
        for attachment in registry
            .attachments
            .into_values()
            .filter(|a| path.starts_with(&a.root))
        {
            let principal = Principal::from(
                file.parent()
                    .and_then(Path::file_name)
                    .and_then(|s| s.to_str())
                    .ok_or("Invalid attachment namespace")?,
            );
            let candidate = RegisteredLocation {
                home: home.to_path_buf(),
                principal,
                attachment,
            };
            if let Some(previous) = found.as_ref() {
                if previous.attachment.root == candidate.attachment.root
                    && previous.principal != candidate.principal
                {
                    return Err("Context checkout is registered under multiple principals; select an explicit principal".into());
                }
                if previous.attachment.root.components().count()
                    > candidate.attachment.root.components().count()
                {
                    continue;
                }
            }
            found = Some(candidate);
        }
    }
    if let Some(location) = found.as_ref() {
        available_root(&location.attachment)?;
        // Check the nearest existing ancestor too: an in-checkout symlink must
        // never authorize an outside target, including a not-yet-created leaf.
        let mut ancestor = path.to_path_buf();
        loop {
            match ancestor.canonicalize() {
                Ok(real) if real.starts_with(&location.attachment.root) => {
                    if real
                        .strip_prefix(&location.attachment.root)
                        .map_err(|e| e.to_string())?
                        .components()
                        .any(|c| c.as_os_str() == ".git")
                    {
                        return Err(
                            "Git metadata in attached contexts is owned by your Git tools".into(),
                        );
                    }
                    break;
                }
                Ok(_) => {
                    return Err(
                        "Path escapes the registered context checkout through a symlink".into(),
                    )
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && ancestor.pop() => {}
                Err(e) => return Err(format!("Cannot resolve attached context path: {e}")),
            }
        }
    }
    Ok(found)
}

/// Path-only compatibility consumers normally run after explicit root resolution.
/// Their cold-start lookup still consults the persisted application home.
pub(crate) fn location_for_known_path(path: &Path) -> Result<Option<RegisteredLocation>, String> {
    // An explicit managed layout carries its authority: do not scan unrelated accounts.
    for ancestor in path.ancestors() {
        if ancestor.file_name().is_some_and(|n| n == super::ORGS_DIR) {
            if let Some(home) = ancestor
                .parent()
                .and_then(Path::parent)
                .filter(|p| p.file_name().is_some_and(|n| n == super::USERS_DIR))
                .and_then(Path::parent)
            {
                return location_for_path(home, path);
            }
        }
    }

    let homes: std::collections::BTreeSet<PathBuf> = {
        let cache = cache()
            .lock()
            .map_err(|_| "Context attachment registry lock unavailable")?;
        cache
            .iter()
            .filter(|(_, cached)| {
                cached
                    .value
                    .as_ref()
                    .is_ok_and(|r| r.attachments.values().any(|a| path.starts_with(&a.root)))
            })
            .filter_map(|(file, _)| file.parent()?.parent()?.parent().map(Path::to_path_buf))
            .collect()
    };
    let known: Vec<PathBuf> = known_roots()
        .lock()
        .map_err(|_| "Context attachment registry lock unavailable")?
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .filter_map(|(_, file)| file.parent()?.parent()?.parent().map(Path::to_path_buf))
        .collect();
    let mut homes: std::collections::BTreeSet<PathBuf> = homes
        .into_iter()
        .chain(known)
        .map(|home| home.canonicalize().unwrap_or(home))
        .collect();
    if let Some(home) = crate::commands::paths::codesense_home_dir() {
        homes.insert(home.canonicalize().unwrap_or(home));
    }
    let mut found = None;
    for home in homes {
        if let Some(location) = location_for_path(&home, path)? {
            if found.is_some() {
                return Err("Context path has ambiguous registration namespaces".into());
            }
            found = Some(location);
        }
    }
    Ok(found)
}

pub(crate) fn path_is_registered_content(home: &Path, path: &Path) -> Result<bool, String> {
    let Some(location) = location_for_path(home, path)? else {
        return Ok(false);
    };
    let parts: Vec<_> = path
        .strip_prefix(&location.attachment.root)
        .map_err(|e| e.to_string())?
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let Some(team) = parts.first() else {
        return Ok(false);
    };
    if team.is_empty() || (team.starts_with('_') || team.starts_with('.')) {
        return Ok(false);
    }
    Ok(parts.get(1) == Some(&"content")
        || (parts.get(1) == Some(&"repos")
            && (parts.get(4) == Some(&"content") || parts.get(5) == Some(&"content"))))
}

pub(crate) fn contains_for_identity(
    home: &Path,
    principal: &Principal,
    slug: &str,
    canonical: &Path,
) -> Result<bool, String> {
    let Some(attachment) = registered(home, principal, slug)? else {
        return Ok(false);
    };
    Ok(canonical.starts_with(available_root(&attachment)?))
}

#[cfg(test)]
mod consumer_tests {
    use super::*;

    /// A path-only lookup still finds an attached checkout after many other
    /// registries churned through the bounded cache (other homes, other tests).
    #[test]
    fn a_path_only_lookup_survives_registry_cache_churn() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = super::tests::checkout(temp.path(), "churn-folder", "acme/churn");
        let who = Principal::local_guest();
        attach(&home, &who, &root).unwrap();
        assert_eq!(
            location_for_known_path(&root)
                .unwrap()
                .unwrap()
                .attachment
                .slug,
            "acme/churn"
        );
        for n in 0..80 {
            let other = temp.path().join(format!("other-{n}"));
            let _ = location_for_path(&other, &other.join("users/1/orgs/x/y"));
        }
        assert_eq!(
            location_for_known_path(&root)
                .unwrap()
                .unwrap()
                .attachment
                .slug,
            "acme/churn"
        );
        assert!(location_for_known_path(&root.join("default/content/x.md"))
            .unwrap()
            .is_some());
    }

    #[test]
    fn attached_consumer_paths_survive_restart_and_reserved_ancestor_names() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root =
            super::tests::checkout(temp.path(), "content/repos/chosen-folder", "acme/context");
        let who = Principal::local_guest();
        let content = root.join("default/content");
        std::fs::create_dir_all(&content).unwrap();
        let story = content.join("story.md");
        std::fs::write(&story, "---\nstatus: draft\n---\nowner's saved edits\n").unwrap();
        attach(&home, &who, &root).unwrap();
        cache().lock().unwrap().remove(&registry_path(&home, &who));
        assert_eq!(
            crate::commands::install::parse_user_id_and_org_slug_anchored(
                home.to_str().unwrap(),
                root.to_str().unwrap()
            ),
            Some((crate::domain::LOCAL_GUEST_USER_ID, "acme/context".into()))
        );
        assert_eq!(
            super::super::derive_org_root(story.to_str().unwrap()),
            root.to_str()
        );
        assert_eq!(
            crate::install::capture::parse_context_repo_parts(story.to_str().unwrap()),
            Some(("default".into(), String::new(), String::new()))
        );
        assert!(!crate::content_engine::is_installed_path(&story, &home));
        assert!(crate::content_engine::is_installed_path(
            &root.join("default/install/secret.md"),
            &home
        ));
        assert!(crate::content_engine::is_installed_path(
            &root.join("default/content/../install/secret.md"),
            &home
        ));
        assert_eq!(
            crate::commands::dashboard_launch::resolve_managed_team_content_root(
                &home,
                &who,
                "acme/context",
                "default"
            )
            .unwrap(),
            content
        );
        assert_eq!(
            crate::domain::user_path::extract_user_id(story.to_str().unwrap()),
            Some(who.into_string())
        );
        assert_eq!(
            std::fs::read_to_string(story).unwrap(),
            "---\nstatus: draft\n---\nowner's saved edits\n"
        );
    }

    #[test]
    fn reverse_lookup_refuses_symlink_escape_and_ambiguous_principals() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = super::tests::checkout(temp.path(), "checkout", "acme/context");
        let who = Principal::local_guest();
        attach(&home, &who, &root).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
            assert!(location_for_path(&home, &root.join("escape/new.md")).is_err());
        }
        let other = Principal::from(42_u64);
        assert!(attach(&home, &other, &root).is_err());
        // Simulate an older, already-ambiguous registry; new admissions refuse it.
        let other_registry = registry_path(&home, &other);
        std::fs::create_dir_all(other_registry.parent().unwrap()).unwrap();
        std::fs::copy(registry_path(&home, &who), &other_registry).unwrap();
        assert!(location_for_path(&home, &root)
            .unwrap_err()
            .contains("multiple principals"));
        assert_eq!(
            super::super::path_to_org_root(&home, &who, "acme/context").unwrap(),
            root
        );
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn attached_context_identity_changes_are_unavailable_and_repairable() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = tests::checkout(temp.path(), "checkout", "acme/context");
        let who = Principal::local_guest();
        let a = attach(&home, &who, &root).unwrap();
        let original = std::fs::read(root.join("org.yml")).unwrap();
        for bytes in [
            b"broken: [".as_slice(),
            b"slug: other/context\ndisplay_name: Changed\ncreated_at: 2026-10-01T00:00:00Z\n"
                .as_slice(),
        ] {
            std::fs::write(root.join("org.yml"), bytes).unwrap();
            assert!(available_root(&a).is_err());
            assert_eq!(list(&home, &who).unwrap().len(), 1);
        }
        std::fs::write(root.join("org.yml"), original).unwrap();
        assert_eq!(available_root(&a).unwrap(), root);
        std::fs::rename(root.join(".git"), root.join("git-preserved")).unwrap();
        assert!(available_root(&a).is_err());
        detach(&home, &who, &a.slug).unwrap();
        assert!(root.join("git-preserved").exists());
    }
    #[test]
    fn attached_context_cross_principal_and_relative_admission_preserve_registry() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = tests::checkout(temp.path(), "checkout", "acme/context");
        let guest = Principal::local_guest();
        attach(&home, &guest, &root).unwrap();
        let before = std::fs::read(registry_path(&home, &guest)).unwrap();
        let signed = Principal::from(42_u64);
        assert!(attach(&home, &signed, &root)
            .unwrap_err()
            .contains("another principal"));
        assert!(!registry_path(&home, &signed).exists());
        assert_eq!(before, std::fs::read(registry_path(&home, &guest)).unwrap());
        assert!(attach(&home, &signed, Path::new("relative"))
            .unwrap_err()
            .contains("absolute"));
        let managed = super::super::path_to_managed_org_root(&home, &signed, "other/context");
        std::fs::write(registry_path(&home, &guest), "broken: [").unwrap();
        assert!(location_for_path(&home, &managed).unwrap().is_none());
    }
    #[test]
    fn attached_context_watch_admission_is_revoked_on_detach() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = tests::checkout(temp.path(), "checkout", "acme/context");
        let who = Principal::local_guest();
        attach(&home, &who, &root).unwrap();
        let content = root.join("default/content");
        require_context_path(&home, &content).unwrap();
        detach(&home, &who, "acme/context").unwrap();
        assert!(require_context_path(&home, &content).is_err());
        assert!(root.join("org.yml").exists());
    }
    #[test]
    fn attached_context_collision_rows_remain_detachable() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let root = tests::checkout(temp.path(), "checkout", "acme/context");
        let who = Principal::local_guest();
        attach(&home, &who, &root).unwrap();
        std::fs::create_dir_all(super::super::path_to_managed_org_root(
            &home,
            &who,
            "acme/context",
        ))
        .unwrap();
        let rows = crate::commands::context_attachments::attachment_summaries(&home, &who).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].availability_error.is_some());
        detach(&home, &who, "acme/context").unwrap();
        assert!(root.join("org.yml").exists());
    }
    #[test]
    fn attached_context_cold_bootstrap_persists_only_successful_guest_registration() {
        use crate::commands::{
            context_attachments::attach_with_session, guest_session::GuestSessionState,
        };
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let who = Principal::local_guest();
        let state = GuestSessionState::default();
        assert!(attach_with_session(&home, &who, &temp.path().join("missing"), &state).is_err());
        assert!(!state.is_active());
        assert!(!GuestSessionState::load(Some(&home)).is_active());
        let root = tests::checkout(temp.path(), "checkout", "acme/context");
        attach_with_session(&home, &who, &root, &state).unwrap();
        assert!(state.is_active());
        assert!(GuestSessionState::load(Some(&home)).is_active());
        cache().lock().unwrap().remove(&registry_path(&home, &who));
        let rows = crate::commands::org::discover_local_orgs_impl(&home, &who).unwrap();
        assert_eq!(rows[0].org_root, root.to_string_lossy());
    }
}

/// Path-only watch admission cannot resurrect an externally detached root.
pub(crate) fn require_context_path(home: &Path, path: &Path) -> Result<(), String> {
    if location_for_path(home, path)?.is_some() || scoped_registry(home, path).is_some() {
        return Ok(());
    }
    Err("Context path is no longer registered; refresh the workspace list".into())
}
