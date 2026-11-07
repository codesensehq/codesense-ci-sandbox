// SPDX-License-Identifier: MIT
//! A guard that the retired pack system leaves no names behind.

use std::path::Path;

/// Splits an identifier into lowercase words across `snake_case` and `camelCase`.
fn identifier_words(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            continue;
        }
        let prev = word.chars().last();
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase() || p.is_ascii_digit() || (p.is_uppercase() && next_lower)
            });
        if boundary {
            words.push(std::mem::take(&mut word));
        }
        word.push(c);
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.into_iter().map(|w| w.to_lowercase()).collect()
}

/// The guard flags a name only when one of its words is exactly `pack`/`packs`, so
/// `package`, `unpack`, `packed` and a lone `pack` local pass, and pack names do not.
#[test]
fn the_guard_flags_pack_names_but_not_package_words() {
    let flagged = |name: &str| {
        let words = identifier_words(name);
        words.len() > 1 && words.iter().any(|w| w == "pack" || w == "packs")
    };
    for ok in [
        "package",
        "packageJson",
        "PACKAGE_NAME",
        "package_manager",
        "unpack",
        "packed",
        "repack_index",
        "pack",
    ] {
        assert!(!flagged(ok), "{ok} is not a pack name");
    }
    for bad in [
        "packId",
        "PackInstall",
        "installed_packs",
        "PACKS_CHANGED_EVENT",
        "usePackInstall",
    ] {
        assert!(flagged(bad), "{bad} names a pack");
    }
}

