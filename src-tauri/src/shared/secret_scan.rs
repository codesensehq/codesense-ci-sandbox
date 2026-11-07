// SPDX-License-Identifier: MIT
//! Shared secret detection + redaction — the single home for the credential-shaped-pattern list.
//!
//! SW-OR-16 *locates* secret spans in a captured agent transcript and **masks** them before
//! persist. Every consumer shares the patterns here:
//!
//! - [`secret_spans`] — OR-16's span locator (byte ranges, merged + non-overlapping).
//! - [`redact_secrets`] — OR-16's net-new mask pass (replace each located span with a placeholder).
//!
//! Patterns split by how they identify a secret: [`VALUE_SHAPE_PATTERNS`] (the value *is* the token)
//! and [`CONTEXTUAL_PATTERNS`] (a key/header names it). The span locator scans their union
//! ([`SECRET_PATTERNS`]).
//!
//! Deliberately biased **fail-loud / over-redact**: a false positive is a fixable annoyance; a false
//! negative leaks a credential. Not an exhaustive scanner (a `gitleaks`-scale dependency is a
//! non-goal) — the keychain-only credential model is the real guarantee; this is defense in depth.

use std::sync::LazyLock;

use regex::Regex;

/// The placeholder a masked secret span is replaced with. Plain ASCII so it stays valid inside a
/// JSON string value (a redacted `stream-json` line remains parseable).
pub const REDACTION_PLACEHOLDER: &str = "[redacted]";

/// A PEM private key's header, END line and body character (base64, key headers, whitespace and
/// escaped newlines at any depth of escaping).
const PEM_BEGIN: &str = r"-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----";
const PEM_END: &str = r"-----END (?:[A-Z0-9 ]+ )?PRIVATE KEY-----";
/// The punctuation a PEM body holds besides ASCII letters, digits and whitespace: one list for
/// the regex class ([`pem_char_class`]) and the streaming predicate ([`pem_body`]).
const PEM_PUNCT: &str = "+/=:,-";
/// The key context of the contextual patterns, up to where the value starts (an `Authorization:`
/// scheme, a credential keyword and its separator, a bare `bearer`), and a value's first and later
/// characters.
const AUTH_SCHEME: &str = r"(?i)authorization:\s*(?:bearer|basic)";
const CREDENTIAL_KEY: &str = r#"(?i)\b(?:access[_-]?token|api[_-]?key|client[_-]?secret|token|secret|password|credential)"?\s*[:=]"#;
const BEARER: &str = r"(?i)\bbearer\s+";
const VALUE_HEAD: &str = "[A-Za-z0-9]";
/// The punctuation a value goes on through besides Unicode word characters: one list for the
/// regex class ([`value_tail_class`]) and the streaming predicate ([`value_tail`]).
const VALUE_PUNCT: &str = ".-~+/=";

/// `[\w` and [`VALUE_PUNCT`]`]`: a value's later characters.
pub(crate) fn value_tail_class() -> String {
    format!(r"[\w{}]", regex_syntax::escape(VALUE_PUNCT))
}

/// One character of a PEM body: an ASCII letter or digit, whitespace or [`PEM_PUNCT`].
pub(crate) fn pem_char_class() -> String {
    format!(r"[A-Za-z0-9\s{}]", regex_syntax::escape(PEM_PUNCT))
}

/// A PEM body's unit: a [`pem_char_class`] character or an escaped newline at any depth.
fn pem_body_class() -> String {
    format!(r"(?:{}|\\+[nrt])", pem_char_class())
}

