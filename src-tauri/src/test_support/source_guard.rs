// SPDX-License-Identifier: MIT
//! Production text for the source-scanning guards: a file's source without its
//! `#[cfg(test)]` modules, whatever their name, visibility or attributes. The
//! one cut every guard shares, so a `pub(crate) mod tests` cannot slip past one
//! guard that matched only an exact module header.

/// Whether `path` is a test-only source file by the repo's naming convention:
/// `tests.rs`, `*_tests.rs`, `*_test.rs`, `*.test.*`, or anything under a
/// `tests/` directory.
pub(crate) fn is_test_file(path: &std::path::Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy());
    name.is_some_and(|name| {
        name == "tests.rs"
            || name.ends_with("_tests.rs")
            || name.ends_with("_test.rs")
            || name.contains(".test.")
    }) || path.components().any(|c| c.as_os_str() == "tests")
}

/// `text` with no comments, no whitespace and no trailing comma before a `)`, so a guard
/// matches a call however rustfmt wraps it and a commented-out call never counts: squash
/// both the source and the expected text. Comment markers inside strings and chars stay.
pub(crate) fn squashed(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut code = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        let comment = bytes[at..].starts_with(b"//") || bytes[at..].starts_with(b"/*");
        match skip_non_code(bytes, at) {
            Some(next) if comment => at = next,
            Some(next) => {
                code.push_str(&text[at..next]);
                at = next;
            }
            None => {
                let character = text[at..].chars().next().unwrap_or_default();
                code.push(character);
                at += character.len_utf8();
            }
        }
    }
    code.split_whitespace()
        .collect::<String>()
        .replace(",)", ")")
}

/// `text` as production code: empty for a `#![cfg(test)]` file, else without its
/// inline test modules.
pub(crate) fn production_text(text: &str) -> String {
    let mut leading = text.trim_start();
    loop {
        if let Some(comment) = leading.strip_prefix("//") {
            leading = comment
                .split_once('\n')
                .map_or("", |(_, rest)| rest)
                .trim_start();
        } else if let Some(comment) = leading.strip_prefix("/*") {
            let mut rest = comment;
            let mut depth = 1;
            while depth > 0 {
                if let Some(next) = rest.strip_prefix("/*") {
                    depth += 1;
                    rest = next;
                } else if let Some(next) = rest.strip_prefix("*/") {
                    depth -= 1;
                    rest = next;
                } else if let Some(character) = rest.chars().next() {
                    rest = &rest[character.len_utf8()..];
                } else {
                    // Malformed source must never be classified as test-only.
                    return text.to_string();
                }
            }
            leading = rest.trim_start();
        } else {
            break;
        }
    }
    if leading.starts_with("#![cfg(test)]") {
        return String::new();
    }
    without_test_modules(text).unwrap_or_else(|| text.to_string())
}

/// `text` without its inline `#[cfg(test)]` modules, whatever their
/// visibility, keeping the code around them. Markers and braces inside
/// comments, strings and chars don't count. `None` when a module never
/// closes: malformed source must never be classified as test-only.
fn without_test_modules(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut production = String::new();
    let (mut kept, mut at) = (0, 0);
    while at < bytes.len() {
        if let Some(next) = skip_non_code(bytes, at) {
            at = next;
        } else if let Some(open) = (bytes[at] == b'#')
            .then(|| test_module_body(&text[at..]))
            .flatten()
        {
            let end = block_end(bytes, at + open)?;
            production.push_str(&text[kept..at]);
            (kept, at) = (end, end);
        } else {
            at += 1;
        }
    }
    production.push_str(&text[kept..]);
    Some(production)
}