/// The pack system is gone: no identifier in the app or the frontend is a pack name, and
/// no `agent-packs` path or `agent_packs` resource is left anywhere. The allowed names
/// below are other things that happen to be called packs: avatar face packs, layout shelf
/// packing, Excalidraw blobs, git pack offsets and the bundled wireframe starter library.
#[test]
fn no_pack_identifiers_or_agent_packs_paths_survive() {
    const NOT_THE_PACK_SYSTEM: &[&str] = &[
        "PackScope",
        "defaultPack",
        "setDefaultPack",
        "pack_dir",
        "packName",
        "BUILTIN_FUNNY_PACK",
        "BUILTIN_AVATAR_PACKS",
        "AvatarPacks",
        "AvatarPackView",
        "AvatarPacksView",
        "AvatarPackScope",
        "AvatarPacksState",
        "avatarPacksList",
        "avatarDefaultPackSet",
        "avatar_packs_list",
        "avatarPacksListForTeam",
        "avatar_packs_list_for_team",
        "useTeamAvatarPacks",
        "avatar_default_pack_set",
        "avatarPackShareWithTeam",
        "avatar_pack_share_with_team",
        "sharePack",
        "share_pack",
        "copy_pack_tree",
        "useAvatarPacks",
        "useAvatarPacksStore",
        "read_default_pack",
        "write_default_pack",
        "packBlob",
        "shelfPackBlock",
        "runShelfPackOn",
        "pack_offset",
        "STARTER_PACK_RESOURCE",
        "STARTER_PACK_MARKER",
        "seed_starter_pack_best_effort",
        "pack_path",
    ];
    // Names allowed in one file only. The places that refuse a retired name have to name
    // it: the absence test for the retired MCP tools, and the bridge refusing a pack named
    // on an open (and its test). The avatar list's `default_pack` field is allowed only
    // where it is put on the wire, mocked or asserted on.
    const RETIRED_NAMES: &[(&str, &[&str])] = &[
        (
            "src-tauri/src/mcp/handler.rs",
            &[
                "list_installable_packs",
                "install_pack",
                "uninstall_pack",
                "set_pack_enabled",
                // The avatar list's default face pack.
                "default_pack",
            ],
        ),
        // The avatar list's default face pack, on the wire and in its mock.
        ("src/tauri/commands.ts", &["default_pack"]),
        ("src/app/mock/handlers/agents.ts", &["default_pack"]),
        // The mock's second team draws its agents from its own avatar pack.
        (
            "src/app/mock/handlers/agents-team.ts",
            &["default_pack", "SECOND_TEAM_PACK_ID"],
        ),
        (
            "src/app/mock/handlers/agents-team.test.ts",
            &["default_pack"],
        ),
        (
            "src/features/viewer/components/agents-list.test.tsx",
            &["default_pack"],
        ),
        (
            "src/features/dashboard/components/html-bridge.ts",
            &["packId"],
        ),
        (
            "src/features/dashboard/components/html-bridge.test.ts",
            &["packId"],
        ),
    ];
    // Files whose job is to name what is gone: this guard, and the routine boundary guard
    // that bans the deleted automation modules by name.
    const EXEMPT: &[&str] = &[
        "src-tauri/src/agents/retired_names_guard.rs",
        "src-tauri/src/routines/boundary_guard.rs",
    ];
    // The seams proving a stale team lock file is ignored have to write one.
    const STALE_LOCK_SEAMS: &[&str] = &[
        "src-tauri/src/agents/resolve/tests.rs",
        "src-tauri/src/content_engine/sync/wave.rs",
    ];

    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let rel = |p: &Path| {
        p.strip_prefix(repo)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    // Only tracked files count, so nothing untracked on a developer's disk decides it.
    let tracked: Vec<std::path::PathBuf> = {
        let out = std::process::Command::new("git")
            .args([
                "ls-files",
                "-z",
                "--",
                "src",
                "src-tauri/src",
                "site/src",
                "site/scripts",
                "codesense-docs",
                "scripts",
            ])
            .current_dir(repo)
            .output()
            .expect("git ls-files runs");
        // A tree without `.git` (a bare export) has no tracked set to check: fail and say so.
        assert!(
            out.status.success(),
            "git ls-files failed in {}: this guard needs a git checkout of the repo \
             (run `git init && git add . && git commit` in an exported tree): {}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        out.stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| repo.join(String::from_utf8_lossy(p).as_ref()))
            .filter(|p| p.is_file())
            .collect()
    };
    let under = |root: &'static str| {
        let root = repo.join(root);
        tracked
            .iter()
            .filter(move |p| p.starts_with(&root))
            .cloned()
    };
    let ident = regex::Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").unwrap();
    let descriptor_dir = regex::Regex::new(r"\bagent_packs\b").unwrap();
    let mut offenders = Vec::new();

    for path in under("src").chain(under("src-tauri/src")) {
        let is_code = path
            .extension()
            .is_some_and(|e| e == "rs" || e == "ts" || e == "tsx");
        let file = rel(&path);
        if !is_code || EXEMPT.contains(&file.as_str()) || file.contains("avatar") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
                continue;
            }
            let code = line.split("//").next().unwrap_or("");
            for m in ident.find_iter(code) {
                let name = m.as_str();
                let words = identifier_words(name);
                let names_a_pack =
                    words.len() > 1 && words.iter().any(|w| w == "pack" || w == "packs");
                let allowed = NOT_THE_PACK_SYSTEM.contains(&name)
                    || RETIRED_NAMES
                        .iter()
                        .any(|(f, names)| file == *f && names.contains(&name));
                if names_a_pack && !allowed {
                    offenders.push(format!("{file}:{}: {name}", n + 1));
                }
            }
        }
    }

    for root in [
        "src",
        "src-tauri/src",
        "site/src",
        "site/scripts",
        "codesense-docs",
        "scripts",
    ] {
        for path in under(root) {
            let file = rel(&path);
            if EXEMPT.contains(&file.as_str()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (n, line) in text.lines().enumerate() {
                let stale_lock_seam =
                    STALE_LOCK_SEAMS.contains(&file.as_str()) && line.contains("agent-packs.lock");
                if (line.contains("agent-packs") && !stale_lock_seam)
                    || descriptor_dir.is_match(line)
                {
                    offenders.push(format!("{file}:{}: agent-packs path", n + 1));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "pack system leftovers:\n{}",
        offenders.join("\n")
    );
}
