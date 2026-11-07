// SPDX-License-Identifier: MIT
//! Durable task correlation; a consumed start is never automatically replayed.
use super::StartParams;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};
const MAX_RECEIPT: usize = 2 * 1024 * 1024;
pub(super) const MAX_OUTPUT: usize = 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub params: StartParams,
    pub team: String,
    pub workspace: String,
    pub machine: String,
    pub server: String,
    pub account: serde_json::Value,
    pub status: String,
    pub text: String,
    pub message: Option<String>,
}
pub(super) struct Store {
    pub dir: PathBuf,
    _lock: std::fs::File,
}
impl Store {
    pub(super) fn open(root: &Path, id: uuid::Uuid) -> Result<Self, String> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(|_| "task store unavailable")?;
        let dir = root.join(id.to_string());
        let existed = dir.exists();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .map_err(|_| "task directory unavailable")?;
        let meta = std::fs::symlink_metadata(&dir).map_err(|_| "task directory unavailable")?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
            return Err("unsafe task directory".into());
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join("lock"))
            .map_err(|_| "task lock unavailable")?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("task busy".into());
        }
        if existed && !dir.join("receipt.json").exists() {
            return Err("task receipt unavailable; start cannot be reconstructed".into());
        }
        Ok(Self { dir, _lock: lock })
    }
    pub(super) fn load(&self) -> Result<Option<Receipt>, String> {
        let mut file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.dir.join("receipt.json"))
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("task receipt unavailable".into()),
        };
        let meta = file.metadata().map_err(|_| "task receipt unavailable")?;
        if !meta.is_file() || meta.nlink() != 1 || meta.len() > MAX_RECEIPT as u64 {
            return Err("unsafe task receipt".into());
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_RECEIPT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "task receipt unavailable")?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "task receipt corrupt; replay forbidden".into())
    }
    pub(super) fn save(&self, receipt: &Receipt) -> Result<(), String> {
        let bytes = serde_json::to_vec(receipt).map_err(|_| "task receipt encoding failed")?;
        if bytes.len() > MAX_RECEIPT {
            return Err("task receipt exceeds bound".into());
        }
        let path = self.dir.join(format!("pending-{}", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| "task receipt unavailable")?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "task receipt persistence unknown")?;
        std::fs::rename(&path, self.dir.join("receipt.json"))
            .map_err(|_| "task receipt persistence unknown")?;
        std::fs::File::open(&self.dir)
            .and_then(|f| f.sync_all())
            .map_err(|_| "task receipt persistence unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consumed_intent_cannot_be_recreated_after_loss_or_corruption() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let store = Store::open(root.path(), id).unwrap();
        assert!(store.load().unwrap().is_none());
        assert!(Store::open(root.path(), id).is_err());
        drop(store);
        assert!(Store::open(root.path(), id).is_err());
        std::fs::write(
            root.path().join(id.to_string()).join("receipt.json"),
            b"invalid",
        )
        .unwrap();
        assert!(Store::open(root.path(), id).unwrap().load().is_err());
    }
    #[test]
    fn journal_roundtrips_exact_intent_and_does_not_follow_receipt_links() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let store = Store::open(root.path(), id).unwrap();
        let params = StartParams {
            request_id: id.to_string(),
            workspace_handle: "bound".into(),
            provider: "codex".into(),
            account_id: None,
            account_generation: None,
            prompt: "remote task".into(),
        };
        let receipt = Receipt {
            params,
            team: "org:team".into(),
            workspace: "ws".into(),
            machine: "m".into(),
            server: "s".into(),
            account: serde_json::json!({"source":"system-default"}),
            status: "starting".into(),
            text: String::new(),
            message: None,
        };
        store.save(&receipt).unwrap();
        assert!(store.load().unwrap().unwrap().params == receipt.params);
        std::fs::remove_file(store.dir.join("receipt.json")).unwrap();
        let target = root.path().join("other");
        std::fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, store.dir.join("receipt.json")).unwrap();
        assert!(store.load().is_err());
    }
}
