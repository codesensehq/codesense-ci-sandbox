// SPDX-License-Identifier: MIT
use super::*;
use crate::routines::test_support::TempHome;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

pub(crate) struct Fixture {
    home: TempHome,
    source: PathBuf,
    program: PathBuf,
    root: PathBuf,
    identity: RuntimeIdentity,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let home = TempHome::new();
        let install = home.root().join("install");
        fs::create_dir(&install).unwrap();
        let source = install.join("source");
        fs::create_dir_all(source.join("ui-tui/dist")).unwrap();
        fs::write(source.join("ui-tui/dist/entry.js"), "bundle").unwrap();
        let interpreter = install.join("python");
        fs::copy("/bin/sh", &interpreter).unwrap();
        // macOS refuses a relocated platform binary until it has a local signature.
        #[cfg(target_os = "macos")]
        assert!(std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&interpreter)
            .output()
            .unwrap()
            .status
            .success());
        let program = install.join("agent");
        fs::write(&program, format!("#!{}\nprintf 'Hermes Agent v0.22.5 local 0123456789ab\\n'\nprintf 'probe\\n' >> '{}'\n", interpreter.display(), install.join("versions").display())).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let root = home.codesense_home().join("runtimes/hermes");
        let identity =
            RuntimeIdentity::from_prepared_install((0, 22, 5), "0123456789ab", &install).unwrap();
        Self {
            home,
            source,
            program,
            root,
            identity,
        }
    }
    pub(crate) fn isolate_xdg_config_home(&mut self) {
        self.home.isolate_xdg_config_home();
    }

    pub(crate) fn root(&self) -> &Path {
        self.home.root()
    }

    /// Only the selected CLI installation is stubbed; managed prepare and cache stay real.
    pub(crate) fn managed_program(&self) -> PathBuf {
        let interpreter = self.program.parent().unwrap().join("fixture-interpreter");
        let metadata = serde_json::json!({"source": self.source, "version": [0,22,5], "commit": "0123456789ab"});
        // A shebang interpreter must itself be native on Darwin; a shell-script
        // interpreter silently falls through without bootstrapping this fixture.
        let source = self.program.parent().unwrap().join("fixture-interpreter.c");
        let metadata_literal = serde_json::to_string(&metadata.to_string()).unwrap();
        let prepares_literal = serde_json::to_string(
            &self
                .program
                .parent()
                .unwrap()
                .join("prepares")
                .to_string_lossy(),
        )
        .unwrap();
        fs::write(
            &source,
            format!(
                r#"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
static int directory(const char *path) {{ return mkdir(path, 0700) == 0 || errno == EEXIST; }}
int main(int argc, char **argv) {{
    if (argc > 2 && strcmp(argv[1], "-c") == 0) {{
        if (strstr(argv[2], "find_spec")) {{ puts({metadata_literal}); return 0; }}
        FILE *log = fopen({prepares_literal}, "a");
        if (!log) return 1;
        fputs("prepared\n", log); return fclose(log) != 0;
    }}
    const char *home = getenv("HERMES_HOME");
    if (!home) return 2;
    char path[4096];
    const char *directories[] = {{"tools", "installs", "installs/release"}};
    for (unsigned i = 0; i < 3; ++i) {{
        if (snprintf(path, sizeof(path), "%s/%s", home, directories[i]) >= sizeof(path)
            || !directory(path)) return 3;
    }}
    puts("Hermes Agent v0.22.5 local 0123456789ab");
    return 0;
}}
"#
            ),
        )
        .unwrap();
        let compiled = std::process::Command::new("cc")
            .arg(&source)
            .arg("-o")
            .arg(&interpreter)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "fixture dispatcher: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let program = self.program.parent().unwrap().join("managed-agent");
        fs::write(&program, format!("#!{}\n", interpreter.display())).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        program
    }

    fn seed(&self, home: &Path, count: &AtomicUsize) -> Result<Vec<(String, String)>, String> {
        prepare(
            &self.root,
            home,
            &self.program,
            self.identity.clone(),
            &self.source,
            |seed, _| {
                count.fetch_add(1, Ordering::SeqCst);
                fs::create_dir_all(seed.join("tools/python")).unwrap();
                fs::write(seed.join("tools/python/bin"), "immutable").unwrap();
                fs::create_dir_all(seed.join("installs/release/bootstrap")).unwrap();
                for name in [
                    "pm-runtime/generations/current/.leases",
                    "pm-runtime/.locks",
                ] {
                    fs::create_dir_all(seed.join("installs/release").join(name)).unwrap();
                }
                for name in [
                    "pm-runtime/.prepare.lock",
                    "pm-runtime/generations/current/.lock",
                    "pm-runtime/generations/current/.lease-managed",
                    "pm-runtime/generations/current/.leases/existing",
                    "pm-runtime/.locks/existing",
                    "pm-runtime/selected.json",
                ] {
                    fs::write(seed.join("installs/release").join(name), "metadata").unwrap();
                }

                for (name, text) in [
                    ("payload", "data"),
                    ("state.json", "{}"),
                    (".install.lock", ""),
                    ("bootstrap/state", "state"),
                ] {
                    fs::write(seed.join("installs/release").join(name), text).unwrap();
                }
                Ok(())
            },
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.root.exists() {
            make_writable(&self.root).unwrap();
        }
    }
}

