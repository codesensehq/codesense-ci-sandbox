// SPDX-License-Identifier: MIT
//! Host-fetched inbox images. Frames only see verified, content-addressed bytes.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tauri::Manager;

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
const CACHE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct MediaInput {
    pub url: Option<String>,
    pub path: Option<String>,
    pub alt: String,
}
impl MediaInput {
    pub fn validate(&self) -> Result<(), String> {
        if self.alt.trim().is_empty() {
            return Err("media.alt is required".into());
        }
        match (&self.url, &self.path) {
            (Some(url), None) => {
                remote_url(url).map_err(|e| e.reason.to_string())?;
            }
            (None, Some(path)) if !path.trim().is_empty() => {}
            _ => return Err("media requires exactly one of url or path".into()),
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Remote {
    #[default]
    Auto,
    Ask,
    Off,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub remote: Remote,
    pub hosts: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            remote: Remote::Auto,
            hosts: vec!["*.giphy.com".into(), "static.klipy.com".into()],
        }
    }
}
impl Config {
    pub fn load(config_dir: &Path) -> Self {
        #[derive(Deserialize, Default)]
        struct File {
            #[serde(default)]
            media: Config,
        }
        match std::fs::read(config_dir.join("inbox.yml")) {
            Ok(bytes) => serde_yaml::from_slice::<File>(&bytes)
                .map(|f| f.media)
                .unwrap_or_else(|_| Self {
                    remote: Remote::Off,
                    ..Self::default()
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(_) => Self {
                remote: Remote::Off,
                ..Self::default()
            },
        }
    }
    pub fn allows(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.hosts.iter().any(|pattern| {
            let pattern = pattern.to_ascii_lowercase();
            if let Some(suffix) = pattern.strip_prefix("*.") {
                host.strip_suffix(suffix)
                    .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1)
            } else {
                host == pattern
            }
        })
    }
    fn should_auto_fetch(&self, host: &str, own: bool) -> bool {
        self.remote == Remote::Auto && own && self.allows(host)
    }
    pub fn apply(&self, attachments: &mut [Attachment]) {
        for a in attachments {
            if a.source_url.is_some() && self.remote == Remote::Off {
                a.state = "blocked".into();
            } else if a.state == "blocked" {
                a.state = "pending".into();
            }
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub alt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_host: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub retryable: bool,
}
impl Attachment {
    pub fn remote(url: &str, alt: &str, config: &Config, own: bool) -> Result<Self, Failure> {
        let url = remote_url(url)?;
        let host = url.host_str().unwrap().to_string();
        let mut a = Self::empty(alt);
        a.state = if config.remote == Remote::Off {
            "blocked"
        } else {
            "pending"
        }
        .into();
        a.reason = Some(
            if !own {
                "sender"
            } else if config.remote == Remote::Ask {
                "ask"
            } else {
                "host"
            }
            .into(),
        );
        a.source_host = Some(host);
        a.source_url = Some(url.to_string());
        Ok(a)
    }
    fn empty(alt: &str) -> Self {
        Self {
            kind: "image".into(),
            sha256: None,
            mime: None,
            width: None,
            height: None,
            bytes: None,
            alt: crate::shared::secret_scan::redact_secrets(alt),
            source_url: None,
            source_host: None,
            state: "pending".into(),
            reason: None,
            retryable: false,
        }
    }
    fn failed(mut self, error: Failure) -> Self {
        self.state = "failed".into();
        self.reason = Some(error.reason.into());
        self.retryable = error.retryable;
        self
    }
}
#[derive(Debug)]
pub struct Failure {
    pub reason: &'static str,
    pub retryable: bool,
}
fn failure(reason: &'static str) -> Failure {
    Failure {
        reason,
        retryable: false,
    }
}
fn network(_: impl std::fmt::Display) -> Failure {
    Failure {
        reason: "network",
        retryable: true,
    }
}
fn remote_url(url: &str) -> Result<url::Url, Failure> {
    let url = url::Url::parse(url).map_err(|_| failure("https"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(failure("https"));
    }
    Ok(url)
}

pub fn inspect(data: &[u8]) -> Result<(&'static str, u32, u32), Failure> {
    if data.len() > MAX_BYTES {
        return Err(failure("size"));
    }
    let le16 = |at| u16::from_le_bytes([data[at], data[at + 1]]) as u32;
    let le24 = |at| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], 0]);
    let be32 = |at| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
    let (mime, w, h) = if data.len() >= 10
        && (data.starts_with(b"GIF89a") || data.starts_with(b"GIF87a"))
    {
        ("image/gif", le16(6), le16(8))
    } else if data.len() >= 24 && data.starts_with(b"\x89PNG\r\n\x1a\n") && &data[12..16] == b"IHDR"
    {
        ("image/png", be32(16), be32(20))
    } else if data.len() >= 30 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        let (w, h) = match &data[12..16] {
            b"VP8X" => (le24(24) + 1, le24(27) + 1),
            b"VP8 " if &data[23..26] == b"\x9d\x01\x2a" => (le16(26) & 0x3fff, le16(28) & 0x3fff),
            b"VP8L" if data[20] == 0x2f => {
                let bits = u32::from_le_bytes(data[21..25].try_into().unwrap());
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            _ => return Err(failure("type")),
        };
        ("image/webp", w, h)
    } else if data.starts_with(b"\xff\xd8\xff") {
        let mut at = 2;
        let mut dimensions = None;
        while at + 4 <= data.len() {
            if data[at] != 0xff {
                break;
            }
            let marker = data[at + 1];
            if marker == 0xff {
                at += 1;
                continue;
            }
            let len = u16::from_be_bytes([data[at + 2], data[at + 3]]) as usize;
            if len < 2 || at + 2 + len > data.len() {
                break;
            }
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) && len >= 7 {
                dimensions = Some((
                    u16::from_be_bytes([data[at + 7], data[at + 8]]) as u32,
                    u16::from_be_bytes([data[at + 5], data[at + 6]]) as u32,
                ));
                break;
            }
            at += len + 2;
        }
        let (w, h) = dimensions.ok_or_else(|| failure("type"))?;
        ("image/jpeg", w, h)
    } else {
        return Err(failure("type"));
    };
    if w == 0 || h == 0 {
        return Err(failure("type"));
    }
    if w > 4096 || h > 4096 || u64::from(w) * u64::from(h) > 16 * 1024 * 1024 {
        return Err(failure("pixels"));
    }
    Ok((mime, w, h))
}

fn redirect_policy(config: &Config) -> reqwest::redirect::Policy {
    let hosts = config.clone();
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > 3 {
            attempt.error("redirect limit")
        } else if !public_url(attempt.url())
            || attempt.url().scheme() != "https"
            || !attempt.url().host_str().is_some_and(|h| hosts.allows(h))
        {
            attempt.error("redirect host")
        } else {
            attempt.follow()
        }
    })
}
fn client_builder(config: &Config) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .use_rustls_tls()
        .user_agent("ImageFetcher/1.0")
        .referer(false)
        .no_proxy()
        .dns_resolver(std::sync::Arc::new(PublicDns))
        .timeout(Duration::from_secs(10))
        .redirect(redirect_policy(config))
}
async fn download(url: &str, config: &Config) -> Result<Vec<u8>, Failure> {
    let url = remote_url(url)?;
    if !public_url(&url) {
        return Err(failure("address"));
    }
    let client = client_builder(config)
        .https_only(true)
        .build()
        .map_err(network)?;
    let response = client.get(url).send().await.map_err(|e| {
        let mut cause: Option<&dyn std::error::Error> = Some(&e);
        while let Some(error) = cause {
            if error.to_string().contains("media-private-address") {
                return failure("address");
            }
            cause = error.source();
        }
        if e.is_redirect() {
            failure("redirect")
        } else {
            network(e)
        }
    })?;
    read_response(response).await
}
async fn read_response(mut response: reqwest::Response) -> Result<Vec<u8>, Failure> {
    if !response.status().is_success() {
        return Err(Failure {
            reason: "http",
            retryable: response.status().is_server_error() || response.status().as_u16() == 429,
        });
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BYTES as u64)
    {
        return Err(failure("size"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err(failure("size"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn cache_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("inbox-media"))
}
pub fn read_cached(dir: &Path, sha: &str) -> Result<Vec<u8>, String> {
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("invalid media hash".into());
    }
    let path = dir.join(sha);
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
        let _ =
            file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()));
    }
    Ok(bytes)
}
fn store_bytes(dir: &Path, bytes: &[u8], a: &mut Attachment) -> Result<(), Failure> {
    static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITES.lock().unwrap_or_else(|e| e.into_inner());
    let (mime, w, h) = inspect(bytes)?;
    let sha = format!("{:x}", Sha256::digest(bytes));
    std::fs::create_dir_all(dir).map_err(network)?;
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(network)?
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file()
                .then(|| (e.path(), meta.len(), meta.modified().ok()))
        })
        .collect();
    let mut total: u64 = entries.iter().map(|(_, len, _)| len).sum();
    entries.sort_by_key(|(_, _, time)| *time);
    for (path, len, _) in entries {
        if total + bytes.len() as u64 <= CACHE_BYTES {
            break;
        }
        std::fs::remove_file(path).map_err(network)?;
        total -= len;
    }
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(network)?;
    std::io::Write::write_all(&mut temp, bytes).map_err(network)?;
    temp.persist(dir.join(&sha)).map_err(network)?;
    a.sha256 = Some(sha);
    a.mime = Some(mime.into());
    a.width = Some(w);
    a.height = Some(h);
    a.bytes = Some(bytes.len() as u64);
    a.state = "cached".into();
    a.reason = None;
    a.retryable = false;
    Ok(())
}

