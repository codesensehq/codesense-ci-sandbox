// SPDX-License-Identifier: MIT
//! Raw version-probe output, reusable only while the executable's identity matches.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Fingerprint {
    canonical: PathBuf,
    modified: SystemTime,
    len: u64,
    inode: u64,
    changed: (i64, i64),
    #[serde(default)]
    interpreter: Option<Box<Fingerprint>>,
}

impl Fingerprint {
    fn at(program: &Path) -> io::Result<Self> {
        let canonical = std::fs::canonicalize(program)?;
        let metadata = std::fs::metadata(&canonical)?;
        #[cfg(unix)]
        let (inode, changed) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.ino(), (metadata.ctime(), metadata.ctime_nsec()))
        };
        #[cfg(not(unix))]
        let (inode, changed) = (0, (0, 0));
        Ok(Self {
            canonical,
            modified: metadata.modified()?,
            len: metadata.len(),
            inode,
            changed,
            interpreter: None,
        })
    }

    fn entry(&self, root: &Path) -> io::Result<PathBuf> {
        let key = serde_json::to_vec(&self.canonical)?;
        Ok(root.join(format!("{}.json", blake3::hash(&key))))
    }
}

/// A pinned managed installation, identified while the caller holds its prepare lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIdentity(StoredIdentity);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredIdentity {
    version: (u64, u64, u64),
    commit: String,
    install: PathBuf,
    #[serde(default)]
    seed: Option<PathBuf>,
}

impl RuntimeIdentity {
    /// Construct only from the managed installation prepared under the install lock.
    /// The caller must retain that lock across version probing and cache access.
    pub fn from_prepared_install(
        version: (u64, u64, u64),
        commit: &str,
        install: &Path,
    ) -> io::Result<Self> {
        if commit.len() != 12 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected exact 12-character commit",
            ));
        }
        let install = std::fs::canonicalize(install)?;
        if !install.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected installation directory",
            ));
        }
        Ok(Self(StoredIdentity {
            version,
            commit: commit.to_owned(),
            install,
            seed: None,
        }))
    }

    /// Permit a pinned interpreter in this prepared runtime's immutable seed.
    pub fn with_seed(mut self, seed: &Path) -> io::Result<Self> {
        self.0.seed = Some(std::fs::canonicalize(seed)?);
        Ok(self)
    }

    /// Version, commit and canonical installation uniquely name the prepare lock.
    pub fn key(&self) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(self.0.version, &self.0.commit, &self.0.install,))
                    .expect("runtime identity serializes")
            )
        )
    }
}

/// A runtime lock retained across identity reconstruction, version probe and cache I/O.
/// A session links to app-owned metadata rather than deserializing an identity from argv.
pub struct LockedRuntime {
    _lock: std::fs::File,
    root: PathBuf,
    identity: RuntimeIdentity,
}

pub const RUNTIME_LINK: &str = ".codesense-runtime";

