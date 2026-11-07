// SPDX-License-Identifier: MIT
//! Pipelined bundle staging. Chunks land at their own offsets, in any
//! order, with no flush; each is recorded with its SHA-256. The upload is
//! flushed exactly once, at seal.
//!
//! What the machine "holds" is an in-memory bitmap for this server process.
//! After a restart (or for an upload this process never saw) it is recomputed
//! from disk: a chunk counts only when its recorded hash matches the bytes
//! staged at its offset now. An ack sent before a crash is never trusted: the
//! unflushed bytes or record may not have survived.
use codesense_pty_core::remote_workspace::{chunk_range, ReceivedChunks, PIPELINED_CHUNK_BYTES};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Where one upload's bytes and per-chunk records live.
pub(crate) trait ChunkStore {
    fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), String>;
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>, String>;
    fn write_record(&self, index: u32, hash: &[u8; 32]) -> Result<(), String>;
    /// All zeroes when nothing was recorded.
    fn read_record(&self, index: u32) -> Result<[u8; 32], String>;
    /// Make the staged bytes durable.
    fn sync(&self) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub(crate) struct UploadState {
    pub(crate) received: ReceivedChunks,
    pub(crate) sealed: bool,
    /// Sealed and its whole-bundle digest checked by this process.
    pub(crate) verified: bool,
}

fn table() -> &'static Mutex<HashMap<String, UploadState>> {
    static TABLE: OnceLock<Mutex<HashMap<String, UploadState>>> = OnceLock::new();
    TABLE.get_or_init(Mutex::default)
}

fn with_table<T>(f: impl FnOnce(&mut HashMap<String, UploadState>) -> T) -> T {
    f(&mut table().lock().unwrap_or_else(|poison| poison.into_inner()))
}

/// Forget every in-memory upload, as a fresh server process starts.
#[cfg(test)]
pub(crate) fn forget_all() {
    with_table(HashMap::clear);
}

/// Forget one upload once its staging is gone.
pub(crate) fn forget(key: &str) {
    with_table(|table| {
        table.remove(key);
    });
}

/// The chunks whose recorded hash matches the bytes on disk now.
pub(crate) fn recompute(store: &dyn ChunkStore, size: u64) -> Result<ReceivedChunks, String> {
    let mut received = ReceivedChunks::for_size(size);
    for index in 0..received.count() {
        let record = store.read_record(index)?;
        if record == [0; 32] {
            continue;
        }
        let (offset, len) = chunk_range(size, index).ok_or("bundle chunk outside range")?;
        if <[u8; 32]>::from(Sha256::digest(store.read_at(offset, len)?)) == record {
            received.insert(index);
        }
    }
    Ok(received)
}

/// This upload's state: this process's own, else recomputed from disk.
/// Called with the upload's staging lock held.
pub(crate) fn state(key: &str, store: &dyn ChunkStore, size: u64) -> Result<UploadState, String> {
    if let Some(state) = with_table(|table| table.get(key).cloned()) {
        if state.received.fits(size) {
            return Ok(state);
        }
    }
    let state = UploadState {
        received: recompute(store, size)?,
        sealed: false,
        verified: false,
    };
    with_table(|table| table.insert(key.into(), state.clone()));
    Ok(state)
}

fn parse_hash(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid bundle chunk hash".into());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "invalid bundle chunk hash")?;
    }
    Ok(out)
}

/// Stage chunk `index` of a `size`-byte upload (no flush). A repeat of a held
/// chunk is acknowledged again; a repeat with other bytes is refused.
pub(crate) fn stage(
    key: &str,
    store: &dyn ChunkStore,
    size: u64,
    index: u32,
    sha256: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let (offset, len) = chunk_range(size, index).ok_or("bundle chunk outside declared range")?;
    if bytes.len() != len || len > PIPELINED_CHUNK_BYTES {
        return Err("bundle chunk has the wrong length".into());
    }
    let hash = parse_hash(sha256)?;
    if <[u8; 32]>::from(Sha256::digest(bytes)) != hash {
        return Err("bundle chunk does not match its hash".into());
    }
    let mut state = state(key, store, size)?;
    if state.received.contains(index) {
        return if store.read_record(index)? == hash {
            Ok(())
        } else {
            Err("bundle retry changed prior bytes".into())
        };
    }
    if state.sealed {
        return Err("bundle upload is already sealed".into());
    }
    store.write_at(offset, bytes)?;
    store.write_record(index, &hash)?;
    state.received.insert(index);
    with_table(|table| table.insert(key.into(), state));
    Ok(())
}

