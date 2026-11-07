// SPDX-License-Identifier: MIT
//! The wake store on disk: JSON data only, never host framing, and no
//! transport access (the session-server framing guard pins both).
//!
//! Two files under `<root>/wake/`, each written durably (fsync of the file,
//! and of the directory after a rename):
//! - `meta.json`: the small, changing part (queue, timers, counters, evicted
//!   keys), rewritten whole on change;
//! - `outcomes.jsonl`: one line per final outcome, appended. Idempotency
//!   records and the capped log are rebuilt from it on open, and it is
//!   compacted to what is still retained once it outgrows that.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

const META: &str = "meta.json";
const OUTCOMES: &str = "outcomes.jsonl";

pub(crate) struct Store {
    dir: PathBuf,
    /// Lines in `outcomes.jsonl`, for compaction.
    pub(crate) lines: usize,
    /// Fail every write (tests of a store that cannot be written).
    #[cfg(test)]
    pub(crate) fail_writes: bool,
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

impl Store {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            lines: 0,
            #[cfg(test)]
            fail_writes: false,
        }
    }

    /// The meta document, if one is readable, and every readable outcome line.
    pub(crate) fn load<M: DeserializeOwned, L: DeserializeOwned>(&mut self) -> (Option<M>, Vec<L>) {
        let meta =
            std::fs::read(self.dir.join(META))
                .ok()
                .and_then(|bytes| match serde_json::from_slice(&bytes) {
                    Ok(meta) => Some(meta),
                    Err(error) => {
                        log::warn!("session wake meta unreadable, starting empty: {error}");
                        None
                    }
                });
        let lines: Vec<L> = std::fs::read_to_string(self.dir.join(OUTCOMES))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        self.lines = lines.len();
        (meta, lines)
    }

    pub(crate) fn save_meta<M: Serialize>(&self, meta: &M) -> std::io::Result<()> {
        #[cfg(test)]
        if self.fail_writes {
            return Err(std::io::Error::other("store write refused (test)"));
        }
        let bytes = serde_json::to_vec(meta).map_err(std::io::Error::other)?;
        self.replace(META, &bytes)
    }

    pub(crate) fn append<L: Serialize>(&mut self, line: &L) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut bytes = serde_json::to_vec(line).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(OUTCOMES))?;
        file.write_all(&bytes)?;
        file.sync_data()?;
        self.lines += 1;
        Ok(())
    }

    /// Rewrite the outcome lines as exactly `lines`.
    pub(crate) fn compact<L: Serialize>(&mut self, lines: &[L]) -> std::io::Result<()> {
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend(serde_json::to_vec(line).map_err(std::io::Error::other)?);
            bytes.push(b'\n');
        }
        self.replace(OUTCOMES, &bytes)?;
        self.lines = lines.len();
        Ok(())
    }

    fn replace(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(name);
        let temp = path.with_extension("tmp");
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        sync_dir(&self.dir)
    }
}
