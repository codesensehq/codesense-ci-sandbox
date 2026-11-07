// SPDX-License-Identifier: MIT
#[cfg(cs_channel = "internal")]
use std::sync::Mutex;
#[cfg(cs_channel = "internal")]
use std::time::Instant;

#[cfg(cs_channel = "internal")]
use log::{debug, info};
#[cfg(cs_channel = "internal")]
use reqwest::Client;
#[cfg(cs_channel = "internal")]
use serde::{Deserialize, Serialize};
#[cfg(cs_channel = "internal")]
use tauri::Emitter;
#[cfg(cs_channel = "internal")]
use tauri_plugin_updater::UpdaterExt;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg(cs_channel = "internal")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg(cs_channel = "internal")]
pub struct DownloadProgress {
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
#[cfg(cs_channel = "internal")]
pub enum UpdateError {
    // Serialized to `kind: "auth" | "network" | "notFound" | "signature" | "unknown"`
    // via serde's camelCase renamer — the "Error" suffix is redundant inside
    // a closed error enum, so variants are named without it (clippy's
    // enum_variant_names rule) and the frontend discriminator follows suit.
    Network(String),
    NotFound(String),
    Signature(String),
    Unknown(String),
}

#[cfg(cs_channel = "internal")]
impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(m) => write!(f, "network: {}", m),
            Self::NotFound(m) => write!(f, "not found: {}", m),
            Self::Signature(m) => write!(f, "signature: {}", m),
            Self::Unknown(m) => write!(f, "{}", m),
        }
    }
}

#[derive(Debug, Deserialize)]
#[cfg(cs_channel = "internal")]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
#[cfg(cs_channel = "internal")]
struct GitHubAsset {
    id: u64,
    name: String,
}

// ---------------------------------------------------------------------------
// State: holds the Update object between check and install commands
// ---------------------------------------------------------------------------

#[cfg(cs_channel = "internal")]
struct PendingUpdate {
    update: tauri_plugin_updater::Update,
    tarball_asset_id: u64,
    checked_at: Instant,
}

#[cfg(cs_channel = "internal")]
pub struct UpdateState(Mutex<Option<PendingUpdate>>);

#[cfg(cs_channel = "internal")]
impl Default for UpdateState {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

// Beta channels have no GitHub update path to hold a pending update for.
#[cfg(not(cs_channel = "internal"))]
#[derive(Default)]
pub struct UpdateState;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The GitHub repository (`owner/repo`) a build from source checks for updates, fixed at build
/// with `CODESENSE_UPDATE_REPO`. The public releases carry no update manifest, so when it is
/// unset the build makes no update check and sends no request.
#[cfg(cs_channel = "internal")]
const UPDATE_REPO: Option<&str> = option_env!("CODESENSE_UPDATE_REPO");
#[cfg(cs_channel = "internal")]
const STALE_THRESHOLD_SECS: u64 = 30 * 60; // 30 minutes

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(cs_channel = "internal")]
/// Build an HTTP client with a short timeout for API calls (not downloads).
fn api_client() -> Result<Client, UpdateError> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| UpdateError::Network(format!("failed to build HTTP client: {}", e)))
}

#[cfg(cs_channel = "internal")]
/// Build an HTTP client with a long timeout for binary downloads.
fn download_client() -> Result<Client, UpdateError> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| UpdateError::Network(format!("failed to build HTTP client: {}", e)))
}

#[cfg(cs_channel = "internal")]
/// The GitHub API URL for `path` under the update repository's releases. An unset or empty
/// repository is `NotFound`, so no request is made. Anything but a plain `owner/repo` is
/// refused, so the value can only name a repository on api.github.com.
fn releases_api_url(repo: Option<&str>, path: &str) -> Result<String, UpdateError> {
    let repo = repo.filter(|r| !r.is_empty()).ok_or_else(|| {
        UpdateError::NotFound(
            "This build has no update source. Install the latest release manually.".to_string(),
        )
    })?;
    let segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if segment(owner) && segment(name) => Ok(format!(
            "https://api.github.com/repos/{owner}/{name}/releases/{path}"
        )),
        _ => Err(UpdateError::Unknown(format!(
            "CODESENSE_UPDATE_REPO={repo:?} is not a GitHub owner/repo"
        ))),
    }
}

