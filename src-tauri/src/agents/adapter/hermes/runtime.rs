// SPDX-License-Identifier: MIT
//! Pinned source-install preparation. No model turn or gateway is started here.
use codesense_pty_core::agent_cli::{
    adapter,
    version_cache::{self, LockedRuntime, RuntimeIdentity},
    AgentCli,
};
use sha2::{Digest, Sha256};
use std::os::unix::{
    fs::{symlink, PermissionsExt},
    process::CommandExt,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

mod lifecycle;
mod transport;
pub(crate) use lifecycle::{sweep, teardown_home};

#[derive(serde::Deserialize)]
struct Installation {
    source: PathBuf,
    interpreter: PathBuf,
    version: (u64, u64, u64),
    commit: String,
}

/// The selected entry must be an absolute-interpreter source installation. Metadata
/// discovery imports no bootstrap and runs with an isolated HOME, before any downloads.
fn discover(program: &Path, scratch: &Path) -> Result<Installation, String> {
    let text = fs::read_to_string(program).map_err(|e| e.to_string())?;
    let interpreter = text
        .lines()
        .next()
        .and_then(|s| s.strip_prefix("#!"))
        .map(str::trim)
        .filter(|s| Path::new(s).is_absolute() && !s.contains(char::is_whitespace))
        .ok_or("managed runtime requires an absolute Python shebang")?;
    if Path::new(interpreter)
        .file_name()
        .is_some_and(|s| s == "env")
    {
        return Err("managed runtime cannot prepare a version-manager dispatcher".into());
    }
    let code = r#"import importlib.util,json,pathlib
s=pathlib.Path(importlib.util.find_spec('hermes_cli').origin).resolve().parent.parent
assert (s/'.git').exists(), 'a pinned source install is required'
from hermes_cli.version_info import get_version_info
v=get_version_info()
print(json.dumps(dict(source=str(s),version=[int(x) for x in v.base_version.split('.')],commit=v.commit[:12])))"#;
    let output = run(
        Command::new(interpreter).args(["-c", code]),
        scratch,
        scratch,
        "discover",
        Duration::from_secs(15),
    )?;
    let mut value: serde_json::Value =
        serde_json::from_str(output.trim()).map_err(|e| format!("runtime metadata: {e}"))?;
    value["interpreter"] = interpreter.into();
    serde_json::from_value(value).map_err(|e| format!("runtime metadata: {e}"))
}

fn private_dir(path: &Path) -> Result<(), String> {
    crate::commands::paths::ensure_private_dir(path)
}

/// A bounded subprocess with a private log, controlled bootstrap home and shared caches.
/// Logs never receive inherited provider credentials or a user CLI home.
fn run(
    command: &mut Command,
    home: &Path,
    runtime: &Path,
    label: &str,
    timeout: Duration,
) -> Result<String, String> {
    private_dir(home)?;
    let log = runtime.join(format!("{label}.log"));
    let file = fs::File::create(&log).map_err(|e| e.to_string())?;
    let errors = runtime.join(format!("{label}.stderr.log"));
    let stderr = fs::File::create(&errors).map_err(|e| e.to_string())?;
    fs::set_permissions(&errors, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    fs::set_permissions(&log, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("HERMES_HOME", home)
        .env("HERMES_MANAGED_DIR", empty_managed_dir(home)?)
        .env("UV_CACHE_DIR", runtime.join("uv-cache"))
        .env("UV_PYTHON_INSTALL_DIR", runtime.join("python-install"))
        .env("UV_PYTHON_BIN_DIR", runtime.join("python-bin"))
        .env("npm_config_cache", runtime.join("npm-cache"))
        .stdin(Stdio::null())
        .stdout(file.try_clone().map_err(|e| e.to_string())?)
        .stderr(stderr)
        .current_dir(runtime)
        .process_group(0);
    if matches!(label, "bootstrap" | "prepare") {
        command.envs(transport::inherited());
    } else {
        // Metadata and version probes must not reach the network, including proxy bypasses.
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            command.env(name, "http://127.0.0.1:9");
        }
        command.env("NO_PROXY", "").env("no_proxy", "");
    }
    let mut child = command.spawn().map_err(|e| format!("{label}: {e}"))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            waited => {
                unsafe {
                    libc::kill(-(std::process::Child::id(&child) as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                break Err(match waited {
                    Err(e) => format!("wait failed: {e}"),
                    _ => format!("timed out after {timeout:?}"),
                });
            }
        }
    };
    let bytes = fs::read(&log).map_err(|e| e.to_string())?;
    let tail = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(4096)..]).into_owned();
    if status.as_ref().is_ok_and(|s| s.success()) {
        Ok(tail)
    } else {
        let errors = fs::read(errors).unwrap_or_default();
        let errors = String::from_utf8_lossy(&errors[errors.len().saturating_sub(4096)..]);
        let status = status.map_or_else(|e| e, |s| s.to_string());
        Err(format!(
            "runtime {label} failed ({status}; log {}): {tail}{errors}",
            Path::display(&log)
        ))
    }
}

fn empty_managed_dir(home: &Path) -> Result<std::path::PathBuf, String> {
    let managed = home.join("managed-policy");
    private_dir(&managed)?;
    if fs::read_dir(&managed).map_err(|e| e.to_string())?.next().is_some() {
        return Err("private managed-policy directory is not empty".into());
    }
    Ok(managed)
}

fn common_install(program: &Path, installation: &Installation) -> Result<PathBuf, String> {
    let program = fs::canonicalize(program).map_err(|e| e.to_string())?;
    let interpreter = fs::canonicalize(&installation.interpreter).map_err(|e| e.to_string())?;
    let mut root = fs::canonicalize(&installation.source).map_err(|e| e.to_string())?;
    while !program.starts_with(&root) || !interpreter.starts_with(&root) {
        if !root.pop() {
            return Err("installation paths do not share a pinned root".into());
        }
    }
    if root.parent().is_none() || dirs::home_dir().as_ref() == Some(&root) {
        return Err(
            "source, entry and interpreter must share a dedicated installation directory".into(),
        );
    }
    Ok(root)
}

pub(super) fn seed_home(home: &Path, program: &Path) -> Result<Vec<(String, String)>, String> {
    super::ensure_unmanaged_host()?;
    let root = crate::commands::paths::codesense_home_dir()
        .ok_or("home unavailable")?
        .join("runtimes/hermes");
    private_dir(&root)?;
    let discovery = root.join(format!("discover-{}", uuid::Uuid::new_v4()));
    private_dir(&discovery)?;
    let result = (|| {
        let installation = discover(program, &discovery)?;
        let install = common_install(program, &installation)?;
        let identity = RuntimeIdentity::from_prepared_install(
            installation.version,
            &installation.commit,
            &install,
        )
        .map_err(|e| e.to_string())?;
        prepare(
            &root,
            home,
            program,
            identity,
            &installation.source,
            |seed, runtime| {
                run(
                    Command::new(program).arg("--version"),
                    seed,
                    runtime,
                    "bootstrap",
                    Duration::from_secs(360),
                )?;
                let tui = installation
                    .source
                    .canonicalize()
                    .map_err(|e| e.to_string())?
                    .join("ui-tui");
                let code = "import hermes_bootstrap; from pathlib import Path; import sys; from hermes_cli.main_tui_launch import _make_tui_argv; _make_tui_argv(Path(sys.argv[1]),False)";
                run(
                    Command::new(&installation.interpreter)
                        .args(["-c", code])
                        .arg(&tui),
                    seed,
                    runtime,
                    "prepare",
                    Duration::from_secs(360),
                )?;
                Ok(())
            },
        )
    })();
    let _ = fs::remove_dir_all(discovery);
    result
}

/// The lock covers prepare, probe-home overlay, actual version probe and cache write.
fn prepare(
    root: &Path,
    home: &Path,
    program: &Path,
    identity: RuntimeIdentity,
    source: &Path,
    bootstrap: impl FnOnce(&Path, &Path) -> Result<(), String>,
) -> Result<Vec<(String, String)>, String> {
    private_dir(root)?;
    private_dir(home)?;
    let runtime = root.join(RuntimeIdentity::key(&identity));
    let mut locked =
        LockedRuntime::acquire(&runtime, identity.clone()).map_err(|e| e.to_string())?;
    private_dir(&runtime)?;
    let runtime = fs::canonicalize(&runtime).map_err(|e| e.to_string())?;
    let seed = runtime.join("seed");
    let tui = fs::canonicalize(source)
        .map_err(|e| e.to_string())?
        .join("ui-tui");
    if !runtime.join("READY").is_file() {
        if Path::exists(&seed) {
            make_writable(&seed).map_err(|e| e.to_string())?;
            fs::remove_dir_all(&seed).map_err(|e| e.to_string())?;
        }
        private_dir(&seed)?;
        bootstrap(&seed, &runtime)?;
        if !tui.join("dist/entry.js").is_file() {
            return Err("runtime prepare produced no ui-tui/dist/entry.js".into());
        }
        if !seed.join("tools").is_dir() || !seed.join("installs").is_dir() {
            return Err("runtime prepare produced no tools/installs".into());
        }
        freeze(&seed).map_err(|e| e.to_string())?;
        fs::write(
            runtime.join("READY"),
            manifest(&seed).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    if !tui.join("dist/entry.js").is_file() {
        return Err("prepared TUI entry is missing".into());
    }
    // Adding the immutable seed leaves the identity's runtime key unchanged.
    locked.set_seed(&seed).map_err(|e| e.to_string())?;
    let probe_home = runtime.join("probe-home");
    overlay(&seed, &probe_home).map_err(|e| e.to_string())?;
    let probe = adapter(AgentCli::Hermes).probe();
    if version_cache::read(program, probe, Some(LockedRuntime::identity(&locked))).is_none() {
        let raw = run(
            Command::new(program).arg("--version"),
            &probe_home,
            &runtime,
            "version",
            Duration::from_secs(15),
        )?;
        if !adapter(AgentCli::Hermes)
            .descriptor()
            .release
            .qualified(&raw)
        {
            return Err("prepared runtime version is outside the qualified release".into());
        }
        version_cache::write(program, &raw, probe, Some(LockedRuntime::identity(&locked)))
            .map_err(|e| e.to_string())?;
    }
    overlay(&seed, home).map_err(|e| e.to_string())?;
    locked.bind_home(home).map_err(|e| e.to_string())?;
    Ok(vec![
        ("HERMES_MANAGED_DIR".into(), empty_managed_dir(home)?.to_string_lossy().into_owned()),
        ("HERMES_TUI_DIR".into(), tui.to_string_lossy().into_owned()),
        (
            "npm_config_cache".into(),
            runtime.join("npm-cache").to_string_lossy().into_owned(),
        ),
        (
            "UV_CACHE_DIR".into(),
            runtime.join("uv-cache").to_string_lossy().into_owned(),
        ),
    ])
}

fn overlay(seed: &Path, home: &Path) -> io::Result<()> {
    fs::create_dir_all(home)?;
    fs::set_permissions(home, fs::Permissions::from_mode(0o700))?;
    let tools = home.join("tools");
    if !Path::exists(&tools) {
        symlink(seed.join("tools"), &tools)?;
    }
    fn copy(source: &Path, target: &Path, metadata: bool) -> io::Result<()> {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let path = fs::DirEntry::path(&entry);
            let dest = target.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                let mutable = metadata
                    || ["bootstrap", ".leases", ".locks"]
                        .iter()
                        .any(|name| entry.file_name() == *name);
                copy(&path, &dest, mutable)?;
            } else if !Path::exists(&dest) {
                // PM opens locks with a+b even when the generation is already current.
                // Lease records and JSON/bootstrap state are writable in both overlays.
                let name = entry.file_name();
                if metadata
                    || name.to_string_lossy().ends_with(".lock")
                    || name == ".lease-managed"
                    || Path::extension(&path).is_some_and(|s| s == "json")
                {
                    fs::copy(&path, &dest)?;
                    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;
                } else if kind.is_symlink() {
                    symlink(fs::read_link(&path)?, &dest)?;
                } else {
                    symlink(&path, &dest)?;
                }
            }
        }
        Ok(())
    }
    copy(&seed.join("installs"), &home.join("installs"), false)
}

fn visit(path: &Path, f: &mut impl FnMut(&Path) -> io::Result<()>) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        for e in fs::read_dir(path)? {
            visit(&fs::DirEntry::path(&e?), f)?;
        }
    }
    f(path)
}
fn freeze(path: &Path) -> io::Result<()> {
    visit(path, &mut |p| {
        let m = fs::symlink_metadata(p)?;
        if !m.file_type().is_symlink() {
            fs::set_permissions(
                p,
                fs::Permissions::from_mode(m.permissions().mode() & !0o222),
            )?;
        }
        Ok(())
    })
}
fn make_writable(path: &Path) -> io::Result<()> {
    // Directories must become writable before their children can be removed.
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() {
        return Ok(());
    }
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(m.permissions().mode() | 0o700),
    )?;
    if m.is_dir() {
        for e in fs::read_dir(path)? {
            make_writable(&fs::DirEntry::path(&e?))?;
        }
    }
    Ok(())
}
fn manifest(seed: &Path) -> io::Result<String> {
    let mut paths = Vec::new();
    visit(seed, &mut |p| {
        paths.push(p.to_owned());
        Ok(())
    })?;
    paths.sort();
    let mut hash = Sha256::new();
    for path in paths {
        hash.update(
            path.strip_prefix(seed)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes(),
        );
        let m = fs::symlink_metadata(&path)?;
        if m.file_type().is_symlink() {
            hash.update(fs::read_link(path)?.as_os_str().as_encoded_bytes());
        } else if m.is_file() {
            hash.update(fs::read(path)?);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
pub(crate) mod tests;