#[test]
fn hermes_absent_admin_policy_pins_empty_private_directory() {
    let fixture = Fixture::new();
    let home = fixture.root().join("managed-session");
    let env = fixture.seed(&home, &AtomicUsize::new(0)).unwrap();
    let values: Vec<_> = env.iter().filter(|(name, _)| name == "HERMES_MANAGED_DIR").collect();
    assert_eq!(values.len(), 1);
    let managed = PathBuf::from(&values[0].1);
    assert_eq!(managed, home.join("managed-policy"));
    assert!(fs::read_dir(managed).unwrap().next().is_none());
}

#[test]
fn three_cold_launches_prepare_once_and_keep_writable_metadata_private() {
    let fixture = Fixture::new();
    let count = Arc::new(AtomicUsize::new(0));
    let homes: Vec<_> = (0..3)
        .map(|i| fixture.home.root().join(format!("session-{i}")))
        .collect();
    std::thread::scope(|threads| {
        let mut jobs = Vec::new();
        for home in &homes {
            let count = count.clone();
            let fixture = &fixture;
            jobs.push(threads.spawn(move || fixture.seed(home, &count).unwrap()));
        }
        for job in jobs {
            let env = job.join().unwrap();
            assert_eq!(env.len(), 4);
            let managed = PathBuf::from(&env.iter().find(|(name, _)| name == "HERMES_MANAGED_DIR").unwrap().1);
            assert!(homes.iter().any(|home| managed == home.join("managed-policy")));
            assert!(fs::read_dir(managed).unwrap().next().is_none());
        }
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let runtime = fixture.root.join(fixture.identity.key());
    let seed = runtime.join("seed").canonicalize().unwrap();
    let before = manifest(&seed).unwrap();
    assert_eq!(fs::read_to_string(runtime.join("READY")).unwrap(), before);
    assert!(runtime
        .join("probe-home/installs/release/.install.lock")
        .is_file());
    for (index, home) in homes.iter().enumerate() {
        assert_eq!(
            fs::read_link(home.join("tools")).unwrap(),
            seed.join("tools")
        );
        assert!(fs::symlink_metadata(home.join("installs/release/payload"))
            .unwrap()
            .file_type()
            .is_symlink());
        for name in ["state.json", ".install.lock", "bootstrap/state"] {
            assert!(
                !fs::symlink_metadata(home.join("installs/release").join(name))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        fs::write(home.join("installs/release/state.json"), index.to_string()).unwrap();
        assert_eq!(
            fs::metadata(home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let locked = LockedRuntime::for_home(home).unwrap();
        assert!(version_cache::read(
            &fixture.program,
            adapter(AgentCli::Hermes).probe(),
            Some(locked.identity())
        )
        .is_some());
    }
    assert_eq!(manifest(&seed).unwrap(), before);
    // The actual executable ran once, in the seeded probe home, then cache hits served the contenders.
    assert_eq!(
        fs::read_to_string(fixture.program.parent().unwrap().join("versions"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let fourth = fixture.home.root().join("restart");
    fixture.seed(&fourth, &count).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(super::super::desktop().seed_home.is_some());
    // Only Hermes's managed home is seeded.
    for cli in AgentCli::ALL {
        assert_eq!(
            crate::agents::adapter::desktop(cli).seed_home.is_some(),
            cli == AgentCli::Hermes,
            "{cli}"
        );
    }
}

#[test]
fn prepare_failure_reports_log_tail_and_never_publishes_ready() {
    let fixture = Fixture::new();
    let home = fixture.home.root().join("failed");
    let result = prepare(
        &fixture.root,
        &home,
        &fixture.program,
        fixture.identity.clone(),
        &fixture.source,
        |seed, runtime| {
            run(
                Command::new("/bin/sh").args(["-c", "printf PREPARE_FAILED; exit 7"]),
                seed,
                runtime,
                "prepare",
                Duration::from_secs(2),
            )
            .map(|_| ())
        },
    );
    let reason = result.unwrap_err();
    assert!(reason.contains("PREPARE_FAILED"));
    assert!(reason.contains("prepare.log"));
    assert!(!fixture
        .root
        .join(fixture.identity.key())
        .join("READY")
        .exists());
    assert!(!home.join(version_cache::RUNTIME_LINK).exists());
    let count = AtomicUsize::new(0);
    fixture.seed(&home, &count).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn missing_tui_bundle_fails_before_ready_and_can_retry() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.source.join("ui-tui/dist/entry.js")).unwrap();
    let count = AtomicUsize::new(0);
    let home = fixture.home.root().join("session");
    assert!(fixture
        .seed(&home, &count)
        .unwrap_err()
        .contains("entry.js"));
    assert!(!fixture
        .root
        .join(fixture.identity.key())
        .join("READY")
        .exists());
    fs::write(fixture.source.join("ui-tui/dist/entry.js"), "bundle").unwrap();
    fixture.seed(&home, &count).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn canonical_install_alias_has_same_runtime_key_and_version_probe_uses_overlay() {
    let fixture = Fixture::new();
    let alias = fixture.home.root().join("alias");
    symlink(fixture.program.parent().unwrap(), &alias).unwrap();
    let identity =
        RuntimeIdentity::from_prepared_install((0, 22, 5), "0123456789ab", &alias).unwrap();
    assert_eq!(identity.key(), fixture.identity.key());
    let home = fixture.home.root().join("session");
    fixture.seed(&home, &AtomicUsize::new(0)).unwrap();
    let locked = LockedRuntime::for_home(&home).unwrap();
    let probe_home = locked.root().join("probe-home");
    assert!(probe_home.join("tools").is_symlink());
    assert!(probe_home.join("installs/release/.install.lock").is_file());
    assert!(
        version_cache::read(&fixture.program, adapter(AgentCli::Hermes).probe(), None).is_none()
    );
    let other = RuntimeIdentity::from_prepared_install(
        (0, 22, 6),
        "0123456789ab",
        fixture.program.parent().unwrap(),
    )
    .unwrap();
    assert!(version_cache::read(
        &fixture.program,
        adapter(AgentCli::Hermes).probe(),
        Some(&other)
    )
    .is_none());
}

#[test]
fn prepared_home_reaches_direct_qualification_without_another_version_spawn() {
    let fixture = Fixture::new();
    let home = fixture.home.root().join("direct");
    fixture.seed(&home, &AtomicUsize::new(0)).unwrap();
    fs::write(home.join("config.yaml"), "{}\n").unwrap();
    let prepared = codesense_pty_core::activity::prepare_direct(
        AgentCli::Hermes,
        fixture.program.as_os_str(),
        &[],
        "/bin/true",
        Some(home.clone()),
        None,
    );
    assert!(prepared.arguments.is_some_and(|(_, qualified)| qualified));
    let config: serde_yaml::Value =
        serde_yaml::from_slice(&fs::read(home.join("config.yaml")).unwrap()).unwrap();
    assert!(config["hooks"].is_mapping());
    assert_eq!(
        fs::read_to_string(fixture.program.parent().unwrap().join("versions"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let live = fixture.home.root().join("live");
    fs::create_dir(&live).unwrap();
    fs::write(live.join("config.yaml"), "{}\n").unwrap();
    let prepared = codesense_pty_core::activity::prepare_direct(
        AgentCli::Hermes,
        fixture.program.as_os_str(),
        &[],
        "/bin/true",
        Some(live),
        None,
    );
    assert!(prepared.arguments.is_some_and(|(_, qualified)| qualified));
    assert_eq!(
        fs::read_to_string(fixture.program.parent().unwrap().join("versions"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[test]
fn every_upstream_runtime_lock_and_lease_is_writable_without_changing_the_seed() {
    use std::io::Write;
    let fixture = Fixture::new();
    let home = fixture.home.root().join("metadata");
    fixture.seed(&home, &AtomicUsize::new(0)).unwrap();
    let runtime = fixture.root.join(fixture.identity.key());
    let seed = runtime.join("seed");
    let before = manifest(&seed).unwrap();
    for overlay_home in [&home, &runtime.join("probe-home")] {
        for name in [
            "pm-runtime/.prepare.lock",
            "pm-runtime/generations/current/.lock",
            "pm-runtime/generations/current/.lease-managed",
            "pm-runtime/generations/current/.leases/existing",
            "pm-runtime/.locks/existing",
            "pm-runtime/selected.json",
        ] {
            let path = overlay_home.join("installs/release").join(name);
            let mut file = fs::OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .open(&path)
                .expect("upstream a+b/r+ metadata open must succeed");
            file.write_all(b"private write").unwrap();
            assert!(
                !fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "{}",
                path.display()
            );
        }
    }
    assert_eq!(manifest(&seed).unwrap(), before);
}

#[test]
fn preparation_transport_classes_match_case_normalization_and_keep_probes_offline() {
    struct Restore(Vec<(String, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
    let home = TempHome::new();
    let mut settings = std::collections::BTreeMap::new();
    for name in transport::EXACT_ENV.iter().chain(transport::UV_ENV) {
        settings.insert((*name).to_owned(), "transport-value");
    }
    for key in transport::NPM_KEYS {
        for name in [
            format!("npm_config_{key}"),
            format!("npm_config_{}", key.replace('-', "_")),
            format!("NPM_CONFIG_{}", key.replace('-', "_").to_ascii_uppercase()),
            format!("NpM_CoNfIg_{key}"),
        ] {
            settings.insert(name, "transport-value");
        }
    }
    let excluded = [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "AZURE_OPENAI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "HF_TOKEN",
        "MISTRAL_API_KEY",
        "GROQ_API_KEY",
        "HERMES_API_KEY",
        "UV_INDEX_URL",
        "UV_INDEX_USERNAME",
        "UV_INDEX_PASSWORD",
        "UV_EXTRA_INDEX_URL",
        "UV_KEYRING_PROVIDER",
        "NPM_TOKEN",
        "npm_config__authToken",
        "NPM_CONFIG__AUTHTOKEN",
        "npm_config_registry",
        "NpM_CoNfIg_ReGiStRy",
        "npm_config_//registry.invalid/:_authToken",
        "npm_config_cafile_extra",
        "UV_NATIVE_TLS_EXTRA",
        "GIT_SSL_CAINFO",
        "PIP_CERT",
    ];
    let restore = Restore(
        settings
            .keys()
            .map(String::as_str)
            .chain(excluded)
            .map(|key| (key.to_owned(), std::env::var_os(key)))
            .collect(),
    );
    for (key, value) in &settings {
        std::env::set_var(key, value);
    }
    for key in excluded {
        std::env::set_var(key, "never-inherit-this-value");
    }
    for label in ["bootstrap", "prepare", "discover", "version"] {
        let output = run(
            &mut Command::new("/usr/bin/env"),
            home.root(),
            home.root(),
            label,
            Duration::from_secs(2),
        )
        .unwrap();
        let child: std::collections::BTreeMap<_, _> = output
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        for (key, value) in &settings {
            if label == "bootstrap" || label == "prepare" {
                assert_eq!(
                    child.get(key.as_str()).copied(),
                    Some(*value),
                    "{label}: {key}"
                );
            } else {
                assert_ne!(
                    child.get(key.as_str()).copied(),
                    Some(*value),
                    "{label}: inherited {key}"
                );
                match key.as_str() {
                    "HTTP_PROXY" | "HTTPS_PROXY" | "http_proxy" | "https_proxy" => {
                        assert_eq!(child.get(key.as_str()).copied(), Some("http://127.0.0.1:9"))
                    }
                    "NO_PROXY" | "no_proxy" => {
                        assert_eq!(child.get(key.as_str()).copied(), Some(""))
                    }
                    _ => assert!(!child.contains_key(key.as_str()), "{label}: {key}"),
                }
            }
        }
        for key in excluded {
            assert!(!child.contains_key(key), "{label}: {key}");
        }
    }
    drop(restore);
}
