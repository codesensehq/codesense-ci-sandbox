// SPDX-License-Identifier: MIT
/// The app's trusted embedding origins, fixed when its HTTP server starts.
#[derive(Clone, Default)]
pub struct AppOrigins {
    dev_origin: Option<String>,
}

const PACKAGED_ORIGINS: [&str; 3] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

impl AppOrigins {
    pub fn from_dev_url(dev_url: Option<&str>) -> Self {
        #[cfg(debug_assertions)]
        let dev_origin = dev_url.and_then(|value| {
            let url = url::Url::parse(value).ok();
            let origin = url
                .as_ref()
                .filter(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
                })
                .map(|url| url.origin().ascii_serialization());
            if origin.is_none() {
                let reason = if url.as_ref().and_then(|url| url.host_str()) == Some("[::1]") {
                    "IPv6 loopback can't be expressed in WebKit frame-ancestors; use localhost"
                } else {
                    "dev origin must use HTTP(S) on localhost or 127.0.0.1"
                };
                log::warn!(target: "http", "Ignoring devUrl {value:?}: {reason}");
            }
            origin
        });
        #[cfg(not(debug_assertions))]
        let dev_origin = {
            let _ = dev_url;
            None
        };
        Self { dev_origin }
    }

    pub fn is_app_origin(&self, origin: &str) -> bool {
        PACKAGED_ORIGINS.contains(&origin) || self.dev_origin.as_deref() == Some(origin)
    }

    pub fn frame_ancestors(&self) -> String {
        let mut csp = format!("frame-ancestors {}", PACKAGED_ORIGINS.join(" "));
        if let Some(origin) = &self.dev_origin {
            csp.push(' ');
            csp.push_str(origin);
        }
        csp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_origins_remain_trusted_without_dev_configuration() {
        let origins = AppOrigins::from_dev_url(None);
        for origin in PACKAGED_ORIGINS {
            assert!(origins.is_app_origin(origin));
        }
        for origin in [
            "http://localhost:1420",
            "http://localhost:5328",
            "https://example.com",
            "null",
        ] {
            assert!(!origins.is_app_origin(origin), "{origin}");
        }
        assert_eq!(
            origins.frame_ancestors(),
            "frame-ancestors tauri://localhost http://tauri.localhost https://tauri.localhost"
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    fn debug_accepts_only_its_configured_port_and_exact_hostname() {
        let origins = AppOrigins::from_dev_url(Some("http://localhost:5328/a/path"));
        assert!(origins.is_app_origin("http://localhost:5328"));
        for port in 1..=u16::MAX {
            if port != 5328 {
                assert!(
                    !origins.is_app_origin(&format!("http://localhost:{port}")),
                    "{port}"
                );
            }
        }
        for origin in [
            "http://127.0.0.1:5328",
            "http://[::1]:5328",
            "https://localhost:5328",
        ] {
            assert!(!origins.is_app_origin(origin), "{origin}");
        }
        assert_eq!(origins.frame_ancestors(), "frame-ancestors tauri://localhost http://tauri.localhost https://tauri.localhost http://localhost:5328");
    }

    #[test]
    #[cfg(debug_assertions)]
    fn debug_accepts_configured_loopback_aliases_without_trusting_other_hosts() {
        for origin in ["http://127.0.0.1:5328", "https://localhost:5328"] {
            let origins = AppOrigins::from_dev_url(Some(origin));
            assert!(origins.is_app_origin(origin), "{origin}");
            assert!(!origins.is_app_origin("http://localhost:5328"));
        }
        for url in [
            "https://example.com:5328",
            "http://other.localhost:5328",
            "file:///tmp/index.html",
        ] {
            let origins = AppOrigins::from_dev_url(Some(url));
            assert_eq!(
                origins.frame_ancestors(),
                AppOrigins::default().frame_ancestors()
            );
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    fn debug_rejects_ipv6_configuration_without_an_ancestor_source() {
        for dev_url in ["http://[::1]:5328", "https://[::1]:5328"] {
            let origins = AppOrigins::from_dev_url(Some(dev_url));
            for origin in [dev_url, "http://localhost:5328", "http://127.0.0.1:5328"] {
                assert!(!origins.is_app_origin(origin), "{dev_url}: {origin}");
            }
            assert_eq!(
                origins.frame_ancestors(),
                AppOrigins::default().frame_ancestors()
            );
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    fn debug_ipv4_configuration_trusts_only_its_exact_origin_and_ancestor() {
        let origins = AppOrigins::from_dev_url(Some("http://127.0.0.1:5328/a/path"));
        assert!(origins.is_app_origin("http://127.0.0.1:5328"));
        for origin in [
            "http://localhost:5328",
            "http://[::1]:5328",
            "http://127.0.0.1:1420",
            "http://127.0.0.1:5329",
            "https://127.0.0.1:5328",
        ] {
            assert!(!origins.is_app_origin(origin), "{origin}");
        }
        assert_eq!(origins.frame_ancestors(), "frame-ancestors tauri://localhost http://tauri.localhost https://tauri.localhost http://127.0.0.1:5328");
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn release_ignores_dev_configuration_and_rejects_all_loopback_ports() {
        for dev_url in [
            "http://localhost:5328",
            "http://localhost:1420",
            "http://[::1]:5328",
            "https://example.com",
        ] {
            let origins = AppOrigins::from_dev_url(Some(dev_url));
            assert_eq!(
                origins.frame_ancestors(),
                AppOrigins::default().frame_ancestors()
            );
            for origin in PACKAGED_ORIGINS {
                assert!(origins.is_app_origin(origin), "{origin}");
            }
            for port in 1..=u16::MAX {
                for host in ["localhost", "127.0.0.1", "[::1]"] {
                    for scheme in ["http", "https"] {
                        assert!(!origins.is_app_origin(&format!("{scheme}://{host}:{port}")));
                    }
                }
            }
            for origin in [
                "https://example.com",
                "http://other.localhost:5328",
                "null",
                "tauri://localhost/",
            ] {
                assert!(!origins.is_app_origin(origin), "{origin}");
            }
        }
    }
}
