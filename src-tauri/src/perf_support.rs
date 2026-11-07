// SPDX-License-Identifier: MIT
//! Fixture builders the criterion perf benches reach through `codesense_lib`.
//!
//! Gated behind the `bench-support` feature so neither these builders nor their
//! `tempfile` scaffolding enter a default `cargo build`/`--release`. Each fixture
//! splits construction (the benchmark's untimed setup) from the single operation
//! under test (the timed routine); the benches never build fixtures inside the
//! measured region.

use std::path::PathBuf;

use codesense_pty_core::spawn::{HostEnvironment, SpawnIntent};
use tempfile::TempDir;

pub mod live_hub;
pub mod scale_profile;

/// Sync-wave reconcile (`Wave::run`). Build in the bench setup closure; the
/// timed routine is [`SyncWaveFixture::run`]. `repo_count` is the fan-out axis.
pub struct SyncWaveFixture {
    inner: crate::content_engine::sync::wave::BenchWaveFixture,
    // Kept alive until `run` returns — the reconcile reads this tree.
    _home: TempDir,
}

impl SyncWaveFixture {
    pub fn build(repo_count: usize) -> Result<Self, String> {
        let home = tempfile::tempdir().map_err(|error| error.to_string())?;
        let inner =
            crate::content_engine::sync::wave::BenchWaveFixture::build(home.path(), repo_count)?;
        Ok(Self { inner, _home: home })
    }

    pub async fn run(self) -> Result<(), String> {
        self.inner.run().await
    }
}

/// Pipeline tree scan (`scan_pipeline_tree_impl_with_cache`). `story_count`
/// deterministic story files sit under one epic dir; the scan is inline-parse
/// (`cache: None`), so every file is walked and parsed.
pub struct ScanFixture {
    _tmp: TempDir,
    team_content_path: PathBuf,
    headings: Vec<String>,
    meta_cfg: crate::commands::pipeline_scanner::MetadataValidityConfig,
}

impl ScanFixture {
    pub fn build(story_count: usize) -> Result<Self, String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let content = tmp.path().to_path_buf();
        let epic = content.join("epic");
        std::fs::create_dir_all(&epic).map_err(|error| error.to_string())?;
        std::fs::write(
            epic.join("README.md"),
            "---\ntype: sub-epic\n---\n# Epic\n\n## Purpose\n\nfixture epic\n",
        )
        .map_err(|error| error.to_string())?;
        for index in 0..story_count {
            std::fs::write(
                epic.join(format!("S-{index}-story.md")),
                format!(
                    "---\ntype: story\nstory_id: S-{index}\n---\n# Story {index}\n\n## Purpose\n\nbody\n"
                ),
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(Self {
            _tmp: tmp,
            team_content_path: content,
            headings: crate::commands::pipeline_scanner::default_summary_headings(),
            meta_cfg: crate::commands::pipeline_scanner::MetadataValidityConfig::default(),
        })
    }

    pub fn run(&self) -> Result<(), String> {
        crate::commands::pipeline_scanner::scan_pipeline_tree_impl_with_cache(
            &self.team_content_path,
            "epic",
            "",
            "README.md",
            &self.headings,
            None,
            &self.meta_cfg,
        )
        .map(|_| ())
    }
}

/// PTY spawn-setup — resolving a spawn intent on its host: argv, the managed
/// environment and the `ZDOTDIR` RC wrappers. The real fork/exec is excluded:
/// it is OS work, not ours. `env_remove_count` is the strip-list axis.
pub struct PtySpawnSetupFixture {
    _tmp: TempDir,
    environment: HostEnvironment,
    intent: SpawnIntent,
}

impl PtySpawnSetupFixture {
    pub fn build(env_remove_count: usize) -> Result<Self, String> {
        let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let home = tmp.path().join("codesense");
        let resources = tmp.path().join("resources");
        std::fs::create_dir_all(home.join("bin")).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(resources.join("shell-integration"))
            .map_err(|error| error.to_string())?;
        for file in [
            "shell-integration/codesense-shell-init.zsh",
            "starship",
            "starship-compact.toml",
        ] {
            std::fs::write(resources.join(file), "").map_err(|error| error.to_string())?;
        }
        let environment = HostEnvironment::with_vars(
            Some(home),
            Some(resources),
            [
                ("SHELL", "/bin/zsh".to_string()),
                ("PATH", "/usr/bin:/bin".to_string()),
                ("HOME", tmp.path().join("orig").display().to_string()),
            ]
            .map(|(key, value)| (key.to_string(), value)),
        );
        let intent = SpawnIntent {
            remote_launch: Default::default(),
            command: None,
            argv: None,
            shell: None,
            cwd: tmp.path().display().to_string(),
            cols: 120,
            rows: 40,
            shell_integration: true,
            starship_preset: Some("compact".to_string()),
            api_port: Some(8737),
            env_remove: (0..env_remove_count)
                .map(|index| format!("BENCH_STRIP_VAR_{index}"))
                .collect(),
            env: Vec::new(),
            agent_env: Vec::new(),
            login_path: None,
        };
        Ok(Self {
            _tmp: tmp,
            environment,
            intent,
        })
    }

    pub fn run(&self) {
        let _ = self.intent.resolve("bench-session", &self.environment);
    }
}
