// SPDX-License-Identifier: MIT
//! Guards on how CodeSense reaches other machines: through the system `ssh`
//! and the `attach --stdio` relay's own stdio — never a port forward, never
//! an SSH library.

use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Every Rust source that builds an `ssh` command line, without its tests
/// (which name the forbidden flags to check they are absent).
fn ssh_sources() -> Vec<(PathBuf, String)> {
    let root = workspace();
    let files = [
        "src-tauri/crates/codesense-session-server/src/ssh.rs",
        "src-tauri/crates/codesense-session-server/src/check.rs",
        "src-tauri/crates/codesense-session-server/src/install.rs",
        "src-tauri/crates/codesense-session-server/src/survival.rs",
        "src-tauri/crates/codesense-session-server/src/askpass.rs",
        "src-tauri/src/commands/machines/mod.rs",
        "src-tauri/src/commands/machines/registry.rs",
        "src-tauri/src/commands/machines/passphrase.rs",
    ];
    files
        .iter()
        .map(|rel| {
            let path = root.join(rel);
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
            let mut code = text
                .split("#[cfg(test)]")
                .next()
                .unwrap_or_default()
                .to_string();
            // `ssh-add -L` lists the agent's public keys: not a forward.
            if let Some(start) = code.find("pub fn agent_keys(") {
                let end = start + code[start..].find("\n}\n").unwrap();
                code.replace_range(start..end, "");
            }
            (path, code)
        })
        .collect()
}

#[test]
fn no_connection_opens_a_forward() {
    for (path, code) in ssh_sources() {
        for forbidden in [
            "\"-L\"",
            "\"-R\"",
            "\"-D\"",
            "\"-w\"",
            "\"-W\"",
            "LocalForward",
            "RemoteForward",
            "DynamicForward",
            "StreamLocalBind",
        ] {
            assert!(!code.contains(forbidden), "{path:?} names {forbidden}");
        }
        assert!(
            !code.contains("\"StrictHostKeyChecking=no"),
            "{path:?} turns host-key checking off"
        );
    }
}

/// Whatever the user's own config forwards, no machine connection does.
#[test]
fn every_connection_clears_forwards() {
    let ssh = ssh_sources()
        .into_iter()
        .find(|(path, _)| path.ends_with("ssh.rs"))
        .unwrap()
        .1;
    let common = &ssh[ssh.find("fn common(").unwrap()..];
    let common = &common[..common.find("\n    }\n").unwrap()];
    assert!(common.contains("ClearAllForwardings=yes"));
    for option in [
        "ForwardAgent=no",
        "ForwardX11=no",
        "ForwardX11Trusted=no",
        "KnownHostsCommand=none",
        "NoHostAuthenticationForProxyCommand=no",
        "VerifyHostKeyDNS=no",
    ] {
        assert!(common.contains(option), "{option}");
    }
    let knows = &ssh[ssh.find("fn knows_option(").unwrap()..];
    assert!(knows.contains("\"-G\", \"-o\", option"));
}

/// `fn name(`'s body in `code`, up to its closing brace at method depth.
fn body<'a>(code: &'a str, name: &str) -> &'a str {
    let start = code.find(&format!("fn {name}(")).unwrap();
    let end = start + code[start..].find("\n    }\n").unwrap();
    &code[start..end]
}

/// Discovery is `ssh-keyscan`, or — for a host the user's config reaches
/// through a ProxyCommand — `ssh` recording the key in a throwaway file with
/// every way to sign in off. No connection path enables automatic trust.
#[test]
fn discovery_never_enables_automatic_trust() {
    let unsafe_tofu = ["accept", "new"].join("-");
    for (path, code) in ssh_sources() {
        let code = if path.ends_with("ssh.rs") {
            code.replace(body(&code, "scan_through_ssh"), "")
        } else {
            code
        };
        assert!(!code.contains(&unsafe_tofu), "{path:?}");
    }
    let ssh = ssh_sources()
        .into_iter()
        .find(|(path, _)| path.ends_with("ssh.rs"))
        .unwrap()
        .1;
    assert!(ssh.contains("ssh-keyscan"));
    assert!(body(&ssh, "ssh").contains("\"StrictHostKeyChecking=yes\""));
    let scan = body(&ssh, "scan_through_ssh");
    for option in [
        "UserKnownHostsFile={}\", Self::option_path(&known_hosts)",
        "\"GlobalKnownHostsFile=/dev/null\"",
        "\"ControlPath=none\"",
        "\"BatchMode=yes\"",
        "\"PubkeyAuthentication=no\"",
        "\"PasswordAuthentication=no\"",
        "\"KbdInteractiveAuthentication=no\"",
        "\"GSSAPIAuthentication=no\"",
        "\"HostbasedAuthentication=no\"",
        "\"IdentityAgent=none\"",
        "\"PKCS11Provider=none\"",
    ] {
        assert!(
            scan.contains(option),
            "the ProxyCommand scan lacks {option}"
        );
    }
    assert!(scan.contains("let known_hosts = dir.join(\"known_hosts\")"));
    assert!(!scan.contains(".pin("), "the ProxyCommand scan pins a key");
}

/// The connector adds no SSH library: `git2`'s own `libssh2` (git over SSH)
/// predates it and never carries a machine connection.
#[test]
fn no_ssh_library_is_a_dependency() {
    let root = workspace();
    for manifest in [
        "src-tauri/Cargo.toml",
        "src-tauri/crates/codesense-session-server/Cargo.toml",
        "src-tauri/crates/codesense-host-proto/Cargo.toml",
    ] {
        let text = std::fs::read_to_string(root.join(manifest)).unwrap();
        for library in [
            "russh",
            "thrussh",
            "ssh2",
            "libssh2",
            "openssh",
            "async-ssh2",
        ] {
            let named = text.lines().any(|line| {
                line.trim_start().starts_with(&format!("{library} "))
                    || line.trim_start().starts_with(&format!("{library}="))
            });
            assert!(
                !named,
                "{manifest} depends on {library}: SSH goes through the system ssh"
            );
        }
    }
}
