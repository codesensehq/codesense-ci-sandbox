// SPDX-License-Identifier: MIT
//! The one value order the engine sorts, groups and range-filters by. It must
//! be a total order (Rust's sort may panic on an inconsistent comparator), so
//! values are bucketed by type first and only compared within a bucket:
//!
//! number < date < text
//!
//! - number: parses as a finite decimal (`3`, `-1.5`, `2e3`) — compared numerically,
//!   ties broken by the text so `1` and `1.0` stay distinct but adjacent;
//! - date: starts with an ISO date (`2026-10-02`, `2026-10-02T09:00Z`) — compared as text,
//!   which orders ISO dates chronologically;
//! - text: the app's natural order (`codesense_fs_core::natural_sort`, the file
//!   tree's — `SW-OC-9` < `SW-OC-10`), then the whole strings by bytes as the tie-break.

use std::cmp::Ordering;

use codesense_fs_core::natural_sort::natural_cmp;

/// The type bucket of a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Bucket {
    Number,
    Date,
    Text,
}

/// A finite number, when the whole (trimmed) value is one.
pub fn as_number(s: &str) -> Option<f64> {
    let t = s.trim();
    // Reject forms `f64::from_str` accepts but nobody means as a number.
    if t.is_empty()
        || t.chars()
            .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
    {
        return None;
    }
    t.parse::<f64>().ok().filter(|f| f.is_finite())
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

fn bucket(s: &str) -> Bucket {
    if as_number(s).is_some() {
        Bucket::Number
    } else if is_iso_date(s) {
        Bucket::Date
    } else {
        Bucket::Text
    }
}

/// Are `a` and `b` the same kind of value (number, date or text)?
pub fn same_kind(a: &str, b: &str) -> bool {
    bucket(a) == bucket(b)
}

/// The total order over values. Equal only for identical strings.
pub fn cmp(a: &str, b: &str) -> Ordering {
    let (ba, bb) = (bucket(a), bucket(b));
    if ba != bb {
        return ba.cmp(&bb);
    }
    match ba {
        Bucket::Number => {
            let (x, y) = (as_number(a).unwrap_or(0.0), as_number(b).unwrap_or(0.0));
            x.total_cmp(&y).then_with(|| a.cmp(b))
        }
        Bucket::Date => a.cmp(b),
        // The app's one natural order (the file tree's), then bytes, so only
        // identical strings tie (`a1` vs `a01`).
        Bucket::Text => natural_cmp(a, b).then_with(|| a.cmp(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POOL: &[&str] = &[
        "9",
        "10",
        "1x",
        "1",
        "01",
        "1.0",
        "-2",
        "2e3",
        "0",
        "0.5",
        "",
        " ",
        "a",
        "A",
        "b",
        "a1",
        "a01",
        "a10",
        "a2",
        "SW-OC-9",
        "SW-OC-10",
        "SW-OC-010",
        "2026-10-02",
        "2026-01-31T09:00Z",
        "2026-1-2",
        "10.0.1",
        "1.10.0",
        "1.9.0",
        "x9y",
        "x10y",
        "inf",
        "NaN",
        "zz",
        "Z",
        "é",
        "1e",
        "e1",
        "p0",
        "p10",
        "p2",
    ];

    #[test]
    fn the_order_is_total_antisymmetric_and_transitive() {
        for a in POOL {
            assert_eq!(cmp(a, a), Ordering::Equal);
            for b in POOL {
                assert_eq!(cmp(a, b), cmp(b, a).reverse(), "antisymmetry {a:?} {b:?}");
                if a != b {
                    assert_ne!(cmp(a, b), Ordering::Equal, "only identical values tie");
                }
                for c in POOL {
                    if cmp(a, b) != Ordering::Greater && cmp(b, c) != Ordering::Greater {
                        assert_ne!(
                            cmp(a, c),
                            Ordering::Greater,
                            "transitivity {a:?} {b:?} {c:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn random_mixed_values_sort_without_panicking_and_in_order() {
        // A small deterministic xorshift so the test needs no extra crate.
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200 {
            let mut v: Vec<String> = (0..64)
                .map(|_| match next() % 4 {
                    0 => (next() % 200).to_string(),
                    1 => format!("{}x", next() % 20),
                    2 => format!("2026-{:02}-{:02}", next() % 12 + 1, next() % 28 + 1),
                    _ => POOL[(next() % POOL.len() as u64) as usize].to_string(),
                })
                .collect();
            v.sort_by(|a, b| cmp(a, b));
            assert!(v.windows(2).all(|w| cmp(&w[0], &w[1]) != Ordering::Greater));
        }
    }

    #[test]
    fn useful_orders() {
        assert_eq!(cmp("9", "10"), Ordering::Less);
        assert_eq!(cmp("SW-OC-9", "SW-OC-10"), Ordering::Less);
        assert_eq!(cmp("2026-01-31", "2026-10-02"), Ordering::Less);
        assert_eq!(
            cmp("10", "2026-01-01"),
            Ordering::Less,
            "numbers before dates"
        );
        assert_eq!(
            cmp("2026-01-01", "abc"),
            Ordering::Less,
            "dates before text"
        );
        assert!(as_number("inf").is_none() && as_number("NaN").is_none());
    }
}
