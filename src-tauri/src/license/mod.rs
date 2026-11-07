// SPDX-License-Identifier: MIT
//! The beta channels (`cs_channel = "beta"` or `"beta-staging"`); internal builds have none of
//! this code. SV-PBF-278: nothing here gates the app any more. The only automatic calls are the
//! update tick's read of the channel's static manifest and the download it can start
//! (`static_channel.rs`, `updates.rs`), both anonymous.
//!
//! The license service and its parts (activation, the daily check, the deep link, the
//! required-update deadline) are gone. What an older build stored is removed once, locally
//! (`static_channel::remove_license_records`).

pub mod commands;
pub mod fallback;
pub mod keychain;
pub mod scheduler;
pub mod static_channel;
pub mod updates;
pub mod view;

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use self::static_channel::{AppCheck, StaticChannel, LICENSE_SERVICES};
use crate::commands::credential_store::{CredentialStore, KeychainStore};

/// Where the app calls: the base URL.
#[derive(Debug, PartialEq, Eq)]
pub struct Host {
    pub base: String,
}

/// Resolves the updates host. The build-time host is the only one, except in a
/// dev build, where a `CODESENSE_UPDATES_HOST` override may name a local
/// `wrangler dev`. Plain HTTP is allowed only to localhost. A non-dev build
/// ignores the override.
pub fn resolve_host(
    build_host: &str,
    override_host: Option<&str>,
    dev_build: bool,
) -> Result<Host, String> {
    match override_host.map(str::trim).filter(|o| !o.is_empty()) {
        Some(o) if dev_build => {
            let (scheme, rest) = if let Some(rest) = o.strip_prefix("https://") {
                ("https", rest)
            } else if let Some(rest) = o.strip_prefix("http://") {
                ("http", rest)
            } else {
                return Err("CODESENSE_UPDATES_HOST must start with https:// or http://".into());
            };
            let host = rest.trim_end_matches('/');
            let name = host.split(':').next().unwrap_or("");
            if scheme == "http" && name != "127.0.0.1" && name != "localhost" {
                return Err("plain HTTP is allowed only to localhost".into());
            }
            if host.is_empty() {
                return Err("CODESENSE_UPDATES_HOST names no host".into());
            }
            Ok(Host {
                base: format!("{scheme}://{host}"),
            })
        }
        _ => Ok(Host {
            base: format!("https://{build_host}"),
        }),
    }
}

/// The override from the environment. Only the staging channel reads it.
#[cfg(cs_channel = "beta-staging")]
fn env_override() -> Option<String> {
    std::env::var("CODESENSE_UPDATES_HOST").ok()
}

#[cfg(cs_channel = "beta")]
fn env_override() -> Option<String> {
    None
}

/// The channel's download host: where its static update manifest and its builds live. The one
/// place a channel names it; a dev build of the staging channel may override it with
/// `CODESENSE_UPDATES_HOST` (`resolve_host`). The staging channel's host comes from its channel
/// config at build time (build.rs), so no source names it.
#[cfg(cs_channel = "beta")]
pub const DOWNLOAD_HOST: &str = "dl.codesense.ai";
#[cfg(cs_channel = "beta-staging")]
pub const DOWNLOAD_HOST: &str = env!("CODESENSE_DOWNLOAD_HOST");

/// The host this build calls, after the dev-only override rules.
fn resolve_for_build() -> Result<Host, String> {
    let dev_build = cfg!(debug_assertions);
    let override_host = env_override();
    if override_host.is_some() && !dev_build {
        log::warn!(target: "license", "CODESENSE_UPDATES_HOST is ignored outside dev builds");
    }
    resolve_host(DOWNLOAD_HOST, override_host.as_deref(), dev_build)
}

/// The shortest check age a release staging build takes from `CODESENSE_CHECK_INTERVAL_SECS`,
/// so a mistyped value cannot flood the staging host. Unused by the production channel, whose
/// tests still cover it.
#[cfg_attr(cs_channel = "beta", allow(dead_code))]
pub const MIN_RELEASE_CHECK_INTERVAL_SECS: u64 = 60;