/// The offset of the `{` opening the module body when `text` starts with
/// `#[cfg(test)]` then an inline `[pub[(..)]] mod <name> {`.
fn test_module_body(text: &str) -> Option<usize> {
    let rest = skip_attributes(text.strip_prefix("#[cfg(test)]")?);
    let rest = match rest.strip_prefix("pub") {
        Some(after) if after.starts_with('(') => after.split_once(')')?.1.trim_start(),
        Some(after) if after.starts_with(char::is_whitespace) => after.trim_start(),
        _ => rest,
    };
    let name = rest.strip_prefix("mod")?;
    if !name.starts_with(char::is_whitespace) {
        return None;
    }
    let body = name
        .trim_start()
        .trim_start_matches(|c: char| c == '_' || c.is_alphanumeric());
    body.trim_start()
        .starts_with('{')
        .then(|| text.len() - body.trim_start().len())
}

/// `text` past any leading attributes (`#[allow(..)]`) and comments. Each
/// attribute ends at its own closing bracket: brackets inside its strings and
/// comments don't count, and what follows it is never searched for one.
fn skip_attributes(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut at = 0;
    loop {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let Some(&byte) = bytes.get(at) else {
            return "";
        };
        if let (b'/', Some(next)) = (byte, skip_non_code(bytes, at)) {
            at = next;
        } else if bytes[at..].starts_with(b"#[") {
            let mut depth = 0usize;
            at += 1; // the `#`
            while at < bytes.len() {
                if let Some(next) = skip_non_code(bytes, at) {
                    at = next;
                    continue;
                }
                match bytes[at] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                at += 1;
                if depth == 0 {
                    break;
                }
            }
        } else {
            return &text[at..];
        }
    }
}

