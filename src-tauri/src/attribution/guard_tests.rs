// SPDX-License-Identifier: MIT
//! Architectural ownership checks for capture preparation.
use proc_macro2::{Delimiter, TokenStream, TokenTree};
use std::path::Path;

fn function_body(source: &str, name: &str) -> Option<String> {
    fn find(tokens: TokenStream, name: &str) -> Option<String> {
        let tokens: Vec<_> = tokens.into_iter().collect();
        for (at, token) in tokens.iter().enumerate() {
            if matches!(token, TokenTree::Ident(id) if id == "fn")
                && matches!(tokens.get(at + 1), Some(TokenTree::Ident(id)) if id == name)
            {
                if let Some(TokenTree::Group(body)) = tokens[at + 2..].iter().find(|token| matches!(token, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace)) {
                    return Some(body.stream().to_string().replace(' ', ""));
                }
            }
            if let TokenTree::Group(group) = token {
                if let Some(body) = find(group.stream(), name) {
                    return Some(body);
                }
            }
        }
        None
    }
    find(source.parse().unwrap(), name)
}

#[test]
fn r4_every_managed_projector_uses_the_fresh_capture_owner() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let body = |file: &str, name: &str| {
        function_body(&std::fs::read_to_string(src.join(file)).unwrap(), name).unwrap()
    };
    for name in ["project_and_apply_for_cli"] {
        assert!(body("agents/launch.rs", name).contains("apply_generated("));
    }
    for (file, name) in [
        ("commands/mcp_workspace.rs", "project_managed_scope"),
        (
            "commands/dashboard_launch.rs",
            "provision_headless_scope_for_cli",
        ),
    ] {
        assert!(
            body(file, name).contains("project_and_apply_for_cli("),
            "{name}"
        );
    }
    assert!(
        body("commands/mcp_authoring.rs", "project_agent_scope").contains("refresh_attribution(")
    );
    for name in ["apply_generated", "refresh_attribution"] {
        assert!(
            body("agents/launch.rs", name)
                .contains("crate::attribution::launch::prepare_for_host("),
            "{name}"
        );
    }
    for key in ["TRACE2_EVENT", "TRACE2_EVENT_NESTING"] {
        let assignment = format!("\"GIT_{key}\".into()");
        let owners: Vec<_> = walkdir::WalkDir::new(&src)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "rs")
            })
            .filter(|entry| {
                std::fs::read_to_string(entry.path())
                    .unwrap()
                    .contains(&assignment)
            })
            .map(|entry| entry.into_path())
            .collect();
        assert_eq!(owners, [src.join("attribution/launch.rs")]);
        assert!(body("attribution/launch.rs", "plan_for").contains(&assignment));
    }
}

#[test]
fn git_ai_commands_have_one_constructor() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/attribution");
    for entry in walkdir::WalkDir::new(&src)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.path().extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(entry.path()).unwrap();
        let tests_start = [
            "#[cfg(test)]\nmod tests",
            "#[cfg(test)]\npub(crate) mod tests",
        ]
        .iter()
        .filter_map(|marker| source.find(marker))
        .min()
        .unwrap_or(source.len());
        let production = &source[..tests_start];
        let constructors: Vec<_> = production
            .lines()
            .filter(|line| line.contains("Command::new("))
            .map(str::trim)
            .collect();
        let expected: &[&str] = match entry.file_name().to_str().unwrap() {
            "detect.rs" => &["let mut command = Command::new(path);"],
            "git_ai.rs" => &["let mut command = Command::new(&self.git);"],
            "testing.rs" | "guard_tests.rs" | "install_tests.rs" | "notes_tests.rs" => continue,
            _ => &[],
        };
        assert_eq!(
            constructors,
            expected,
            "{} must use git_ai_command for git-ai",
            entry.path().display()
        );
    }
    // Also catch direct named git-ai spawns introduced outside this module.
    for entry in walkdir::WalkDir::new(src.parent().unwrap())
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.path().extension().is_none_or(|e| e != "rs") || entry.path().starts_with(&src) {
            continue;
        }
        let source = std::fs::read_to_string(entry.path()).unwrap();
        let tokens = source
            .parse::<TokenStream>()
            .unwrap()
            .to_string()
            .replace(' ', "");
        for constructor in tokens.split("Command::new(").skip(1) {
            let argument = constructor.split(')').next().unwrap();
            assert!(
                !argument.contains("git-ai") && !argument.contains("git_ai"),
                "{} must use git_ai_command: {argument}",
                entry.path().display()
            );
        }
    }
    let source = std::fs::read_to_string(src.join("detect.rs")).unwrap();
    assert!(function_body(&source, "git_ai_command")
        .unwrap()
        .contains("arg0(BINARY)"));
}
