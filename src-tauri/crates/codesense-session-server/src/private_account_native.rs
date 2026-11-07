// SPDX-License-Identifier: MIT
//! Restricted entry in the shipped server. Every operation holds descriptors
//! for the verified account/root; no default-root probe or arbitrary command.
use crate::machine_owner::BootstrapOwner;
use crate::private_account_data::{self as data, Marker};
use base64::Engine;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const REFUSED: &str = "private account unavailable; check account enrollment";
fn fail<T>() -> Result<T, String> {
    Err(REFUSED.into())
}
fn io<T>(result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|_| REFUSED.into())
}
fn component(value: &str) -> Result<CString, String> {
    if value.is_empty() || value == "." || value == ".." || value.contains('/') {
        return fail();
    }
    CString::new(value).map_err(|_| REFUSED.into())
}
fn opened(parent: &File, name: &str, flags: i32, mode: u32) -> Result<File, String> {
    let name = component(name)?;
    // SAFETY: valid held directory descriptor and NUL-terminated component;
    // ownership of a successful descriptor is transferred exactly once.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return fail();
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(
    parent: &File,
    name: &str,
    uid: u32,
    create: bool,
    exact: bool,
) -> Result<File, String> {
    if create {
        let name = component(name)?;
        // SAFETY: held parent descriptor, fixed component and owner-only mode.
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return fail();
        }
    }
    let file = opened(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    let meta = io(file.metadata())?;
    if meta.uid() != uid || meta.mode() & 0o022 != 0 || (exact && meta.mode() & 0o777 != 0o700) {
        return fail();
    }
    Ok(file)
}
fn read_file(
    parent: &File,
    name: &str,
    uid: u32,
    mode: u32,
    bound: u64,
) -> Result<Vec<u8>, String> {
    let mut file = opened(parent, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
    let meta = io(file.metadata())?;
    if !meta.is_file()
        || meta.uid() != uid
        || meta.nlink() != 1
        || meta.mode() & 0o777 != mode
        || meta.len() > bound
    {
        return fail();
    }
    let mut bytes = Vec::new();
    io((&mut file).take(bound + 1).read_to_end(&mut bytes))?;
    if bytes.len() as u64 > bound {
        return fail();
    }
    Ok(bytes)
}
fn missing(parent: &File, name: &str) -> Result<bool, String> {
    let name = component(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: valid descriptor, component and output storage.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Ok(false);
    }
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        Ok(true)
    } else {
        fail()
    }
}
fn verify_version(version: &File, uid: u32, expected: &[u8]) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    if read_file(version, ".codesense-manifest", uid, 0o600, 8192)? != expected {
        return fail();
    }
    let expected_paths: Vec<String> = std::iter::once(crate::install::BINARY.to_string())
        .chain(std::iter::once(crate::install::BRIDGE.to_string()))
        .chain(std::iter::once(
            codesense_pty_core::emulator::WORKER_BIN.to_string(),
        ))
        .chain(std::iter::once(
            crate::install::EMULATOR_NOTICES_REL.to_string(),
        ))
        .chain(
            crate::install::SHELL_INIT_SCRIPTS
                .iter()
                .map(|name| format!("resources/shell-integration/{name}")),
        )
        .chain(std::iter::once(
            crate::install::PROTOCOL_MAJOR_FILE.to_string(),
        ))
        .collect();
    let lines: Vec<_> = std::str::from_utf8(expected)
        .map_err(|_| REFUSED)?
        .lines()
        .collect();
    if lines.len() != expected_paths.len() {
        return fail();
    }
    for (line, expected_path) in lines.into_iter().zip(expected_paths) {
        let (digest, path) = line.split_once("  ").ok_or(REFUSED)?;
        if path != expected_path
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return fail();
        }
        let parts: Vec<_> = path.split('/').collect();
        let mut parent = io(version.try_clone())?;
        for part in &parts[..parts.len() - 1] {
            parent = directory(&parent, part, uid, false, true)?;
        }
        let bytes = read_file(
            &parent,
            parts[parts.len() - 1],
            uid,
            if path == crate::install::BINARY
                || path == crate::install::BRIDGE
                || path == codesense_pty_core::emulator::WORKER_BIN
            {
                0o700
            } else {
                0o600
            },
            256 * 1024 * 1024,
        )?;
        if format!("{:x}", Sha256::digest(bytes)) != digest {
            return fail();
        }
    }
    Ok(())
}
fn create_file(parent: &File, name: &str, bytes: &[u8], mode: u32) -> Result<(), String> {
    let mut file = opened(
        parent,
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        mode,
    )?;
    io(file.write_all(bytes))?;
    io(file.sync_all())
}
fn rename(parent: &File, from: &str, to: &str) -> Result<(), String> {
    let from = component(from)?;
    let to = component(to)?;
    // SAFETY: both names are validated components anchored to the held directory.
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
        )
    } != 0
    {
        return fail();
    }
    io(parent.sync_all())
}
fn random_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    io(File::open("/dev/urandom"))?
        .read_exact(&mut bytes)
        .map_err(|_| REFUSED)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}
