// SPDX-License-Identifier: MIT
//! URL normalization for source-repo remotes.
//!
//! Pure string processing — no I/O, no git2, no Tauri. Takes a source
//! repo URL in any common form and returns a canonical comparison form.
//!
//! Still lives under `context_repo/` because [`canonical_remote_key`] is the
//! cross-boundary key context-repo attachments resolve on too — the file name
//! predates the SW-CR-2 retirement of `normalize_context_repo_url` and a
//! rename would be churn. A follow-up story may move it.

use std::fmt;

// ---------- Types ----------

/// Canonical, comparable form of a source-repo remote URL plus a
/// filesystem-safe `org_slug` derived from the owner and repo segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedRepoUrl {
    /// Canonical comparison form: lowercase, no protocol, no userinfo,
    /// no port, no `.git` suffix, no trailing slash, no query/fragment.
    /// Example: `"github.com/alpha-corp/codesense-context"`.
    /// Two URLs that represent the same remote always produce the
    /// same `normalized_url`.
    pub normalized_url: String,

    /// The owner segment (user or org). Example: `"alpha-corp"`.
    /// Preserves original case from the URL path so that display and
    /// round-trip to a clone-able form is lossless. (Only the
    /// comparison form in `normalized_url` is lowercased.)
    pub owner: String,

    /// The repo segment. Example: `"codesense-context"`.
    /// Preserves original case for the same reason.
    pub repo: String,

    /// Filesystem-safe identifier for `{codesense_home}/orgs/{slug}/`,
    /// rendered as two nested path segments. Format:
    /// `{sanitize(owner)}/{sanitize(repo)}` — the `/` keeps the on-disk
    /// layout identical to GitHub's native `owner/repo` form.
    /// Example: `"alpha-corp/codesense-context"`.
    pub org_slug: String,
}

/// Typed rejection reasons from [`normalize_source_repo_url`]. Each
/// variant carries enough context for the caller to build a user-facing
/// toast without reaching into raw strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoUrlError {
    /// Input was empty or whitespace-only.
    Empty,
    /// Invalid host or control characters in a source remote.
    InvalidSourceRemote,
    /// URL parsed but the path didn't contain `{owner}/{repo}`.
    /// Examples: `github.com`, `github.com/alpha-corp`, `github.com/`.
    MissingOwnerRepo { url: String },
    /// URL path had MORE than `{owner}/{repo}` — e.g. extra
    /// segments like `github.com/alpha-corp/context/tree/main`.
    /// Reject rather than silently dropping the extra segments.
    UnexpectedPathSegments { url: String },
    /// The local `sanitize_slug_segment` helper rejected the owner or
    /// repo segment (e.g. all special characters, or empty / `.` / `..`
    /// after filtering). `field` is `"owner"` or `"repo"`.
    SlugRejected { field: &'static str, value: String },
}

impl fmt::Display for RepoUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "source repo url is empty or whitespace-only"),
            Self::InvalidSourceRemote => write!(f, "Expected a Git remote with a valid DNS hostname and owner/repo path; IP addresses and explicit ports are not supported"),
            Self::MissingOwnerRepo { url } => write!(
                f,
                "source repo url is missing owner/repo segments: {}",
                url
            ),
            Self::UnexpectedPathSegments { url } => write!(
                f,
                "source repo url has unexpected path segments beyond owner/repo: {}",
                url
            ),
            Self::SlugRejected { field, value } => write!(
                f,
                "source repo url {} segment rejected by sanitizer: {}",
                field, value
            ),
        }
    }
}

impl std::error::Error for RepoUrlError {}

// ---------- Constants ----------

/// Case-insensitive leading protocol prefixes stripped by
/// `strip_protocol`. Order matters: longer prefixes must appear first
/// so `git+ssh://` wins over `git://` (the loop takes the first match,
/// not the longest). Every entry MUST be non-empty — an empty prefix
/// would match any input and cause `strip_protocol`'s loop to spin
/// forever. The compile-time assertion immediately below enforces this.
const PROTOCOL_PREFIXES: &[&[u8]] = &[b"git+ssh://", b"https://", b"http://", b"ssh://", b"git://"];

