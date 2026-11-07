// SPDX-License-Identifier: MIT
//! The seeded cursor-addressed TUI fixture.
//!
//! ```text
//! # write the checked-in fixture (no PTY involved; byte-identical for a seed)
//! cargo run -p codesense-pty-core --example tui_fixture --features tui-fixture -- \
//!   --emit-ptylog tests/fixtures/terminal/tui-fixture.ptylog --seed 38
//! # run live on this tty: redraws on SIGWINCH, exits on SIGTERM/SIGINT/SIGHUP
//! cargo run -p codesense-pty-core --example tui_fixture --features tui-fixture -- [--seed 38]
//! ```

use std::time::Duration;

use codesense_pty_core::tui_fixture;

/// How often live mode draws a frame.
const LIVE_TICK: Duration = Duration::from_millis(30);

fn main() -> Result<(), String> {
    let mut raw = std::env::args().skip(1);
    let mut emit = None;
    let mut seed = 38u64;
    while let Some(flag) = raw.next() {
        let value = raw.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--emit-ptylog" => emit = Some(value),
            "--seed" => seed = value.parse().map_err(|e| format!("seed {value}: {e}"))?,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    match emit {
        Some(path) => std::fs::write(&path, tui_fixture::ptylog(seed))
            .map_err(|e| format!("write {path}: {e}")),
        None => tui_fixture::run_live(seed, LIVE_TICK).map_err(|e| e.to_string()),
    }
}
