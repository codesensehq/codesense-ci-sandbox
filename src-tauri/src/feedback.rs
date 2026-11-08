// SPDX-License-Identifier: MIT
//! Redaction for text that leaves the app (dashboard publish errors), and where the app
//! log lives. The beta feedback senders that once posted from here are gone (SV-PBF-328).

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

/// CodeSense's own tokens: install (`csb_`), publisher (`csp_`) and viewer session (`cvs_`).
/// No word boundary before the token patterns: a URL-encoded `%20csb_…` or `%3Dghp_…`
/// has none, and the regex crate has no lookbehind. Over-redacting is fine.
static CODESENSE_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(csb|csp|cvs)_[A-Za-z0-9_-]+").expect("token pattern"));
// A compact JWS: three base64url segments, the first a JSON header (`eyJ` is `{"`).
static JWS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+").expect("jws pattern")
});
static ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("ansi pattern"));
/// GitHub tokens (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`).
static GITHUB_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(gh[pousr]_|github_pat_)[A-Za-z0-9_]+").expect("github token pattern")
});
/// API keys in the `sk-…` form (OpenAI, Anthropic and others).
static SK_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"sk-[A-Za-z0-9_-]{16,}").expect("sk key pattern"));
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{6,}").expect("bearer pattern")
});
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
        .expect("email pattern")
});
/// A home folder: `/Users/<name>` on macOS, `/home/<name>` elsewhere.
static HOME_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(/Users|/home)/[^/\s"'`:;,)\]]+"#).expect("home path pattern"));
/// A long secret-looking value after a `key=`, `token=`, `secret=` or `password=` style name.
static SECRET_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([A-Za-z0-9_-]*(?:key|token|secret|password|passwd)[A-Za-z0-9_-]*["']?\s*[=:]\s*["']?)[A-Za-z0-9+/_.=-]{16,}"#,
    )
    .expect("secret pair pattern")
});

/// Removes from `text` every CodeSense token (`csb_`, `csp_`, `cvs_`), signed token
/// (JWS), GitHub token, `sk-` key, `Bearer` credential, long value of a `key=` /
/// `token=` / `secret=` pair, email address and home folder name, and the terminal
/// colour codes the log file carries. Running it again changes nothing.
pub fn redact(text: &str) -> String {
    let out = ANSI.replace_all(text, "");
    let out = JWS.replace_all(&out, "[signed token removed]");
    let out = CODESENSE_TOKEN.replace_all(&out, "${1}_[removed]");
    let out = GITHUB_TOKEN.replace_all(&out, "${1}[removed]");
    let out = SK_KEY.replace_all(&out, "sk-[removed]");
    let out = BEARER.replace_all(&out, "${1} [removed]");
    let out = SECRET_PAIR.replace_all(&out, "${1}[removed]");
    let out = EMAIL.replace_all(&out, "[email removed]");
    HOME_PATH.replace_all(&out, "${1}/<redacted>").into_owned()
}

/// The app's log directory.
pub fn log_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    app.path()
        .app_log_dir()
        .map_err(|e| format!("Couldn't find the app log: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "csb_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_abcde";
    const JWS_SAMPLE: &str = "eyJhbGciOiJFZERTQSJ9.eyJzdGF0dXMiOiJhY3RpdmUifQ.c2lnbmF0dXJlLWJ5dGVz"; // trufflehog:ignore

    #[test]
    fn redaction_removes_install_tokens_and_signed_tokens() {
        let log = format!(
            "\x1b[32m12:00:00 [INFO ]\x1b[0m [license] token {TOKEN} reply {JWS_SAMPLE} done\n"
        );
        let out = redact(&log);
        assert!(!out.contains(TOKEN));
        assert!(!out.contains(JWS_SAMPLE));
        assert!(!out.contains("csb_AbCd"));
        assert!(!out.contains('\x1b'));
        assert!(out.contains("csb_[removed]"));
        assert!(out.contains("[signed token removed]"));
        assert!(out.contains("[license] token"));
    }

    #[test]
    fn redaction_removes_other_secrets_emails_and_home_folders() {
        let gh = format!("ghp_{}", "a1B2".repeat(9));
        let pat = format!("github_pat_{}", "Z9y8".repeat(10));
        let sk = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv";
        let log = format!(
            "publisher csp_{a} viewer cvs_{a}\n\
             gh {gh} pat {pat} key {sk}\n\
             Authorization: Bearer abc.DEF-123_xyz\n\
             api_key=0123456789abcdef0123456789abcdef token: \"QUJDREVGR0hJSktMTU5PUA==\" SECRET=s3cr3tValueThatIsLong\n\
             mail ada.lovelace+beta@example.co.uk\n\
             open /Users/grace/codesense/src and /home/ada/.config\n\
             short key=abc and the word token stays\n",
            a = "Q".repeat(43)
        );
        let out = redact(&log);
        for secret in [
            "QQQQ",
            gh.as_str(),
            pat.as_str(),
            sk,
            "abc.DEF-123_xyz",
            "0123456789abcdef",
            "QUJDREVGR0hJSktMTU5PUA",
            "s3cr3tValueThatIsLong",
            "ada.lovelace",
            "example.co.uk",
            "grace",
            "/home/ada",
        ] {
            assert!(!out.contains(secret), "{secret} left in:\n{out}");
        }
        for kept in [
            "csp_[removed]",
            "cvs_[removed]",
            "ghp_[removed]",
            "github_pat_[removed]",
            "sk-[removed]",
            "Bearer [removed]",
            "api_key=[removed]",
            "SECRET=[removed]",
            "[email removed]",
            "/Users/<redacted>/codesense/src",
            "/home/<redacted>/.config",
            "short key=abc and the word token stays",
        ] {
            assert!(out.contains(kept), "{kept} missing from:\n{out}");
        }
    }

    #[test]
    fn url_encoded_tokens_are_removed_too() {
        let csb = format!("csb_{}", "Q".repeat(43));
        let ghp = format!("ghp_{}", "a1B2".repeat(9));
        let sk = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv";
        let log = format!("GET /x?auth=Bearer%20{csb}&next=token%3D{ghp}&k=%22{sk}%22\n");
        let out = redact(&log);
        for secret in [&csb[4..], &ghp[4..], &sk[3..]] {
            assert!(!out.contains(secret), "{secret} left in: {out}");
        }
        assert!(out.contains("Bearer%20csb_[removed]"), "{out}");
        assert!(out.contains("token%3Dghp_[removed]"), "{out}");
        assert_eq!(redact(&out), out);
    }

    #[test]
    fn redacting_twice_changes_nothing() {
        let log = format!(
            "{TOKEN} {JWS_SAMPLE} Bearer xyz123 a@b.io /Users/me/x token={}\n",
            "f".repeat(32)
        );
        let once = redact(&log);
        assert_eq!(redact(&once), once);
    }
}
