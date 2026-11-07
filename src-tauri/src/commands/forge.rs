// SPDX-License-Identifier: MIT
//! Repository identity and transport host vocabulary; no service access.
struct GitForgeHost {
    host: &'static str,
    provider: &'static str,
}

static GIT_FORGE_HOSTS: &[GitForgeHost] = &[
    GitForgeHost {
        host: "github.com",
        provider: "github",
    },
    GitForgeHost {
        host: "gitlab.com",
        provider: "gitlab",
    },
    GitForgeHost {
        host: "bitbucket.org",
        provider: "bitbucket",
    },
];

/// Whether `host` is a git forge codesense can transport to — the supported-forge
/// membership test the context-repo URL gate consults in place of the literal
/// `github.com` (SW-FG-7 AC-3).
pub fn is_supported_git_host(host: &str) -> bool {
    provider_for_host(host).is_some()
}

pub fn provider_for_host(host: &str) -> Option<&'static str> {
    GIT_FORGE_HOSTS
        .iter()
        .find(|entry| host.eq_ignore_ascii_case(entry.host))
        .map(|entry| entry.provider)
}

/// The canonical cloud host registered for a provider id.
pub fn host_for_provider(provider: &str) -> Option<&'static str> {
    GIT_FORGE_HOSTS
        .iter()
        .find(|entry| provider.eq_ignore_ascii_case(entry.provider))
        .map(|entry| entry.host)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountSelection<T> {
    None,
    One(T),
    Ambiguous,
}

pub fn select_provider_account<T: Clone>(accounts: &[T]) -> AccountSelection<T> {
    match accounts {
        [] => AccountSelection::None,
        [account] => AccountSelection::One(account.clone()),
        _ => AccountSelection::Ambiguous,
    }
}