/// The known-provider token shapes, as an unambiguous prefix and the body after it: ~zero
/// false-positive risk. A streaming scrub recognises a token at its prefix alone.
const TOKEN_SHAPES: &[(&str, &str)] = &[
    (r"gh[oprsu]_", r"[A-Za-z0-9]{20,}"), // GitHub PAT/OAuth/refresh/server/user tokens
    (r"github_pat_", r"[A-Za-z0-9_]{20,}"), // GitHub fine-grained PAT
    (r"glpat-", r"[A-Za-z0-9_\-]{20,}"),  // GitLab personal access token
    (r"xox[abceoprs]-", r"[A-Za-z0-9-]{10,}"), // Slack token (bot/user/legacy/config/refresh)
    (r"sk-", r"[A-Za-z0-9]{20,}"),        // OpenAI-style secret key
    (r"(?:sk|rk)_(?:live|test)_", r"[A-Za-z0-9]{10,}"), // Stripe secret / restricted key
    (r"A(?:KIA|SIA)", r"[0-9A-Z]{16}"),   // AWS access key id (long-term / temporary)
    (r"AIza", r"[0-9A-Za-z_\-]{20,}"),    // Google API key
];
/// A JWT segment's start (`eyJ`) and its base64url characters.
const JWT_SEGMENT: &str = r"eyJ[A-Za-z0-9_\-]";

/// The known-provider tokens ([`TOKEN_SHAPES`]) and JWTs (header.payload.signature).
static TOKEN_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let shapes = Iterator::map(<[(&str, &str)]>::iter(TOKEN_SHAPES), |(prefix, body)| {
        format!("{prefix}{body}")
    });
    let jwt = format!(r"{JWT_SEGMENT}{{10,}}\.{JWT_SEGMENT}{{10,}}\.");
    Iterator::collect(Iterator::map(Iterator::chain(shapes, [jwt]), |p| {
        compile_one(&p)
    }))
});

/// Value-shape patterns: the credential is the matched substring itself — a known-provider token
/// prefix ([`TOKEN_PATTERNS`]), a JWT, a PEM private-key block, or a bare `bearer <token>` value.
/// Each needs literal token characters where a real secret would sit, so a documentation
/// placeholder like `<token>` is NOT matched. Because the match *is* the secret value, masking one
/// inside a JSON string keeps the surrounding JSON valid.
static VALUE_SHAPE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let (body, value_tail) = (pem_body_class(), value_tail_class());
    let pem = format!("{PEM_BEGIN}(?:{body}*?{PEM_END}|{body}*)");
    let bearer = format!("{BEARER}{VALUE_HEAD}{value_tail}{{19,}}");
    let mut patterns = Vec::clone(&TOKEN_PATTERNS);
    Extend::extend(
        &mut patterns,
        compile([
            // PEM private-key block: header, key body, END line, in raw or JSON-escaped form at any depth
            // of escaping (a tool result that is itself JSON doubles every backslash again). The body
            // is only what a key body can hold (base64, key headers, whitespace and escaped newlines),
            // so a match never crosses a quote, a brace or a record: a PEM header named inside a quoted
            // command masks the header alone, and a later record is not touched. A body whose END line
            // was cut off still loses its run of base64, so the header never survives alone.
            String::as_str(&pem),
            // A bare `bearer <token>` value with no `authorization:` key context. The 20-char floor keeps
            // it off ordinary prose ("bearer of good news") while catching every real opaque bearer token,
            // which is always long.
            String::as_str(&bearer),
        ]),
    );
    patterns
});

/// Contextual patterns: the credential is identified by the *surrounding* key/header, not by the
/// value's own shape. They need the key context, so they only fire where a key/header is genuinely
/// present.
static CONTEXTUAL_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let value_tail = value_tail_class();
    let header = format!(r"{AUTH_SCHEME}\s+{VALUE_HEAD}{value_tail}{{15,}}");
    let keyword = format!(r#"{CREDENTIAL_KEY}\s*"?(?P<val>{VALUE_HEAD}{value_tail}{{3,}})"#);
    compile([
        // An inline `Authorization: Bearer <literal>` / `Basic <literal>` header value (the smuggle),
        // NOT the structured `name: Authorization` + `scheme: Bearer` AuthSpec keys (separate lines).
        String::as_str(&header),
        // A credential-KEYWORD assignment carrying a literal value. None of these words is a valid
        // connector-def key, so any `<keyword>: <value>` / `<keyword>=<value>` is suspicious — keyed on
        // `:` OR `=` so a smuggled YAML key (`token: …`), a query (`?access_token=…`), AND a JSON key
        // (`"password":"…"`, the native shape of a captured transcript) all trip (optional quotes around
        // key and value). The `\b` boundary keeps `manualToken` / `dedupKey` from matching, and a
        // `<value>` needs 4+ chars. Only the VALUE is captured (`val`) so redaction masks the secret
        // without eating the key/colon — a JSON object stays structurally valid after the scrub.
        String::as_str(&keyword),
    ])
});