pub(crate) async fn config(
    app: &tauri::AppHandle,
    team: &crate::commands::dashboard_launch::LaunchTeam,
) -> Result<Config, String> {
    let ctx =
        crate::commands::route_to_context::derive_context_for(app, &team.org, &team.team).await?;
    let crate::commands::route_to_context::ContextResolution::Ok(ctx) = ctx else {
        return Err("media team unavailable".into());
    };
    let root =
        crate::context_repo::path_to_org_root(&ctx.codesense_home, &ctx.principal(), &team.org)?;
    let dir = crate::context_repo::path_to_team_config_dir(&root, &team.team);
    crate::commands::await_blocking("media.config", move || Ok(Config::load(&dir))).await
}
pub(crate) async fn prepare(
    app: &tauri::AppHandle,
    team: &crate::commands::dashboard_launch::LaunchTeam,
    input: &MediaInput,
    own: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
    workspace_root: Option<&Path>,
) -> Result<(Attachment, bool), String> {
    input.validate()?;
    let config = config(app, team).await?;
    if let Some(url) = &input.url {
        let a =
            Attachment::remote(url, &input.alt, &config, own).map_err(|e| e.reason.to_string())?;
        let auto = config.should_auto_fetch(a.source_host.as_deref().unwrap(), own);
        return Ok((a, auto));
    }
    let raw = PathBuf::from(input.path.as_ref().unwrap());
    // Relative media checks the sender's own content root first; only a miss there
    // can fall back to the local workspace captured for that sender session.
    let (content_path, relative_path) = if raw.is_absolute() {
        (raw, None)
    } else {
        let root = match scope {
            crate::context_repo::caller_scope::CallerScope::Team(view) => {
                view.own_content().own_path().map(Path::to_path_buf)
            }
            crate::context_repo::caller_scope::CallerScope::Human => {
                let ctx = crate::commands::route_to_context::derive_context_for(
                    app, &team.org, &team.team,
                )
                .await?;
                let crate::commands::route_to_context::ContextResolution::Ok(ctx) = ctx else {
                    return Err("media team unavailable".into());
                };
                let org_root = crate::context_repo::path_to_org_root(
                    &ctx.codesense_home,
                    &ctx.principal(),
                    &team.org,
                )?;
                crate::context_repo::inheritance::team_view(&org_root, &team.org, &team.team)
                    .own_content()
                    .own_path()
                    .map(Path::to_path_buf)
            }
            crate::context_repo::caller_scope::CallerScope::Unknown => None,
        }
        .ok_or_else(|| "media team unavailable".to_string())?;
        (root.join(&raw), Some(raw))
    };
    let dir = cache_dir(app)?;
    let alt = input.alt.clone();
    let scope = scope.clone();
    let workspace_root = workspace_root.map(Path::to_path_buf);
    let a = crate::commands::await_blocking("media.local", move || {
        let scoped = prepare_local_for_scope_or_workspace(
            &scope,
            &content_path,
            workspace_root.as_deref(),
            &alt,
            &dir,
        );
        match scoped {
            Ok(a) => Ok(a),
            Err(content_error) => {
                if let (Some(relative), Some(workspace_root)) =
                    (relative_path, workspace_root.as_deref())
                {
                    prepare_local_for_scope_or_workspace(
                        &scope,
                        &workspace_root.join(relative),
                        Some(workspace_root),
                        &alt,
                        &dir,
                    )
                    .map_err(|_| content_error)
                } else {
                    Err(content_error)
                }
            }
        }
    })
    .await?;
    Ok((a, false))
}