const _: () = {
    let mut i = 0;
    while i < PROTOCOL_PREFIXES.len() {
        assert!(
            !PROTOCOL_PREFIXES[i].is_empty(),
            "PROTOCOL_PREFIXES must not contain an empty prefix — would cause infinite loop in strip_protocol"
        );
        i += 1;
    }
};

// ---------- Internal helpers ----------

/// Sanitize a single owner-or-repo segment into a filesystem-safe slug.
///
/// Preserves underscores (GitHub repo names distinguish `my_repo` from
/// `my-repo`). Returns `None` if the segment:
///
/// - is empty, `.`, or `..`
/// - contains any character outside `[A-Za-z0-9._-]`
/// - starts or ends with `.` or `-`
///
/// **Rejects** invalid input rather than silently dropping characters.
/// Leading/trailing `.` and `-` rejection matches GitHub's own
/// server-side rules and guards against hidden directories on macOS
/// (leading `.`) and argv-flag collisions downstream (leading `-`).
///
/// Deliberately NOT reusing `commands::paths::sanitize_name` which
/// replaces `_` with `-` — that transformation collides GitHub repos
/// `my_repo` and `my-repo` into the same slug.
fn sanitize_slug_segment(segment: &str) -> Option<String> {
    if segment.is_empty() || segment == "." || segment == ".." {
        return None;
    }
    if !segment
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return None;
    }
    let bytes = segment.as_bytes();
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if matches!(first, b'.' | b'-') || matches!(last, b'.' | b'-') {
        return None;
    }
    Some(segment.to_lowercase())
}

/// Strip a single leading, case-insensitive `www.` from a host. `www.` is a
/// browser-canonical prefix that 301-redirects to the bare host on every
/// supported forge, so `www.github.com` names the same remote as `github.com`.
/// Only the outermost `www.` is removed (a real host literally named
/// `www.www.example.com` keeps its inner label) and only when a non-empty
/// remainder follows, so a degenerate `www.` alone is left untouched to fail
/// the host lookup honestly.
fn strip_www_prefix(host: &str) -> &str {
    // SW-CR-5: `is_char_boundary(4)` guards the byte slice on a host whose
    // byte 4 lands mid-codepoint (e.g. `飯飯.com` — 3-byte chars). Supported
    // forges all have ASCII hosts, but the hardened guard avoids any UTF-8
    // panic path on an unexpected input. The `host.len() > 4` precondition is
    // kept so the degenerate `"www."` alone stays untouched per the module
    // doc above (would otherwise strip to `""` since
    // `is_char_boundary(len) == true`).
    if host.len() > 4 && host.is_char_boundary(4) && host[..4].eq_ignore_ascii_case("www.") {
        &host[4..]
    } else {
        host
    }
}

// ---------- Public API ----------

/// SW-FG-3/SW-FG-17: the single canonical map key for a remote URL — lowercase
/// `"host/owner/repo"`. The one canonicalization shared by
/// `group_attachments_by_remote` (the per-repo attachment lookup) and the
/// backup-remote binding map, so a URL never keys two ways between them.
/// Returns `None` for a URL that doesn't normalize (mirrors the "silently skip"
/// contract of the attachment grouping). Delegates to
/// [`normalize_source_repo_url`] — no second normalizer.
///
/// The host is part of the cross-boundary identity, matching attachment,
/// lockfile, MCP, and frontend lookup keys so equal owner/repo slugs on distinct
/// forges never collide.
pub fn canonical_remote_key(url: &str) -> Option<String> {
    normalize_source_repo_url(url)
        .ok()
        .map(|normalized| normalized.normalized_url)
}

