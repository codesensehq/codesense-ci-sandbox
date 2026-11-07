// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single entry in the server registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub port: u16,
    pub pid: u32,
    pub started_at: DateTime<Utc>,
}

/// The full registry: PID (as string key) -> ServerEntry.
pub type Registry = HashMap<String, ServerEntry>;

/// Returns `~/.codesense/servers.json`.
pub fn registry_path() -> PathBuf {
    crate::commands::paths::codesense_home_dir()
        .expect("could not determine home directory")
        .join("servers.json")
}

/// Register this instance (current PID) in the server registry.
///
/// Acquires an exclusive lock, prunes stale entries, inserts the new
/// entry, and writes back.
pub fn register(port: u16) -> std::io::Result<()> {
    register_at(&registry_path(), port, std::process::id())
}

/// Deregister this instance (current PID) from the server registry.
///
/// Acquires an exclusive lock, removes the entry for this PID, and
/// writes back. No-op if the PID is not in the registry.
pub fn deregister() -> std::io::Result<()> {
    deregister_at(&registry_path(), std::process::id())
}

/// `true` when this instance (current PID) is the only live one in the registry.
/// Only then does it own the port the next start takes back (`http.last-port`).
pub fn sole_live_instance() -> bool {
    sole_live_instance_at(&registry_path(), std::process::id()).unwrap_or(false)
}

fn sole_live_instance_at(path: &Path, pid: u32) -> std::io::Result<bool> {
    let mut sole = false;
    with_locked_registry(path, |registry| {
        prune_stale(registry);
        sole = registry.len() == 1 && registry.contains_key(&pid.to_string());
    })?;
    Ok(sole)
}

