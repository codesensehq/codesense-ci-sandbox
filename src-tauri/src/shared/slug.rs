// SPDX-License-Identifier: MIT
//! A stable, legible id slug from a human name.

/// Lowercase, non-alphanumeric runs collapse to a single `-`, trimmed; an empty/blank name
/// falls back to `item` (never an empty id).
pub(crate) fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let slug = out.trim_matches('-');
    if slug.is_empty() {
        "item".to_string()
    } else {
        slug.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_normalizes_and_falls_back() {
        assert_eq!(slugify("PR Review!"), "pr-review");
        assert_eq!(slugify("  Nightly   Sync  "), "nightly-sync");
        assert_eq!(slugify("...///"), "item");
        assert_eq!(slugify(""), "item");
    }
}
