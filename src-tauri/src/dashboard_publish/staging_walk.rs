// SPDX-License-Identifier: MIT
//! The staging walk (SW-PB-22f): the app's own publish code against the real
//! staging dashboard host. Both tests are ignored and run only when asked, in a
//! build that names the staging host:
//!
//! ```text
//! CODESENSE_DASH_HOST=<staging share host> \
//!   STAGING_VIEWER=you@example.com \
//!   cargo test -p codesense --features dashboard-publish --lib staging_walk_publish -- --ignored --nocapture
//! CODESENSE_DASH_HOST=<staging share host> \
//!   cargo test -p codesense --features dashboard-publish --lib staging_walk_takedown -- --ignored --nocapture
//! ```
//!
//! The staging share host is `plugins.codesense-channel.shareHost` in
//! `src-tauri/tauri.beta-staging.conf.json`; the walk refuses any other host.
//! The publisher token comes from `STAGING_TOKEN`, set from the Keychain by the
//! `security` tool so no Keychain dialog is needed (a test binary is not on the
//! item's access list): prefix each command with
//! `STAGING_TOKEN=$(security find-generic-password -s ai.codesense.dashboards -a publisher-token -w)`.
//! The publish code under test is the app's own (Publisher, the watcher loop). The walk
//! keeps its dashboard folder and record in `STAGING_WALK_DIR` (default
//! `~/codesense-staging-walk`) between the two steps.