#[cfg(cs_channel = "internal")]
/// Classify a GitHub API HTTP status into an UpdateError.
fn classify_github_error(status: u16, body: &str) -> UpdateError {
    match status {
        401 | 403 | 404 => UpdateError::NotFound(format!(
            "Public update unavailable ({status}). Install the latest signed release manually."
        )),
        _ => UpdateError::Network(format!("GitHub API returned {}: {}", status, body)),
    }
}

#[cfg(cs_channel = "internal")]
/// Fetch the latest release metadata from the GitHub API.
async fn fetch_release(client: &Client) -> Result<GitHubRelease, UpdateError> {
    let url = releases_api_url(UPDATE_REPO, "latest")?;

    let resp = client
        .get(&url)
        .header("User-Agent", "CodeSense")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| UpdateError::Network(format!("failed to fetch release: {}", e)))?;

    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        return Err(classify_github_error(status, &body));
    }

    resp.json::<GitHubRelease>()
        .await
        .map_err(|e| UpdateError::Unknown(format!("failed to parse release JSON: {}", e)))
}

#[cfg(cs_channel = "internal")]
/// Find an asset by name in the release.
fn find_asset<'a>(release: &'a GitHubRelease, name: &str) -> Result<&'a GitHubAsset, UpdateError> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| {
            UpdateError::NotFound(format!(
                "asset '{}' not found in release {}",
                name, release.tag_name
            ))
        })
}

#[cfg(cs_channel = "internal")]
/// Verify an update's signature using minisign.
fn verify_signature(data: &[u8], signature_b64: &str, pubkey_b64: &str) -> Result<(), UpdateError> {
    use base64::Engine;
    use minisign_verify::{PublicKey, Signature};

    let pubkey_str = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(pubkey_b64)
            .map_err(|e| UpdateError::Signature(format!("bad pubkey base64: {}", e)))?,
    )
    .map_err(|e| UpdateError::Signature(format!("pubkey not utf8: {}", e)))?;

    let sig_str = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(signature_b64)
            .map_err(|e| UpdateError::Signature(format!("bad signature base64: {}", e)))?,
    )
    .map_err(|e| UpdateError::Signature(format!("signature not utf8: {}", e)))?;

    let public_key = PublicKey::decode(&pubkey_str)
        .map_err(|e| UpdateError::Signature(format!("invalid pubkey: {}", e)))?;
    let signature = Signature::decode(&sig_str)
        .map_err(|e| UpdateError::Signature(format!("invalid signature: {}", e)))?;

    public_key
        .verify(data, &signature, true)
        .map_err(|_| UpdateError::Signature("signature verification failed".to_string()))
}

#[cfg(cs_channel = "internal")]
/// Resolve a GitHub release asset to its pre-signed S3 download URL.
async fn resolve_asset_download_url(asset_id: u64) -> Result<String, UpdateError> {
    let url = releases_api_url(UPDATE_REPO, &format!("assets/{asset_id}"))?;
    let no_redirect_client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| UpdateError::Network(format!("failed to build HTTP client: {}", e)))?;

    let resp = no_redirect_client
        .get(&url)
        .header("User-Agent", "CodeSense")
        .header("Accept", "application/octet-stream")
        .send()
        .await
        .map_err(|e| UpdateError::Network(format!("asset redirect failed: {}", e)))?;

    let status = resp.status().as_u16();
    if status == 302 {
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                UpdateError::Network("302 redirect missing Location header".to_string())
            })?;
        Ok(location.to_string())
    } else if status == 200 {
        Ok(url)
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(classify_github_error(status, &body))
    }
}