fn account() -> Result<(u32, PathBuf), String> {
    // SAFETY: this fixed entry runs before the executable creates any threads.
    let uid = unsafe { libc::geteuid() };
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return fail();
    }
    let home = unsafe { CStr::from_ptr((*pw).pw_dir) }
        .to_str()
        .map_err(|_| REFUSED)?;
    let home = io(Path::new(home).canonicalize())?;
    if !home.is_absolute() {
        return fail();
    }
    Ok((uid, home))
}
struct Root {
    directory: File,
    marker: Marker,
    bytes: Vec<u8>,
}
impl Root {
    fn path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()))
    }
    fn host(&self, create: bool) -> Result<File, String> {
        directory(&self.directory, "host", self.marker.uid, create, true)
    }
    fn installed(&self, version: &str) -> Result<File, String> {
        component(version)?;
        let installed = directory(&self.host(false)?, version, self.marker.uid, false, true)?;
        let manifest = read_file(
            &installed,
            ".codesense-manifest",
            self.marker.uid,
            0o600,
            8192,
        )?;
        use sha2::{Digest, Sha256};
        let digest = format!("{:x}", Sha256::digest(&manifest));
        if version.rsplit_once('-').map(|(_, suffix)| suffix) != Some(&digest[..12]) {
            return fail();
        }
        verify_version(&installed, self.marker.uid, &manifest)?;
        Ok(installed)
    }
}
fn inspect_at(
    home: &Path,
    uid: u32,
    owner: impl Into<BootstrapOwner>,
    enroll: bool,
) -> Result<Option<Root>, String> {
    let owner = owner.into();
    if owner.principal == 0 || owner.machine_owner.validate().is_err() {
        return fail();
    }
    let home_c = CString::new(home.as_os_str().as_encoded_bytes()).map_err(|_| REFUSED)?;
    // SAFETY: canonical absolute path; successful descriptor is uniquely owned.
    let fd = unsafe {
        libc::open(
            home_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return fail();
    }
    let home_fd = unsafe { File::from_raw_fd(fd) };
    let meta = io(home_fd.metadata())?;
    if meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return fail();
    }
    let mut parent = home_fd;
    for name in [".codesense", "remote-private"] {
        match directory(&parent, name, uid, enroll, false) {
            Ok(next) => parent = next,
            Err(error) => {
                let name = component(name)?;
                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                // SAFETY: valid output storage, directory descriptor and name.
                let missing = unsafe {
                    libc::fstatat(
                        parent.as_raw_fd(),
                        name.as_ptr(),
                        stat.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } != 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound;
                if missing && !enroll {
                    return Ok(None);
                }
                return Err(error);
            }
        }
    }
    let mut created = false;
    if enroll {
        let name = c"v1";
        // SAFETY: held parent descriptor and static NUL-terminated name.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
            created = true;
        } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
            return fail();
        }
    }
    let root = match directory(&parent, "v1", uid, false, true) {
        Ok(root) => root,
        Err(error) => {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            let missing = unsafe {
                libc::fstatat(
                    parent.as_raw_fd(),
                    c"v1".as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound;
            if missing && !enroll {
                return Ok(None);
            }
            return Err(error);
        }
    };
    if created {
        let marker = Marker {
            canonical_home: home.to_str().ok_or(REFUSED)?.into(),
            enrollment_id: random_id()?,
            owner_id: owner.machine_owner.marker_parts().2,
            provider: owner.machine_owner.marker_parts().1.into(),
            uid,
            version: owner.machine_owner.marker_parts().0,
        };
        create_file(&root, "owner.json", &data::encode(&marker)?, 0o600)?;
        io(root.sync_all())?;
        io(parent.sync_all())?;
    }
    let bytes = read_file(&root, "owner.json", uid, 0o600, 4096)?;
    let marker = data::parse(&bytes)?.ok_or(REFUSED)?;
    if marker.uid != uid || Path::new(&marker.canonical_home) != home {
        return fail();
    }
    if marker.machine_owner()? != owner.machine_owner {
        return Err(data::OTHER_PROFILE.into());
    }
    Ok(Some(Root {
        directory: root,
        marker,
        bytes,
    }))
}
fn install(root: &Root) -> Result<String, String> {
    let mut input = std::io::BufReader::new(std::io::stdin());
    let mut bytes = Vec::new();
    io(input.by_ref().take(8193).read_until(b'\n', &mut bytes))?;
    let data = data::parse_install(&bytes)?;
    component(&data.version)?;
    let executable = io(std::env::current_exe())?;
    if crate::install::sha256_of(&executable)? != data.binary_sha || data.files.len() != 6 {
        return fail();
    }
    let allowed: Vec<String> = std::iter::once(crate::install::BRIDGE.to_string())
        .chain(std::iter::once(
            codesense_pty_core::emulator::WORKER_BIN.to_string(),
        ))
        .chain(std::iter::once(
            crate::install::EMULATOR_NOTICES_REL.to_string(),
        ))
        .chain(
            crate::install::SHELL_INIT_SCRIPTS
                .iter()
                .map(|name| format!("resources/shell-integration/{name}")),
        )
        .collect();
    if data
        .files
        .iter()
        .zip(&allowed)
        .any(|(file, path)| file.rel != *path || file.len > 256 * 1024 * 1024)
    {
        return fail();
    }
    let host = root.host(true)?;
    // Owned staging is not discoverable as an installed version. Never remove a
    // foreign/damaged existing version; preserve it and require recovery.
    let temporary = format!(".incoming-{}", random_id()?);
    let stage = directory(&host, &temporary, root.marker.uid, true, true)?;
    let result = (|| {
        let mut source = io(File::open(&executable))?;
        let mut dest = opened(
            &stage,
            crate::install::BINARY,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o700,
        )?;
        io(std::io::copy(&mut source, &mut dest))?;
        io(dest.sync_all())?;
        let mut manifest = format!("{}  {}\n", data.binary_sha, crate::install::BINARY);
        use sha2::{Digest, Sha256};
        for file in &data.files {
            let parts: Vec<_> = file.rel.split('/').collect();
            let mut parent = io(stage.try_clone())?;
            for part in &parts[..parts.len() - 1] {
                parent = directory(&parent, part, root.marker.uid, true, true)?;
            }
            let mode = if file.rel == crate::install::BRIDGE
                || file.rel == codesense_pty_core::emulator::WORKER_BIN
            {
                0o700
            } else {
                0o600
            };
            let mut dest = opened(
                &parent,
                parts[parts.len() - 1],
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                mode,
            )?;
            let mut remaining = file.len;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 65536];
            while remaining > 0 {
                let count = io(input.read(&mut buffer[..remaining.min(65536) as usize]))?;
                if count == 0 {
                    return fail();
                }
                io(dest.write_all(&buffer[..count]))?;
                hasher.update(&buffer[..count]);
                remaining -= count as u64;
            }
            if format!("{:x}", hasher.finalize()) != file.sha256 {
                return fail();
            }
            io(dest.sync_all())?;
            manifest.push_str(&format!("{}  {}\n", file.sha256, file.rel));
        }
        if io(input.read(&mut [0u8; 1]))? != 0 {
            return fail();
        }
        let major = format!("{}\n", codesense_host_proto::PROTO_MAJOR);
        create_file(
            &stage,
            crate::install::PROTOCOL_MAJOR_FILE,
            major.as_bytes(),
            0o600,
        )?;
        manifest.push_str(&format!(
            "{:x}  {}\n",
            Sha256::digest(major.as_bytes()),
            crate::install::PROTOCOL_MAJOR_FILE
        ));
        if data.version.rsplit_once('-').map(|(_, suffix)| suffix)
            != Some(&format!("{:x}", Sha256::digest(manifest.as_bytes()))[..12])
        {
            return fail();
        }
        create_file(&stage, ".codesense-manifest", manifest.as_bytes(), 0o600)?;
        io(stage.sync_all())?;
        match root.installed(&data.version) {
            Ok(existing) => {
                verify_version(&existing, root.marker.uid, manifest.as_bytes())?;
                // Only our freshly created stage is removed; held root stays anchored.
                io(std::fs::remove_dir_all(
                    root.path().join("host").join(&temporary),
                ))?;
            }
            Err(_) if missing(&host, &data.version)? => rename(&host, &temporary, &data.version)?,
            Err(_) => return fail(),
        }
        let next = format!(".current-{}", random_id()?);
        create_file(&host, &next, data.version.as_bytes(), 0o600)?;
        rename(&host, &next, "current")?;
        Ok(data.version)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(root.path().join("host").join(&temporary));
    }
    result
}
/// Private-root files and directories the running server writes. A host
/// whose umask is 0002 (the default with user private groups) left them
/// group-writable before the server set its own umask.
const RUNTIME_FILES: [&str; 2] = ["pty-server.lock", "pty-server-id"];
const RUNTIME_DIRS: [&str; 2] = ["logs", "emulator"];

/// Restrict this account's own runtime entries to owner-only, in place. Only
/// an entry this uid owns that is a plain directory or a singly linked file
/// is changed, so the permission checks below keep their meaning; anything
/// else is left for them to refuse. The new mode only ever removes bits.
fn tighten_runtime(root: &Root) -> Result<(), String> {
    let uid = root.marker.uid;
    let tighten = |parent: &File, name: &str, dir: bool| -> Result<Option<File>, String> {
        let flags = libc::O_RDONLY | libc::O_NONBLOCK | if dir { libc::O_DIRECTORY } else { 0 };
        let Ok(entry) = opened(parent, name, flags, 0) else {
            return Ok(None);
        };
        let Ok(meta) = entry.metadata() else {
            return Ok(None);
        };
        let plain = if dir {
            meta.is_dir()
        } else {
            meta.is_file() && meta.nlink() == 1
        };
        let desired = if dir { 0o700 } else { 0o600 };
        if meta.uid() == uid && plain && meta.mode() & 0o077 != 0 {
            let mode = meta.mode() & 0o700 & desired;
            // SAFETY: fchmod on the descriptor just verified above.
            if unsafe { libc::fchmod(entry.as_raw_fd(), mode) } != 0 {
                let error = std::io::Error::last_os_error();
                return Err(format!(
                    "terminal server runtime file {name} could not be restricted to owner-only ({error})"
                ));
            }
        }
        Ok(Some(entry))
    };
    for name in RUNTIME_FILES {
        tighten(&root.directory, name, false)?;
    }
    for name in RUNTIME_DIRS {
        if let Some(dir) = tighten(&root.directory, name, true)? {
            if name == "logs" {
                tighten(&dir, "pty-server.log", false)?;
            }
        }
    }
    Ok(())
}

fn runtime_file(root: &Root, name: &str, bound: u64) -> Result<Vec<u8>, String> {
    let file =
        opened(&root.directory, name, libc::O_RDONLY | libc::O_NONBLOCK, 0).map_err(|_| {
            format!("terminal server runtime file {name} is missing or not a plain file")
        })?;
    let meta = io(file.metadata())?;
    let mode = meta.mode() & 0o777;
    let problem = if meta.uid() != root.marker.uid {
        Some("is owned by another account".to_string())
    } else if !meta.is_file() || meta.nlink() != 1 {
        Some("is not a singly linked plain file".to_string())
    } else if meta.mode() & 0o077 != 0 {
        Some(format!("is accessible to group or others (mode {mode:o})"))
    } else if meta.len() > bound {
        Some(format!("is larger than {bound} bytes"))
    } else {
        None
    };
    if let Some(problem) = problem {
        return Err(format!("terminal server runtime file {name} {problem}"));
    }
    let mut bytes = Vec::new();
    io(file.take(bound + 1).read_to_end(&mut bytes))?;
    if bytes.len() as u64 > bound {
        return Err(format!(
            "terminal server runtime file {name} is larger than {bound} bytes"
        ));
    }
    Ok(bytes)
}
fn proof(root: &Root) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let pid_bytes = runtime_file(root, "pty-server.lock", 32)?;
    let pid: u32 = std::str::from_utf8(&pid_bytes)
        .map_err(|_| REFUSED)?
        .trim()
        .parse()
        .map_err(|_| REFUSED)?;
    if pid == 0 || io(std::fs::metadata(format!("/proc/{pid}")))?.uid() != root.marker.uid {
        return fail();
    }
    let start = crate::replace::process_start(pid).map_err(|_| REFUSED)?;
    let executable = io(std::fs::canonicalize(format!("/proc/{pid}/exe")))?;
    let logical_root = Path::new(&root.marker.canonical_home).join(".codesense/remote-private/v1");
    let host = logical_root.join("host");
    let relative = executable.strip_prefix(&host).map_err(|_| REFUSED)?;
    let parts: Vec<_> = relative.components().collect();
    if parts.len() != 2 || parts[1].as_os_str() != crate::install::BINARY {
        return fail();
    }
    let version = parts[0].as_os_str().to_str().ok_or(REFUSED)?;
    let installed = root.installed(version)?;
    let binary = opened(&installed, crate::install::BINARY, libc::O_RDONLY, 0)?;
    let metadata = io(binary.metadata())?;
    let running = io(std::fs::metadata(format!("/proc/{pid}/exe")))?;
    if metadata.dev() != running.dev() || metadata.ino() != running.ino() {
        return fail();
    }
    let manifest = read_file(
        &installed,
        ".codesense-manifest",
        root.marker.uid,
        0o600,
        8192,
    )?;
    let server_id = runtime_file(root, "pty-server-id", 256)?;
    let server_id = std::str::from_utf8(&server_id).map_err(|_| REFUSED)?.trim();
    if server_id.is_empty()
        || server_id.contains(['\n', '\r'])
        || crate::replace::process_start(pid).map_err(|_| REFUSED)? != start
    {
        return fail();
    }
    Ok(format!("server_id={server_id}\nversion={version}\nmanifest={:x}\npid={pid}\nstart={start}\ninode={}:{}\nexecutable={}\nroot={}\nuid={}\n", Sha256::digest(manifest), metadata.dev(), metadata.ino(), executable.to_str().ok_or(REFUSED)?, logical_root.to_str().ok_or(REFUSED)?, root.marker.uid))
}