/// Remove entries whose PID is no longer alive.
pub fn prune_stale(registry: &mut Registry) {
    registry.retain(|_, entry| is_pid_alive(entry.pid));
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
fn read_registry_from(path: &Path) -> Registry {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Registry::new(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

/// Open (or create) the registry file, acquire an exclusive `flock`,
/// read-modify-write under the lock.
fn with_locked_registry<F>(path: &Path, f: F) -> std::io::Result<()>
where
    F: FnOnce(&mut Registry),
{
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;

    // Acquire exclusive advisory lock (blocks until available).
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // Read current content.
    let mut content = String::new();
    file.read_to_string(&mut content)?;

    let mut registry: Registry = if content.is_empty() {
        Registry::new()
    } else {
        serde_json::from_str(&content).unwrap_or_default()
    };

    // Apply caller's modification.
    f(&mut registry);

    // Write back: truncate, seek to start, serialize.
    let json = serde_json::to_string_pretty(&registry).map_err(std::io::Error::other)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;

    // Lock released when `file` is dropped.
    Ok(())
}

fn register_at(path: &Path, port: u16, pid: u32) -> std::io::Result<()> {
    with_locked_registry(path, |registry| {
        prune_stale(registry);
        registry.insert(
            pid.to_string(),
            ServerEntry {
                port,
                pid,
                started_at: Utc::now(),
            },
        );
    })
}

fn deregister_at(path: &Path, pid: u32) -> std::io::Result<()> {
    with_locked_registry(path, |registry| {
        registry.remove(&pid.to_string());
    })
}

/// Check whether a PID is still alive using `kill(pid, 0)`.
///
/// Returns `false` for PID 0 (kernel) and PIDs exceeding `i32::MAX`
/// (invalid for `kill(2)`).
fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM means the process exists but we lack permission to signal it.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sole_live_instance_only_when_no_other_live_entry() {
        let (_dir, path) = temp_registry();
        let me = std::process::id();
        assert!(!sole_live_instance_at(&path, me).unwrap(), "unregistered");
        register_at(&path, 54129, me).unwrap();
        assert!(sole_live_instance_at(&path, me).unwrap());
        // A stale entry (dead pid) does not count.
        register_at(&path, 1111, u32::MAX - 1).unwrap();
        assert!(sole_live_instance_at(&path, me).unwrap());
        // Another live instance (the test runner's parent stands in) does.
        let other = unsafe { libc::getppid() } as u32;
        register_at(&path, 58879, other).unwrap();
        assert!(!sole_live_instance_at(&path, me).unwrap());
    }

    fn temp_registry() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.json");
        (dir, path)
    }

    #[test]
    fn test_registry_path_ends_with_expected_suffix() {
        let path = registry_path();
        assert!(path.ends_with(".codesense/servers.json"));
    }

    #[test]
    fn test_registry_path_is_absolute() {
        assert!(registry_path().is_absolute());
    }

    #[test]
    fn test_read_registry_missing_file() {
        let (_dir, path) = temp_registry();
        let reg = read_registry_from(&path);
        assert!(reg.is_empty());
    }

    #[test]
    fn test_read_registry_corrupt_json() {
        let (_dir, path) = temp_registry();
        fs::write(&path, "this is not json{{{").unwrap();
        let reg = read_registry_from(&path);
        assert!(reg.is_empty());
    }

    #[test]
    fn test_register_creates_file_and_entry() {
        let (_dir, path) = temp_registry();
        let pid = std::process::id();
        register_at(&path, 12345, pid).unwrap();

        let reg = read_registry_from(&path);
        assert_eq!(reg.len(), 1);
        let entry = reg.get(&pid.to_string()).unwrap();
        assert_eq!(entry.port, 12345);
        assert_eq!(entry.pid, pid);
    }

    #[test]
    fn test_two_registers_produce_two_entries() {
        let (_dir, path) = temp_registry();
        let our_pid = std::process::id();
        // PID 1 (init/launchd) is always alive.
        register_at(&path, 12345, 1).unwrap();
        register_at(&path, 54321, our_pid).unwrap();

        let reg = read_registry_from(&path);
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.get("1").unwrap().port, 12345);
        assert_eq!(reg.get(&our_pid.to_string()).unwrap().port, 54321);
    }

    #[test]
    fn test_deregister_removes_only_own_entry() {
        let (_dir, path) = temp_registry();
        let our_pid = std::process::id();
        register_at(&path, 12345, 1).unwrap();
        register_at(&path, 54321, our_pid).unwrap();

        deregister_at(&path, our_pid).unwrap();

        let reg = read_registry_from(&path);
        assert_eq!(reg.len(), 1);
        assert!(!reg.contains_key(&our_pid.to_string()));
        assert!(reg.contains_key("1"));
    }

    #[test]
    fn test_prune_stale_removes_dead_pids() {
        let mut registry = Registry::new();
        // i32::MAX is a valid pid_t value but guaranteed not to be running.
        registry.insert(
            "2147483647".to_string(),
            ServerEntry {
                port: 12345,
                pid: i32::MAX as u32,
                started_at: Utc::now(),
            },
        );

        prune_stale(&mut registry);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_prune_stale_keeps_live_pids() {
        let mut registry = Registry::new();
        let pid = std::process::id();
        registry.insert(
            pid.to_string(),
            ServerEntry {
                port: 12345,
                pid,
                started_at: Utc::now(),
            },
        );

        prune_stale(&mut registry);
        assert_eq!(registry.len(), 1);
    }

    // -- Integration tests (4.1–4.4) ------------------------------------------

    #[test]
    fn test_integration_two_instances_both_present() {
        let (_dir, path) = temp_registry();
        let our_pid = std::process::id();
        // PID 1 (init/launchd) is always alive.
        register_at(&path, 11111, 1).unwrap();
        register_at(&path, 22222, our_pid).unwrap();

        // Read the on-disk file directly and verify both entries.
        let raw = fs::read_to_string(&path).unwrap();
        let reg: Registry = serde_json::from_str(&raw).unwrap();
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.get("1").unwrap().port, 11111);
        assert_eq!(reg.get(&our_pid.to_string()).unwrap().port, 22222);
    }

    #[test]
    fn test_integration_deregister_one_other_survives() {
        let (_dir, path) = temp_registry();
        let our_pid = std::process::id();
        register_at(&path, 11111, 1).unwrap();
        register_at(&path, 22222, our_pid).unwrap();

        deregister_at(&path, our_pid).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let reg: Registry = serde_json::from_str(&raw).unwrap();
        assert_eq!(reg.len(), 1);
        assert!(!reg.contains_key(&our_pid.to_string()));
        assert_eq!(reg.get("1").unwrap().port, 11111);
    }

    #[test]
    fn test_integration_register_prunes_stale_from_file() {
        let (_dir, path) = temp_registry();
        // Pre-seed registry with a dead PID.
        let stale_pid = i32::MAX as u32;
        let mut seed = Registry::new();
        seed.insert(
            stale_pid.to_string(),
            ServerEntry {
                port: 65535,
                pid: stale_pid,
                started_at: Utc::now(),
            },
        );
        fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        // Register a live instance — stale entry should be pruned.
        let our_pid = std::process::id();
        register_at(&path, 12345, our_pid).unwrap();

        let reg = read_registry_from(&path);
        assert_eq!(reg.len(), 1);
        assert!(!reg.contains_key(&stale_pid.to_string()));
        assert_eq!(reg.get(&our_pid.to_string()).unwrap().port, 12345);
    }

    #[test]
    fn test_integration_concurrent_writes_both_survive() {
        let (_dir, path) = temp_registry();
        let path_a = path.clone();
        let path_b = path.clone();
        let our_pid = std::process::id();

        // Spawn two threads that register simultaneously.
        // flock serialises the writes; both entries must be present.
        let handle_a = std::thread::spawn(move || {
            register_at(&path_a, 11111, 1).unwrap();
        });
        let handle_b = std::thread::spawn(move || {
            register_at(&path_b, 22222, our_pid).unwrap();
        });

        handle_a.join().unwrap();
        handle_b.join().unwrap();

        let reg = read_registry_from(&path);
        assert_eq!(reg.len(), 2);
        assert_eq!(reg.get("1").unwrap().port, 11111);
        assert_eq!(reg.get(&our_pid.to_string()).unwrap().port, 22222);
    }
}