/// The offset just past the `}` closing the block opened at `open`.
fn block_end(bytes: &[u8], open: usize) -> Option<usize> {
    let (mut depth, mut at) = (0usize, open);
    while at < bytes.len() {
        if let Some(next) = skip_non_code(bytes, at) {
            at = next;
            continue;
        }
        match bytes[at] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// When a comment, string or char literal starts at `at`, the offset just
/// past it (the end of the text when it never closes).
fn skip_non_code(bytes: &[u8], at: usize) -> Option<usize> {
    let rest = &bytes[at..];
    let past = |offset: usize| Some((at + offset).min(bytes.len()));
    if rest.starts_with(b"//") {
        let line = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        return past(line);
    }
    if rest.starts_with(b"/*") {
        let (mut depth, mut i) = (1, 2);
        while depth > 0 && i < rest.len() {
            if rest[i..].starts_with(b"/*") {
                (depth, i) = (depth + 1, i + 2);
            } else if rest[i..].starts_with(b"*/") {
                (depth, i) = (depth - 1, i + 2);
            } else {
                i += 1;
            }
        }
        return past(i);
    }
    let identifier_before =
        at > 0 && (bytes[at - 1] == b'_' || bytes[at - 1].is_ascii_alphanumeric());
    let raw = [b"br".as_slice(), b"cr", b"r"]
        .iter()
        .find_map(|prefix| rest.strip_prefix(*prefix));
    if let (false, Some(after)) = (identifier_before, raw) {
        let hashes = after.iter().take_while(|&&b| b == b'#').count();
        if after.get(hashes) == Some(&b'"') {
            let start = rest.len() - after.len() + hashes + 1;
            let mut close = vec![b'"'];
            close.extend(std::iter::repeat(b'#').take(hashes));
            let end = rest[start..]
                .windows(close.len())
                .position(|window| window == close.as_slice());
            return past(end.map_or(rest.len(), |end| start + end + close.len()));
        }
    }
    if rest[0] == b'"' {
        let mut i = 1;
        while i < rest.len() && rest[i] != b'"' {
            i += if rest[i] == b'\\' { 2 } else { 1 };
        }
        return past(i + 1);
    }
    if rest[0] == b'\'' {
        // A char literal ('x', '\n', '\u{..}'), not a lifetime ('a).
        let close = if rest.get(1) == Some(&b'\\') {
            // The escape ends after `\u{..}`, `\xNN` or one escaped char.
            let end = match rest.get(2) {
                Some(&b'u') => rest.iter().position(|&b| b == b'}').map(|i| i + 1),
                Some(&b'x') => Some(5),
                _ => Some(3),
            };
            end.filter(|&i| rest.get(i) == Some(&b'\''))
        } else {
            let width = match *rest.get(1)? {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            (rest.get(1 + width) == Some(&b'\'')).then_some(1 + width)
        };
        return close.map(|i| at + i + 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{is_test_file, production_text, squashed};
    use std::path::Path;

    #[test]
    fn source_guard_recognizes_only_compile_enforced_whole_file_tests() {
        let mint = "fn production() { WorkspaceKey::new(\"real\"); }";
        for preamble in [
            "",
            "//! Module docs\n",
            "/* Docs */\n",
            "/* outer /* nested */ */\n",
        ] {
            assert!(production_text(&format!("{preamble}#![cfg(test)]\n{mint}")).is_empty());
        }
        for decoy in [
            "",
            "// #![cfg(test)]\n",
            "/* #![cfg(test)] */\n",
            "const NOTE: &str = \"#![cfg(test)]\";\n",
            "#[cfg(test)]\nfn test_only() {}\n",
            "// file named arbitrary_tests.rs is not a compiler gate\n",
        ] {
            assert!(production_text(&format!("{decoy}{mint}")).contains("WorkspaceKey::new("));
        }
    }

    /// SV-PBF-134: a trailing test module is excluded whatever its visibility,
    /// and a production call in the same file is still counted.
    #[test]
    fn source_guard_excludes_a_trailing_test_module_of_any_visibility() {
        let mint = "fn production() { WorkspaceKey::new(\"real\"); }\n";
        for visibility in ["", "pub ", "pub(crate) ", "pub(super) "] {
            let file = format!(
                "{mint}#[cfg(test)]\n{visibility}mod tests {{\n    fn t() {{ WorkspaceKey::new(\"test\"); }}\n}}\n"
            );
            assert_eq!(
                production_text(&file).matches("WorkspaceKey::new(").count(),
                1,
                "{visibility:?}"
            );
        }
    }

    /// A test module is cut whatever its name or the attributes after the gate.
    #[test]
    fn source_guard_excludes_a_test_module_with_any_name_or_attributes() {
        let mint = "fn production() { WorkspaceKey::new(\"real\"); }\n";
        for header in [
            "mod fixtures",
            "pub(crate) mod session_tests",
            "#[allow(dead_code)]\nmod tests",
            "#[allow(unused)]\n#[rustfmt::skip]\npub(crate) mod tests",
        ] {
            let file = format!(
                "#[cfg(test)]\n{header} {{\n    fn t() {{ WorkspaceKey::new(\"test\"); }}\n}}\n{mint}"
            );
            let production = production_text(&file);
            assert!(
                production.contains("WorkspaceKey::new(\"real\")"),
                "{header}"
            );
            assert!(
                !production.contains("WorkspaceKey::new(\"test\")"),
                "{header}"
            );
        }
    }

    /// An attribute ends at its own `]`: a trailing comment on it, or a test-only
    /// function after it, never makes the next production module look like a
    /// test module.
    #[test]
    fn source_guard_keeps_a_production_module_after_a_test_only_function() {
        let production = "#[cfg(unix)]\nmod production {\n    pub fn regression() { WorkspaceKey::new(\"real\"); }\n}\n";
        for fixture in [
            "#[cfg(test)]\n#[allow(dead_code)] // fixture-only helper\nfn fixture() {}\n",
            "#[cfg(test)]\n#[allow(dead_code)]\nfn fixture() {}\n",
            "#[cfg(test)]\nfn fixture() {}\n",
            "#[cfg(test)]\n#[doc = \"a ] b\"] /* ] */\nfn fixture() {}\n",
        ] {
            let kept = production_text(&format!("{fixture}{production}"));
            assert!(kept.contains("mod production"), "{fixture}");
            assert!(kept.contains("WorkspaceKey::new(\"real\")"), "{fixture}");
        }
    }

    /// A comment between the gate and the module, or brackets in an attribute's
    /// own string, still finds the test module.
    #[test]
    fn source_guard_cuts_a_test_module_behind_commented_or_bracketed_attributes() {
        for header in [
            "#[allow(dead_code)] // note\nmod tests",
            "// note\n#[allow(dead_code)]\nmod tests",
            "#[doc = \"a ] b\"]\nmod tests",
            "#[cfg_attr(test, allow(unused))]\npub(crate) mod tests",
        ] {
            let file = format!(
                "#[cfg(test)]\n{header} {{\n    fn t() {{ WorkspaceKey::new(\"test\"); }}\n}}\nfn after() {{ WorkspaceKey::new(\"real\"); }}\n"
            );
            let kept = production_text(&file);
            assert!(!kept.contains("\"test\""), "{header}");
            assert!(kept.contains("\"real\""), "{header}");
        }
    }

    /// SV-PBF-134 review: excluding a test module keeps the production code
    /// after it (the `context_attachments.rs` layout), braces in its strings and
    /// chars don't end it early, and a module marker inside a string or comment
    /// hides nothing.
    #[test]
    fn source_guard_keeps_production_after_a_test_module() {
        let mint = "fn production() { WorkspaceKey::new(\"real\"); }\n";
        let test_module = "#[cfg(test)]\npub(crate) mod tests {\n    fn t() { let _ = ('{', \"}\", r#\"}\"#); WorkspaceKey::new(\"test\"); }\n}\n";
        for file in [
            format!("{test_module}{mint}"),
            format!("{mint}{test_module}{mint}"),
            format!("const DOC: &str = r#\"\n#[cfg(test)]\nmod tests {{\n\"#;\n{mint}"),
            format!("/*\n#[cfg(test)]\npub(crate) mod tests {{\n*/\n{mint}"),
            format!("mod parent {{\n#[cfg(test)]\npub(crate) mod tests {{\n    fn e() {{ let _ = ['\\'','{{']; }}\n}}\n{mint}}}\n"),
            format!("mod parent {{\n#[cfg(test)]\npub(crate) mod tests {{\n    const T: &std::ffi::CStr = cr#\"\" {{ \"\"#;\n}}\n{mint}}}\n"),
        ] {
            let production = production_text(&file);
            assert!(production.contains("WorkspaceKey::new(\"real\")"), "{file}");
            assert!(
                !production.contains("WorkspaceKey::new(\"test\")"),
                "{file}"
            );
        }
    }

    /// A commented-out call never counts, and `//` inside a string is not a comment.
    #[test]
    fn squashed_drops_comments_but_keeps_strings() {
        assert_eq!(squashed("f(a, // b\n  c, /* d, /* e */ */\n)"), "f(a,c)");
        assert_eq!(squashed("// f(a)\ng()"), "g()");
        assert_eq!(
            squashed("let u = \"http://x\"; f()"),
            "letu=\"http://x\";f()"
        );
        assert_eq!(squashed("let c = '/'; f(a, b)"), "letc='/';f(a,b)");
    }

    #[test]
    fn test_files_are_recognised_by_name_or_directory() {
        for test in [
            "src/terminal/session_server/tests.rs",
            "src/mcp/handler/cp2_inventory_tests.rs",
            "src/a/b_test.rs",
            "src/tests/helper.rs",
            "src/feature/thing.test.ts",
        ] {
            assert!(is_test_file(Path::new(test)), "{test}");
        }
        for production in [
            "src/terminal/mod.rs",
            "src/context_repo/contest.rs",
            "src/latest.rs",
        ] {
            assert!(!is_test_file(Path::new(production)), "{production}");
        }
    }
}
