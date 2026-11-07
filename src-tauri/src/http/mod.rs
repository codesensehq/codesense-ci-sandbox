// SPDX-License-Identifier: MIT
pub mod app_origins;
pub mod frame_boundary;
pub mod registry;
pub mod routes;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use tauri::Manager;
use tokio::net::TcpListener;

/// Port the HTTP API server is bound to. Registered as Tauri managed state
/// so other commands (e.g. a terminal spawn) can read it without filesystem I/O.
pub struct HttpApiPort(pub u16);

/// Internal credential for non-frame read clients. Never sent to a document.
pub struct ProxyCapabilityState(pub String);

/// Mint a fresh 256-bit random secret, hex-encoded (64 chars). Shared by
/// `ProxyCapabilityState` (SW-LD-2) and the SW-DX-3 scoped SSE token store
/// (`dashboard_stream::token`) so both draw capability tokens from ONE primitive.
pub fn random_hex_secret() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(64);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Constant-time byte comparison. Shared capability-token primitive. The early
/// length check only reveals the secret's (fixed, non-secret) length; the byte
/// loop then avoids a short-circuit timing signal on the secret's value.
pub fn constant_time_eq(secret: &[u8], given: &[u8]) -> bool {
    if secret.len() != given.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in secret.iter().zip(given.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

impl ProxyCapabilityState {
    /// Mint a fresh 256-bit random secret, hex-encoded.
    pub fn new_random() -> Self {
        Self(random_hex_secret())
    }

    /// Constant-time comparison of a presented token against the secret.
    pub fn matches(&self, presented: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), presented.as_bytes())
    }
}

/// Shared state passed to axum route handlers via `State<AppState>`.
#[derive(Clone)]
pub struct AppState {
    pub app_handle: tauri::AppHandle,
    pub app_origins: app_origins::AppOrigins,
}

/// Returns `~/.codesense/http.port`.
///
/// Deprecated: superseded by `registry::registry_path()` (`servers.json`).
/// Retained for backward-compat file write — remove in v0.4.0.
fn port_file_path() -> PathBuf {
    crate::commands::paths::codesense_home_dir()
        .expect("could not determine home directory")
        .join("http.port")
}

/// Returns `~/.codesense/http.last-port`: the port this app last listened on. Unlike
/// `http.port` it survives exit, so the next start can take the same port back.
fn last_port_file_path() -> PathBuf {
    crate::commands::paths::codesense_home_dir()
        .expect("could not determine home directory")
        .join("http.last-port")
}

/// Bind the HTTP API listener. An explicit `requested` port is used as given. With
/// none (`0`), the port the previous instance listened on (`previous`) is tried
/// first, then any port.
///
/// Terminal sessions outlive the app (the session server keeps their PTYs through a
/// restart or update), and every process in them carries the port of the app that
/// spawned it in `CODESENSE_API_PORT`: hook scripts that post status or inbox events
/// read it first. Taking the same port back keeps those sessions reaching the app.
/// When it is held (another instance still runs, or something else took it) the OS
/// assigns one, and hooks fall back to `http.port`.
async fn bind_api_listener(
    requested: u16,
    previous: Option<u16>,
) -> std::io::Result<(TcpListener, TcpListener)> {
    bind_api_listener_with(requested, previous, |port| async move {
        let v4 = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let port = v4.local_addr()?.port();
        let v6 = TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port)).await?;
        Ok((v4, v6))
    })
    .await
}

/// [`bind_api_listener`] over any `bind` (the ports it tries, in order, are testable).
async fn bind_api_listener_with<L, F, Fut>(
    requested: u16,
    previous: Option<u16>,
    bind: F,
) -> std::io::Result<L>
where
    F: Fn(u16) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<L>>,
{
    if requested == 0 {
        if let Some(port) = previous.filter(|port| *port != 0) {
            match bind(port).await {
                Ok(listener) => return Ok(listener),
                Err(e) => log::info!(
                    target: "http",
                    "Previous HTTP address {} unavailable ({}); binding a new one",
                    SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                    e
                ),
            }
        }
    }
    bind(requested).await
}

