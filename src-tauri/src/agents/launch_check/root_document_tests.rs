// SPDX-License-Identifier: MIT
#![cfg(test)]
use super::*;

#[test]
fn shadowing_document_lookup_matches_nearest_git_root_and_both_native_names() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    let cwd = root.path().join("project/src");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(root.path().join(".hermes.md"), "ancestor").unwrap();
    let warnings = root_document_warnings(AgentCli::Hermes, &cwd);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains(".hermes.md"));
    std::fs::remove_file(root.path().join(".hermes.md")).unwrap();
    std::fs::write(root.path().join("HERMES.md"), "uppercase").unwrap();
    let warnings = root_document_warnings(AgentCli::Hermes, root.path());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("HERMES.md"));
    std::fs::write(cwd.join(".hermes.md"), "nearest").unwrap();
    let warnings = root_document_warnings(AgentCli::Hermes, &cwd);
    assert_eq!(
        warnings.len(),
        1,
        "only the consumer's first chosen document shadows the injected root"
    );
    assert!(warnings[0].message.contains(".hermes.md"));
}

#[test]
fn shadowing_document_lookup_stays_in_cwd_outside_git_and_stops_at_nested_git_file() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("outside/subdir");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(root.path().join(".hermes.md"), "outside repo").unwrap();
    assert!(root_document_warnings(AgentCli::Hermes, &cwd).is_empty());
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(cwd.parent().unwrap().join(".git"), "gitdir: elsewhere").unwrap();
    assert!(
        root_document_warnings(AgentCli::Hermes, &cwd).is_empty(),
        "a .git file is a boundary even without a discoverable repository"
    );
    std::fs::write(cwd.join("HERMES.md"), "local").unwrap();
    assert_eq!(root_document_warnings(AgentCli::Hermes, &cwd).len(), 1);
}