pub(crate) fn prepare_local_for_scope(
    scope: &crate::context_repo::caller_scope::CallerScope,
    path: &std::path::Path,
    alt: &str,
    cache_dir: &std::path::Path,
) -> Result<Attachment, String> {
    let bytes = read_scoped_media(scope, path)?;
    cache_local_media(&bytes, alt, cache_dir)
}

pub(crate) fn prepare_local_for_scope_or_workspace(
    scope: &crate::context_repo::caller_scope::CallerScope,
    path: &std::path::Path,
    workspace_root: Option<&std::path::Path>,
    alt: &str,
    cache_dir: &std::path::Path,
) -> Result<Attachment, String> {
    if matches!(
        scope,
        crate::context_repo::caller_scope::CallerScope::Unknown
    ) {
        return Err("local media sender is unknown".into());
    }
    if let Ok(a) = prepare_local_for_scope(scope, path, alt, cache_dir) {
        return Ok(a);
    }
    let root = workspace_root.ok_or_else(|| "local media is outside sender scope".to_string())?;
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let canonical_file = path.canonicalize().map_err(|e| e.to_string())?;
    if !canonical_file.starts_with(&canonical_root) {
        return Err("local media is outside sender workspace".into());
    }
    let bytes = read_local_media(&canonical_file)?;
    cache_local_media(&bytes, alt, cache_dir)
}

