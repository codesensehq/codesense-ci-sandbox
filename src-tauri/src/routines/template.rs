// SPDX-License-Identifier: MIT
//! The `{{name}}` / `{{trigger.key}}` grammar used in a routine's `task`.
//!
//! This is a second interpolation form, separate from the single-brace
//! `{field}` form in `commands::field_schema`. That extractor would match the
//! inner `{last_run_at}` of `{{last_run_at}}` and leave stray braces, so routines
//! own this small renderer. A token is an identifier (`[A-Za-z0-9_]+`) with an
//! optional single `trigger.` prefix; anything else between `{{` and `}}` is
//! malformed.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

/// Values every run can fill without a declaration.
pub const BUILTINS: [&str; 4] = ["date", "timestamp", "last_run_at", "last_run_summary"];

const TRIGGER_PREFIX: &str = "trigger.";

static DOUBLE_BRACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{([^{}]*)\}\}").expect("static regex"));

/// One `{{…}}` occurrence, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// One of [`BUILTINS`].
    Builtin(String),
    /// `{{trigger.<key>}}`: a field of the event payload. Holds `<key>`.
    Trigger(String),
    /// Any other identifier; valid only when declared as a variable.
    Name(String),
    /// Text between the braces that is not a token (spaces, dots, empty).
    Malformed(String),
}

impl Token {
    /// The token as written between the braces.
    pub fn text(&self) -> String {
        match self {
            Self::Builtin(name) | Self::Name(name) | Self::Malformed(name) => name.clone(),
            Self::Trigger(key) => format!("{TRIGGER_PREFIX}{key}"),
        }
    }

    fn classify(inner: &str) -> Self {
        if let Some(key) = inner.strip_prefix(TRIGGER_PREFIX) {
            return if is_identifier(key) {
                Self::Trigger(key.to_string())
            } else {
                Self::Malformed(inner.to_string())
            };
        }
        if !is_identifier(inner) {
            Self::Malformed(inner.to_string())
        } else if BUILTINS.contains(&inner) {
            Self::Builtin(inner.to_string())
        } else {
            Self::Name(inner.to_string())
        }
    }
}

/// `[A-Za-z0-9_]+`, ASCII only.
pub fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Names a declared variable may not take: the built-ins and `trigger`.
pub fn is_reserved_name(name: &str) -> bool {
    BUILTINS.contains(&name) || name == "trigger"
}

/// Every distinct token in `text`, in first-seen order.
pub fn tokens(text: &str) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    for caps in DOUBLE_BRACE.captures_iter(text) {
        let token = Token::classify(&caps[1]);
        if !out.contains(&token) {
            out.push(token);
        }
    }
    out
}

/// The result of [`render`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    /// Tokens left as literal `{{…}}` because no value was supplied (or they are
    /// malformed), de-duplicated in first-seen order.
    pub unresolved: Vec<String>,
}

/// Substitute every token whose name (`date`, `channel`, `trigger.key`) has a
/// value in `values`. A token with no value stays as its literal `{{…}}` and is
/// reported, never silently blanked.
pub fn render(text: &str, values: &BTreeMap<String, String>) -> Rendered {
    let mut unresolved: Vec<String> = Vec::new();
    let rendered = DOUBLE_BRACE.replace_all(text, |caps: &regex::Captures| {
        let token = Token::classify(&caps[1]);
        let name = token.text();
        match (&token, values.get(&name)) {
            (Token::Malformed(_), _) | (_, None) => {
                if !unresolved.contains(&name) {
                    unresolved.push(name);
                }
                caps[0].to_string()
            }
            (_, Some(value)) => value.clone(),
        }
    });
    Rendered {
        text: rendered.into_owned(),
        unresolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn tokens_classify_builtins_triggers_names_and_malformed() {
        let got =
            tokens("{{date}} {{trigger.pr_url}} {{channel}} {{ date }} {{a.b}} {{}} {{date}}");
        assert_eq!(
            got,
            vec![
                Token::Builtin("date".into()),
                Token::Trigger("pr_url".into()),
                Token::Name("channel".into()),
                Token::Malformed(" date ".into()),
                Token::Malformed("a.b".into()),
                Token::Malformed(String::new()),
            ]
        );
    }

    #[test]
    fn single_braces_are_not_tokens() {
        assert!(tokens("{field} and {{ but not closed").is_empty());
        assert_eq!(tokens("{{{x}}}"), vec![Token::Name("x".into())]);
    }

    #[test]
    fn nested_trigger_prefix_is_malformed() {
        assert_eq!(
            tokens("{{trigger.trigger.x}} {{trigger.}}"),
            vec![
                Token::Malformed("trigger.trigger.x".into()),
                Token::Malformed("trigger.".into()),
            ]
        );
    }

    #[test]
    fn render_substitutes_known_and_reports_unknown() {
        let out = render(
            "since {{last_run_at}} in #{{channel}} for {{trigger.pr}} {{missing}} {{missing}} {{ bad }}",
            &values(&[
                ("last_run_at", "2026-09-30T09:00:00Z"),
                ("channel", "all-codesense"),
                ("trigger.pr", "#12"),
            ]),
        );
        assert_eq!(
            out.text,
            "since 2026-09-30T09:00:00Z in #all-codesense for #12 {{missing}} {{missing}} {{ bad }}"
        );
        assert_eq!(
            out.unresolved,
            vec!["missing".to_string(), " bad ".to_string()]
        );
    }

    #[test]
    fn render_does_not_reinterpret_substituted_values() {
        let out = render("{{a}}", &values(&[("a", "{{b}}")]));
        assert_eq!(out.text, "{{b}}");
        assert!(out.unresolved.is_empty());
    }
}
