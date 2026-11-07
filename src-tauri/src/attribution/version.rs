// SPDX-License-Identifier: MIT
//! The git-ai releases CodeSense captures with: `>=1.7.5 <1.8`, the range SV-CDE-1 proved.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version(pub u32, pub u32, pub u32);

const MIN: Version = Version(1, 7, 5);
const BELOW: Version = Version(1, 8, 0);

impl Version {
    /// The first `major.minor.patch` in `output`, allowing a prerelease/build suffix.
    /// Ranges compare the numeric core; the installer compares the exact pinned string.
    pub(crate) fn parse(output: &str) -> Option<Self> {
        output.split_whitespace().find_map(|word| {
            let numbers = word.trim_start_matches('v');
            let end = std::primitive::str::find(numbers, |c: char| !c.is_ascii_digit() && c != '.')
                .unwrap_or(numbers.len());
            let mut parts = numbers[..end].split('.');
            let version = Self(
                std::result::Result::ok(parts.next()?.parse())?,
                std::result::Result::ok(parts.next()?.parse())?,
                std::result::Result::ok(parts.next()?.parse())?,
            );
            // A fourth component is not a release number.
            parts.next().is_none().then_some(version)
        })
    }

    pub(crate) fn in_range(self) -> bool {
        (MIN..BELOW).contains(&self)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_version_strings_parse_and_the_range_is_1_7_5_up_to_1_8() {
        for (output, parsed, supported) in [
            ("1.7.5", Some(Version(1, 7, 5)), true),
            ("1.7.5\n", Some(Version(1, 7, 5)), true),
            ("1.7.5-cs.1", Some(Version(1, 7, 5)), true),
            ("1.7.5+build.1", Some(Version(1, 7, 5)), true),
            ("1.7.9", Some(Version(1, 7, 9)), true),
            ("1.7.5 (debug)", Some(Version(1, 7, 5)), true),
            ("git-ai 1.7.6", Some(Version(1, 7, 6)), true),
            ("v1.7.5", Some(Version(1, 7, 5)), true),
            ("1.7.4", Some(Version(1, 7, 4)), false),
            ("1.8.0", Some(Version(1, 8, 0)), false),
            ("2.0.0", Some(Version(2, 0, 0)), false),
            ("1.7", None, false),
            ("1.7.5.2", None, false),
            ("garbage", None, false),
            ("", None, false),
        ] {
            let version = Version::parse(output);
            assert_eq!(version, parsed, "{output:?}");
            assert_eq!(
                version.is_some_and(Version::in_range),
                supported,
                "{output:?}"
            );
        }
    }
}