fn read_scoped_media(
    scope: &crate::context_repo::caller_scope::CallerScope,
    path: &std::path::Path,
) -> Result<Vec<u8>, String> {
    use crate::agents::content_reader::ContentReader;
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    match scope {
        crate::context_repo::caller_scope::CallerScope::Human => {}
        crate::context_repo::caller_scope::CallerScope::Team(view) => {
            if crate::context_repo::inheritance::real_content_owner(&path).is_none()
                || !matches!(
                    view.admits_path(&path),
                    crate::context_repo::inheritance::Admit::Own
                        | crate::context_repo::inheritance::Admit::Inherited { .. }
                )
            {
                return Err("local media is outside team content".into());
            }
        }
        crate::context_repo::caller_scope::CallerScope::Unknown => {
            return Err("local media sender is unknown".into());
        }
    }
    read_limited(scope.open(&path).map_err(|e| e.to_string())?)
}

fn read_local_media(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    read_limited(file)
}

fn read_limited(mut reader: impl std::io::Read) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn cache_local_media(
    bytes: &[u8],
    alt: &str,
    cache_dir: &std::path::Path,
) -> Result<Attachment, String> {
    let mut attachment = Attachment::empty(alt);
    Ok(match store_bytes(cache_dir, bytes, &mut attachment) {
        Ok(()) => attachment,
        Err(error) => attachment.failed(Failure {
            retryable: false,
            ..error
        }),
    })
}

pub(crate) async fn fetch(
    app: &tauri::AppHandle,
    team: &crate::commands::dashboard_launch::LaunchTeam,
    mut a: Attachment,
    automatic: bool,
) -> Result<Attachment, String> {
    let config = config(app, team).await?;
    if config.remote == Remote::Off {
        return Ok(Attachment {
            state: "blocked".into(),
            ..a
        });
    }
    if automatic && !config.should_auto_fetch(a.source_host.as_deref().unwrap_or(""), true) {
        a.state = "pending".into();
        a.reason = Some(
            if config.remote == Remote::Ask {
                "ask"
            } else {
                "host"
            }
            .into(),
        );
        return Ok(a);
    }
    let bytes = match download(
        a.source_url.as_deref().ok_or("media source missing")?,
        &config,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(e) => return Ok(a.failed(e)),
    };
    let dir = cache_dir(app)?;
    crate::commands::await_blocking("media.cache", move || {
        let mut a = a;
        Ok(match store_bytes(&dir, &bytes, &mut a) {
            Ok(()) => a,
            Err(e) => a.failed(e),
        })
    })
    .await
}
fn cached_presence(dir: &Path, attachments: &mut [Attachment]) {
    for a in attachments {
        if a.state == "cached"
            && a.sha256
                .as_ref()
                .is_some_and(|sha| !dir.join(sha).is_file())
        {
            a.state = "failed".into();
            a.reason = Some("cache".into());
            a.retryable = a.source_url.is_some();
        }
    }
}
pub(crate) fn present_cached(app: &tauri::AppHandle, attachments: &mut [Attachment]) {
    if let Ok(dir) = cache_dir(app) {
        cached_presence(&dir, attachments);
    }
}
pub(crate) fn cached(app: &tauri::AppHandle, sha: &str) -> Result<Vec<u8>, String> {
    read_cached(&cache_dir(app)?, sha)
}

