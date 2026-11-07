// SPDX-License-Identifier: MIT
//! Helpers for tool inputs that accept several forms through one flat object.
//!
//! An MCP tool's input schema must be a single object: the Anthropic API
//! refuses a top-level `anyOf`/`oneOf`/`allOf`. A tool that takes more than one
//! form therefore decodes one flat struct holding every form's fields, then
//! picks the form from its discriminating field and refuses whatever that form
//! does not take. These helpers keep that decode as strict as a per-form
//! `deny_unknown_fields` struct would be.
use serde::{Deserialize, Deserializer};

/// A field that may be absent but, when present, must hold a `T` (never
/// `null`): pair it with `#[serde(default, skip_serializing_if =
/// "Option::is_none")]` (the skip keeps schemars from publishing
/// `default: null`). `Some` means it was sent.
pub(crate) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// A field whose form takes it as optional (`null` allowed): pair it as
/// [`present`] is. The outer `Some` means it was sent, even as `null`.
pub(crate) fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// The first field sent that `form` does not take, as a refusal.
pub(crate) fn refuse_extra(form: &str, sent: &[(&str, bool)]) -> Result<(), String> {
    match sent.iter().find(|(_, sent)| *sent) {
        Some((field, _)) => Err(format!("invalid: {form} does not take {field}")),
        None => Ok(()),
    }
}

/// A field `form` requires.
pub(crate) fn require<T>(form: &str, field: &str, value: Option<T>) -> Result<T, String> {
    value.ok_or_else(|| format!("invalid: {form} requires {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Probe {
        #[serde(default, deserialize_with = "present")]
        strict: Option<bool>,
        #[serde(default, deserialize_with = "nullable")]
        loose: Option<Option<String>>,
    }

    #[test]
    fn present_refuses_null_and_nullable_tells_null_from_absent() {
        let absent: Probe = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(absent.strict.is_none() && absent.loose.is_none());
        let sent: Probe =
            serde_json::from_value(serde_json::json!({"strict":false,"loose":null})).unwrap();
        assert_eq!(sent.strict, Some(false));
        assert_eq!(sent.loose, Some(None));
        assert!(serde_json::from_value::<Probe>(serde_json::json!({"strict":null})).is_err());
        assert_eq!(
            refuse_extra("attach", &[("cwd", false), ("name", true)]).unwrap_err(),
            "invalid: attach does not take name"
        );
        assert_eq!(
            require::<u8>("selected form", "requestId", None).unwrap_err(),
            "invalid: selected form requires requestId"
        );
    }
}
