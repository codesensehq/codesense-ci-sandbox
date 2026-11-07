// SPDX-License-Identifier: MIT
//! Natural sort comparator shared between the file listing
//! ([`crate::FsService::list`]) and the app's `pipeline_scanner`
//! story/sub-epic ordering.
//!
//! These two surfaces MUST sort identically — the file tree's sibling
//! order is the visual source of truth, and any divergence between the
//! tree and the scanner means a pipeline.json's natural-order stories
//! disagree with the order the user sees in the tree. Centralizing the
//! comparator here makes that drift impossible.

use std::cmp::Ordering;

/// Compare two strings using natural sort order: numeric segments are
/// compared as numbers so `"2-foo" < "10-foo"` instead of the
/// lexicographic `"10-foo" < "2-foo"`. Non-digit bytes fall back to
/// byte-wise comparison.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let (mut i, mut j) = (0, 0);

    while i < ab.len() && j < bb.len() {
        let ca = ab[i];
        let cb = bb[j];

        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            // Extract the full numeric run on each side and compare as
            // u64. `unwrap_or(0)` defends against improbable parse
            // failures on extremely long numeric runs.
            let ai = i;
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            let bj = j;
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let an: u64 = a[ai..i].parse().unwrap_or(0);
            let bn: u64 = b[bj..j].parse().unwrap_or(0);
            match an.cmp(&bn) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        } else {
            match ca.cmp(&cb) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                ord => return ord,
            }
        }
    }

    // What's left decides: the side with nothing left is less. (Comparing the
    // whole lengths would be wrong once numeric runs of different widths were
    // matched — `"01"` vs `"1x"` — and broke transitivity.)
    (ab.len() - i).cmp(&(bb.len() - j))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_runs_compare_as_numbers() {
        assert_eq!(natural_cmp("2-foo", "10-foo"), Ordering::Less);
        assert_eq!(natural_cmp("10-foo", "2-foo"), Ordering::Greater);
        assert_eq!(natural_cmp("1", "1"), Ordering::Equal);
    }

    #[test]
    fn alpha_segments_use_byte_order() {
        assert_eq!(natural_cmp("apple", "banana"), Ordering::Less);
        assert_eq!(natural_cmp("banana", "apple"), Ordering::Greater);
    }

    #[test]
    fn mixed_runs_handle_alternation() {
        assert_eq!(natural_cmp("v1.10", "v1.2"), Ordering::Greater);
        assert_eq!(natural_cmp("a1b", "a1c"), Ordering::Less);
    }

    #[test]
    fn shorter_prefix_is_less_when_otherwise_equal() {
        assert_eq!(natural_cmp("foo", "foobar"), Ordering::Less);
    }

    #[test]
    fn test_natural_cmp_numbered_entries() {
        // AC 1: numbered entries sort as 1, 2, 9, 10, 11 — not 1, 10, 11, 2, 9
        assert_eq!(natural_cmp("1-foo", "2-bar"), Ordering::Less);
        assert_eq!(natural_cmp("2-bar", "10-baz"), Ordering::Less);
        assert_eq!(natural_cmp("9-qux", "10-baz"), Ordering::Less);
        assert_eq!(natural_cmp("10-baz", "11-quux"), Ordering::Less);
        assert_eq!(natural_cmp("10-baz", "2-bar"), Ordering::Greater);

        let mut names = vec!["11-quux", "1-foo", "10-baz", "2-bar", "9-qux"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["1-foo", "2-bar", "9-qux", "10-baz", "11-quux"]);
    }

    #[test]
    fn test_natural_cmp_alphabetical_only() {
        // AC 2: entries with no numeric prefix sort alphabetically as before
        assert_eq!(natural_cmp("alpha", "beta"), Ordering::Less);
        assert_eq!(natural_cmp("beta", "alpha"), Ordering::Greater);
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);

        let mut names = vec!["charlie", "alpha", "beta"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["alpha", "beta", "charlie"]);
    }

    #[test]
    fn test_natural_cmp_mixed_entries() {
        // AC 3: mixed entries sort correctly
        let mut names = vec!["readme.md", "10-story", "2-story", "about.md", "1-story"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            vec!["1-story", "2-story", "10-story", "about.md", "readme.md"]
        );
    }

    #[test]
    fn a_shorter_remainder_is_less_after_numeric_runs_of_different_width() {
        assert_eq!(natural_cmp("01", "1x"), Ordering::Less);
        assert_eq!(natural_cmp("1x", "01"), Ordering::Greater);
        assert_eq!(natural_cmp("01", "1"), Ordering::Equal);
    }

    #[test]
    fn the_order_is_consistent_across_mixed_values() {
        // Sort comparators must be a consistent (weak) order: antisymmetric and
        // transitive, or `sort_by` may panic.
        let pool = [
            "9",
            "10",
            "1x",
            "1",
            "01",
            "a",
            "A",
            "a1",
            "a01",
            "a10",
            "a2",
            "SW-OC-9",
            "SW-OC-10",
            "SW-OC-010",
            "x9y",
            "x10y",
            "1.10.0",
            "1.9.0",
            " ",
            "",
            "zz",
            "a 5",
            "a10b",
            "99999999999999999999",
            "0",
            "p0",
            "p10",
        ];
        for a in pool {
            for b in pool {
                assert_eq!(
                    natural_cmp(a, b),
                    natural_cmp(b, a).reverse(),
                    "{a:?} {b:?}"
                );
                for c in pool {
                    if natural_cmp(a, b) != Ordering::Greater
                        && natural_cmp(b, c) != Ordering::Greater
                    {
                        assert_ne!(natural_cmp(a, c), Ordering::Greater, "{a:?} {b:?} {c:?}");
                    }
                }
            }
        }
    }
}
