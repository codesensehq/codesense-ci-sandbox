// SPDX-License-Identifier: MIT
use crate::domain::CredentialRef;
use crate::shared::yaml_io::{read_yaml_or_default, write_yaml_atomic, YamlIoError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrgBindings {
    #[serde(default)]
    pub bindings: BTreeMap<String, CredentialRef>,
}

pub fn normalize_org_slug(slug: &str) -> String {
    slug.trim().to_ascii_lowercase()
}

pub fn read_org_bindings(path: &Path) -> Result<OrgBindings, YamlIoError> {
    read_yaml_or_default(path)
}

fn path_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

#[allow(dead_code)] // production API for the adjacent 10c rebind/clear surface
pub fn set_org_binding(
    path: &Path,
    org_slug: &str,
    binding: Option<CredentialRef>,
) -> Result<(), YamlIoError> {
    let lock = path_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = read_org_bindings(path)?;
    let slug = normalize_org_slug(org_slug);
    match binding {
        Some(binding) => {
            store.bindings.insert(slug, binding);
        }
        None => {
            store.bindings.remove(&slug);
        }
    }
    write_yaml_atomic(path, &store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn binding_roundtrips_and_clear_is_idempotent() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("org_bindings.yml");
        let expected = CredentialRef::github(42);

        set_org_binding(&path, " Acme ", Some(expected.clone())).unwrap();
        assert_eq!(
            read_org_bindings(&path).unwrap().bindings.get("acme"),
            Some(&expected)
        );

        set_org_binding(&path, "ACME", None).unwrap();
        set_org_binding(&path, "acme", None).unwrap();
        assert!(read_org_bindings(&path).unwrap().bindings.is_empty());
    }

    #[test]
    fn corrupt_file_is_not_overwritten() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("org_bindings.yml");
        let corrupt = b"bindings: [[[";
        std::fs::write(&path, corrupt).unwrap();

        assert!(set_org_binding(&path, "acme", Some(CredentialRef::github(7))).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
    }

    #[test]
    fn concurrent_updates_to_one_store_preserve_every_binding() {
        let tmp = tempdir().unwrap();
        let path = Arc::new(tmp.path().join("org_bindings.yml"));
        let start = Arc::new(std::sync::Barrier::new(17));
        let mut threads = Vec::new();
        for id in 0..16 {
            let path = Arc::clone(&path);
            let start = Arc::clone(&start);
            threads.push(std::thread::spawn(move || {
                start.wait();
                set_org_binding(
                    &path,
                    &format!("owner/repo-{id}"),
                    Some(CredentialRef::github(id)),
                )
                .unwrap();
            }));
        }
        start.wait();
        for thread in threads {
            thread.join().unwrap();
        }

        let bindings = read_org_bindings(&path).unwrap().bindings;
        assert_eq!(bindings.len(), 16);
        for id in 0..16 {
            assert_eq!(
                bindings.get(&format!("owner/repo-{id}")),
                Some(&CredentialRef::github(id))
            );
        }
    }
}