impl LockedRuntime {
    pub fn acquire(root: &Path, identity: RuntimeIdentity) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        if root.file_name().and_then(|v| v.to_str()) != Some(identity.key().as_str()) {
            return Err(io::Error::other(
                "runtime directory does not match identity",
            ));
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.with_extension("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _lock: lock,
            root: root.to_owned(),
            identity,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn identity(&self) -> &RuntimeIdentity {
        &self.identity
    }
    pub fn set_seed(&mut self, seed: &Path) -> io::Result<()> {
        self.identity = self.identity.clone().with_seed(seed)?;
        Ok(())
    }

    pub fn for_probe(probe: &super::Probe, home: Option<&Path>) -> Option<Self> {
        if !probe.cache_version {
            return None;
        }
        Self::for_home(home?)
    }

    /// Publish only after READY has been written by the successful preparer.
    pub fn bind_home(&self, home: &Path) -> io::Result<()> {
        if !self.root.join("READY").is_file() {
            return Err(io::Error::other("runtime is not ready"));
        }
        std::fs::write(
            self.root.join("identity.json"),
            serde_json::to_vec(&self.identity.0)?,
        )?;
        let link = home.join(RUNTIME_LINK);
        if let Ok(existing) = std::fs::read_link(&link) {
            if existing == self.root {
                return Ok(());
            }
            return Err(io::Error::other("home already belongs to another runtime"));
        }
        std::os::unix::fs::symlink(&self.root, link)
    }

    pub fn for_home(home: &Path) -> Option<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let root = std::fs::canonicalize(home.join(RUNTIME_LINK)).ok()?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.with_extension("lock"))
            .ok()?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return None;
        }
        // Reconstruct from app-owned metadata only after acquiring the prepare lock.
        let stored: StoredIdentity =
            serde_json::from_slice(&std::fs::read(root.join("identity.json")).ok()?).ok()?;
        let mut identity =
            RuntimeIdentity::from_prepared_install(stored.version, &stored.commit, &stored.install)
                .ok()?;
        if let Some(seed) = stored.seed {
            identity = identity.with_seed(&seed).ok()?;
        }
        if root.file_name().and_then(|v| v.to_str()) != Some(identity.key().as_str())
            || !root.join("READY").is_file()
        {
            return None;
        }
        Some(Self {
            _lock: lock,
            root,
            identity,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Entry {
    binary: Fingerprint,
    #[serde(default)]
    runtime: Option<StoredIdentity>,
    raw: String,
}

fn eligible(
    program: &Path,
    probe: &super::Probe,
    runtime: Option<&RuntimeIdentity>,
) -> Option<Fingerprint> {
    if !probe.cache_version {
        return None;
    }
    let runtime = runtime?;
    let mut binary = Fingerprint::at(program).ok()?;
    if [program, &binary.canonical]
        .iter()
        .any(|path| path.components().any(|part| part.as_os_str() == "shims"))
        || !binary.canonical.starts_with(&runtime.0.install)
    {
        return None;
    }
    let metadata = std::fs::metadata(&binary.canonical).ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    use std::io::Read;
    let mut magic = [0; 4];
    std::fs::File::open(&binary.canonical)
        .ok()?
        .read_exact(&mut magic)
        .ok()?;
    let native = matches!(
        magic,
        [0x7f, b'E', b'L', b'F']
            | [0xfe, 0xed, 0xfa, 0xce]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    );
    if !native {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::BufReader::new(std::fs::File::open(&binary.canonical).ok()?)
            .take(4096)
            .read_line(&mut line)
            .ok()?;
        let interpreter = Path::new(line.strip_prefix("#!")?.trim());
        if !interpreter.is_absolute() {
            return None;
        }
        let interpreter = std::fs::canonicalize(interpreter).ok()?;
        if interpreter
            .components()
            .any(|part| part.as_os_str() == "shims")
            || !(interpreter.starts_with(&runtime.0.install)
                || runtime
                    .0
                    .seed
                    .as_ref()
                    .is_some_and(|seed| interpreter.starts_with(seed)))
        {
            return None;
        }
        // An env dispatcher or a shebang with options cannot name a pinned interpreter.
        if interpreter.file_name().is_some_and(|name| name == "env") {
            return None;
        }
        let metadata = std::fs::metadata(&interpreter).ok()?;
        use std::os::unix::fs::PermissionsExt;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
        binary.interpreter = Some(Box::new(Fingerprint::at(&interpreter).ok()?));
    }
    Some(binary)
}

// Avoid reading or writing CLI homes. The fallback survives application restarts
// even on hosts that do not provide XDG_CACHE_HOME; OS temporary cleanup is a miss.
fn default_root() -> PathBuf {
    if let Some(root) = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return root.join("codesense/agent-versions");
    }
    #[cfg(unix)]
    let owner = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let owner = std::env::var("USERNAME").unwrap_or_else(|_| "local".into());
    std::env::temp_dir().join(format!("codesense-{owner}/agent-versions"))
}

/// Read only for an opted-in CLI in a pinned, managed native installation.
pub fn read(
    program: &Path,
    probe: &super::Probe,
    runtime: Option<&RuntimeIdentity>,
) -> Option<String> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(raw) = known_releases().lock().unwrap().get(program) {
        return Some(raw.clone());
    }
    let binary = eligible(program, probe, runtime)?;
    read_data(&default_root(), &binary, runtime)
}

/// Store only for an opted-in CLI in a pinned, managed native installation.
pub fn write(
    program: &Path,
    raw: &str,
    probe: &super::Probe,
    runtime: Option<&RuntimeIdentity>,
) -> io::Result<()> {
    let Some(binary) = eligible(program, probe, runtime) else {
        return Ok(());
    };
    write_data(&default_root(), binary, raw, runtime)
}

fn read_data(
    root: &Path,
    binary: &Fingerprint,
    runtime: Option<&RuntimeIdentity>,
) -> Option<String> {
    let entry: Entry =
        serde_json::from_slice(&std::fs::read(binary.entry(root).ok()?).ok()?).ok()?;
    (entry.binary == *binary && entry.runtime.as_ref() == runtime.map(|identity| &identity.0))
        .then_some(entry.raw)
}

/// Releases a test has declared for fixture programs, by path.
#[cfg(any(test, feature = "test-support"))]
fn known_releases() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, String>> {
    static KNOWN: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, String>>,
    > = std::sync::OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// Declare that `program --version` reads `raw`, so a launch preparation reaches no
/// probe for it: its qualification depends on no child process or clock, and no
/// cache root is written. Keyed by the fixture's own path.
#[cfg(any(test, feature = "test-support"))]
pub fn known_release_for_test(program: &Path, raw: &str) {
    known_releases()
        .lock()
        .unwrap()
        .insert(program.to_owned(), raw.to_owned());
}

/// Seed old/raw entries to prove that production policy ignores them.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_for_test(program: &Path, raw: &str) -> io::Result<()> {
    write_data(&default_root(), Fingerprint::at(program)?, raw, None)
}

/// Inspect fixture data without weakening production availability.
#[cfg(any(test, feature = "test-support"))]
pub fn peek_for_test(program: &Path) -> Option<String> {
    read_data(&default_root(), &Fingerprint::at(program).ok()?, None)
}

/// Atomically persist an entry under an explicit cache directory.
fn write_data(
    root: &Path,
    binary: Fingerprint,
    raw: &str,
    runtime: Option<&RuntimeIdentity>,
) -> io::Result<()> {
    let destination = binary.entry(root)?;
    let bytes = serde_json::to_vec(&Entry {
        binary,
        runtime: runtime.map(|identity| identity.0.clone()),
        raw: raw.to_owned(),
    })?;
    std::fs::create_dir_all(root)?;
    let temporary = root.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        use std::io::Write;
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        std::fs::rename(&temporary, &destination)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Expand version-probe environment values; skip runtime-dependent values until
/// the caller has a runtime directory. Never manufacture a CLI home during probing.
pub fn resolve_env(values: &[(&str, &str)], runtime: Option<&Path>) -> Vec<(String, String)> {
    values
        .iter()
        .filter_map(|(name, value)| {
            let resolved = if value.contains("{runtime}") {
                value.replace("{runtime}", runtime?.to_str()?)
            } else {
                (*value).to_owned()
            };
            Some(((*name).to_owned(), resolved))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::AgentCli;
    use super::*;
    fn probe() -> &'static super::super::Probe {
        super::super::adapter(super::super::AgentCli::Hermes).probe()
    }
    fn identity(root: &Path) -> RuntimeIdentity {
        RuntimeIdentity::from_prepared_install((0, 22, 5), "0123456789ab", root).unwrap()
    }
    fn native(path: &Path) {
        std::fs::write(path, b"\x7fELFfixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    #[test]
    fn cache_data_hits_misses_reloads_and_invalidates_binary_or_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("agent");
        native(&binary);
        let root = dir.path().join("cache");
        let runtime = identity(dir.path());
        let key = eligible(&binary, probe(), Some(&runtime)).unwrap();
        assert_eq!(read_data(&root, &key, Some(&runtime)), None);
        write_data(&root, key, "raw banner", Some(&runtime)).unwrap();
        let key = eligible(&binary, probe(), Some(&runtime)).unwrap();
        assert_eq!(
            read_data(&PathBuf::from(&root), &key, Some(&runtime)).as_deref(),
            Some("raw banner")
        );
        let other =
            RuntimeIdentity::from_prepared_install((0, 22, 5), "aaaaaaaaaaaa", dir.path()).unwrap();
        assert_eq!(read_data(&root, &key, Some(&other)), None);
        let other =
            RuntimeIdentity::from_prepared_install((0, 22, 6), "0123456789ab", dir.path()).unwrap();
        assert_eq!(read_data(&root, &key, Some(&other)), None);
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(read_data(&root, &key, Some(&identity(&nested))), None);
        // Old entries have no runtime key and cannot qualify a prepared install.
        write_data(&root, Fingerprint::at(&binary).unwrap(), "legacy", None).unwrap();
        assert_eq!(read_data(&root, &key, Some(&runtime)), None);
        std::fs::write(&binary, b"\x7fELFchanged length").unwrap();
        assert_eq!(
            read_data(&root, &Fingerprint::at(&binary).unwrap(), Some(&runtime)),
            None
        );
        std::fs::remove_file(binary).unwrap();
        assert!(Fingerprint::at(&dir.path().join("agent")).is_err());
    }
    #[test]
    fn adapters_only_read_matching_identity_cache_when_their_probe_opts_in() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("agent");
        native(&binary);
        let runtime = identity(dir.path());
        let other =
            RuntimeIdentity::from_prepared_install((0, 22, 5), "aaaaaaaaaaaa", dir.path()).unwrap();
        let root = default_root();
        for cli in AgentCli::every_cli() {
            let probe = super::super::adapter(cli).probe();
            let key = Fingerprint::at(&binary).unwrap();
            let entry = key.entry(&root).unwrap();
            write_data(
                &root,
                Fingerprint::at(&binary).unwrap(),
                "cached",
                Some(&runtime),
            )
            .unwrap();
            assert_eq!(read(&binary, probe, None), None, "{cli}: no identity");
            assert_eq!(
                read(&binary, probe, Some(&other)),
                None,
                "{cli}: mismatched identity"
            );
            if probe.cache_version {
                assert_eq!(
                    read(&binary, probe, Some(&runtime)).as_deref(),
                    Some("cached"),
                    "{cli}"
                );
                write(&binary, "fresh", probe, Some(&runtime)).unwrap();
                assert_eq!(
                    read(&binary, probe, Some(&runtime)).as_deref(),
                    Some("fresh"),
                    "{cli}"
                );
            } else {
                assert!(eligible(&binary, probe, Some(&runtime)).is_none(), "{cli}");
                assert_eq!(read(&binary, probe, Some(&runtime)), None, "{cli}");
                write(&binary, "ignored", probe, Some(&runtime)).unwrap();
                assert_eq!(
                    read_data(&root, &key, Some(&runtime)).as_deref(),
                    Some("cached"),
                    "{cli}"
                );
            }
            std::fs::remove_file(entry).unwrap();
        }
    }
    #[test]
    fn scripts_shims_and_unmanaged_images_are_ineligible() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = identity(dir.path());
        let program = dir.path().join("agent");
        native(&program);
        for script in ["#!/bin/sh\necho version", "plain text"] {
            std::fs::write(&program, script).unwrap();
            assert!(eligible(&program, probe(), Some(&runtime)).is_none());
        }
        let shims = dir.path().join("shims");
        std::fs::create_dir(&shims).unwrap();
        native(&shims.join("agent"));
        assert!(eligible(&shims.join("agent"), probe(), Some(&runtime)).is_none());
        native(&program);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(eligible(&program, probe(), Some(&runtime)).is_none());
            native(&program);
        }
        let other = tempfile::tempdir().unwrap();
        assert!(eligible(&program, probe(), Some(&identity(other.path()))).is_none());
        #[cfg(unix)]
        {
            let alias = shims.join("alias");
            std::os::unix::fs::symlink(&program, &alias).unwrap();
            assert!(eligible(&alias, probe(), Some(&runtime)).is_none());
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(shims.join("agent"), &alias).unwrap();
            assert!(eligible(&alias, probe(), Some(&runtime)).is_none());
        }
    }
    #[test]
    fn pinned_scripts_require_an_absolute_managed_interpreter_and_matching_identity() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("install");
        let seed = dir.path().join("seed");
        std::fs::create_dir(&install).unwrap();
        std::fs::create_dir(&seed).unwrap();
        let interpreter = seed.join("python");
        native(&interpreter);
        let program = install.join("agent");
        native(&program);
        std::fs::write(&program, format!("#!{}\nscript", interpreter.display())).unwrap();
        let runtime = identity(&install).with_seed(&seed).unwrap();
        let root = dir.path().join("cache");
        let key = eligible(&program, probe(), Some(&runtime)).expect("pinned seed interpreter");
        write_data(&root, key, "qualified", Some(&runtime)).unwrap();
        let key = eligible(&program, probe(), Some(&runtime)).unwrap();
        assert_eq!(
            read_data(&root, &key, Some(&runtime)).as_deref(),
            Some("qualified")
        );
        let other = RuntimeIdentity::from_prepared_install((0, 22, 6), "0123456789ab", &install)
            .unwrap()
            .with_seed(&seed)
            .unwrap();
        assert!(read_data(&root, &key, Some(&other)).is_none());
        assert!(eligible(&program, probe(), None).is_none());
        std::fs::write(&interpreter, b"\x7fELFnew interpreter").unwrap();
        let key = eligible(&program, probe(), Some(&runtime)).unwrap();
        assert!(
            read_data(&root, &key, Some(&runtime)).is_none(),
            "interpreter changes invalidate output"
        );
        for shebang in [
            "/usr/bin/env python".to_owned(),
            "/bin/sh".to_owned(),
            "python".to_owned(),
        ] {
            std::fs::write(&program, format!("#!{shebang}\nscript")).unwrap();
            assert!(
                eligible(&program, probe(), Some(&runtime)).is_none(),
                "{shebang}"
            );
        }
        let local = install.join("python");
        native(&local);
        std::fs::write(&program, format!("#!{}\nscript", local.display())).unwrap();
        assert!(eligible(&program, probe(), Some(&runtime)).is_some());
        let shims = seed.join("shims");
        std::fs::create_dir(&shims).unwrap();
        native(&shims.join("python"));
        std::fs::write(
            &program,
            format!("#!{}\nscript", shims.join("python").display()),
        )
        .unwrap();
        assert!(eligible(&program, probe(), Some(&runtime)).is_none());
    }

    #[test]
    fn strong_identity_rejects_incomplete_commit_or_non_directory() {
        let dir = tempfile::tempdir().unwrap();
        for commit in ["", "abcdef", "0123456789abcdef", "0123456789xy"] {
            assert!(
                RuntimeIdentity::from_prepared_install((0, 22, 5), commit, dir.path()).is_err()
            );
        }
        let file = dir.path().join("file");
        native(&file);
        assert!(RuntimeIdentity::from_prepared_install((0, 22, 5), "0123456789ab", &file).is_err());
    }
    #[test]
    fn probe_environment_skips_unresolved_runtime() {
        let values = [
            ("HOME", "{runtime}/probe-home"),
            ("HTTP_PROXY", "http://127.0.0.1:9"),
        ];
        assert_eq!(
            resolve_env(&values, None),
            vec![("HTTP_PROXY".into(), "http://127.0.0.1:9".into())]
        );
        assert_eq!(
            resolve_env(&values, Some(Path::new("/tmp/runtime")))[0],
            ("HOME".into(), "/tmp/runtime/probe-home".into())
        );
    }
}