/// The staging channel config's share host, read when a test runs. The public tree does
/// not carry that config, so no test compiles it in.
#[cfg(test)]
pub(super) fn staging_share_host() -> Option<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.beta-staging.conf.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    config
        .pointer("/plugins/codesense-channel/shareHost")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::super::client::{Client, DASH_HOST, TOKEN_ACCOUNT};
    use super::super::driver::Driver;
    use super::super::manifest::{self, Visibility, MANIFEST_FILE};
    use super::super::push::Publisher;
    use super::super::state::LocalState;
    use super::super::{state, verbs};
    use crate::commands::credential_store::{CredentialStore, FakeStore, Secret};

    /// `host` is exactly the HTTPS origin `staging` names, on the default port.
    fn is_staging_host(host: &str, staging: &str) -> bool {
        let Ok(staging) = url::Url::parse(staging) else {
            return false;
        };
        matches!(url::Url::parse(host), Ok(url)
            if url.scheme() == "https"
                && url.host_str().is_some()
                && url.host_str() == staging.host_str()
                && url.port_or_known_default() == Some(443))
    }

    #[test]
    fn staging_walk_requires_the_exact_staging_host() {
        let staging = "https://share.example.invalid";
        assert!(is_staging_host(staging, staging));
        for host in [
            "https://share.codesense.ai",
            "https://share.example.invalid.evil.example",
            "https://evil.example/share.example.invalid",
            "https://share.example.invalid@evil.example",
            "http://share.example.invalid",
            "https://share.example.invalid:8443",
            "not a URL",
        ] {
            assert!(!is_staging_host(host, staging), "accepted {host}");
        }
        assert!(!is_staging_host(staging, "not a URL"));
    }

    fn host() -> &'static str {
        let host = DASH_HOST.expect("build with CODESENSE_DASH_HOST set to the staging host");
        let staging = super::staging_share_host().expect(
            "the walk needs the staging channel config (src-tauri/tauri.beta-staging.conf.json)",
        );
        assert!(
            is_staging_host(host, &staging),
            "the walk only runs against the HTTPS staging host, not {host}"
        );
        host
    }

    fn walk_dir() -> PathBuf {
        std::env::var("STAGING_WALK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join("codesense-staging-walk"))
    }

    fn token() -> Secret {
        let token = std::env::var("STAGING_TOKEN").expect("set STAGING_TOKEN from the Keychain (see the module doc)");
        assert!(!token.is_empty(), "STAGING_TOKEN is empty");
        Secret::new(token.as_str())
    }

    /// The token in a store, as the app's Keychain would hold it.
    fn store() -> Arc<dyn CredentialStore> {
        let store = FakeStore::default();
        store.set(TOKEN_ACCOUNT, &token()).unwrap();
        Arc::new(store)
    }

    fn publisher(data: &Path, folder: &Path) -> Publisher {
        Publisher::new(data, folder, Client::new(host()).unwrap(), store(), Instant::now()).unwrap()
    }

    /// The dashboard as the server has it (R8), as JSON.
    async fn server_view(slug: &str) -> serde_json::Value {
        let reply = reqwest::Client::new()
            .get(format!("{}/api/dash/v1/d/{slug}", host()))
            .bearer_auth(token().expose())
            .send()
            .await
            .expect("R8 did not respond");
        assert_eq!(reply.status(), 200, "R8 answered {}", reply.status());
        reply.json().await.expect("R8 reply is not JSON")
    }

    fn write_dashboard(folder: &std::path::Path, slug: &str, runs: u32) {
        fs::create_dir_all(folder.join("data")).unwrap();
        fs::write(
            folder.join("index.html"),
            "<!doctype html><title>Staging walk</title><h1>Staging walk</h1>\
             <p>Agent runs: <b id=\"runs\">loading</b></p>\
             <p><a href=\"results.html\">Results</a> \
             <a href=\"https://codesense.ai\">codesense.ai</a></p>\
             <script>fetch(\"data/usage.json\").then(r => r.json())\
             .then(d => { document.getElementById(\"runs\").textContent = d.runs; })\
             .catch(e => { document.getElementById(\"runs\").textContent = \"error: \" + e; });</script>",
        )
        .unwrap();
        fs::write(
            folder.join("results.html"),
            "<!doctype html><title>Results</title><h1>Results</h1>\
             <p>Runs so far: <b id=\"runs\">loading</b></p>\
             <p><a href=\"index.html\">Back</a></p>\
             <script>fetch(\"data/usage.json\").then(r => r.json())\
             .then(d => { document.getElementById(\"runs\").textContent = d.runs; });</script>",
        )
        .unwrap();
        fs::write(folder.join("data/usage.json"), format!("{{\"runs\": {runs}}}")).unwrap();
        fs::write(
            folder.join(MANIFEST_FILE),
            format!(
                "slug: {slug}\ntitle: Staging walk\npublish:\n  state: draft\n  visibility: private\n  files:\n    - index.html\n    - results.html\n    - data/usage.json\n"
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "manual staging publish walk requires staging credentials and explicit operator authorization"]
    async fn staging_walk_publish() {
        host();
        let dir = walk_dir();
        let folder = dir.join("dashboard");
        let data = dir.join("data");
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let slug = format!("walk-{secs}");
        assert!(!folder.exists(), "{} exists: run the takedown step first", folder.display());
        write_dashboard(&folder, &slug, 1);
        fs::create_dir_all(&data).unwrap();

        // 1. Publish: the local step, then the first push (R1, R2, R3 to R5).
        verbs::publish(&folder, &data, Visibility::Private, None).expect("local publish refused");
        let mut first = publisher(&data, &folder);
        first.drive(Instant::now()).await.expect("publish did not run");
        let record = state::load(&data, &slug).unwrap();
        assert_eq!(record.state, LocalState::Published, "first push did not go live: {:?}", record.last_error);
        let view = server_view(&slug).await;
        assert_eq!(view["state"], "published");
        assert_eq!(view["current_version"].as_u64(), record.current_version);
        println!("published {slug}: version {:?}", record.current_version);

        // 2. An agent refreshes the data file. The watcher loop sees it and pushes.
        fs::write(folder.join("data/usage.json"), "{\"runs\": 2}").unwrap();
        let mut driver = Driver::new(&data, Some(host()), store());
        let t0 = Instant::now();
        driver.tick(t0).await;
        driver.tick(t0 + Duration::from_secs(61)).await;
        let after = state::load(&data, &slug).unwrap();
        assert!(
            after.current_version > record.current_version,
            "the change was not pushed: {:?} then {:?} ({:?})",
            record.current_version,
            after.current_version,
            after.last_error
        );
        assert_eq!(server_view(&slug).await["current_version"].as_u64(), after.current_version);
        println!("updated {slug}: version {:?}", after.current_version);

        // 3. The viewer, if one is named: the share list (R9). Send is pressed in the admin outbox.
        if let Ok(viewer) = std::env::var("STAGING_VIEWER") {
            let reply = reqwest::Client::new()
                .put(format!("{}/api/dash/v1/d/{slug}/shares", host()))
                .bearer_auth(token().expose())
                .json(&serde_json::json!({ "emails": [viewer] }))
                .send()
                .await
                .expect("R9 did not respond");
            assert_eq!(reply.status(), 200, "R9 answered {}", reply.status());
            println!("shared {slug} with {viewer}: press Send in the staging admin outbox");
        }
    }

    #[tokio::test]
    #[ignore = "manual staging takedown walk requires the prior publish fixture and explicit operator authorization"]
    async fn staging_walk_takedown() {
        host();
        let dir = walk_dir();
        let folder = dir.join("dashboard");
        let data = dir.join("data");
        let slug = manifest::load_slug(&folder).expect("no walk dashboard: run the publish step first");

        verbs::unpublish(&folder, &data).expect("local unpublish refused");
        publisher(&data, &folder).unpublish().await.expect("takedown did not run");
        assert!(!state::load(&data, &slug).unwrap().takedown_pending(), "takedown still owed");
        assert_eq!(server_view(&slug).await["state"], "unpublished");
        println!("took down {slug}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
