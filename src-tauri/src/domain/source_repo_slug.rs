// SPDX-License-Identifier: MIT
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use super::error::DomainError;
use super::validation::validate_non_empty_no_control;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, schemars::JsonSchema)]
pub struct SourceRepoSlug {
    pub owner: String,
    pub repo: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Canonical, collision-free physical namespace for repository-owned state.
/// Display casing stays on [`SourceRepoSlug`]; disk identity follows
/// [`SourceRepoSlug::remote_key`] so case-insensitive filesystems cannot alias
/// two spellings of the same repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryPathKey {
    host: String,
    owner: String,
    repo: String,
}

impl RepositoryPathKey {
    pub fn append_to(&self, root: &Path) -> PathBuf {
        root.join(&self.host).join(&self.owner).join(&self.repo)
    }
}

impl TryFrom<&SourceRepoSlug> for RepositoryPathKey {
    type Error = DomainError;

    fn try_from(slug: &SourceRepoSlug) -> Result<Self, Self::Error> {
        Ok(Self {
            host: encode_physical_component(slug.canonical_host(), "host")?,
            owner: encode_physical_component(&slug.owner, "owner")?,
            repo: encode_physical_component(&slug.repo, "repo")?,
        })
    }
}

fn encode_physical_component(value: &str, field: &'static str) -> Result<String, DomainError> {
    let folded = value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_uppercase() {
                byte + 32
            } else {
                byte
            }
        })
        .collect::<Vec<_>>();
    if folded.is_empty()
        || folded == b"."
        || folded == b".."
        || folded
            .iter()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'/' | b'\\'))
    {
        return Err(DomainError::InvalidSourceRepoSegment {
            field,
            segment: value.to_owned(),
            reason: "cannot be represented as a physical path component",
        });
    }
    let upper = value.trim_end_matches(['.', ' ']).to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or_default();
    let reserved = matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && matches!(&stem[..3], "COM" | "LPT")
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    let mut encoded = String::new();
    for (index, byte) in folded.iter().copied().enumerate() {
        let final_dot_or_space = index + 1 == folded.len() && matches!(byte, b'.' | b' ');
        let safe = byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'_' | b'-');
        if safe && !final_dot_or_space && !(reserved && index == 0) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    if encoded.len() > 200 {
        return Err(DomainError::InvalidSourceRepoSegment {
            field,
            segment: value.to_owned(),
            reason: "encoded physical path component exceeds 200 bytes",
        });
    }
    Ok(encoded)
}

#[derive(Deserialize)]
struct SourceRepoSlugWire {
    owner: String,
    repo: String,
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default)]
    host: Option<String>,
}

impl<'de> Deserialize<'de> for SourceRepoSlug {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SourceRepoSlugWire::deserialize(deserializer)?;
        Self::with_provider_and_host(wire.owner, wire.repo, wire.provider, wire.host)
            .map_err(serde::de::Error::custom)
    }
}

fn default_provider() -> String {
    "github".to_string()
}

impl SourceRepoSlug {
    pub fn new(owner: impl Into<String>, repo: impl Into<String>) -> Result<Self, DomainError> {
        let owner: String = owner.into();
        let repo: String = repo.into();
        validate_segment(&owner, "owner")?;
        validate_segment(&repo, "repo")?;
        Ok(Self {
            owner,
            repo,
            provider: default_provider(),
            host: Some("github.com".to_string()),
        })
    }

    pub fn with_provider(
        owner: impl Into<String>,
        repo: impl Into<String>,
        provider: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let mut slug = Self::new(owner, repo)?;
        let provider = provider.into().trim().to_ascii_lowercase();
        validate_segment(&provider, "provider")?;
        let host = crate::commands::forge::host_for_provider(&provider).ok_or_else(|| {
            DomainError::InvalidSourceRepoSegment {
                field: "host",
                segment: String::new(),
                reason: "is required for an unknown provider",
            }
        })?;
        slug.provider = provider;
        slug.host = Some(host.to_owned());
        Ok(slug)
    }

