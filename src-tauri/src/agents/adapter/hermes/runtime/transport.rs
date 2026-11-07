// SPDX-License-Identifier: MIT
//! Only preparation transport settings cross the ambient-environment boundary.
//! Registry/index/auth credentials and provider keys never match these classes.
use std::ffi::OsString;

pub(super) const EXACT_ENV: &[&str] = &[
    // Proxy and bypass spellings consumed by uv, requests and npm's proxy agent.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    // uv/Node's certificate paths and the pinned requests CA-file fallbacks.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    // Node's additional certificates and operating-system trust switch.
    "NODE_EXTRA_CA_CERTS",
    "NODE_USE_SYSTEM_CA",
];

// These are npm's canonical transport keys, not an npm_config_* wildcard.
pub(super) const NPM_KEYS: &[&str] = &[
    "ca",
    "cafile",
    "strict-ssl",
    "proxy",
    "https-proxy",
    "noproxy",
];

// Pinned pm/index_config.py forwards native TLS, insecure hosts and HTTP timeout.
// uv also reads UV_SYSTEM_CERTS, but pinned Hermes drops it in its own child filter:
// retain it in the parent without claiming to fix that upstream limitation.
pub(super) const UV_ENV: &[&str] = &[
    "UV_NATIVE_TLS",
    "UV_SYSTEM_CERTS",
    "UV_INSECURE_HOST",
    "UV_HTTP_TIMEOUT",
];

fn npm_key(name: &str) -> Option<String> {
    const PREFIX: &str = "npm_config_";
    if !name.get(..PREFIX.len())?.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let key = &name[PREFIX.len()..];
    // Match Config.loadEnv(): nerf-darted names aren't normalized, and the first
    // underscore is preserved. Neither form can match a transport key by accident.
    if str::starts_with(key, "//") {
        return None;
    }
    Some(
        key.chars()
            .enumerate()
            .map(|(index, ch)| {
                if ch == '_' && index > 0 {
                    '-'
                } else {
                    ch.to_ascii_lowercase()
                }
            })
            .collect(),
    )
}

fn allowed(name: &str) -> bool {
    EXACT_ENV.contains(&name)
        || UV_ENV.contains(&name)
        || npm_key(name).is_some_and(|key| NPM_KEYS.contains(&String::as_str(&key)))
}

pub(super) fn inherited() -> Vec<(OsString, OsString)> {
    let mut result = Vec::new();
    for (name, value) in std::env::vars_os() {
        if name.to_str().is_some_and(allowed) {
            result.push((name, value));
        }
    }
    result
}