// Recover at most 32 expired staging receipts, never arbitrary /tmp entries.
// Only the fixed files belonging to the exact recorded directory are unlinked.
fn recover_staging(uid: u32) {
    let Ok(parent) = File::open("/tmp") else {
        return;
    };
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return;
    };
    let mut recovered = 0;
    for entry in entries.take(4096).flatten() {
        if recovered == 32 {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(suffix) = name.strip_prefix("codesense-private.") else {
            continue;
        };
        if suffix.len() != 12 || !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
            continue;
        }
        let Ok(stage) = directory(&parent, name, uid, false, true) else {
            continue;
        };
        let Ok(meta) = stage.metadata() else {
            continue;
        };
        if meta
            .modified()
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_none_or(|age| age.as_secs() <= 86400)
        {
            continue;
        }
        let Ok(receipt) = read_file(&stage, "receipt", uid, 0o600, 128) else {
            continue;
        };
        if receipt != format!("{}:{}:{}\n", uid, meta.dev(), meta.ino()).as_bytes() {
            continue;
        }
        for file in [c"helper", c"receipt"] {
            // SAFETY: fixed component within our receipt-verified held directory.
            unsafe {
                libc::unlinkat(stage.as_raw_fd(), file.as_ptr(), 0);
            }
        }
        if let Ok(name) = component(name) {
            // Empty-directory removal cannot recursively reach other entries.
            unsafe {
                libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR);
            }
        }
        recovered += 1;
    }
}

