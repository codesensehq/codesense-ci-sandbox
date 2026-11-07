// SPDX-License-Identifier: MIT
use std::path::Path;

pub fn run_sync_wave_iteration(root: &Path) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("build leak-gate runtime: {error}"))?;
    runtime.block_on(crate::content_engine::sync::wave::leak_gate_iteration(root))
}