    pub fn with_provider_and_host(
        owner: impl Into<String>,
        repo: impl Into<String>,
        provider: impl Into<String>,
        host: Option<impl Into<String>>,
    ) -> Result<Self, DomainError> {
        let mut slug = Self::new(owner, repo)?;
        let provider = provider.into().trim().to_ascii_lowercase();
        validate_segment(&provider, "provider")?;
        slug.provider = provider;
        slug.host = crate::commands::forge::host_for_provider(&slug.provider).map(str::to_owned);
        if let Some(host) = host {
            let host = host
                .into()
                .trim()
                .trim_end_matches('.')
                .to_ascii_lowercase();
            validate_host(&host)?;
            if let Some(expected) = crate::commands::forge::host_for_provider(&slug.provider) {
                if !host.eq_ignore_ascii_case(expected) {
                    return Err(DomainError::InvalidSourceRepoSegment {
                        field: "host",
                        segment: host,
                        reason: "does not match provider",
                    });
                }
            }
            slug.host = Some(host);
        }
        if slug.host.is_none() {
            return Err(DomainError::InvalidSourceRepoSegment {
                field: "host",
                segment: String::new(),
                reason: "is required for an unknown provider",
            });
        }
        Ok(slug)
    }

    /// Canonical host identity. Constructors guarantee this is always present.
    pub fn canonical_host(&self) -> &str {
        self.host
            .as_deref()
            .or_else(|| crate::commands::forge::host_for_provider(&self.provider))
            .expect("SourceRepoSlug invariant: unknown providers require an explicit host")
    }

    /// Canonical HTTPS clone remote for supported cloud forges. Self-managed and
    /// unknown providers fail closed until their auth/transport contract exists.
    pub fn cloud_https_remote(&self) -> Result<String, DomainError> {
        let expected =
            crate::commands::forge::host_for_provider(&self.provider).ok_or_else(|| {
                DomainError::InvalidSourceRepoSegment {
                    field: "provider",
                    segment: self.provider.clone(),
                    reason: "is not a supported cloud forge",
                }
            })?;
        if !matches!(self.provider.as_str(), "github" | "gitlab")
            || !self.canonical_host().eq_ignore_ascii_case(expected)
        {
            return Err(DomainError::InvalidSourceRepoSegment {
                field: "host",
                segment: self.canonical_host().to_string(),
                reason: "is not a supported cloud forge host",
            });
        }
        Ok(format!("https://{expected}/{}/{}", self.owner, self.repo))
    }

    pub fn physical_path_key(&self) -> Result<RepositoryPathKey, DomainError> {
        RepositoryPathKey::try_from(self)
    }

    pub(crate) fn has_valid_host(&self) -> bool {
        match (
            crate::commands::forge::host_for_provider(&self.provider),
            self.host.as_deref(),
        ) {
            (Some(expected), Some(host)) => {
                validate_host(host).is_ok() && host.eq_ignore_ascii_case(expected)
            }
            (Some(_), None) => true,
            (None, Some(host)) => validate_host(host).is_ok(),
            (None, None) => false,
        }
    }

    pub fn remote_key(&self) -> String {
        format!(
            "{}/{}/{}",
            self.canonical_host().to_ascii_lowercase(),
            self.owner.to_ascii_lowercase(),
            self.repo.to_ascii_lowercase()
        )
    }

    /// Parse an `owner/repo` slug string into a [`SourceRepoSlug`]. Returns
    /// `None` for malformed input — no `/`, extra `/` (a path separator in
    /// `repo` is rejected by [`new`](Self::new)), or empty/invalid segments.
    /// Single source of truth for `owner/repo` parsing; callers that want to
    /// surface *why* an entry was dropped log at their own call site.
    pub fn parse_owner_repo(slug: &str) -> Option<Self> {
        let (owner, repo) = slug.trim().split_once('/')?;
        Self::new(owner, repo).ok()
    }