/// Everything a private operation or server creates is this account's alone,
/// whatever the host's umask (0002 is common with user private groups);
/// writers also pass explicit modes.
fn owner_only_umask() {
    // SAFETY: umask has no preconditions and only affects this process tree.
    unsafe { libc::umask(0o077) };
}

/// Invoked only by `private-account OWNER OP EXPECTED [VERSION]`.
/// Owner is supplied by the backend, never a frontend authority parameter.
pub fn run(args: &[String]) -> Result<(), String> {
    owner_only_umask();
    if args.len() < 3 || args.len() > 4 {
        return fail();
    }
    let owner: BootstrapOwner = args[0].parse().map_err(|_| REFUSED)?;
    let operation = args[1].as_str();
    if !matches!(
        operation,
        "inspect" | "enroll" | "status" | "install" | "attach" | "proof" | "replace"
    ) {
        return fail();
    }
    let (uid, home) = account()?;
    recover_staging(uid);
    let root = inspect_at(&home, uid, owner.clone(), operation == "enroll")?;
    if matches!(operation, "inspect" | "enroll") {
        if args.len() != 3 || args[2] != "-" {
            return fail();
        }
        match root {
            Some(root) => print!("{}", std::str::from_utf8(&root.bytes).map_err(|_| REFUSED)?),
            None => println!("null"),
        }
        return Ok(());
    }
    let root = root.ok_or(REFUSED)?;
    let expected = base64::engine::general_purpose::STANDARD
        .decode(&args[2])
        .map_err(|_| REFUSED)?;
    if expected != root.bytes {
        return fail();
    }
    tighten_runtime(&root)?;
    match operation {
        "proof" => {
            print!("{}", proof(&root)?);
            Ok(())
        }
        "status" => {
            if !missing(&root.directory, "host")? {
                let host = root.host(false)?;
                if missing(&host, "current")? {
                    return Ok(());
                }
                let current = read_file(&host, "current", uid, 0o600, 128)?;
                let version = std::str::from_utf8(&current).map_err(|_| REFUSED)?;
                root.installed(version)?;
                println!("{version}");
            }
            Ok(())
        }
        "install" => {
            println!("{}", install(&root)?);
            Ok(())
        }
        "attach" | "replace" => {
            let version = args.get(3).ok_or(REFUSED)?;
            let installed = root.installed(version)?;
            let logical_root =
                Path::new(&root.marker.canonical_home).join(".codesense/remote-private/v1");
            let directory = logical_root.join("host").join(version);
            // Execute through the verified version descriptor. The kernel
            // resolves it before CLOEXEC closes the FD; --root stays canonical.
            let binary = PathBuf::from(format!("/proc/self/fd/{}", installed.as_raw_fd()))
                .join(crate::install::BINARY);
            let target = crate::attach::Target {
                root: root.path(),
                resources: Some(directory.join("resources")),
            };
            if operation == "replace" {
                let authorization =
                    crate::replace::Authorization::read_stdin().map_err(|_| REFUSED)?;
                if authorization.root != logical_root || authorization.uid != uid {
                    return fail();
                }
                // The old installed payload is checked too; the helper cannot
                // retire a process through a changed or symlinked old version.
                root.installed(&authorization.version)?;
                crate::replace::retire_authorized(&authorization, &logical_root, &root.path())
                    .map_err(|_| REFUSED)?;
                drop(
                    crate::attach::connect_private(
                        &target,
                        &binary,
                        &owner,
                        &args[2],
                        version,
                        &logical_root,
                    )
                    .map_err(|_| REFUSED)?,
                );
                Ok(())
            } else {
                crate::attach::run_private(
                    &target,
                    &binary,
                    &owner,
                    &args[2],
                    version,
                    &logical_root,
                )
                .map_err(|_| REFUSED.into())
            }
        }
        _ => fail(),
    }
}