/// The check age: a day, unless `value` names a positive number of seconds. A debug build takes
/// it as given; a release build takes at least `MIN_RELEASE_CHECK_INTERVAL_SECS`. Only the
/// staging channel calls this: the production channel's `check_interval` never reads the
/// variable, so it always checks daily.
#[cfg_attr(cs_channel = "beta", allow(dead_code))]
pub fn interval_from_env(value: Option<&str>, dev_build: bool) -> Duration {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map_or(scheduler::DEFAULT_MAX_AGE, |secs| {
            let secs = if dev_build {
                secs
            } else {
                secs.max(MIN_RELEASE_CHECK_INTERVAL_SECS)
            };
            Duration::from_secs(secs)
        })
}

/// `CODESENSE_CHECK_INTERVAL_SECS` in beta-staging, debug or release (SW-PB-15 shows a
/// staged N+1 within minutes on a notarized staging build), subject to `interval_from_env`.
#[cfg(cs_channel = "beta-staging")]
fn check_interval() -> Duration {
    interval_from_env(
        std::env::var("CODESENSE_CHECK_INTERVAL_SECS")
            .ok()
            .as_deref(),
        cfg!(debug_assertions),
    )
}

/// The production channel always checks daily. It never reads `CODESENSE_CHECK_INTERVAL_SECS`.
#[cfg(cs_channel = "beta")]
fn check_interval() -> Duration {
    scheduler::DEFAULT_MAX_AGE
}

/// SW-PB-23: installs the staged build and relaunches. The gate (the latest manifest naming this
/// build, a location this user can replace, and the bundle checks the staged bytes passed) is
/// asked first; then the app exits through its usual path, so the exit coordinator drains the
/// remote connections and the terminal servers keep their sessions. The exit it allows installs
/// the build and relaunches (`lib.rs`). `refresh` tells the UI again when the gate refuses.
/// Returns the version.
pub(crate) fn restart_to_update<R: tauri::Runtime>(
    app: &AppHandle<R>,
    refresh: &dyn Fn(),
    updater: &updates::Updates,
) -> Result<String, &'static str> {
    let version = match updater.arm_restart(false) {
        Ok(version) => version,
        Err(refused) => {
            // The UI drops the button and shows the install-by-hand line where that applies.
            refresh();
            return Err(refused.message());
        }
    };
    log::info!(target: "license", "restarting to install update {version}, as asked");
    app.exit(0);
    Ok(version)
}

/// How many headless (no-terminal) agent runs are going in this app. A restart ends them.
pub(crate) fn headless_runs<R: tauri::Runtime>(app: &AppHandle<R>) -> usize {
    app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
        .map_or(0, |state| state.running_headless_count())
}

/// SHA-256 (hex) of this build's channel updater key and of the internal build's key, from
/// `build.rs`. Hashes, not the configs: a beta binary must carry no updater key but its own,
/// which Tauri embeds from the merged config (the release script checks that).
const CHANNEL_KEY_SHA256: &str = env!("CODESENSE_CHANNEL_UPDATER_KEY_SHA256");
const INTERNAL_KEY_SHA256: &str = env!("CODESENSE_INTERNAL_UPDATER_KEY_SHA256");

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether this build may install updates: the key it verifies them with must be its own
/// channel's key, never the internal key, and the updater must require the signed version. A
/// beta build made without its channel config carries the internal key and no
/// `requireSignedVersion`, and gets no updates.
/// `channel_sha256` and `internal_sha256` are the keys' SHA-256 hashes (hex).
pub fn updates_key_ok(
    runtime: Option<&str>,
    channel_sha256: &str,
    internal_sha256: &str,
    require_signed_version: bool,
) -> bool {
    let Some(runtime) = runtime else {
        return false;
    };
    let runtime = sha256_hex(runtime);
    require_signed_version && runtime == channel_sha256 && runtime != internal_sha256
}