/// Parse a local source checkout's remote identity. No network access.
/// Unknown DNS hosts retain their exact identity; the existing owner/repo
/// namespace and host validation still apply.
///
/// **Idempotent.** Calling with the already-normalized form yields the
/// same result. Case-preserving: `owner` and `repo` reflect the input's
/// case; only `normalized_url` and `org_slug` are lowercased.
pub fn normalize_source_repo_url(url: &str) -> Result<NormalizedRepoUrl, RepoUrlError> {
    if url.chars().any(char::is_control) {
        return Err(RepoUrlError::InvalidSourceRemote);
    }
    let original = url;

    let s = url.trim();
    if s.is_empty() {
        return Err(RepoUrlError::Empty);
    }

    let s = strip_protocol(s);
    let s = strip_userinfo(s);
    // A colon in a scheme URL's authority is a port, never an SSH path separator.
    if original.contains("://") && s.split('/').next().unwrap_or_default().contains(':') {
        return Err(RepoUrlError::InvalidSourceRemote);
    }

    let s = replace_ssh_colon(s);
    let s = strip_query_and_fragment(&s);
    let s = strip_trailing_slashes(&s);
    let s = strip_git_suffix(&s);

    // Verify the host. If the bare (www-stripped) host matches a supported
    // forge, use the bare host — so `www.github.com` collapses onto `github.com`.
    // Otherwise keep the raw host so self-hosted DNS retains its full identity
    // (`www.corp.example` stays distinct from `corp.example`).
    let host_end = s.find('/').unwrap_or(s.len());
    let raw_host = &s[..host_end];
    let bare_host = strip_www_prefix(raw_host);
    let host = if crate::commands::forge::is_supported_git_host(bare_host) {
        bare_host
    } else {
        raw_host
    };
    if crate::domain::validate_source_repo_host(&host.to_ascii_lowercase()).is_err() {
        return Err(RepoUrlError::InvalidSourceRemote);
    }

    let segments: Vec<&str> = s.split('/').collect();
    match segments.len() {
        len if len < 3 => {
            return Err(RepoUrlError::MissingOwnerRepo {
                url: original.to_string(),
            });
        }
        3 => {}
        _ => {
            return Err(RepoUrlError::UnexpectedPathSegments {
                url: original.to_string(),
            });
        }
    }
    let owner = segments[1];
    let repo = segments[2];
    if owner.is_empty() || repo.is_empty() {
        return Err(RepoUrlError::MissingOwnerRepo {
            url: original.to_string(),
        });
    }

    let sanitized_owner =
        sanitize_slug_segment(owner).ok_or_else(|| RepoUrlError::SlugRejected {
            field: "owner",
            value: owner.to_string(),
        })?;
    let sanitized_repo = sanitize_slug_segment(repo).ok_or_else(|| RepoUrlError::SlugRejected {
        field: "repo",
        value: repo.to_string(),
    })?;

    let normalized_url = format!(
        "{}/{}/{}",
        host.to_lowercase(),
        owner.to_lowercase(),
        repo.to_lowercase()
    );
    let org_slug = format!("{}/{}", sanitized_owner, sanitized_repo);

    Ok(NormalizedRepoUrl {
        normalized_url,
        owner: owner.to_string(),
        repo: repo.to_string(),
        org_slug,
    })
}

/// Strip case-insensitive leading protocol prefixes from `s`, looping
/// until no prefix matches. See `PROTOCOL_PREFIXES` for the list and
/// the non-empty invariant that prevents infinite loops.
///
/// The loop handles pathological stacked forms like
/// `"https://https://github.com/..."` — strip both layers and let the
/// remainder flow through the host check.
///
/// Byte-level comparison via `as_bytes()` avoids a UTF-8 boundary panic
/// on non-ASCII inputs: if the first bytes of `s` sit inside a
/// multi-byte codepoint, string slicing `s[..prefix.len()]` would
/// panic. Comparing byte slices is safe regardless of char boundaries,
/// and the post-match slice `&s[prefix.len()..]` is safe because the
/// prefix (being all ASCII) guarantees the next byte is a char boundary.
fn strip_protocol(s: &str) -> &str {
    let mut s = s;
    loop {
        let mut matched = false;
        for prefix in PROTOCOL_PREFIXES {
            let bytes = s.as_bytes();
            if bytes.len() >= prefix.len() && bytes[..prefix.len()].eq_ignore_ascii_case(prefix) {
                s = &s[prefix.len()..];
                matched = true;
                break;
            }
        }
        if !matched {
            break;
        }
    }
    s
}

