// SPDX-License-Identifier: MIT
//! The narrow filesystem surface the manifest resolver reads content through.
//!
//! Resolution must touch only the paths an agent starts with, never the rest
//! of a team's content. Routing every content read through this trait keeps
//! that bound checkable: a test swaps in a reader that records each path it
//! is asked about.

use std::ffi::OsString;
use std::io;
use std::path::Path;

/// What a path is, without following a symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
    Symlink,
    /// A socket, FIFO or device: never content.
    Other,
}

/// One path's metadata, read without following a symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    /// The permission bits, `mode & 0o777`.
    pub mode: u32,
}

/// Reads content without following symlinks.
pub trait ContentReader {
    /// The node at `path` itself (a symlink is reported, not followed).
    fn node(&self, path: &Path) -> io::Result<Node>;
    /// The names directly under the directory `dir`, in no particular order.
    fn list(&self, dir: &Path) -> io::Result<Vec<OsString>>;
    /// The file at `path`, opened for streaming so a large file is never held
    /// in memory whole.
    fn open(&self, path: &Path) -> io::Result<Box<dyn io::Read>>;
    /// The size in bytes of the file at `path`, without following a symlink.
    fn file_len(&self, path: &Path) -> io::Result<u64> {
        std::fs::symlink_metadata(path).map(|m| m.len())
    }
}

/// The real filesystem.
pub struct FsContentReader;

impl ContentReader for FsContentReader {
    fn node(&self, path: &Path) -> io::Result<Node> {
        let meta = std::fs::symlink_metadata(path)?;
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            NodeKind::Symlink
        } else if file_type.is_dir() {
            NodeKind::Dir
        } else if file_type.is_file() {
            NodeKind::File
        } else {
            NodeKind::Other
        };
        Ok(Node {
            kind,
            mode: permission_bits(&meta),
        })
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        std::fs::read_dir(dir)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect()
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn io::Read>> {
        let file = open_no_follow(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no longer a regular file",
            ));
        }
        Ok(Box::new(file))
    }
}

/// Open `path` for reading, refusing a symlink in its last component, so a
/// file swapped for a link after it was checked is not followed, and without
/// blocking, so one swapped for a FIFO cannot hang the open.
#[cfg(unix)]
fn open_no_follow(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(unix)]
fn permission_bits(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn permission_bits(meta: &std::fs::Metadata) -> u32 {
    if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

/// The real filesystem, recording every path it is asked about.
#[cfg(test)]
#[derive(Default)]
pub struct CountingReader {
    touched: std::sync::Mutex<Vec<std::path::PathBuf>>,
    opened: std::sync::Mutex<Vec<std::path::PathBuf>>,
}

#[cfg(test)]
impl CountingReader {
    /// Every path statted, listed or read, in call order.
    pub fn touched(&self) -> Vec<std::path::PathBuf> {
        self.touched.lock().unwrap().clone()
    }

    /// Every file read, in call order.
    pub fn opened(&self) -> Vec<std::path::PathBuf> {
        self.opened.lock().unwrap().clone()
    }

    fn touch(&self, path: &Path) {
        self.touched.lock().unwrap().push(path.to_path_buf());
    }
}

#[cfg(test)]
impl ContentReader for CountingReader {
    fn node(&self, path: &Path) -> io::Result<Node> {
        self.touch(path);
        FsContentReader.node(path)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        self.touch(dir);
        FsContentReader.list(dir)
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn io::Read>> {
        self.touch(path);
        self.opened.lock().unwrap().push(path.to_path_buf());
        FsContentReader.open(path)
    }
}
