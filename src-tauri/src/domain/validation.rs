// SPDX-License-Identifier: MIT
use super::error::DomainError;

/// Validate that a string is non-empty (after trimming) and contains no
/// control characters. Used by both `SourceRepoSlug` segments and
/// `Attachment` field validation.
pub fn validate_non_empty_no_control(s: &str, field: &'static str) -> Result<(), DomainError> {
    if s.trim().is_empty() {
        return Err(DomainError::EmptyField { field });
    }

    if s.chars().any(|c| c.is_control()) {
        return Err(DomainError::InvalidSourceRepoSegment {
            field,
            segment: s.to_string(),
            reason: "control character",
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_string() {
        assert!(validate_non_empty_no_control("hello", "test").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(
            validate_non_empty_no_control("", "name"),
            Err(DomainError::EmptyField { field: "name" })
        );
    }

    #[test]
    fn rejects_whitespace_only() {
        assert_eq!(
            validate_non_empty_no_control("   ", "name"),
            Err(DomainError::EmptyField { field: "name" })
        );
    }

    #[test]
    fn rejects_null_byte() {
        let result = validate_non_empty_no_control("foo\0bar", "path");
        assert!(matches!(
            result,
            Err(DomainError::InvalidSourceRepoSegment {
                reason: "control character",
                ..
            })
        ));
    }

    #[test]
    fn rejects_ascii_control() {
        let result = validate_non_empty_no_control("foo\x01bar", "path");
        assert!(matches!(
            result,
            Err(DomainError::InvalidSourceRepoSegment {
                reason: "control character",
                ..
            })
        ));
    }

    #[test]
    fn accepts_slashes_and_dots() {
        // Paths legitimately contain slashes and dots.
        assert!(validate_non_empty_no_control("/Users/dev/my.repo", "path").is_ok());
    }
}
