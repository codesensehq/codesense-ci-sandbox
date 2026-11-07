// SPDX-License-Identifier: MIT
//! SW-CP-24: structured config-format adapters for the merge engine.
//!
//! There is ONE merge algorithm ([`json_io::deep_merge_with`] over
//! `serde_json::Value`); each supported format is a thin parse/serialize
//! adapter that normalizes to/from that common `Value`. Adding a format is one
//! adapter arm — the engine never changes.
//!
//! **Fidelity caveat (accepted):** a Value-level round-trip canonicalizes
//! formatting — comments are dropped and object keys serialize in
//! `serde_json`'s order. For the driving cases (`.mcp.json`,
//! `.claude/settings.json` — comment-free JSON) this is invisible; CP-27's
//! toggle surfaces it before a user marks a comment-rich file.
//!
//! A mark on a non-structured / unsupported file ([`ConfigFormat::detect`]
//! returns `None`) is rejected — never a silent clobber-fallback.

use serde_json::Value;
use std::path::Path;

/// A structured config format the merge engine can parse and re-serialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// JSON, tolerant of JSONC/JSON5 (comments, trailing commas) on parse so
    /// `tsconfig.json` / `.vscode/settings.json` "just work". Serializes as
    /// pretty JSON (comments not preserved).
    Json,
    /// YAML (`serde_yaml`). Comments/anchors not preserved on re-serialize.
    Yaml,
    /// TOML (`toml`). Comments/formatting not preserved on re-serialize.
    Toml,
}

impl From<codesense_pty_core::agent_cli::ConfigFragmentFormat> for ConfigFormat {
    fn from(format: codesense_pty_core::agent_cli::ConfigFragmentFormat) -> Self {
        use codesense_pty_core::agent_cli::ConfigFragmentFormat;
        match format {
            ConfigFragmentFormat::Json => Self::Json,
            ConfigFragmentFormat::Yaml => Self::Yaml,
            ConfigFragmentFormat::Toml => Self::Toml,
        }
    }
}

/// Failure to parse or serialize a config file for merging. (Detection of an
/// unsupported/non-structured path is [`ConfigFormat::detect`] returning `None`
/// — the caller decides how to reject that, never a clobber-fallback.)
#[derive(Debug)]
pub enum FormatError {
    Parse(String),
    Serialize(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Parse(e) => write!(f, "config parse: {e}"),
            FormatError::Serialize(e) => write!(f, "config serialize: {e}"),
        }
    }
}

impl std::error::Error for FormatError {}

impl ConfigFormat {
    /// Detect the format by file extension. Returns `None` for a
    /// non-structured / unsupported file — the caller MUST treat `None` as
    /// "reject the merge mark", never as "clobber anyway".
    pub fn detect(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "json" | "jsonc" | "json5" => Some(Self::Json),
            "yaml" | "yml" => Some(Self::Yaml),
            "toml" => Some(Self::Toml),
            _ => None,
        }
    }

    /// Parse `bytes` into a schema-blind `serde_json::Value`.
    pub fn parse(self, bytes: &[u8]) -> Result<Value, FormatError> {
        let s = std::str::from_utf8(bytes).map_err(|e| FormatError::Parse(e.to_string()))?;
        // Strip a leading UTF-8 BOM — none of serde_json/json5/serde_yaml/toml
        // accept it, and Windows-authored configs (e.g. `.vscode/settings.json`)
        // commonly carry one; without this a BOM'd file would silently never merge.
        let s = s.strip_prefix('\u{feff}').unwrap_or(s);
        match self {
            Self::Json => parse_json_tolerant(s),
            Self::Yaml => serde_yaml::from_str(s).map_err(|e| FormatError::Parse(e.to_string())),
            Self::Toml => toml::from_str(s).map_err(|e| FormatError::Parse(e.to_string())),
        }
    }

    /// Serialize a merged `Value` back to this format's bytes. JSON uses the
    /// crate's canonical pretty form (no trailing newline — matches
    /// `json_io::write_json_atomic`, so the two-writer `.mcp.json` output is
    /// byte-stable).
    pub fn serialize(self, value: &Value) -> Result<Vec<u8>, FormatError> {
        match self {
            Self::Json => serde_json::to_string_pretty(value)
                .map(String::into_bytes)
                .map_err(|e| FormatError::Serialize(e.to_string())),
            Self::Yaml => serde_yaml::to_string(value)
                .map(String::into_bytes)
                .map_err(|e| FormatError::Serialize(e.to_string())),
            Self::Toml => toml::to_string_pretty(value)
                .map(String::into_bytes)
                .map_err(|e| FormatError::Serialize(e.to_string())),
        }
    }
}

/// Strict JSON first (canonical for `.mcp.json`), falling back to JSON5 so a
/// `.json` that carries comments (`tsconfig.json`) still parses.
fn parse_json_tolerant(s: &str) -> Result<Value, FormatError> {
    match serde_json::from_str(s) {
        Ok(v) => Ok(v),
        Err(strict) => json5::from_str(s)
            .map_err(|lenient| FormatError::Parse(format!("json: {strict}; json5: {lenient}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detect_by_extension() {
        let cases = [
            (".mcp.json", Some(ConfigFormat::Json)),
            ("tsconfig.json", Some(ConfigFormat::Json)),
            (".vscode/settings.jsonc", Some(ConfigFormat::Json)),
            ("config.yaml", Some(ConfigFormat::Yaml)),
            ("x.yml", Some(ConfigFormat::Yaml)),
            ("pyproject.toml", Some(ConfigFormat::Toml)),
            (".editorconfig", None),
            (".env", None),
            ("Makefile", None),
        ];
        for (name, want) in cases {
            assert_eq!(ConfigFormat::detect(&PathBuf::from(name)), want, "{name}");
        }
    }

    #[test]
    fn json_parse_is_jsonc_tolerant() {
        // A .json with comments + trailing comma still parses (tsconfig case).
        let src = br#"{
            // leading comment
            "compilerOptions": { "strict": true, }, // trailing comma
        }"#;
        let v = ConfigFormat::Json.parse(src).unwrap();
        assert_eq!(v, serde_json::json!({"compilerOptions": {"strict": true}}));
    }

    #[test]
    fn parse_error_on_garbage() {
        assert!(matches!(
            ConfigFormat::Toml.parse(b"this is not = = toml"),
            Err(FormatError::Parse(_))
        ));
    }

    #[test]
    fn json_parse_tolerates_leading_bom() {
        // A UTF-8 BOM-prefixed JSON (Windows-authored .vscode/settings.json) must
        // still parse, not silently skip.
        let mut base = "\u{feff}".as_bytes().to_vec();
        base.extend_from_slice(br#"{"a": 1}"#);
        let v = ConfigFormat::Json.parse(&base).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1}));
    }

    #[test]
    fn unsupported_path_has_no_adapter() {
        // AC-7 (format layer): a non-structured file has no adapter, so the
        // caller (mark store / projection) rejects it instead of clobbering.
        assert_eq!(ConfigFormat::detect(&PathBuf::from(".editorconfig")), None);
        assert_eq!(ConfigFormat::detect(&PathBuf::from("notes.txt")), None);
    }
}