#[cfg(cs_channel = "internal")]
/// Lock the update state mutex, handling poison gracefully.
fn lock_update_state(
    state: &Mutex<Option<PendingUpdate>>,
) -> std::sync::MutexGuard<'_, Option<PendingUpdate>> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[cfg(cs_channel = "internal")]
/// Check for updates using the hybrid approach (a build without `CODESENSE_UPDATE_REPO`
/// returns `NotFound` first, before an HTTP client is built or any request made):
/// 1. Fetch release metadata via GitHub API (anonymous)
/// 2. Find latest.json asset ID
/// 3. Resolve S3 pre-signed URL for the manifest
/// 4. Point updater_builder at the S3 URL, call check()
/// 5. Store the Update object + tarball asset ID for the install command
#[tauri::command]
pub async fn check_for_update(
    app: tauri::AppHandle,
    update_state: tauri::State<'_, UpdateState>,
    simulate: Option<bool>,
) -> Result<Option<UpdateInfo>, UpdateError> {
    // Dev simulation mode
    if simulate.unwrap_or(false) {
        let current_version = app
            .config()
            .version
            .clone()
            .unwrap_or_else(|| "0.3.0".to_string());
        debug!(target: "update", "SIMULATE mode — returning fake update");
        return Ok(Some(UpdateInfo {
            version: "99.0.0".to_string(),
            current_version,
            body: Some("Simulated update for UI development.".to_string()),
        }));
    }

    // A build with no update source stops here, before any client is built or request made.
    releases_api_url(UPDATE_REPO, "latest")?;

    let client = api_client()?;

    // 1. Fetch release metadata
    let release = fetch_release(&client).await?;

    // 2. Find latest.json asset
    let manifest_asset = find_asset(&release, "latest.json")?;

    // 3. Find the tarball asset (platform-specific)
    let tarball_name = tarball_asset_name();
    let tarball_asset = find_asset(&release, &tarball_name)?;

    // 4. Resolve the manifest's pre-signed S3 URL
    let s3_url = resolve_asset_download_url(manifest_asset.id).await?;

    let manifest_url = s3_url
        .parse()
        .map_err(|e| UpdateError::Unknown(format!("bad manifest URL: {}", e)))?;

    let updater = app
        .updater_builder()
        .endpoints(vec![manifest_url])
        .map_err(|e| UpdateError::Unknown(format!("failed to set endpoints: {}", e)))?
        .header("User-Agent", "CodeSense")
        .map_err(|e| UpdateError::Unknown(format!("failed to set user-agent: {}", e)))?
        .build()
        .map_err(|e| UpdateError::Unknown(format!("failed to build updater: {}", e)))?;

    // 5. check() fetches latest.json via S3, parses manifest, compares versions
    let update = updater
        .check()
        .await
        .map_err(|e| UpdateError::Network(format!("update check failed: {}", e)))?;

    let current_version = app
        .config()
        .version
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    match update {
        Some(update) => {
            info!(target: "update", "update available: v{} (current: v{})", update.version, current_version);
            let info = UpdateInfo {
                version: update.version.clone(),
                current_version,
                body: update.body.clone(),
            };

            // Store for install command
            let mut state = lock_update_state(&update_state.0);
            *state = Some(PendingUpdate {
                update,
                tarball_asset_id: tarball_asset.id,
                checked_at: Instant::now(),
            });

            Ok(Some(info))
        }
        None => {
            // Quiet — no need to log when already up to date
            Ok(None)
        }
    }
}

