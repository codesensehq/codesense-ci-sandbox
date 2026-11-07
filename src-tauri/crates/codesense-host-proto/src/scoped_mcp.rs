// SPDX-License-Identifier: MIT
//! Private desktop launch grant. Not the remote process-lineage broker protocol.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
pub const VERSION: u32 = 1;
pub const MAX_HANDSHAKE: usize = 4096;
/// Stored only in the owned launch directory, never printed or placed on argv.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub version: u32,
    pub socket: PathBuf,
    pub capability: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub version: u32,
    pub capability: String,
    pub server: String,
}
impl Descriptor {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != VERSION
            || !self.socket.is_absolute()
            || self.capability.len() != 64
            || !self.capability.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid local MCP launch descriptor");
        }
        Ok(())
    }
}

#[cfg(unix)]
pub fn read_descriptor(path: &std::path::Path) -> Result<Descriptor, &'static str> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "local MCP launch descriptor unavailable")?;
    let meta = file
        .metadata()
        .map_err(|_| "local MCP descriptor metadata unavailable")?;
    // SAFETY: geteuid has no preconditions.
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() > MAX_HANDSHAKE as u64
    {
        return Err("local MCP descriptor must be an owned private regular file");
    }
    let mut bytes = Vec::new();
    file.take(MAX_HANDSHAKE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "local MCP descriptor read failed")?;
    if bytes.len() > MAX_HANDSHAKE {
        return Err("local MCP descriptor exceeds limit");
    }
    let descriptor: Descriptor =
        serde_json::from_slice(&bytes).map_err(|_| "invalid local MCP descriptor")?;
    descriptor.validate()?;
    Ok(descriptor)
}

/// Replace a launch endpoint without exposing a partially written capability to bridges.
#[cfg(unix)]
pub fn write_descriptor(path: &std::path::Path, descriptor: &Descriptor) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    descriptor.validate().map_err(str::to_owned)?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = path.with_extension(format!("{}.{serial}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(descriptor).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn descriptor_replacement_is_private_and_reads_the_new_endpoint() {
        let path = std::env::temp_dir().join(format!("scoped-mcp-{}.json", std::process::id()));
        let mut descriptor = Descriptor {
            version: VERSION,
            socket: "/tmp/first.sock".into(),
            capability: "a".repeat(64),
        };
        write_descriptor(&path, &descriptor).unwrap();
        assert_eq!(read_descriptor(&path).unwrap().socket, descriptor.socket);
        descriptor.socket = "/tmp/restarted.sock".into();
        descriptor.capability = "b".repeat(64);
        write_descriptor(&path, &descriptor).unwrap();
        let read = read_descriptor(&path).unwrap();
        assert_eq!(read.socket, descriptor.socket);
        assert_eq!(read.capability, descriptor.capability);
        std::fs::remove_file(path).unwrap();
    }
}
