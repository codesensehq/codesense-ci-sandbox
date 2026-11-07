// SPDX-License-Identifier: MIT
fn main() {
    configure_beta_channel();
    configure_site_contract_tests();
    println!("cargo:rerun-if-env-changed=GITHUB_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=GITLAB_CLIENT_ID");

    // Expose the rustc target triple to runtime code so
    // `commands::deps::ensure_bridge_symlink` can reconstruct Tauri's
    // dev-mode sidecar filename (`<name>-<target-triple>`). `TARGET` is
    // set by Cargo for build scripts; re-emit as a rustc-env var so the
    // library can read it via `env!("TARGET_TRIPLE")`.
    let target = std::env::var("TARGET").expect("TARGET env var must be set by Cargo");
    println!("cargo:rustc-env=TARGET_TRIPLE={}", target);

    // Keep direct Cargo builds bootstrapping the same generated manifests.
    // Each generator owns its inputs; unrelated Rust edits must not rerun Node.
    run_docs_generator("../scripts/generate-recipes-manifest.mjs");
    println!("cargo:rerun-if-env-changed=UPDATE_DOCS_SCHEMAS");
    // Sealed MCP exports are generated/checked by pnpm gen:docs, not Cargo.
    // Their seal includes schema Rust files, including ordinary domain edits.
    run_docs_generator("../scripts/generate-primitives-reference.mjs");
    run_docs_generator("../scripts/generate-yaml-schemas.mjs");
    run_docs_generator("../scripts/generate-ui-mcp-crossref.mjs");

    run_docs_generator_with_args(
        "../scripts/build-codesense-docs.mjs",
        &["resources/codesense-docs.tar.gz"],
    );
    run_docs_generator_with_args(
        "../scripts/build-sample-source-repo.mjs",
        &["resources/sample-source-repo.tar.gz"],
    );

    tauri_build::build()
}

/// The build-time update channel. `CODESENSE_CHANNEL` is `internal`
/// (the default, no license module), `beta` or `beta-staging`. The beta channels
/// must carry `CODESENSE_RELEASED_AT`, or the build fails.
fn configure_beta_channel() {
    println!(
        "cargo::rustc-check-cfg=cfg(cs_channel, values(\"internal\", \"beta\", \"beta-staging\"))"
    );
    println!("cargo:rerun-if-env-changed=CODESENSE_CHANNEL");
    println!("cargo:rerun-if-env-changed=CODESENSE_RELEASED_AT");

    let channel = std::env::var("CODESENSE_CHANNEL").unwrap_or_else(|_| "internal".to_string());
    let manifest = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    // Only production hosts are written in source. The staging channel names its hosts in its
    // own channel config, which only this arm reads.
    match channel.as_str() {
        "internal" | "beta" => println!("cargo:rustc-env=CODESENSE_PUBLIC_HOST=codesense.ai"),
        "beta-staging" => {
            let config = read_json(&manifest.join("tauri.beta-staging.conf.json"));
            for (env, key) in [
                ("CODESENSE_PUBLIC_HOST", "publicHost"),
                ("CODESENSE_DOWNLOAD_HOST", "downloadHost"),
            ] {
                let host = config
                    .pointer(&format!("/plugins/codesense-channel/{key}"))
                    .and_then(|h| h.as_str())
                    // A bare host name, so the value cannot carry a path or another directive.
                    .filter(|h| {
                        !h.is_empty()
                            && h.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "tauri.beta-staging.conf.json has no host name at plugins.codesense-channel.{key}"
                        )
                    });
                println!("cargo:rustc-env={env}={host}");
            }
        }
        other => panic!("CODESENSE_CHANNEL={other:?} is not one of internal, beta, beta-staging"),
    }
    println!("cargo:rustc-cfg=cs_channel=\"{channel}\"");
    if channel == "internal" {
        return;
    }

    // The app compares the updater key it was built with against these hashes, so no
    // config file's text (and no other channel's key) has to be embedded in the binary. The
    // only updater key a beta binary carries is the one Tauri embeds from the merged config.
    let channel_conf = match channel.as_str() {
        "beta-staging" => "tauri.beta-staging.conf.json",
        _ => "tauri.beta-prod.conf.json",
    };
    // Literal paths keep the source guard closed to unresolved rerun interpolation.
    println!("cargo:rerun-if-changed=tauri.conf.json");
    match channel.as_str() {
        "beta-staging" => println!("cargo:rerun-if-changed=tauri.beta-staging.conf.json"),
        _ => println!("cargo:rerun-if-changed=tauri.beta-prod.conf.json"),
    }
    for (env, file) in [
        ("CODESENSE_INTERNAL_UPDATER_KEY_SHA256", "tauri.conf.json"),
        ("CODESENSE_CHANNEL_UPDATER_KEY_SHA256", channel_conf),
    ] {
        let path = manifest.join(file);
        let config = read_json(&path);
        let key = config
            .pointer("/plugins/updater/pubkey")
            .and_then(|k| k.as_str())
            .unwrap_or_else(|| panic!("{} has no plugins.updater.pubkey", path.display()));
        let digest = <sha2::Sha256 as sha2::Digest>::digest(key.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        println!("cargo:rustc-env={env}={hex}");
    }

    let released_at = std::env::var("CODESENSE_RELEASED_AT").unwrap_or_default();
    assert!(
        released_at.parse::<u64>().is_ok(),
        "CODESENSE_CHANNEL={channel} needs CODESENSE_RELEASED_AT set to a unix time in seconds"
    );
    println!("cargo:rustc-env=CODESENSE_RELEASED_AT={released_at}");
}

/// A few contract tests compile only when their inputs are present in the checkout
/// (`cs_site` and the Claude settings guard); a checkout without them skips those tests.
fn configure_site_contract_tests() {
    println!("cargo::rustc-check-cfg=cfg(cs_site)");
    if std::path::Path::new("../site").is_dir() {
        // Reruns when site/ goes away, so a stale cfg never includes a missing file.
        println!("cargo:rerun-if-changed=../site/package.json");
        println!("cargo:rustc-cfg=cs_site");
    }
    // The committed Claude settings drift guard needs the file, which the export also leaves out.
    println!("cargo::rustc-check-cfg=cfg(cs_claude_settings)");
    if std::path::Path::new("../.claude/settings.json").is_file() {
        println!("cargo:rerun-if-changed=../.claude/settings.json");
        println!("cargo:rustc-cfg=cs_claude_settings");
    }
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

/// Runs a docs generator during the build, optionally with a generator-specific mode.
fn run_docs_generator(script_rel: &str) {
    run_docs_generator_with_args(script_rel, &[]);
}

fn run_docs_generator_with_args(script_rel: &str, args: &[&str]) {
    let inputs = std::process::Command::new("node")
        .args([script_rel, "--list-inputs"])
        .output()
        .unwrap_or_else(|e| panic!("docs generator {script_rel} needs Node.js (node: {e})"));
    assert!(
        inputs.status.success(),
        "{script_rel} --list-inputs failed: {}",
        String::from_utf8_lossy(&inputs.stderr)
    );
    for input in String::from_utf8_lossy(&inputs.stdout).lines() {
        println!("cargo:rerun-if-changed={input}");
    }
    let status = std::process::Command::new("node")
        .arg(script_rel)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("docs generator {script_rel} needs Node.js (node: {e})"));
    assert!(
        status.success(),
        "docs generator {script_rel} failed ({status})"
    );
}