/// SV-PBF-278: builds the updater and the static channel, manages them for the commands,
/// removes the license records an older build stored, and starts the update tick. Builds no
/// license service, license client, deep-link handler or daily license check. Called once from
/// the app's setup.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let running = env!("CARGO_PKG_VERSION").to_string();
    let app_dir = tauri::Manager::path(app)
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?;
    let host = resolve_for_build()?;
    let max_age = check_interval();
    let records_dir = app_dir.join("updates");
    let updater = Arc::new(
        updates::Updates::new(&host.base, running.clone(), max_age)?
            .with_marker_dir(records_dir.clone()),
    );
    let updater_config = app.config().plugins.0.get("updater").cloned();
    let runtime_key = updater_config
        .as_ref()
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .map(str::to_string);
    let require_signed_version = updater_config
        .as_ref()
        .and_then(|u| u.get("requireSignedVersion"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let key_ok = updates_key_ok(
        runtime_key.as_deref(),
        CHANNEL_KEY_SHA256,
        INTERNAL_KEY_SHA256,
        require_signed_version,
    );
    let emitter = app.clone();
    let channel = StaticChannel::new(
        &host.base,
        running,
        updater.clone(),
        !key_ok,
        Box::new(move |view| {
            let _ = emitter.emit(view::EVENT, view);
        }),
    )?;
    app.manage(updater);
    app.manage(channel.clone());
    // Keychain calls can block (a locked Keychain asks), so the cleanup runs off the setup.
    tauri::async_runtime::spawn_blocking(move || {
        let stores = LICENSE_SERVICES.map(KeychainStore::new);
        let stores: Vec<(&str, &dyn CredentialStore)> = LICENSE_SERVICES
            .iter()
            .zip(&stores)
            .map(|(service, store)| (*service, store as &dyn CredentialStore))
            .collect();
        static_channel::remove_license_records(&stores, &app_dir, &records_dir);
    });
    if key_ok {
        let checker = Arc::new(AppCheck {
            app: app.clone(),
            channel,
        });
        tauri::async_runtime::spawn(static_channel::run_ticks(checker, max_age));
    } else {
        log::warn!(target: "license", "updates are off: this build's updater config is not its channel's (build with its channel config)");
    }
    Ok(())
}

#[cfg(all(test, not(cs_channel = "internal")))]
mod channel_tests {
    use super::*;

    /// The GitHub release path in `commands/update.rs` is internal-only: each of its
    /// items carries the internal cfg, and so do its command registrations, so no
    /// beta build compiles or exposes it.
    #[test]
    fn github_update_path_is_compiled_out_of_beta() {
        let update = include_str!("../commands/update.rs");
        for item in [
            "fn fetch_release",
            "async fn resolve_asset_download_url",
            "pub async fn check_for_update",
            "pub async fn download_and_install_update",
        ] {
            let at = update
                .find(item)
                .unwrap_or_else(|| panic!("{item} is in update.rs"));
            let above: Vec<&str> = update[..at].lines().rev().take(12).collect();
            assert!(
                above
                    .iter()
                    .any(|l| l.trim() == "#[cfg(cs_channel = \"internal\")]"),
                "{item} must carry the internal cfg"
            );
        }
        let lib = include_str!("../lib.rs");
        for command in [
            "commands::update::check_for_update,",
            "commands::update::download_and_install_update,",
        ] {
            let at = lib
                .find(command)
                .unwrap_or_else(|| panic!("{command} is registered"));
            assert!(lib[..at]
                .lines()
                .rev()
                .take(2)
                .any(|l| l.trim() == "#[cfg(cs_channel = \"internal\")]"));
        }
    }

    /// SW-PB-13: the updater plugin is registered in every channel, once and with no cfg, so
    /// the beta updater can build its updater at runtime. The webview still cannot drive it:
    /// the default capability grants no updater permission. In beta builds the only caller is
    /// `license/updates.rs`, which names the static manifest on the download host. The plugin's
    /// config endpoint list is empty and both callers set their endpoint at runtime; the GitHub
    /// commands stay internal-only.
    #[test]
    fn the_updater_plugin_is_registered_in_every_channel_and_beta_reaches_it_only_through_the_gated_route(
    ) {
        let lib = include_str!("../lib.rs");
        assert_eq!(lib.matches("tauri_plugin_updater::Builder").count(), 1);
        let at = lib.find("tauri_plugin_updater::Builder").unwrap();
        assert!(
            !lib[..at]
                .lines()
                .rev()
                .take(3)
                .any(|l| l.trim().starts_with("#[cfg(")),
            "the plugin is registered in every channel"
        );
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut scanned = 0;
        for entry in std::fs::read_dir(&dir).expect("the capabilities folder") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let caps = std::fs::read_to_string(&path).unwrap();
            assert!(
                !caps.contains("updater:"),
                "{} grants an updater permission",
                path.display()
            );
            scanned += 1;
        }
        assert!(scanned > 0, "no capability files under {}", dir.display());
        let callers: Vec<&str> = [
            ("license/mod.rs", include_str!("mod.rs")),
            ("license/scheduler.rs", include_str!("scheduler.rs")),
            ("license/commands.rs", include_str!("commands.rs")),
            ("license/updates.rs", include_str!("updates.rs")),
            (
                "license/static_channel.rs",
                include_str!("static_channel.rs"),
            ),
        ]
        .into_iter()
        .filter(|(_, src)| src.contains(concat!("updater", "_builder()")))
        .map(|(name, _)| name)
        .collect();
        assert_eq!(callers, ["license/updates.rs"]);
        assert!(include_str!("updates.rs").contains("/v1/update/"));
    }

    /// SW-PB-13 AC-4 and SW-PB-23: the staged build installs on `ExitRequested`, once the exit
    /// coordinator allows it, and the app relaunches only from there, only when that exit was
    /// armed. Only Restart to update (a command) arms it; nothing else in the license module
    /// restarts or relaunches the app.
    #[test]
    fn the_staged_build_installs_when_the_exit_is_allowed_and_only_an_armed_exit_relaunches() {
        let lib = include_str!("../lib.rs");
        let allow = lib
            .find("exit_coordinator::ExitDisposition::Allow =>")
            .expect("the exit coordinator's allow arm");
        // The arm runs to the first closing brace at its own indent.
        let indent = lib[..allow].rsplit('\n').next().unwrap();
        let close = format!("\n{indent}}}");
        let end = lib[allow..].find(&close).expect("the arm's end");
        let arm = &lib[allow..allow + end];
        assert!(arm.contains("if install_staged_update(app)"), "{arm}");
        assert!(arm.contains("relaunch_after_update(app)"), "{arm}");
        // The one restart call in lib.rs is the relaunch after an allowed, armed exit.
        assert_eq!(lib.matches(".restart()").count(), 1);
        assert_eq!(lib.matches("relaunch_after_update(app)").count(), 1);
        let relaunch = lib.find("fn relaunch_after_update").unwrap();
        assert!(lib[relaunch..].find(".restart()").unwrap() < lib[relaunch..].find("\n}").unwrap());
        // The relaunch follows the install's own decision: `finish_exit` relaunches only an armed exit.
        assert!(
            lib.contains("plan.relaunch\n}"),
            "install_staged_update returns the plan's relaunch"
        );
        for (name, src) in [
            ("updates.rs", include_str!("updates.rs")),
            ("static_channel.rs", include_str!("static_channel.rs")),
            ("scheduler.rs", include_str!("scheduler.rs")),
            ("commands.rs", include_str!("commands.rs")),
        ] {
            let src = src.find("#[cfg(all(test").map_or(src, |at| &src[..at]);
            for call in [".restart()", ".request_restart()", "relaunch()", ".exit("] {
                assert!(!src.contains(call), "{name} calls {call}");
            }
        }
        // The exit that arms a relaunch is asked in one place, after the gate.
        let here = include_str!("mod.rs");
        let shipped = &here[..here.find("#[cfg(all(test").unwrap()];
        assert_eq!(shipped.matches("app.exit(0)").count(), 1);
        let arm_at = shipped.find("updater.arm_restart(").expect("the gate");
        assert!(arm_at < shipped.find("app.exit(0)").unwrap());
        // Its one caller: the user's command.
        let callers: Vec<&str> = [
            ("commands.rs", include_str!("commands.rs")),
            ("mod.rs", shipped),
        ]
        .into_iter()
        .flat_map(|(name, src)| src.matches("restart_to_update(&app").map(move |_| name))
        .collect();
        assert_eq!(callers, ["commands.rs"]);
    }

    /// Each beta channel verifies updates with its own key, and neither is the internal key.
    /// The channel configs also build no updater artifacts (the release script signs them in
    /// its own step) and require a signed version.
    /// The updater key in a config file. Tests only: the app reads no config file's text.
    fn config_pubkey(config: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(config)
            .ok()?
            .pointer("/plugins/updater/pubkey")?
            .as_str()
            .map(str::to_string)
    }

    /// Only this test module may embed a config file: the shipped code holds hashes.
    #[test]
    fn the_app_embeds_no_config_file_only_key_hashes() {
        let src = include_str!("mod.rs");
        let shipped = &src[..src.find("#[cfg(all(test").expect("the test module")];
        assert!(
            !shipped.contains(concat!("include_str!(\"../../tauri", ".")),
            "{shipped}"
        );
        assert_eq!(CHANNEL_KEY_SHA256.len(), 64);
        assert_eq!(INTERNAL_KEY_SHA256.len(), 64);
        assert_ne!(CHANNEL_KEY_SHA256, INTERNAL_KEY_SHA256);
    }

    #[test]
    fn each_beta_channel_has_its_own_updater_key_and_none_is_the_internal_key() {
        let internal =
            config_pubkey(include_str!("../../tauri.conf.json")).expect("the internal key");
        let prod_conf = include_str!("../../tauri.beta-prod.conf.json");
        let prod = config_pubkey(prod_conf).expect("the production key");
        assert_ne!(prod, internal);
        let staging_conf = staging_conf_text();
        #[cfg(cs_channel = "beta-staging")]
        let staging_conf = Some(staging_conf.expect("a staging build has its channel config"));
        let staging = staging_conf.as_deref().map(|conf| {
            let staging = config_pubkey(conf).expect("the staging key");
            assert_ne!(staging, internal);
            assert_ne!(staging, prod);
            staging
        });
        for conf in staging_conf.as_deref().into_iter().chain([prod_conf]) {
            let v: serde_json::Value = serde_json::from_str(conf).unwrap();
            assert_eq!(
                v.pointer("/bundle/createUpdaterArtifacts"),
                Some(&false.into())
            );
            assert_eq!(
                v.pointer("/plugins/updater/requireSignedVersion"),
                Some(&true.into())
            );
        }
        // The hashes build.rs compiled in are this channel's key and the internal key.
        assert_eq!(INTERNAL_KEY_SHA256, sha256_hex(&internal));
        #[cfg(cs_channel = "beta-staging")]
        assert_eq!(CHANNEL_KEY_SHA256, sha256_hex(&staging.unwrap()));
        #[cfg(cs_channel = "beta")]
        {
            let _ = staging;
            assert_eq!(CHANNEL_KEY_SHA256, sha256_hex(&prod));
        }
    }

    /// The staging channel config, read when the test runs: the public tree builds only the
    /// production channel and does not carry it, so no test compiles it in.
    fn staging_conf_text() -> Option<String> {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tauri.beta-staging.conf.json"
        ))
        .ok()
    }

    #[cfg(cs_channel = "beta-staging")]
    fn staging_channel_config() -> serde_json::Value {
        let text = staging_conf_text().expect("a staging build has its channel config");
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn updates_are_off_unless_the_build_carries_its_own_channel_key() {
        let (internal, channel) = (&sha256_hex("internal-key"), &sha256_hex("channel-key"));
        assert!(updates_key_ok(Some("channel-key"), channel, internal, true));
        assert!(
            !updates_key_ok(Some("internal-key"), channel, internal, true),
            "built without its config"
        );
        assert!(!updates_key_ok(Some("other-key"), channel, internal, true));
        assert!(!updates_key_ok(None, channel, internal, true));
        assert!(
            !updates_key_ok(Some("internal-key"), internal, internal, true),
            "a channel key that is the internal key"
        );
        assert!(
            !updates_key_ok(Some("channel-key"), channel, internal, false),
            "an updater that does not require the signed version"
        );
    }

    /// SV-PBF-278: the static manifest's host is the channel's download host.
    #[test]
    fn each_channel_names_its_download_host() {
        #[cfg(cs_channel = "beta")]
        assert_eq!(DOWNLOAD_HOST, "dl.codesense.ai");
        #[cfg(cs_channel = "beta-staging")]
        assert_eq!(
            Some(DOWNLOAD_HOST),
            staging_channel_config()
                .pointer("/plugins/codesense-channel/downloadHost")
                .and_then(|h| h.as_str())
        );
        let host = resolve_host(DOWNLOAD_HOST, None, false).unwrap();
        assert_eq!(host.base, format!("https://{DOWNLOAD_HOST}"));
    }

    #[test]
    fn beta_uses_the_build_host_and_ignores_overrides() {
        let host = resolve_host("updates.codesense.ai", None, false).unwrap();
        assert_eq!(host.base, "https://updates.codesense.ai");
        let ignored =
            resolve_host("updates.codesense.ai", Some("http://127.0.0.1:8787"), false).unwrap();
        assert_eq!(ignored, host, "an override is refused outside dev builds");
    }

    #[test]
    fn a_dev_build_may_point_at_a_local_wrangler() {
        let host = resolve_host(
            "updates.example.invalid",
            Some("http://127.0.0.1:8787"),
            true,
        )
        .unwrap();
        assert_eq!(host.base, "http://127.0.0.1:8787");
    }

    #[test]
    fn the_staging_check_interval_override_works_in_release_with_a_floor() {
        let day = scheduler::DEFAULT_MAX_AGE;
        // Release staging build: honoured, at least 60 seconds.
        assert_eq!(
            interval_from_env(Some("300"), false),
            Duration::from_secs(300)
        );
        assert_eq!(
            interval_from_env(Some("60"), false),
            Duration::from_secs(60)
        );
        assert_eq!(interval_from_env(Some("5"), false), Duration::from_secs(60));
        // Debug build: as given.
        assert_eq!(interval_from_env(Some("5"), true), Duration::from_secs(5));
        for release in [false, true] {
            assert_eq!(interval_from_env(Some("0"), release), day);
            assert_eq!(interval_from_env(Some("soon"), release), day);
            assert_eq!(interval_from_env(None, release), day);
        }
        // A 60-second age ticks every 60 seconds, so a new build is found within minutes.
        assert_eq!(
            scheduler::tick_for(Duration::from_secs(60)),
            Duration::from_secs(60)
        );
    }

    /// The production channel's interval is fixed at compile time: its `check_interval` never
    /// reads the environment.
    #[test]
    fn the_production_channel_ignores_the_check_interval_override() {
        let src = include_str!("mod.rs");
        let at = src
            .find("#[cfg(cs_channel = \"beta\")]\nfn check_interval()")
            .expect("the production channel's check_interval");
        let body = &src[at..at + src[at..].find("\n}\n").unwrap()];
        assert!(body.contains("scheduler::DEFAULT_MAX_AGE"), "{body}");
        assert!(!body.contains("env"), "{body}");
    }

    #[cfg(cs_channel = "beta")]
    #[test]
    fn a_production_build_checks_daily_whatever_the_environment_says() {
        const VAR: &str = "CODESENSE_CHECK_INTERVAL_SECS";
        /// Puts the variable back as it was, however the test ends.
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var(VAR, v),
                    None => std::env::remove_var(VAR),
                }
            }
        }
        // The process-wide lock every test that changes the environment takes.
        let _env = crate::content_engine::test_support::env_guard();
        let _restore = Restore(std::env::var_os(VAR));
        std::env::set_var(VAR, "60");
        assert_eq!(check_interval(), scheduler::DEFAULT_MAX_AGE);
    }

    #[test]
    fn plain_http_is_refused_outside_localhost_and_schemes_are_checked() {
        assert!(resolve_host("h", Some("http://updates.example.com"), true).is_err());
        assert!(resolve_host("h", Some("ftp://127.0.0.1"), true).is_err());
        assert!(resolve_host("h", Some("https://"), true).is_err());
        let ok = resolve_host("h", Some("https://updates.example.invalid/"), true).unwrap();
        assert_eq!(ok.base, "https://updates.example.invalid");
    }
}