/// Strip leading userinfo (`user@`, `user:pass@`, `git@`) if an `@`
/// precedes the first `/`, OR if there's no `/` in the string at all.
fn strip_userinfo(s: &str) -> &str {
    let at_pos = match s.find('@') {
        Some(p) => p,
        None => return s,
    };
    let slash_pos = s.find('/');
    let should_strip = match slash_pos {
        None => true,
        Some(sp) => at_pos < sp,
    };
    if should_strip {
        &s[at_pos + 1..]
    } else {
        s
    }
}

/// Replace the first `:` with `/` when the string is an SSH shorthand
/// form (`:` precedes first `/`, or there's no `/` at all).
fn replace_ssh_colon(s: &str) -> String {
    let colon_pos = match s.find(':') {
        Some(p) => p,
        None => return s.to_string(),
    };
    let slash_pos = s.find('/');
    let should_replace = match slash_pos {
        None => true,
        Some(sp) => colon_pos < sp,
    };
    if should_replace {
        let mut out = String::with_capacity(s.len());
        out.push_str(&s[..colon_pos]);
        out.push('/');
        out.push_str(&s[colon_pos + 1..]);
        out
    } else {
        s.to_string()
    }
}

/// Truncate the string at the first `?` or `#`, whichever comes first.
fn strip_query_and_fragment(s: &str) -> String {
    let cut = s.find(['?', '#']).unwrap_or(s.len());
    s[..cut].to_string()
}

/// Strip a trailing `.git` (case-insensitive) from `s`.
///
/// Must run AFTER trailing-slash stripping so that `"github.com/o/r.git/"`
/// and `"github.com/o/r.git"` reduce to the same `"github.com/o/r"` — the
/// idempotency guarantee tested by `dot_git_with_trailing_slash_is_idempotent`.
/// Reversing the order would leave `"r.git"` in the slash form (because
/// `.git/` doesn't match `.git`) and break the shared contract.
///
/// Guards against UTF-8 boundary panics: `s.len() - 4` is a byte offset,
/// and if the last 4 bytes of `s` cross a codepoint boundary (e.g. the
/// string ends with a 3-byte non-ASCII character like `飯`), the byte
/// slice `s[tail_start..]` would panic. The `is_char_boundary` check
/// short-circuits in that case — non-ASCII tails can't be `.git` anyway,
/// so the guard loses no valid matches.
fn strip_git_suffix(s: &str) -> String {
    if s.len() >= 4 {
        let tail_start = s.len() - 4;
        if s.is_char_boundary(tail_start) && s[tail_start..].eq_ignore_ascii_case(".git") {
            return s[..tail_start].to_string();
        }
    }
    s.to_string()
}