#[cfg(cs_channel = "internal")]
/// Download and install a pending update.
/// Downloads the tarball via GitHub API (authenticated) with streaming
/// progress, verifies the signature, then calls update.install(bytes).
#[tauri::command]
pub async fn download_and_install_update(
    app: tauri::AppHandle,
    update_state: tauri::State<'_, UpdateState>,
    simulate: Option<bool>,
) -> Result<(), UpdateError> {
    // Dev simulation mode
    if simulate.unwrap_or(false) {
        debug!(target: "update", "SIMULATE mode — faking download + install");
        let total: u64 = 250_000_000;
        for step in 1..=50 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = app.emit(
                "update-download-progress",
                DownloadProgress {
                    bytes_downloaded: total * step / 50,
                    total_bytes: Some(total),
                },
            );
        }
        debug!(target: "update", "SIMULATE mode — install 'complete'");
        return Ok(());
    }

    // Extract the pending update
    let pending = {
        let mut state = lock_update_state(&update_state.0);
        state.take()
    };

    let pending = pending.ok_or_else(|| {
        UpdateError::Unknown("no pending update — call check_for_update first".to_string())
    })?;

    // If stale, put it back so retry works
    if pending.checked_at.elapsed().as_secs() > STALE_THRESHOLD_SECS {
        let mut state = lock_update_state(&update_state.0);
        *state = Some(pending);
        return Err(UpdateError::Unknown(
            "update check is stale (>30 min) — please re-check".to_string(),
        ));
    }

    // Resolve the tarball's pre-signed S3 URL, then download with streaming progress
    let s3_url = resolve_asset_download_url(pending.tarball_asset_id).await?;

    // Use a longer timeout for the actual binary download
    let dl_client = download_client()?;
    let resp = dl_client
        .get(&s3_url)
        .header("User-Agent", "CodeSense")
        .send()
        .await
        .map_err(|e| UpdateError::Network(format!("download failed: {}", e)))?;

    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(classify_github_error(status, &body));
    }

    let total_bytes: Option<u64> = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    // Stream the download with incremental progress events
    let mut buffer = Vec::new();
    let mut bytes_downloaded: u64 = 0;
    let mut stream = resp.bytes_stream();

    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| UpdateError::Network(format!("download stream error: {}", e)))?;
        bytes_downloaded += chunk.len() as u64;
        buffer.extend_from_slice(&chunk);

        let _ = app.emit(
            "update-download-progress",
            DownloadProgress {
                bytes_downloaded,
                total_bytes,
            },
        );
    }

    // Signature verification + install — only log on success

    // install() re-verifies since tauri-plugin-updater 2.14; our verify_signature stays as a belt-and-braces check
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| UpdateError::Signature("no pubkey found in tauri.conf.json".to_string()))?;

    verify_signature(&buffer, &pending.update.signature, pubkey)?;

    // install(bytes) handles extraction + binary replacement
    pending
        .update
        .install(&buffer)
        .map_err(|e| UpdateError::Unknown(format!("install failed: {}", e)))?;

    info!(target: "update", "installed successfully — restart required");
    Ok(())
}

#[cfg(cs_channel = "internal")]
/// Determine the expected tarball asset name for the current platform.
fn tarball_asset_name() -> String {
    #[cfg(target_os = "macos")]
    {
        "CodeSense.app.tar.gz".to_string()
    }
    #[cfg(target_os = "windows")]
    {
        "CodeSense_x64-setup.exe.zip".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        "CodeSense_amd64.AppImage.tar.gz".to_string()
    }
}

#[cfg(all(test, cs_channel = "internal"))]
mod tests {
    use super::*;

