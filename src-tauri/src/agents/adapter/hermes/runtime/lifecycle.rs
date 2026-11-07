// SPDX-License-Identifier: MIT
//! Stop only a verified home-owned fallback gateway; reclaim unused prepared seeds.
use super::*;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

pub(crate) fn teardown_home(home: &Path) {
    adapter(AgentCli::Hermes).teardown_home(home, false);
}

pub(crate) fn sweep(scopes: &Path) {
    let Some(home) = crate::commands::paths::codesense_home_dir() else {
        return;
    };
    sweep_in(&home.join("runtimes/hermes"), scopes);
}

fn sweep_in(root: &Path, scopes: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        if !name.to_str().is_some_and(|name| {
            name.len() == 64
                && Iterator::all(&mut str::bytes(name), |byte| byte.is_ascii_hexdigit())
        }) {
            continue;
        }
        let Ok(path) = fs::DirEntry::path(&entry).canonicalize() else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(lock) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))
        else {
            continue;
        };
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        // Scan under the same lock as prepare. Every remaining link protects its runtime,
        // including live sessions, unknown liveness and scopes owned by another instance.
        let referenced = (|| -> io::Result<bool> {
            for scope in fs::read_dir(scopes)? {
                let scope = fs::DirEntry::path(&scope?);
                for name in [version_cache::RUNTIME_LINK, "tools"] {
                    match fs::read_link(scope.join(name)) {
                        Ok(target) => {
                            let target = if target.is_absolute() {
                                target
                            } else {
                                scope.join(target)
                            };
                            if target.canonicalize()?.starts_with(&path) {
                                return Ok(true);
                            }
                        }
                        Err(e) if std::io::Error::kind(&e) == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            Ok(false)
        })();
        if matches!(referenced, Ok(false)) {
            // The sibling lock inode is intentionally retained for concurrent preparers.
            if make_writable(&path).is_ok() {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sweep_keeps_every_remaining_link_and_the_prepare_lock_inode() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("runtimes");
        let scopes = tmp.path().join("scopes");
        let runtime = root.join("a".repeat(64));
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(scopes.join("foreign-or-unknown")).unwrap();
        let lock_path = runtime.with_extension("lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        let inode = lock.metadata().unwrap().ino();
        unsafe {
            libc::flock(lock.as_raw_fd(), libc::LOCK_EX);
        }
        sweep_in(&root, &scopes);
        assert!(runtime.exists(), "concurrent prepare protects the seed");
        unsafe {
            libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
        }
        let link = scopes
            .join("foreign-or-unknown")
            .join(version_cache::RUNTIME_LINK);
        symlink(&runtime, &link).unwrap();
        sweep_in(&root, &scopes);
        assert!(
            runtime.exists(),
            "remaining scope protects the seed regardless of owner or liveness"
        );
        fs::remove_file(link).unwrap();
        sweep_in(&root, &scopes);
        assert!(!runtime.exists());
        assert_eq!(fs::metadata(lock_path).unwrap().ino(), inode);
    }
}