/// The port to record in `http.last-port` for the next start to take back: only a
/// port the OS chose (an explicit CODESENSE_HTTP_PORT, as tests and a second dev
/// instance use, is not remembered), and only by the sole live instance, so a second
/// instance never takes over the first one's port after it quits.
fn port_to_remember(requested: u16, bound: u16, sole_instance: bool) -> Option<u16> {
    (requested == 0 && sole_instance).then_some(bound)
}

/// The port recorded in `path`, if any.
fn read_port_file(path: &std::path::Path) -> Option<u16> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Returns the canonical port-file path for cleanup from outside this module.
///
/// Deprecated: superseded by `registry::deregister()`. Remove in v0.4.0.
pub fn http_port_file_path() -> PathBuf {
    port_file_path()
}

/// Starts the HTTP API server on localhost.
///
/// - Binds to `localhost:0` (OS-assigned port) unless `CODESENSE_HTTP_PORT`
///   env var overrides with a specific port.
/// - Writes the chosen port to `~/.codesense/http.port`.
/// - Stores the port as Tauri managed state (`HttpApiPort`).
///
/// Called from Tauri's `setup()` hook.
pub fn start_http_server(app_handle: tauri::AppHandle) {
    if std::env::var("CODESENSE_DISABLE_HTTP").is_ok() {
        log::info!(target: "http", "HTTP API server disabled via CODESENSE_DISABLE_HTTP");
        return;
    }

    let requested_port: u16 = std::env::var("CODESENSE_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    tauri::async_runtime::spawn(async move {
        let previous_port = read_port_file(&last_port_file_path());
        let (listener, listener_v6) = match bind_api_listener(requested_port, previous_port).await {
            Ok(l) => l,
            Err(e) => {
                log::error!(target: "http", "Failed to bind HTTP server on {}: {}", SocketAddr::from((Ipv4Addr::LOCALHOST, requested_port)), e);
                return;
            }
        };

        let bound_port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => {
                log::error!(target: "http", "Failed to get local address: {}", e);
                return;
            }
        };

        // Store port in Tauri managed state for internal consumers.
        app_handle.manage(HttpApiPort(bound_port));

        // Register in the multi-instance server registry.
        if let Err(e) = registry::register(bound_port) {
            log::warn!(target: "http", "Failed to register in server registry: {}", e);
        }

        // Deprecated: also write http.port for backward compat (remove in v0.4.0).
        let port_file = port_file_path();
        if let Some(dir) = port_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&port_file, bound_port.to_string()) {
            log::warn!(target: "http", "Failed to write port file {:?}: {}", port_file, e);
        }
        // The port the next start tries first (see `bind_api_listener`).
        if let Some(port) =
            port_to_remember(requested_port, bound_port, registry::sole_live_instance())
        {
            let _ = std::fs::write(last_port_file_path(), port.to_string());
        }

        let state = AppState {
            app_origins: app_origins::AppOrigins::from_dev_url(
                app_handle
                    .config()
                    .build
                    .dev_url
                    .as_ref()
                    .map(|url| url.as_str()),
            ),
            app_handle: app_handle.clone(),
        };
        let app = routes::api_router(state);

        log::info!(target: "http", "HTTP API server listening on http://localhost:{}", bound_port);

        let ipv6_app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = axum::serve(listener_v6, ipv6_app).await {
                log::error!(target: "http", "IPv6 HTTP server error: {}", e);
            }
        });
        if let Err(e) = axum::serve(listener, app.into_make_service()).await {
            log::error!(target: "http", "HTTP server error: {}", e);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_file_path_ends_with_expected_suffix() {
        let path = port_file_path();
        assert!(path.ends_with(".codesense/http.port"));
    }

    #[test]
    fn test_port_file_path_is_absolute() {
        let path = port_file_path();
        assert!(path.is_absolute());
    }

    // SW-LD-2: the proxy capability secret.
    #[test]
    fn test_proxy_capability_is_random_hex() {
        let a = ProxyCapabilityState::new_random();
        let b = ProxyCapabilityState::new_random();
        assert_eq!(a.0.len(), 64, "256-bit secret is 64 hex chars");
        assert!(a.0.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.0, b.0, "each mint is distinct");
    }

    #[test]
    fn test_proxy_capability_matches_is_exact() {
        let cap = ProxyCapabilityState("deadbeef".to_string());
        assert!(cap.matches("deadbeef"));
        assert!(!cap.matches("deadbeee"), "one-byte diff rejected");
        assert!(!cap.matches("deadbee"), "shorter rejected");
        assert!(!cap.matches("deadbeef0"), "longer rejected");
        assert!(!cap.matches(""), "empty rejected");
    }

    /// Sessions that outlive an app restart keep the old port in CODESENSE_API_PORT:
    /// the new instance tries it first, then any port; an explicit port is used alone.
    #[tokio::test]
    async fn api_listener_tries_the_previous_port_first_then_any() {
        async fn run(requested: u16, previous: Option<u16>, free: &[u16]) -> (u16, Vec<u16>) {
            let attempts = std::sync::Mutex::new(Vec::new());
            let bound = bind_api_listener_with(requested, previous, |port| {
                attempts.lock().unwrap().push(port);
                std::future::ready(if port == 0 || free.contains(&port) {
                    Ok(port)
                } else {
                    Err(std::io::Error::from(std::io::ErrorKind::AddrInUse))
                })
            })
            .await
            .unwrap();
            (bound, attempts.into_inner().unwrap())
        }
        // Free: taken back.
        assert_eq!(run(0, Some(54129), &[54129]).await, (54129, vec![54129]));
        // Held: any port.
        assert_eq!(run(0, Some(54129), &[]).await, (0, vec![54129, 0]));
        // None recorded: any port.
        assert_eq!(run(0, None, &[]).await, (0, vec![0]));
        // Explicit wins; the previous one is never tried.
        assert_eq!(
            run(8737, Some(54129), &[8737, 54129]).await,
            (8737, vec![8737])
        );
    }

    /// The real bind: a held previous port yields another port, never an error.
    #[tokio::test]
    async fn api_listener_binds_another_port_when_the_previous_is_held() {
        let held = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let previous = held.local_addr().unwrap().port();
        let other = bind_api_listener(0, Some(previous)).await.unwrap();
        assert_ne!(other.0.local_addr().unwrap().port(), previous);
    }

    #[tokio::test]
    async fn both_loopback_families_answer_on_the_same_port() {
        let (v4, v6) = bind_api_listener(0, None).await.unwrap();
        let port = v4.local_addr().unwrap().port();
        assert_eq!(port, v6.local_addr().unwrap().port());
        let app = axum::Router::new().route("/health", axum::routing::get(|| async { "ok" }));
        let app6 = app.clone();
        let t4 = tokio::spawn(async move {
            axum::serve(v4, app).await.unwrap();
        });
        let t6 = tokio::spawn(async move {
            axum::serve(v6, app6).await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for address in [
            format!("http://127.0.0.1:{port}/health"),
            format!("http://[::1]:{port}/health"),
        ] {
            assert_eq!(
                client
                    .get(address)
                    .send()
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap(),
                "ok"
            );
        }
        t4.abort();
        t6.abort();
    }

    #[test]
    fn only_an_os_chosen_port_of_the_sole_instance_is_remembered() {
        assert_eq!(port_to_remember(0, 54129, true), Some(54129));
        assert_eq!(
            port_to_remember(0, 54129, false),
            None,
            "another instance runs"
        );
        assert_eq!(port_to_remember(8737, 8737, true), None, "explicit port");
    }

    #[test]
    fn port_file_reads_a_port_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("http.last-port");
        assert_eq!(read_port_file(&path), None);
        std::fs::write(&path, "54129\n").unwrap();
        assert_eq!(read_port_file(&path), Some(54129));
        std::fs::write(&path, "junk").unwrap();
        assert_eq!(read_port_file(&path), None);
    }

    #[tokio::test]
    async fn test_health_endpoint_over_tcp() {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        // Use just the health router — no AppState needed for health.
        let app = routes::health::router();

        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = reqwest::get(format!("http://localhost:{}/api/v1/health", port))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");
        assert!(!body["version"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_port_file_write_and_cleanup() {
        let dir = std::env::temp_dir().join(format!("http-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let port_file = dir.join("http.port");

        // Write
        std::fs::write(&port_file, "12345").unwrap();
        assert_eq!(std::fs::read_to_string(&port_file).unwrap(), "12345");

        // Cleanup
        std::fs::remove_file(&port_file).unwrap();
        assert!(!port_file.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