/// A private daemon opens its own CLOEXEC descriptor before binding. Canonical
/// names remain the protocol/process identity; descriptor paths stay internal.
pub struct ServeRoot {
    root: Root,
    principal: u64,
    pub logical_root: PathBuf,
    pub resources: PathBuf,
}
impl ServeRoot {
    pub fn owner_id(&self) -> u64 {
        self.principal
    }
    pub fn filesystem_root(&self) -> PathBuf {
        self.root.path()
    }
}
pub fn prepare_serve(args: &[String]) -> Result<ServeRoot, String> {
    owner_only_umask();
    if args.len() != 5 || args[3] != "--root" {
        return fail();
    }
    let owner: BootstrapOwner = args[0].parse().map_err(|_| REFUSED)?;
    let (uid, home) = account()?;
    let root = inspect_at(&home, uid, owner.clone(), false)?.ok_or(REFUSED)?;
    let expected = base64::engine::general_purpose::STANDARD
        .decode(&args[1])
        .map_err(|_| REFUSED)?;
    if expected != root.bytes {
        return fail();
    }
    root.installed(&args[2])?;
    let logical_root = home.join(".codesense/remote-private/v1");
    if Path::new(&args[4]) != logical_root {
        return fail();
    }
    let version = logical_root.join("host").join(&args[2]);
    if io(std::env::current_exe())? != version.join(crate::install::BINARY) {
        return fail();
    }
    Ok(ServeRoot {
        principal: owner.principal,
        root,
        logical_root,
        resources: version.join("resources"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn group_writable_runtime_files_are_tightened_or_refused_with_their_reason() {
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = unsafe { libc::geteuid() };
        let root = inspect_at(home.path(), uid, 41u64, true).unwrap().unwrap();
        let dir = home.path().join(".codesense/remote-private/v1");
        let chmod = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        std::fs::write(dir.join("pty-server-id"), "id").unwrap();
        std::fs::create_dir(dir.join("emulator")).unwrap();
        std::fs::create_dir(dir.join("logs")).unwrap();
        std::fs::write(dir.join("logs/pty-server.log"), "").unwrap();
        chmod(&dir.join("pty-server-id"), 0o664);
        chmod(&dir.join("emulator"), 0o775);
        chmod(&dir.join("logs/pty-server.log"), 0o664);

        let error = runtime_file(&root, "pty-server-id", 256).unwrap_err();
        assert!(
            error.contains("pty-server-id is accessible to group or others (mode 664)"),
            "{error}"
        );

        tighten_runtime(&root).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.join("pty-server-id")), 0o600);
        assert_eq!(mode(&dir.join("emulator")), 0o700);
        assert_eq!(mode(&dir.join("logs/pty-server.log")), 0o600);
        assert_eq!(runtime_file(&root, "pty-server-id", 256).unwrap(), b"id");

        // Group-readable is refused too, and the heal never adds a bit the
        // entry lacked: owner read-only stays read-only.
        chmod(&dir.join("pty-server-id"), 0o440);
        let error = runtime_file(&root, "pty-server-id", 256).unwrap_err();
        assert!(error.contains("(mode 440)"), "{error}");
        tighten_runtime(&root).unwrap();
        assert_eq!(mode(&dir.join("pty-server-id")), 0o400);
        assert_eq!(runtime_file(&root, "pty-server-id", 256).unwrap(), b"id");
        chmod(&dir.join("pty-server-id"), 0o600);

        // A hard-linked file is never changed: it stays refused.
        std::fs::remove_file(dir.join("pty-server-id")).unwrap();
        std::fs::write(home.path().join("elsewhere"), "id").unwrap();
        chmod(&home.path().join("elsewhere"), 0o664);
        std::fs::hard_link(home.path().join("elsewhere"), dir.join("pty-server-id")).unwrap();
        tighten_runtime(&root).unwrap();
        assert_eq!(mode(&home.path().join("elsewhere")), 0o664);
        assert!(runtime_file(&root, "pty-server-id", 256).is_err());
    }
    #[test]
    fn private_enrollment_is_exclusive_and_refusals_preserve_marker() {
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = unsafe { libc::geteuid() };
        assert!(inspect_at(home.path(), uid, 41u64, false)
            .unwrap()
            .is_none());
        assert!(!home.path().join(".codesense").exists());
        let root = inspect_at(home.path(), uid, 41u64, true).unwrap().unwrap();
        let marker = root.bytes.clone();
        for enroll in [false, true] {
            assert_eq!(
                inspect_at(home.path(), uid, 42u64, enroll).err().unwrap(),
                data::OTHER_PROFILE
            );
        }
        assert_eq!(
            inspect_at(home.path(), uid, 41u64, true)
                .unwrap()
                .unwrap()
                .bytes,
            marker
        );
        let path = home.path().join(".codesense/remote-private/v1/owner.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            inspect_at(home.path(), uid, 41u64, false).err().unwrap(),
            REFUSED
        );
        assert_eq!(std::fs::read(path).unwrap(), marker);
    }
    #[test]
    fn private_local_marker_enrolls_reads_and_refuses_other_typed_owners_without_mutation() {
        use crate::machine_owner::MachineOwner;
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        // SAFETY: querying the effective uid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let local = BootstrapOwner {
            principal: 41,
            machine_owner: MachineOwner::Local {
                id: "550e8400-e29b-41d4-a716-446655440000".into(),
            },
        };
        assert!(inspect_at(home.path(), uid, local.clone(), false)
            .unwrap()
            .is_none());
        let marker = inspect_at(home.path(), uid, local.clone(), true)
            .unwrap()
            .unwrap()
            .bytes;
        let json: serde_json::Value = serde_json::from_slice(&marker).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["provider"], "local");
        assert_eq!(json["owner_id"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(
            inspect_at(home.path(), uid, local.clone(), false)
                .unwrap()
                .unwrap()
                .bytes,
            marker
        );
        let different = BootstrapOwner {
            principal: 41,
            machine_owner: MachineOwner::Local {
                id: "550e8400-e29b-41d4-a716-446655440001".into(),
            },
        };
        for owner in [different, BootstrapOwner::from(41u64)] {
            for enroll in [false, true] {
                assert_eq!(
                    inspect_at(home.path(), uid, owner.clone(), enroll)
                        .err()
                        .unwrap(),
                    data::OTHER_PROFILE
                );
            }
        }
        assert_eq!(
            std::fs::read(home.path().join(".codesense/remote-private/v1/owner.json")).unwrap(),
            marker
        );
        let legacy_home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(legacy_home.path(), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let legacy = inspect_at(legacy_home.path(), uid, 41u64, true)
            .unwrap()
            .unwrap()
            .bytes;
        for enroll in [false, true] {
            assert_eq!(
                inspect_at(legacy_home.path(), uid, local.clone(), enroll)
                    .err()
                    .unwrap(),
                data::OTHER_PROFILE
            );
        }
        assert_eq!(
            inspect_at(legacy_home.path(), uid, 41u64, false)
                .unwrap()
                .unwrap()
                .bytes,
            legacy
        );
    }

    #[test]
    fn private_installed_version_rejects_damaged_binary_and_resource_symlink() {
        use sha2::{Digest, Sha256};
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let uid = unsafe { libc::geteuid() };
        let root = inspect_at(home.path(), uid, 41u64, true).unwrap().unwrap();
        let host = root.host(true).unwrap();
        let stage = directory(&host, "stage", uid, true, true).unwrap();
        create_file(&stage, crate::install::BINARY, b"binary", 0o700).unwrap();
        create_file(&stage, crate::install::BRIDGE, b"bridge", 0o700).unwrap();
        let resources = directory(&stage, "resources", uid, true, true).unwrap();
        let shell = directory(&resources, "shell-integration", uid, true, true).unwrap();
        let mut manifest = format!(
            "{:x}  {}\n",
            Sha256::digest(b"binary"),
            crate::install::BINARY
        );
        manifest.push_str(&format!(
            "{:x}  {}\n",
            Sha256::digest(b"bridge"),
            crate::install::BRIDGE
        ));
        create_file(
            &stage,
            codesense_pty_core::emulator::WORKER_BIN,
            b"worker",
            0o700,
        )
        .unwrap();
        let emulator = directory(&resources, "emulator", uid, true, true).unwrap();
        create_file(&emulator, "NOTICE.md", b"notice", 0o600).unwrap();
        manifest.push_str(&format!(
            "{:x}  {}\n",
            Sha256::digest(b"worker"),
            codesense_pty_core::emulator::WORKER_BIN
        ));
        manifest.push_str(&format!(
            "{:x}  {}\n",
            Sha256::digest(b"notice"),
            crate::install::EMULATOR_NOTICES_REL
        ));
        for name in crate::install::SHELL_INIT_SCRIPTS {
            create_file(&shell, name, b"script", 0o600).unwrap();
            manifest.push_str(&format!(
                "{:x}  resources/shell-integration/{name}\n",
                Sha256::digest(b"script")
            ));
        }
        create_file(&stage, crate::install::PROTOCOL_MAJOR_FILE, b"2\n", 0o600).unwrap();
        manifest.push_str(&format!(
            "{:x}  {}\n",
            Sha256::digest(b"2\n"),
            crate::install::PROTOCOL_MAJOR_FILE
        ));
        create_file(&stage, ".codesense-manifest", manifest.as_bytes(), 0o600).unwrap();
        let version = format!(
            "0.1.0-{}",
            &format!("{:x}", Sha256::digest(manifest.as_bytes()))[..12]
        );
        rename(&host, "stage", &version).unwrap();
        assert!(root.installed(&version).is_ok());
        let dir = root.path().join("host").join(&version);
        std::fs::write(dir.join(crate::install::BINARY), b"damaged").unwrap();
        assert!(root.installed(&version).is_err());
        assert_eq!(
            std::fs::read(dir.join(crate::install::BINARY)).unwrap(),
            b"damaged"
        );
        std::fs::write(dir.join(crate::install::BINARY), b"binary").unwrap();
        let resource = dir
            .join("resources/shell-integration")
            .join(crate::install::SHELL_INIT_SCRIPTS[0]);
        std::fs::remove_file(&resource).unwrap();
        symlink("/etc/passwd", &resource).unwrap();
        assert!(root.installed(&version).is_err());
        assert!(resource.is_symlink());
    }
}