    /// This path's own check refuses the bad signatures the public channel refuses. The
    /// fixture's own signature passes it with the test key, and an app holding the internal
    /// key refuses the test-signed build.
    #[test]
    fn verify_signature_refuses_a_bad_signature() {
        use crate::test_support::updater::{
            bad_signatures, shipped_pubkey, ANOTHER_KEY, TEST_PUBKEY,
        };

        let bytes = include_bytes!("../../tests/fixtures/updater/update-0.0.32.app.tar.gz");
        let good = include_str!("../../tests/fixtures/updater/update-0.0.32.app.tar.gz.sig");
        let test_key = TEST_PUBKEY.trim();
        verify_signature(bytes, good.trim(), test_key).expect("the fixture verifies");
        let internal_key = shipped_pubkey(include_str!("../../tauri.conf.json"));
        assert_ne!(
            internal_key, test_key,
            "the shipped key is not the test key"
        );
        for bad in bad_signatures(&internal_key) {
            let case = bad.case;
            assert_eq!(
                bad.pubkey == test_key,
                case != ANOTHER_KEY,
                "{case}: the key"
            );
            let expected = match case {
                "corrupted" | ANOTHER_KEY => "signature verification failed",
                "cut in the trusted comment"
                | "cut in the signature line"
                | "missing"
                | "empty" => "invalid signature",
                "not base64" => "bad signature base64",
                "not utf8" => "signature not utf8",
                other => unreachable!("{other}"),
            };
            // The plugin refuses a manifest with no signature, so this path never sees one; an
            // empty one stands in for it.
            let sig = bad.sig.unwrap_or_default();
            match verify_signature(bytes, &sig, &bad.pubkey) {
                Err(UpdateError::Signature(message)) => {
                    assert!(message.starts_with(expected), "{case}: {message}")
                }
                other => panic!("{case}: unexpected result: {other:?}"),
            }
        }
    }

    #[test]
    fn anonymous_inaccessible_release_has_manual_install_guidance() {
        for status in [401, 403, 404] {
            match classify_github_error(status, "private hosting body") {
                UpdateError::NotFound(message) => {
                    assert!(message.contains("Install the latest signed release manually"))
                }
                other => panic!("unexpected update error: {other:?}"),
            }
        }
    }

    #[test]
    fn a_build_without_an_update_repo_has_no_update_source() {
        for repo in [None, Some("")] {
            match releases_api_url(repo, "latest") {
                Err(UpdateError::NotFound(message)) => {
                    assert!(message.contains("Install the latest release manually"))
                }
                other => panic!("unexpected result for {repo:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_build_time_repo_names_only_the_github_releases_api() {
        assert_eq!(
            releases_api_url(Some("acme/code_sense.app"), "latest").unwrap(),
            "https://api.github.com/repos/acme/code_sense.app/releases/latest"
        );
        assert_eq!(
            releases_api_url(Some("acme-1/codesense"), "assets/42").unwrap(),
            "https://api.github.com/repos/acme-1/codesense/releases/assets/42"
        );
    }

    #[test]
    fn a_repo_value_that_is_not_owner_slash_repo_is_refused() {
        for repo in [
            "acme",
            "acme/",
            "/codesense",
            "acme/codesense/releases",
            "../codesense",
            "acme/..",
            "acme/.",
            "evil.example@acme/codesense",
            "acme/codesense?x=1",
            "acme/codesense#x",
            "acme /codesense",
            "acme/codesense\n",
            "https://evil.example/acme",
        ] {
            assert!(
                matches!(
                    releases_api_url(Some(repo), "latest"),
                    Err(UpdateError::Unknown(_))
                ),
                "{repo:?} must be refused"
            );
        }
    }

    /// The no-source check is the first step of a real check, ahead of the HTTP client.
    #[test]
    fn check_for_update_returns_no_source_before_building_a_client() {
        let src = include_str!("update.rs");
        let body = &src[src.find("pub async fn check_for_update").unwrap()..];
        let guard = body
            .find("releases_api_url(UPDATE_REPO, \"latest\")?;")
            .unwrap();
        assert!(guard < body.find("api_client()?").unwrap());
    }

    /// No private repository is named in the update path: neither here nor as the updater
    /// plugin's config endpoint, which both callers replace at runtime.
    #[test]
    fn the_update_path_names_no_private_repository() {
        let private = ["alfy", "pops/"].concat();
        assert!(!include_str!("update.rs").contains(&private));
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        assert!(!conf.to_string().contains(&private));
        assert_eq!(
            conf.pointer("/plugins/updater/endpoints"),
            Some(&serde_json::json!([]))
        );
    }
}