/// Strip all trailing `/` characters from `s`.
fn strip_trailing_slashes(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_form_all_variants_agree() {
        let inputs = &[
            "git@github.com:alpha-corp/codesense-context.git",
            "git@github.com:alpha-corp/codesense-context",
            "https://github.com/alpha-corp/codesense-context.git",
            "https://github.com/alpha-corp/codesense-context",
            "ssh://git@github.com/alpha-corp/codesense-context.git",
            "https://github.com/alpha-corp/codesense-context/",
            "  https://github.com/alpha-corp/codesense-context  ",
        ];
        let expected = NormalizedRepoUrl {
            normalized_url: "github.com/alpha-corp/codesense-context".to_string(),
            owner: "alpha-corp".to_string(),
            repo: "codesense-context".to_string(),
            org_slug: "alpha-corp/codesense-context".to_string(),
        };
        for input in inputs {
            let result = normalize_source_repo_url(input)
                .unwrap_or_else(|e| panic!("{input:?} failed: {e}"));
            assert_eq!(result, expected, "input={input:?}");
        }
    }

    #[test]
    fn case_preservation_in_owner_and_repo() {
        let result =
            normalize_source_repo_url("https://github.com/Alpha-Corp/Codesense-Context").unwrap();
        assert_eq!(result.owner, "Alpha-Corp");
        assert_eq!(result.repo, "Codesense-Context");
        assert_eq!(
            result.normalized_url,
            "github.com/alpha-corp/codesense-context"
        );
        assert_eq!(result.org_slug, "alpha-corp/codesense-context");
    }

    #[test]
    fn mixed_case_hostname_is_insensitive() {
        for url in &[
            "https://GitHub.Com/alpha-corp/codesense-context",
            "https://GITHUB.COM/alpha-corp/codesense-context",
        ] {
            let result =
                normalize_source_repo_url(url).unwrap_or_else(|e| panic!("{url:?} failed: {e}"));
            assert_eq!(
                result.normalized_url, "github.com/alpha-corp/codesense-context",
                "input={url:?}",
            );
        }
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(matches!(
            normalize_source_repo_url(""),
            Err(RepoUrlError::Empty)
        ));
        assert!(matches!(
            normalize_source_repo_url("   "),
            Err(RepoUrlError::Empty)
        ));
    }

    #[test]
    fn admits_supported_forges_host_preserving() {
        // GitLab and Bitbucket normalize alongside GitHub, and the normalized
        // form carries the REAL host so cross-forge owner/repo pairs can't
        // collide.
        let gitlab =
            normalize_source_repo_url("https://gitlab.com/alpha-corp/context.git").unwrap();
        assert_eq!(gitlab.normalized_url, "gitlab.com/alpha-corp/context");
        assert_eq!(gitlab.org_slug, "alpha-corp/context");

        let bitbucket =
            normalize_source_repo_url("https://bitbucket.org/alpha-corp/context").unwrap();
        assert_eq!(bitbucket.normalized_url, "bitbucket.org/alpha-corp/context");
        assert_eq!(bitbucket.org_slug, "alpha-corp/context");

        assert_ne!(gitlab.normalized_url, bitbucket.normalized_url);
    }

    #[test]
    fn canonical_remote_key_is_host_aware() {
        assert_eq!(
            canonical_remote_key("https://github.com/Owner/Repo.git").as_deref(),
            Some("github.com/owner/repo")
        );
        assert_eq!(
            canonical_remote_key("git@gitlab.com:Owner/Repo.git").as_deref(),
            Some("gitlab.com/owner/repo")
        );
        assert_eq!(canonical_remote_key("not a url"), None);
    }

    #[test]
    fn source_remotes_admit_self_hosted_dns_without_aliasing_hosts() {
        for remote in [
            "https://git.corp.example/Acme/App.git",
            "git@git.corp.example:Acme/App.git",
            "ssh://git@git.corp.example/Acme/App.git",
            "git.corp.example/Acme/App",
        ] {
            let parsed = normalize_source_repo_url(remote).unwrap();
            assert_eq!(parsed.normalized_url, "git.corp.example/acme/app");
            assert_eq!(
                canonical_remote_key(remote).as_deref(),
                Some("git.corp.example/acme/app")
            );
        }
        assert_eq!(
            canonical_remote_key("https://www.git.corp.example/acme/app").unwrap(),
            "www.git.corp.example/acme/app"
        );
        assert_ne!(
            canonical_remote_key("https://git.corp.example/acme/app"),
            canonical_remote_key("https://other.corp.example/acme/app")
        );
        for remote in [
            "https://bad_host/a/b",
            "https://../a/b",
            "https://git.corp.example:2222/a/b",
            "ssh://git@git.corp.example:2222/repo.git",
            "git@git.corp.example:a/../b",
            "git@git.corp.example:a/b\n",
        ] {
            assert!(normalize_source_repo_url(remote).is_err(), "{remote}");
        }
    }

    #[test]
    fn rejects_missing_owner_repo() {
        for url in &[
            "github.com",
            "github.com/alpha-corp",
            "https://github.com/",
            "git@github.com:",
        ] {
            assert!(
                matches!(
                    normalize_source_repo_url(url),
                    Err(RepoUrlError::MissingOwnerRepo { .. })
                ),
                "input={url:?}",
            );
        }
    }

    #[test]
    fn rejects_unexpected_path_segments() {
        for url in &[
            "https://github.com/alpha-corp/context/tree/main",
            "https://github.com/alpha-corp/context/issues/1",
            "https://github.com/alpha-corp/context/pull/42/files",
        ] {
            assert!(
                matches!(
                    normalize_source_repo_url(url),
                    Err(RepoUrlError::UnexpectedPathSegments { .. })
                ),
                "input={url:?}",
            );
        }
    }

    #[test]
    fn strips_git_suffix_case_insensitively() {
        for url in &[
            "https://github.com/alpha-corp/codesense-context.git",
            "https://github.com/alpha-corp/codesense-context.GIT",
            "https://github.com/alpha-corp/codesense-context.Git",
        ] {
            let result =
                normalize_source_repo_url(url).unwrap_or_else(|e| panic!("{url:?} failed: {e}"));
            assert_eq!(result.repo, "codesense-context", "input={url:?}");
            assert_eq!(
                result.normalized_url, "github.com/alpha-corp/codesense-context",
                "input={url:?}",
            );
        }
    }

    #[test]
    fn strips_git_plus_ssh_protocol() {
        let result =
            normalize_source_repo_url("git+ssh://git@github.com/alpha-corp/codesense-context.git")
                .unwrap();
        assert_eq!(
            result.normalized_url,
            "github.com/alpha-corp/codesense-context"
        );
        assert_eq!(result.org_slug, "alpha-corp/codesense-context");
    }

    #[test]
    fn slash_delimiter_prevents_owner_repo_collision() {
        let a = normalize_source_repo_url("git@github.com:alpha-corp/context.git").unwrap();
        let b = normalize_source_repo_url("git@github.com:alpha/corp-context.git").unwrap();
        assert_eq!(a.org_slug, "alpha-corp/context");
        assert_eq!(b.org_slug, "alpha/corp-context");
        assert_ne!(a.org_slug, b.org_slug);
    }

    #[test]
    fn no_panic_on_non_ascii_input() {
        for url in &[
            "αααααα/owner/repo",
            "github.com/o/r飯",
            "αααααααααα",
            "https://αβγ/owner/repo",
            "\u{1F600}github.com/o/r",
            "https://github.com/o/r.飯",
        ] {
            let result = normalize_source_repo_url(url);
            assert!(
                result.is_err(),
                "{url:?} should return Err, not panic — got {result:?}",
            );
        }
    }

    #[test]
    fn dot_git_with_trailing_slash_is_idempotent() {
        let with_slash =
            normalize_source_repo_url("https://github.com/alpha-corp/codesense-context.git/")
                .unwrap();
        let without_slash =
            normalize_source_repo_url("https://github.com/alpha-corp/codesense-context.git")
                .unwrap();
        let bare =
            normalize_source_repo_url("https://github.com/alpha-corp/codesense-context").unwrap();

        assert_eq!(with_slash, without_slash);
        assert_eq!(with_slash, bare);
        assert_eq!(with_slash.repo, "codesense-context");
        assert_eq!(
            with_slash.normalized_url,
            "github.com/alpha-corp/codesense-context"
        );
        assert_eq!(with_slash.org_slug, "alpha-corp/codesense-context");
    }

    #[test]
    fn normalizes_its_own_output() {
        let first =
            normalize_source_repo_url("git@github.com:Alpha-Corp/Codesense-Context.git").unwrap();
        let second = normalize_source_repo_url(&first.normalized_url).unwrap();

        assert_eq!(
            second,
            NormalizedRepoUrl {
                normalized_url: "github.com/alpha-corp/codesense-context".to_string(),
                owner: "alpha-corp".to_string(),
                repo: "codesense-context".to_string(),
                org_slug: "alpha-corp/codesense-context".to_string(),
            },
        );
        assert_eq!(first.normalized_url, second.normalized_url);
        assert_eq!(first.org_slug, second.org_slug);
    }

    #[test]
    fn rejects_leading_or_trailing_dot_or_hyphen_in_owner_or_repo() {
        let rejected_inputs = &[
            ("https://github.com/.hidden/repo", "owner"),
            ("https://github.com/owner/.hidden", "repo"),
            ("https://github.com/owner./repo", "owner"),
            ("https://github.com/owner/repo.", "repo"),
            ("https://github.com/-rf/repo", "owner"),
            ("https://github.com/owner/-rf", "repo"),
            ("https://github.com/owner-/repo", "owner"),
            ("https://github.com/owner/repo-", "repo"),
            ("https://github.com/.../repo", "owner"),
            ("https://github.com/owner/...", "repo"),
            ("https://github.com/..foo/repo", "owner"),
            ("https://github.com/-/repo", "owner"),
            ("https://github.com/owner/-", "repo"),
        ];
        for (url, expected_field) in rejected_inputs {
            let result = normalize_source_repo_url(url);
            match result {
                Err(RepoUrlError::SlugRejected { field, .. }) => {
                    assert_eq!(
                        field, *expected_field,
                        "input={url:?} expected field={expected_field}",
                    );
                }
                other => panic!("input={url:?} expected SlugRejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_stacked_protocol_gracefully() {
        let result =
            normalize_source_repo_url("https://https://github.com/alpha-corp/codesense-context")
                .unwrap();
        assert_eq!(
            result.normalized_url,
            "github.com/alpha-corp/codesense-context"
        );
        assert_eq!(result.org_slug, "alpha-corp/codesense-context");
    }

    #[test]
    fn rejects_invalid_owner_characters_from_two_colon_ssh() {
        let result = normalize_source_repo_url("git@github.com::owner/repo");
        assert!(
            matches!(
                result,
                Err(RepoUrlError::SlugRejected { field: "owner", .. })
            ),
            "got {result:?}",
        );
    }

    #[test]
    fn rejects_dot_and_dotdot_as_owner_or_repo() {
        for (url, expected_field) in &[
            ("https://github.com/./repo", "owner"),
            ("https://github.com/owner/.", "repo"),
            ("https://github.com/../repo", "owner"),
            ("https://github.com/owner/..", "repo"),
        ] {
            let result = normalize_source_repo_url(url);
            match result {
                Err(RepoUrlError::SlugRejected { field, .. }) => {
                    assert_eq!(
                        field, *expected_field,
                        "input={url:?} expected field={expected_field}",
                    );
                }
                other => panic!("input={url:?} expected SlugRejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn underscore_preserved_and_distinct_from_hyphen() {
        let with_underscore =
            normalize_source_repo_url("git@github.com:alderco/my_repo.git").unwrap();
        let with_hyphen = normalize_source_repo_url("git@github.com:alderco/my-repo.git").unwrap();

        assert_eq!(with_underscore.org_slug, "alderco/my_repo");
        assert_eq!(with_hyphen.org_slug, "alderco/my-repo");
        assert_ne!(with_underscore.org_slug, with_hyphen.org_slug);

        assert_eq!(with_underscore.owner, "alderco");
        assert_eq!(with_underscore.repo, "my_repo");
        assert_eq!(with_underscore.normalized_url, "github.com/alderco/my_repo");
    }
}