/// The full detector set — value-shape ∪ contextual — driving the span-locate/scrub pass ([`secret_spans`] / [`redact_secrets`]).
static SECRET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    VALUE_SHAPE_PATTERNS
        .iter()
        .chain(CONTEXTUAL_PATTERNS.iter())
        .cloned()
        .collect()
});

/// Where a streaming scrub (the live terminal) recognises that a secret starts, from the same
/// pieces as the detectors above. It masks the value itself character by character, so a value
/// of any length, split across any number of chunks, stays masked.
pub(crate) struct StreamPatterns {
    /// A value-shape token, whole or by its start alone (a provider prefix after a word boundary,
    /// a JWT's `eyJ` and eight more characters): the match is the start of the secret.
    pub tokens: Vec<Regex>,
    /// A bare `bearer <token>`: the secret starts at `val`.
    pub bearer: Regex,
    /// A credential keyword and its separator, or an `Authorization:` scheme: the value follows.
    pub keys: Vec<Regex>,
    /// A PEM private key's header: its body follows, through its END line.
    pub pem_begin: Regex,
    /// A PEM END line at the very end of a text.
    pub pem_end: Regex,
}

pub(crate) static STREAM_PATTERNS: LazyLock<StreamPatterns> = LazyLock::new(|| {
    let value_tail = value_tail_class();
    let bearer = format!("{BEARER}(?P<val>{VALUE_HEAD}{value_tail}{{19,}})");
    let pem_end = format!(r"{PEM_END}\z");
    let [bearer, pem_begin, pem_end]: [Regex; 3] = Result::expect(
        TryInto::try_into(compile([
            String::as_str(&bearer),
            PEM_BEGIN,
            String::as_str(&pem_end),
        ])),
        "three patterns",
    );
    let prefixes: Vec<&str> = Iterator::collect(Iterator::map(
        <[(&str, &str)]>::iter(TOKEN_SHAPES),
        |(prefix, _)| *prefix,
    ));
    let prefix = format!(r"\b(?:{})", <[&str]>::join(&prefixes, "|"));
    let jwt = format!(r"\b{JWT_SEGMENT}{{8,}}");
    let mut tokens = Vec::clone(&TOKEN_PATTERNS);
    Extend::extend(
        &mut tokens,
        compile([String::as_str(&prefix), String::as_str(&jwt)]),
    );
    StreamPatterns {
        tokens,
        bearer,
        keys: compile([CREDENTIAL_KEY, AUTH_SCHEME]),
        pem_begin,
        pem_end,
    }
});

/// A character [`VALUE_HEAD`] matches: a value starts with it.
pub(crate) fn value_head(c: char) -> bool {
    char::is_ascii_alphanumeric(&c)
}

/// A character [`value_tail_class`] matches: a value or token goes on through it. `\w` is the
/// regex crate's own Unicode word class.
pub(crate) fn value_tail(c: char) -> bool {
    regex_syntax::is_word_character(c) || str::contains(VALUE_PUNCT, c)
}

/// A character of a PEM key's body: one [`pem_char_class`] matches, or the backslash of an
/// escaped newline.
pub(crate) fn pem_body(c: char) -> bool {
    char::is_ascii_alphanumeric(&c)
        || char::is_whitespace(c)
        || str::contains(PEM_PUNCT, c)
        || c == '\\'
}

fn compile<const N: usize>(patterns: [&str; N]) -> Vec<Regex> {
    Iterator::collect(Iterator::map(
        IntoIterator::into_iter(patterns),
        compile_one,
    ))
}

fn compile_one(p: &str) -> Regex {
    Result::unwrap_or_else(Regex::new(p), |e| {
        panic!("invalid secret pattern `{p}`: {e}")
    })
}