    /// SW-LN-B29: resolve a story's `source_repo` string against the team's CONFIGURED
    /// source repos, so the slug carries the team's real provider + host.
    ///
    /// A story names its repo as `owner/repo` (or `host/owner/repo`); the bare
    /// [`parse_owner_repo`](Self::parse_owner_repo) reads that as a GitHub slug. For a repo
    /// with no remote (`provider: local`, `host: local`) or on any other forge, that
    /// GitHub-shaped slug never matches the team's slug, so every store lookup keyed by the
    /// team's slug (forward fan-out, refocus catch-up) misses the worktree. This picks the
    /// configured slug the string names:
    /// - `host/owner/repo` → the configured slug with that [`remote_key`](Self::remote_key);
    /// - `owner/repo` → the one configured slug with that owner/repo (case-insensitive);
    ///   when several hosts share it, the GitHub one if configured, else the first by
    ///   [`remote_key`](Self::remote_key) (deterministic — never an unconfigured slug).
    ///
    /// An `owner/repo` that matches nothing configured falls back to
    /// [`parse_owner_repo`](Self::parse_owner_repo) (a team with no config yet keeps
    /// today's behaviour). A `host/owner/repo` that matches nothing configured is `None`,
    /// as is malformed input.
    pub fn resolve_against(source_repo: &str, configured: &[SourceRepoSlug]) -> Option<Self> {
        let trimmed = source_repo.trim();
        let parts: Vec<&str> = trimmed.split('/').collect();
        match parts.as_slice() {
            [_, _, _] => {
                let key = trimmed.to_ascii_lowercase();
                configured.iter().find(|s| s.remote_key() == key).cloned()
            }
            [owner, repo] => {
                let matches: Vec<&SourceRepoSlug> = configured
                    .iter()
                    .filter(|s| {
                        s.owner.eq_ignore_ascii_case(owner) && s.repo.eq_ignore_ascii_case(repo)
                    })
                    .collect();
                match matches.as_slice() {
                    [only] => Some((*only).clone()),
                    [] => Self::parse_owner_repo(trimmed),
                    several => several
                        .iter()
                        .find(|s| s.canonical_host().eq_ignore_ascii_case("github.com"))
                        .or_else(|| several.iter().min_by_key(|s| s.remote_key()))
                        .map(|s| (*s).clone()),
                }
            }
            _ => None,
        }
    }

    /// SW-LN-B29: this slug as the team configures it. A slug that is already one of the
    /// `configured` slugs, or that carries its own non-default provider/host, is kept; a
    /// bare GitHub-shaped parse of a configured repo's
    /// `owner/repo` is swapped for the configured slug (see
    /// [`resolve_against`](Self::resolve_against)); anything else is kept unchanged.
    pub fn as_configured(&self, configured: &[SourceRepoSlug]) -> Self {
        let bare_parse = Self::new(self.owner.clone(), self.repo.clone()).ok();
        if configured.contains(self) || bare_parse.as_ref() != Some(self) {
            return self.clone();
        }
        Self::resolve_against(&format!("{}/{}", self.owner, self.repo), configured)
            .unwrap_or_else(|| self.clone())
    }
}

pub(crate) fn validate_host(host: &str) -> Result<(), DomainError> {
    let valid = host.len() <= 253
        && host.parse::<std::net::IpAddr>().is_err()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        });
    if !valid {
        return Err(DomainError::InvalidSourceRepoSegment {
            field: "host",
            segment: host.to_string(),
            reason: "must be a bare DNS name",
        });
    }
    Ok(())
}

fn validate_segment(segment: &str, field: &'static str) -> Result<(), DomainError> {
    // SS-FU-2: leading/trailing whitespace gate, mirroring
    // `validate_path_segment` at content_engine/layers.rs:388-389 so the
    // two segment validators agree on what they reject. The
    // `!trim().is_empty()` guard preserves the pre-existing behavior that
    // pure-whitespace strings ("", "   ", "\t") surface as
    // `DomainError::EmptyField` via `validate_non_empty_no_control`'s
    // `trim().is_empty()` check below — keeps reason-string output stable
    // for the empty case while tightening the whitespace-padded case.
    let trimmed = segment.trim();
    if !trimmed.is_empty() && segment != trimmed {
        return Err(DomainError::InvalidSourceRepoSegment {
            field,
            segment: segment.to_owned(),
            reason: "leading or trailing whitespace",
        });
    }

    validate_non_empty_no_control(segment, field)?;

    // Slug-specific: path traversal
    if trimmed == "." || trimmed == ".." {
        return Err(DomainError::InvalidSourceRepoSegment {
            field,
            segment: segment.to_string(),
            reason: "path traversal",
        });
    }

    // Slug-specific: path separators
    if segment.contains('/') || segment.contains('\\') {
        return Err(DomainError::InvalidSourceRepoSegment {
            field,
            segment: segment.to_string(),
            reason: "path separator",
        });
    }
    Ok(())
}