/// Seal a complete upload: its one flush. Sealing again flushes nothing.
pub(crate) fn seal(key: &str, store: &dyn ChunkStore, size: u64) -> Result<(), String> {
    let mut state = state(key, store, size)?;
    if !state.received.is_complete() {
        return Err("bundle transfer is incomplete; resume original request".into());
    }
    if !state.sealed {
        store.sync()?;
        state.sealed = true;
        with_table(|table| table.insert(key.into(), state));
    }
    Ok(())
}

/// Seal, then check the whole bundle's SHA-256 once per process.
pub(crate) fn verify(
    key: &str,
    store: &dyn ChunkStore,
    size: u64,
    digest: &str,
) -> Result<(), String> {
    seal(key, store, size)?;
    let mut state = state(key, store, size)?;
    if state.verified {
        return Ok(());
    }
    let mut hash = Sha256::new();
    let mut offset = 0;
    while offset < size {
        let len = (size - offset).min(1024 * 1024) as usize;
        hash.update(store.read_at(offset, len)?);
        offset += len as u64;
    }
    if format!("{:x}", hash.finalize()) != digest {
        // Never wedge on a bad upload: what this process believed is dropped
        // and the held set recomputed from disk. If every chunk still matches
        // its own hash, the chunks themselves were wrong for this bundle, so
        // the whole upload starts over.
        forget(key);
        let received = recompute(store, size)?;
        if received.is_complete() {
            for index in 0..received.count() {
                store.write_record(index, &[0; 32])?;
            }
        }
        forget(key);
        return Err("bundle digest mismatch; the upload restarts".into());
    }
    state.verified = true;
    with_table(|table| table.insert(key.into(), state));
    Ok(())
}

/// The real store: `bundle-<key>` (the bytes, at full length) and
/// `bundle-<key>.chunks` (32 bytes per chunk), in the private receipt
/// directory. Neither is flushed until seal.
pub(crate) struct FileStore {
    pub(crate) data: std::fs::File,
    pub(crate) records: std::fs::File,
}

impl ChunkStore for FileStore {
    fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), String> {
        use std::os::unix::fs::FileExt;
        self.data
            .write_all_at(bytes, offset)
            .map_err(|_| "bundle staging write failed".into())
    }
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        use std::os::unix::fs::FileExt;
        let mut bytes = vec![0; len];
        self.data
            .read_exact_at(&mut bytes, offset)
            .map_err(|_| "bundle staging read failed")?;
        Ok(bytes)
    }
    fn write_record(&self, index: u32, hash: &[u8; 32]) -> Result<(), String> {
        use std::os::unix::fs::FileExt;
        self.records
            .write_all_at(hash, index as u64 * 32)
            .map_err(|_| "bundle staging record failed".into())
    }
    fn read_record(&self, index: u32) -> Result<[u8; 32], String> {
        use std::os::unix::fs::FileExt;
        let mut hash = [0u8; 32];
        match self.records.read_exact_at(&mut hash, index as u64 * 32) {
            Ok(()) => Ok(hash),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok([0; 32]),
            Err(_) => Err("bundle staging record unreadable".into()),
        }
    }
    fn sync(&self) -> Result<(), String> {
        super::durable::sync(&self.data).map_err(|_| "bundle staging flush failed".into())
    }
}

#[cfg(test)]
#[path = "bundle_staging_tests.rs"]
mod tests;