#[cfg(test)]
mod tests;

pub(crate) fn config_sync<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    team: &crate::commands::dashboard_launch::LaunchTeam,
) -> Config {
    let dir = (|| {
        let home = crate::commands::paths::codesense_home_dir()?;
        let profile = crate::commands::local_profile::active(app).ok()?;
        let root =
            crate::context_repo::path_to_org_root(&home, &profile.principal, &team.org).ok()?;
        Some(crate::context_repo::path_to_team_config_dir(
            &root, &team.team,
        ))
    })();
    dir.map(|dir| Config::load(&dir)).unwrap_or_else(|| Config {
        remote: Remote::Off,
        ..Config::default()
    })
}

pub(crate) fn note_config_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    team_dir: &Path,
    org: &str,
    team: &str,
    path: &Path,
) {
    use tauri::Emitter;
    if path != team_dir.join("config/inbox.yml") {
        return;
    }
    let team = crate::commands::dashboard_launch::LaunchTeam {
        org: org.into(),
        team: team.into(),
    };
    let ids = app
        .try_state::<crate::live_hub::LiveHub>()
        .and_then(|hub| hub.inbox_view(&team).ok())
        .map(|view| {
            view.messages
                .into_iter()
                .filter(|m| !m.attachments.is_empty())
                .map(|m| m.message_id)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let _ = app.emit(
        crate::live_hub::inbox_events::INBOX_EVENT,
        crate::live_hub::inbox_events::InboxEvent::new(
            crate::live_hub::inbox_events::InboxChange::Stored,
            &team,
            ids,
        ),
    );
}

fn public_v4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || a == 0
        || a >= 240
        || (a == 100 && (64..=127).contains(&b)))
}
fn public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => public_v4(ip),
        std::net::IpAddr::V6(ip) => {
            let s = ip.segments();
            let low32 = std::net::Ipv4Addr::from(((s[6] as u32) << 16) | s[7] as u32);
            // Translation prefixes can reach any IPv4, so they are judged by the
            // embedded address. The NAT64 local-use prefix (RFC 8215) is refused
            // outright because a local translator may map it anywhere.
            let embedded_ok = match (s[0], s[1]) {
                (0x0064, 0xff9b) if s[2..6] == [0, 0, 0, 0] => public_v4(low32),
                (0x0064, 0xff9b) => false,
                (0x2002, _) => public_v4(std::net::Ipv4Addr::from(
                    ((s[1] as u32) << 16) | s[2] as u32,
                )),
                // Teredo and documentation ranges.
                (0x2001, 0x0000) | (0x2001, 0x0db8) => false,
                _ => true,
            };
            embedded_ok
                && ip.to_ipv4().is_none()
                && !(ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local()
                    || ip.is_multicast()
                    || s[0] & 0xffc0 == 0xfec0)
        }
    }
}
fn public_url(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => public_ip(ip.into()),
        Some(url::Host::Ipv6(ip)) => public_ip(ip.into()),
        Some(url::Host::Domain(_)) => true,
        None => false,
    }
}
async fn public_dns(name: &str) -> Result<Vec<std::net::SocketAddr>, Failure> {
    let ips: Vec<_> = tokio::net::lookup_host((name, 0))
        .await
        .map_err(network)?
        .collect();
    if ips.is_empty() || ips.iter().any(|addr| !public_ip(addr.ip())) {
        return Err(failure("address"));
    }
    Ok(ips)
}
struct PublicDns;
impl reqwest::dns::Resolve for PublicDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let name = name.as_str().to_owned();
        Box::pin(async move {
            let ips = public_dns(&name).await.map_err(|failure| {
                let message = if failure.reason == "address" {
                    "media-private-address"
                } else {
                    "media DNS lookup failed"
                };
                Box::new(std::io::Error::other(message)) as Box<dyn std::error::Error + Send + Sync>
            })?;
            Ok(Box::new(ips.into_iter()) as reqwest::dns::Addrs)
        })
    }
}