/// SW-OR-16 (AC-4): locate every secret span across all patterns, returned as **sorted, merged,
/// non-overlapping** `(start, end)` byte ranges (regex matches land on char boundaries). Empty when
/// clean.
pub(crate) fn secret_spans(raw: &str) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = SECRET_PATTERNS
        .iter()
        .flat_map(|re| {
            re.captures_iter(raw).map(|caps| {
                // Prefer a `val` capture (the credential VALUE only) so a keyword pattern masks the
                // secret while leaving the surrounding `key:`/`"key":` structure intact — a JSON line
                // stays valid after the scrub. Patterns with no `val` group (provider prefixes, JWT,
                // PEM) match the secret itself, so the whole match is the span.
                let m = caps.name("val").unwrap_or_else(|| caps.get(0).unwrap());
                (m.start(), m.end())
            })
        })
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            // Overlapping or touching a prior span → extend it (keeps replacement unambiguous).
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// SW-OR-16 (AC-4): the net-new **scrub** pass — replace every located secret span with
/// [`REDACTION_PLACEHOLDER`]. Returns the input unchanged (no allocation churn beyond the copy) when
/// clean. Runs BEFORE persist/display so an unredacted secret never reaches disk.
pub(crate) fn redact_secrets(raw: &str) -> String {
    let spans = secret_spans(raw);
    if spans.is_empty() {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0;
    for (start, end) in spans {
        // `secret_spans` is sorted + merged, so `start >= cursor` always holds here.
        out.push_str(&raw[cursor..start]);
        out.push_str(REDACTION_PLACEHOLDER);
        cursor = end;
    }
    out.push_str(&raw[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_detects_a_provider_token_and_passes_clean_text() {
        assert!(!secret_spans("ghp_abcdefghijklmnopqrstuvwxyz0123").is_empty());
        assert!(secret_spans("just a normal transcript line").is_empty());
    }

    #[test]
    fn redact_masks_a_seeded_token_and_leaves_prose() {
        let raw = "running: export GH=ghp_abcdefghijklmnopqrstuvwxyz0123 then done";
        let out = redact_secrets(raw);
        assert!(
            !out.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"),
            "secret must be gone: {out}"
        );
        assert!(out.contains(REDACTION_PLACEHOLDER));
        assert!(out.starts_with("running: export GH="));
        assert!(out.ends_with("then done"));
    }

    #[test]
    fn redact_is_identity_on_clean_text() {
        let raw = "no secrets here, just tool output\nline two";
        assert_eq!(redact_secrets(raw), raw);
    }

    #[test]
    fn redact_masks_a_secret_inside_a_json_string_and_keeps_it_parseable() {
        // The raw stdout of a stream-json run is NDJSON; a secret in a tool_result value must be
        // masked without corrupting the surrounding JSON (AC-4 text-floor-of-a-structured-run).
        let line = r#"{"type":"user","content":[{"tool_result":"AKIA0123456789ABCDEF leaked"}]}"#;
        let out = redact_secrets(line);
        assert!(!out.contains("AKIA0123456789ABCDEF"));
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("redacted line stays valid JSON");
        assert_eq!(parsed["type"], "user");
    }

    #[test]
    fn redact_handles_multiple_and_adjacent_secrets() {
        let raw = "a=sk-abcdefghijklmnopqrstuvwxyz b=AKIA0123456789ABCDEF end";
        let out = redact_secrets(raw);
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        assert!(!out.contains("AKIA0123456789ABCDEF"));
        assert_eq!(out.matches(REDACTION_PLACEHOLDER).count(), 2);
    }

    #[test]
    fn redact_masks_an_opaque_value_under_a_json_credential_key_keeping_json_valid() {
        // The dominant transcript shape: an OPAQUE secret (no provider prefix) under a credential key
        // in JSON. Only the value is masked, so the object stays structurally valid.
        let line = r#"{"type":"tool_result","content":{"password":"hunter2-opaque-xyz"}}"#;
        let out = redact_secrets(line);
        assert!(
            !out.contains("hunter2-opaque-xyz"),
            "opaque secret masked: {out}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("stays valid JSON after masking the value only");
        assert_eq!(parsed["content"]["password"], "[redacted]");
    }

    #[test]
    fn redact_masks_a_yaml_and_query_credential_value_only() {
        // YAML: the key/colon survive, the value is masked.
        assert_eq!(redact_secrets("token: hunter2secret"), "token: [redacted]");
        // Query string: same.
        assert_eq!(
            redact_secrets("https://x/cb?access_token=abcd1234efgh"),
            "https://x/cb?access_token=[redacted]"
        );
    }

    #[test]
    fn spans_are_sorted_and_non_overlapping() {
        let raw = "x sk-abcdefghijklmnopqrstuvwxyz y ghp_abcdefghijklmnopqrstuvwxyz0123 z";
        let spans = secret_spans(raw);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].1 <= spans[1].0, "non-overlapping + ordered");
    }

    #[test]
    fn detects_the_full_union_of_provider_shapes() {
        for token in [
            "ghu_abcdefghijklmnopqrst0123", // GitHub user-to-server
            "ghs_abcdefghijklmnopqrst0123", // GitHub server-to-server
            "ghr_abcdefghijklmnopqrst0123", // GitHub refresh
            "glpat-abcdefghijklmnopqrst",   // GitLab PAT
            "ASIA0123456789ABCDEF",         // AWS temporary access key id
        ] {
            assert!(!secret_spans(token).is_empty(), "should detect {token}");
            assert!(
                !redact_secrets(&format!("val={token} end")).contains(token),
                "redact should mask {token}"
            );
        }
    }

    #[test]
    fn detects_a_bare_bearer_token_value() {
        // A `bearer <opaque>` value with no `authorization:` key context.
        let bearer = "Bearer abcdefghijklmnopqrstuvwx";
        assert!(!secret_spans(bearer).is_empty());
        // Prose that merely starts with the word "bearer" is not a token — no false positive.
        assert!(secret_spans("bearer of good news today").is_empty());
    }

    // An obviously fake key body: base64-shaped, repeated marker, not a real key.
    const FAKE_BODY: &str = "RkFLRUtFWV9OT1RfQV9SRUFMX0tFWV9SRUFEX01FX0hFUkU9PT0K";

    #[test]
    fn redact_masks_the_whole_pem_private_key_block_raw() {
        let raw = format!(
            "before\n-----BEGIN RSA PRIVATE KEY-----\n{FAKE_BODY}\n{FAKE_BODY}\n-----END RSA PRIVATE KEY-----\nafter"
        );
        let out = redact_secrets(&raw);
        assert!(!out.contains(FAKE_BODY), "key body must be gone: {out}");
        assert!(!out.contains("END RSA PRIVATE KEY"));
        assert!(out.starts_with("before\n"));
        assert!(out.ends_with("\nafter"));
    }

    #[test]
    fn redact_masks_a_json_escaped_pem_block_and_keeps_the_line_parseable() {
        let line = serde_json::json!({
            "type": "user",
            "content": format!("cat key:\n-----BEGIN PRIVATE KEY-----\n{FAKE_BODY}\n-----END PRIVATE KEY-----\nok"),
        })
        .to_string();
        assert!(line.contains("\\n"), "the fixture is the escaped form");
        let out = redact_secrets(&line);
        assert!(!out.contains(FAKE_BODY), "escaped body must be gone: {out}");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("still valid JSON");
        assert!(parsed["content"].as_str().unwrap().starts_with("cat key:"));
        assert!(parsed["content"].as_str().unwrap().ends_with("ok"));
    }

    #[test]
    fn redact_masks_a_pem_body_whose_end_line_was_cut_off() {
        let line = format!(
            r#"{{"content":"-----BEGIN OPENSSH PRIVATE KEY-----\n{FAKE_BODY}\n{FAKE_BODY}"}}"#
        );
        let out = redact_secrets(&line);
        assert!(!out.contains(FAKE_BODY), "cut-off body must be gone: {out}");
        serde_json::from_str::<serde_json::Value>(&out).expect("still valid JSON");
    }

    /// `text` as the value of a JSON string field, `depth` times over (a JSON document inside a
    /// string inside a document...), the way a nested tool result reaches a native log.
    fn nested_in_json(text: &str, depth: usize) -> String {
        let mut value = text.to_owned();
        for _ in 0..depth {
            value = serde_json::to_string(&value).unwrap();
            // Strip the quotes the encoder added: the next level wraps it again as a string value.
            value = value[1..value.len() - 1].to_owned();
        }
        value
    }

    #[test]
    fn a_private_key_is_masked_at_every_depth_of_json_escaping_whole_or_cut_off() {
        for depth in 0..=4 {
            let whole = format!(
                "-----BEGIN PRIVATE KEY-----\n{FAKE_BODY}\n{FAKE_BODY}\n-----END PRIVATE KEY-----\nafter"
            );
            let cut = format!("-----BEGIN PRIVATE KEY-----\n{FAKE_BODY}\n{FAKE_BODY}\n");
            for (name, pem) in [("whole", whole), ("cut off", cut)] {
                let line = format!(
                    r#"{{"out":"{}","next":"keep me"}}"#,
                    nested_in_json(&pem, depth)
                );
                let redacted = redact_secrets(&line);
                assert!(
                    !redacted.contains(FAKE_BODY),
                    "depth {depth}, {name}: the key body survived: {redacted}"
                );
                assert!(
                    redacted.contains(r#""next":"keep me""#),
                    "depth {depth}, {name}: a neighbouring field was masked: {redacted}"
                );
                if depth > 0 {
                    // (At depth 0 the fixture's newlines are raw, which is no JSON to begin with.)
                    serde_json::from_str::<serde_json::Value>(&redacted).unwrap_or_else(|e| {
                        panic!("depth {depth}, {name}: not JSON any more: {e}")
                    });
                }
            }
        }
    }

    #[test]
    fn a_pem_header_named_in_a_command_does_not_swallow_the_records_after_it() {
        let key = format!("-----BEGIN PRIVATE KEY-----\n{FAKE_BODY}\n{FAKE_BODY}\n-----END PRIVATE KEY-----\n");
        for depth in 1..=3 {
            let command = serde_json::json!({
                "type": "assistant",
                "cmd": "grep -A 100 -- '-----BEGIN PRIVATE KEY-----' key.pem",
            })
            .to_string();
            let unrelated = r#"{"type":"event","note":"keep this record","n":7}"#;
            let result = format!(
                r#"{{"type":"user","out":"{}"}}"#,
                nested_in_json(&key, depth)
            );
            let tail = r#"{"type":"assistant","text":"done"}"#;
            let stream = [command.as_str(), unrelated, result.as_str(), tail].join("\n");
            let redacted = redact_secrets(&stream);
            assert!(!redacted.contains(FAKE_BODY), "depth {depth}: {redacted}");
            let lines: Vec<&str> = redacted.lines().collect();
            assert_eq!(lines.len(), 4, "depth {depth}: records are intact");
            assert_eq!(lines[1], unrelated, "depth {depth}");
            assert_eq!(lines[3], tail, "depth {depth}");
            for line in &lines {
                serde_json::from_str::<serde_json::Value>(line)
                    .unwrap_or_else(|e| panic!("depth {depth}: {line}: {e}"));
            }
            assert!(lines[0].contains("key.pem"), "the command is still readable");
        }
    }

    #[test]
    fn a_public_key_and_ordinary_base64_survive() {
        let raw = format!("-----BEGIN PUBLIC KEY-----\n{FAKE_BODY}\n-----END PUBLIC KEY-----");
        assert_eq!(redact_secrets(&raw), raw, "a public key is not a secret");
        assert_eq!(redact_secrets(FAKE_BODY), FAKE_BODY);
    }

    #[test]
    fn keyword_assignment_masks_only_the_value() {
        assert_eq!(redact_secrets("token: hunter2secret"), "token: [redacted]");
    }
}