impl fmt::Display for SourceRepoSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(repo: &str) -> SourceRepoSlug {
        SourceRepoSlug::with_provider_and_host("local", repo, "local", Some("local")).unwrap()
    }

    // SW-LN-B29: a local (no-remote) repo's story says `local/fixture-api`; the bare parse
    // reads that as GitHub, which never matches the team's `local`-host slug. Resolving
    // against the team's configured repos yields the team's own slug.
    #[test]
    fn resolve_against_picks_the_configured_local_slug() {
        let configured = vec![local("fixture-api"), local("fixture-app")];
        assert_eq!(
            SourceRepoSlug::resolve_against("local/fixture-api", &configured),
            Some(local("fixture-api"))
        );
        assert_eq!(
            SourceRepoSlug::resolve_against("Local/Fixture-App", &configured),
            Some(local("fixture-app"))
        );
        assert_eq!(
            SourceRepoSlug::resolve_against("local/local/fixture-app", &configured),
            Some(local("fixture-app"))
        );
    }

    #[test]
    fn resolve_against_falls_back_to_the_github_parse() {
        let gh = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(
            SourceRepoSlug::resolve_against("alderco/codesense", &[]),
            Some(gh.clone())
        );
        assert_eq!(
            SourceRepoSlug::resolve_against("alderco/codesense", &[local("other")]),
            Some(gh.clone())
        );
        // Two hosts share owner/repo → the GitHub one wins (the legacy reading).
        let both = vec![
            local("codesense"),
            SourceRepoSlug::new("local", "codesense").unwrap(),
        ];
        assert_eq!(
            SourceRepoSlug::resolve_against("local/codesense", &both),
            Some(SourceRepoSlug::new("local", "codesense").unwrap())
        );
        // Two NON-GitHub hosts share owner/repo → a configured one, deterministically, never
        // the unconfigured GitHub parse.
        let gl = SourceRepoSlug::with_provider_and_host(
            "local",
            "codesense",
            "gitlab",
            Some("gitlab.com"),
        )
        .unwrap();
        let two = vec![local("codesense"), gl.clone()];
        let picked = SourceRepoSlug::resolve_against("local/codesense", &two).unwrap();
        assert!(two.contains(&picked));
        assert_eq!(
            SourceRepoSlug::resolve_against("local/codesense", &[gl.clone(), local("codesense")]),
            Some(picked)
        );
        assert_eq!(SourceRepoSlug::resolve_against("nope", &[]), None);
        assert_eq!(SourceRepoSlug::resolve_against("gitlab.com/a/b", &[]), None);
    }

    #[test]
    fn as_configured_swaps_only_a_bare_github_parse() {
        let configured = vec![local("fixture-api")];
        let bare = SourceRepoSlug::parse_owner_repo("local/fixture-api").unwrap();
        assert_eq!(bare.as_configured(&configured), local("fixture-api"));
        // Already configured, or carrying its own host: unchanged.
        assert_eq!(
            local("fixture-api").as_configured(&configured),
            local("fixture-api")
        );
        let gl = SourceRepoSlug::with_provider_and_host(
            "local",
            "fixture-api",
            "gitlab",
            Some("gitlab.com"),
        )
        .unwrap();
        assert_eq!(gl.as_configured(&configured), gl);
    }

    #[test]
    fn happy_path() {
        let s = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(s.owner, "alderco");
        assert_eq!(s.repo, "codesense");
    }

    #[test]
    fn rejects_empty_owner() {
        assert_eq!(
            SourceRepoSlug::new("", "r"),
            Err(DomainError::EmptyField { field: "owner" })
        );
    }

    #[test]
    fn rejects_empty_repo() {
        assert_eq!(
            SourceRepoSlug::new("o", ""),
            Err(DomainError::EmptyField { field: "repo" })
        );
    }

    #[test]
    fn display_format() {
        let s = SourceRepoSlug::new("alderco", "codesense").unwrap();
        assert_eq!(format!("{}", s), "alderco/codesense");
    }

    #[test]
    fn serde_roundtrip() {
        let s = SourceRepoSlug::new("o", "r").unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let back: SourceRepoSlug = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn provider_defaults_and_roundtrips() {
        assert_eq!(SourceRepoSlug::new("o", "r").unwrap().provider, "github");
        let slug = SourceRepoSlug::with_provider("o", "r", " GitLab ").unwrap();
        assert_eq!(slug.provider, "gitlab");
        assert_eq!(
            serde_json::from_str::<SourceRepoSlug>(&serde_json::to_string(&slug).unwrap()).unwrap(),
            slug
        );
    }

    #[test]
    fn remote_key_defaults_legacy_rows_to_github_and_separates_hosts() {
        let github = SourceRepoSlug::new("Acme", "Api").unwrap();
        let gitlab = SourceRepoSlug::with_provider("Acme", "Api", "gitlab").unwrap();
        assert_eq!(github.remote_key(), "github.com/acme/api");
        assert_eq!(gitlab.remote_key(), "gitlab.com/acme/api");
        assert_ne!(github.remote_key(), gitlab.remote_key());
    }

    #[test]
    fn rejects_empty_or_unsafe_provider() {
        assert!(SourceRepoSlug::with_provider("o", "r", "   ").is_err());
        assert!(SourceRepoSlug::with_provider("o", "r", "git\nlab").is_err());
    }

    #[test]
    fn explicit_host_roundtrips_and_mismatch_is_rejected() {
        let slug = SourceRepoSlug::with_provider_and_host("o", "r", "gitlab", Some("GitLab.com."))
            .unwrap();
        assert_eq!(slug.host.as_deref(), Some("gitlab.com"));
        assert!(
            SourceRepoSlug::with_provider_and_host("o", "r", "gitlab", Some("github.com")).is_err()
        );
    }

    #[test]
    fn unknown_provider_requires_a_bare_dns_host() {
        assert!(SourceRepoSlug::with_provider_and_host("o", "r", "gitea", None::<String>).is_err());
        for host in [
            "https://git.example.com",
            "git.example.com/path",
            "127.0.0.1",
            "[::1]",
            "-",
            "bad-.example.com",
        ] {
            assert!(
                SourceRepoSlug::with_provider_and_host("o", "r", "gitea", Some(host)).is_err(),
                "host {host:?} must be rejected"
            );
        }
        let slug =
            SourceRepoSlug::with_provider_and_host("o", "r", "gitea", Some("Git.Example.COM."))
                .unwrap();
        assert_eq!(slug.remote_key(), "git.example.com/o/r");
        assert_eq!(
            slug.physical_path_key()
                .expect("repository identity has a physical path key")
                .append_to(Path::new("/base")),
            PathBuf::from("/base/git.example.com/o/r")
        );
    }

    #[test]
    fn physical_path_key_namespaces_all_hosts_and_folds_identity_case() {
        let github = SourceRepoSlug::new("Acme", "Api").unwrap();
        let gitlab = SourceRepoSlug::with_provider("Acme", "Api", "gitlab").unwrap();
        assert_eq!(
            github
                .physical_path_key()
                .expect("repository identity has a physical path key")
                .append_to(Path::new("/base")),
            PathBuf::from("/base/github.com/acme/api")
        );
        assert_eq!(
            gitlab
                .physical_path_key()
                .expect("repository identity has a physical path key")
                .append_to(Path::new("/base")),
            PathBuf::from("/base/gitlab.com/acme/api")
        );
    }

    #[test]
    fn identity_accepts_reserved_names_while_physical_key_encodes_them() {
        for value in ["CON", "nul.txt", "COM1", "lpt9.log", "name.", "bad:name"] {
            let slug = SourceRepoSlug::new(value, "repo").unwrap();
            let path = slug
                .physical_path_key()
                .unwrap()
                .append_to(Path::new("/base"));
            assert!(!path.to_string_lossy().ends_with(&format!("/{value}/repo")));
        }
    }

    #[test]
    fn physical_encoding_is_injective_and_enforces_encoded_byte_budget() {
        let fixtures = ["é", "e\u{301}", "É", "%65", "name.", "CON"];
        let paths = fixtures
            .iter()
            .map(|value| {
                SourceRepoSlug::new(*value, "repo")
                    .unwrap()
                    .physical_path_key()
                    .unwrap()
                    .append_to(Path::new("/base"))
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), fixtures.len());
        assert!(SourceRepoSlug::new("a".repeat(200), "repo")
            .unwrap()
            .physical_path_key()
            .is_ok());
        assert!(SourceRepoSlug::new("a".repeat(201), "repo")
            .unwrap()
            .physical_path_key()
            .is_err());
    }

    #[test]
    fn rejects_owner_with_path_separator_slash() {
        let err = SourceRepoSlug::new("foo/bar", "r").unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::InvalidSourceRepoSegment {
                    field: "owner",
                    reason: "path separator",
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn rejects_owner_with_path_separator_backslash() {
        let err = SourceRepoSlug::new("foo\\bar", "r").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "owner",
                reason: "path separator",
                ..
            }
        ));
    }

    #[test]
    fn rejects_repo_with_path_separator() {
        let err = SourceRepoSlug::new("o", "foo/bar").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "repo",
                reason: "path separator",
                ..
            }
        ));
    }

    #[test]
    fn rejects_path_traversal_double_dot() {
        let err = SourceRepoSlug::new("..", "r").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "owner",
                reason: "path traversal",
                ..
            }
        ));
    }

    #[test]
    fn rejects_path_traversal_single_dot() {
        let err = SourceRepoSlug::new(".", "r").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "owner",
                reason: "path traversal",
                ..
            }
        ));
    }

    // SS-FU-2: whitespace-first ordering means `"  ..  "` now surfaces as
    // "leading or trailing whitespace" rather than "path traversal". Matches
    // `validate_path_segment`'s behavior for the same input (layers.rs:388-389
    // fires before the `==.`/`==..` check at layers.rs:391-393). Rejection is
    // preserved; only the reason returned changes.
    #[test]
    fn rejects_whitespace_padded_path_traversal() {
        let err = SourceRepoSlug::new("  ..  ", "r").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "owner",
                reason: "leading or trailing whitespace",
                ..
            }
        ));
    }

    #[test]
    fn rejects_null_byte() {
        let err = SourceRepoSlug::new("foo\0bar", "r").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "owner",
                reason: "control character",
                ..
            }
        ));
    }

    #[test]
    fn rejects_ascii_control_character() {
        let err = SourceRepoSlug::new("foo\x01bar", "r").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                reason: "control character",
                ..
            }
        ));
    }

    #[test]
    fn rejects_del_character() {
        let err = SourceRepoSlug::new("foo\x7fbar", "r").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                reason: "control character",
                ..
            }
        ));
    }

    #[test]
    fn accepts_dots_in_middle() {
        let s = SourceRepoSlug::new("alice.codes", "my.repo").unwrap();
        assert_eq!(s.owner, "alice.codes");
        assert_eq!(s.repo, "my.repo");
    }

    #[test]
    fn accepts_underscores_and_hyphens() {
        let s = SourceRepoSlug::new("user_name", "repo-name").unwrap();
        assert_eq!(s.owner, "user_name");
        assert_eq!(s.repo, "repo-name");
    }

    #[test]
    fn parse_owner_repo_happy_path() {
        let s = SourceRepoSlug::parse_owner_repo("alderco/codesense").unwrap();
        assert_eq!(s.owner, "alderco");
        assert_eq!(s.repo, "codesense");
    }

    #[test]
    fn parse_owner_repo_trims_surrounding_whitespace() {
        let s = SourceRepoSlug::parse_owner_repo("  alderco/codesense  ").unwrap();
        assert_eq!(s, SourceRepoSlug::new("alderco", "codesense").unwrap());
    }

    #[test]
    fn parse_owner_repo_rejects_malformed() {
        assert!(SourceRepoSlug::parse_owner_repo("noslash").is_none());
        assert!(SourceRepoSlug::parse_owner_repo("a/b/c").is_none()); // path sep in repo
        assert!(SourceRepoSlug::parse_owner_repo("/repo").is_none()); // empty owner
        assert!(SourceRepoSlug::parse_owner_repo("owner/").is_none()); // empty repo
    }

    // SS-FU-2: close asymmetry where `validate_segment` accepted whitespace-padded
    // inputs that `validate_path_segment` (used downstream by `validate_repo_slug`)
    // rejects. New whitespace branch sits at the **top** of `validate_segment`
    // with a `!trim().is_empty()` guard, mirroring `validate_path_segment`'s
    // whitespace-first ordering. Reason wording matches verbatim:
    // "leading or trailing whitespace". See `validate_segment` for the full
    // ordering rationale.

    #[test]
    fn rejects_owner_leading_whitespace() {
        let err = SourceRepoSlug::new(" alderco", "repo").unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::InvalidSourceRepoSegment {
                    field: "owner",
                    reason: "leading or trailing whitespace",
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn rejects_owner_trailing_whitespace() {
        let err = SourceRepoSlug::new("alderco ", "repo").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "owner",
                reason: "leading or trailing whitespace",
                ..
            }
        ));
    }

    #[test]
    fn rejects_repo_trailing_whitespace() {
        let err = SourceRepoSlug::new("alderco", "codesense ").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "repo",
                reason: "leading or trailing whitespace",
                ..
            }
        ));
    }

    #[test]
    fn rejects_owner_tab() {
        let err = SourceRepoSlug::new("\talderco", "repo").unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::InvalidSourceRepoSegment {
                    field: "owner",
                    reason: "leading or trailing whitespace",
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn rejects_owner_non_ascii_whitespace() {
        let err = SourceRepoSlug::new("\u{a0}alderco", "repo").unwrap_err();
        assert!(matches!(
            err,
            DomainError::InvalidSourceRepoSegment {
                field: "owner",
                reason: "leading or trailing whitespace",
                ..
            }
        ));
    }

    #[test]
    fn rejects_repo_leading_whitespace() {
        let err = SourceRepoSlug::new("alderco", " codesense").unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::InvalidSourceRepoSegment {
                    field: "repo",
                    reason: "leading or trailing whitespace",
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    // SS-FU-2 review iter-1 patch (Blind#6): the new whitespace branch is
    // guarded by `!segment.trim().is_empty()` so pure-whitespace and empty
    // inputs fall through to `validate_non_empty_no_control` and surface as
    // `EmptyField`, not as `"leading or trailing whitespace"`. AC-1 explicitly
    // carved this out ("leave that pre-existing rejection in place"). Lock
    // the contract with a test so a future change that drops the guard fails
    // here.
    //
    // SS-FU-2 review iter-2 patch (Edge#1): ASCII-whitespace coverage extended
    // to `\n`, `\r`, `\x0B` (vertical tab), `\x0C` (form feed). All are
    // `char::is_whitespace`, so `str::trim` reduces them to empty — they
    // exercise the same fall-through path. NBSP (U+00A0) and other Unicode
    // *whitespace* characters are also handled correctly (str::trim follows
    // char::is_whitespace which covers them) but are not explicitly asserted
    // here. Unicode *format* characters that are NOT whitespace per
    // char::is_whitespace — ZWSP (U+200B), BOM (U+FEFF), WJ (U+2060) — slip
    // through both validators; that's the genuine gap deferred per iter-1
    // (see deferred-work.md).
    //
    // SS-FU-2 review iter-3 patch (Blind#2): mirror the engine-side parity
    // test by exercising BOTH `owner` and `repo` fields per input. The
    // previous shape only fed `input` into the owner slot, leaving an
    // unchecked asymmetry vs `ac4_cross_api_pure_whitespace_diverges_*` in
    // `content_engine.rs::tests`. A future refactor that special-cased one
    // field on the domain side would have slipped past.
    #[test]
    fn rejects_empty_or_pure_whitespace_as_empty_field() {
        let inputs = ["", "   ", "\t", "\t  ", "\n", "\r", "\r\n", "\x0B", "\x0C"];

        for input in inputs {
            // Owner-side: input in owner slot, clean repo.
            let err = SourceRepoSlug::new(input, "r")
                .expect_err(&format!("owner-side input {input:?} must reject"));
            assert!(
                matches!(err, DomainError::EmptyField { field: "owner" }),
                "owner-side input {input:?} should surface as EmptyField, got: {err:?}"
            );

            // Repo-side: clean owner, input in repo slot.
            let err = SourceRepoSlug::new("o", input)
                .expect_err(&format!("repo-side input {input:?} must reject"));
            assert!(
                matches!(err, DomainError::EmptyField { field: "repo" }),
                "repo-side input {input:?} should surface as EmptyField, got: {err:?}"
            );
        }
    }
}
