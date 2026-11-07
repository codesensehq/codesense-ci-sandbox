// SPDX-License-Identifier: MIT
//! One demand-driven resource sampler for all host connections.

use codesense_host_proto::machine_stats::MachineStats;
use std::{
    collections::HashSet,
    fs::File,
    os::fd::AsRawFd,
    path::Path,
    time::{Duration, Instant},
};

pub(crate) const CADENCE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct WatchSchedule {
    watchers: HashSet<u64>,
    next: Option<Instant>,
}
impl WatchSchedule {
    #[cfg(test)]
    pub(crate) fn make_due(&mut self) {
        if self.next.is_some() {
            self.next = Some(Instant::now());
        }
    }
    pub fn watch(&mut self, connection: u64, watching: bool, now: Instant) {
        if watching {
            self.watchers.insert(connection);
            if self.next.is_none() {
                self.next = Some(now + CADENCE);
            }
        } else {
            self.watchers.remove(&connection);
            if self.watchers.is_empty() {
                self.next = None;
            }
        }
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.next
    }
    pub fn take_due(&mut self, now: Instant) -> bool {
        if self.next.is_some_and(|due| due <= now) {
            self.next = Some(now + CADENCE);
            true
        } else {
            false
        }
    }
}

#[allow(
    clippy::unnecessary_cast,
    reason = "statvfs widths differ across Unix targets"
)]
pub(crate) fn sample(workspace: &Path) -> Result<MachineStats, String> {
    let mut load = [0.0; 1];
    // SAFETY: the output holds the one requested load average.
    if unsafe { libc::getloadavg(load.as_mut_ptr(), 1) } != 1 {
        return Err("machine load unavailable".into());
    }
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.refresh_cpu_list(sysinfo::CpuRefreshKind::new());
    let file = File::open(workspace).map_err(|error| error.to_string())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the open workspace descriptor and output pointer remain valid.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err("machine disk unavailable".into());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(MachineStats {
        load1: load[0],
        cpu_count: system.cpus().len() as u32,
        mem_used_bytes: system
            .total_memory()
            .saturating_sub(system.available_memory()),
        mem_total_bytes: system.total_memory(),
        disk_free_bytes: (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64),
        disk_total_bytes: (stat.f_blocks as u64).saturating_mul(stat.f_frsize as u64),
        disk_path: workspace.display().to_string(),
        sampled_at: chrono::Utc::now().timestamp_millis().max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_sampling_without_watchers_and_stops_on_last_unwatch() {
        let mut schedule = WatchSchedule::default();
        let now = std::time::Instant::now();
        assert!(schedule.deadline().is_none());
        assert!(!schedule.take_due(now));
        schedule.watch(1, true, now);
        let due = now + CADENCE;
        assert_eq!(schedule.deadline(), Some(due));
        assert!(!schedule.take_due(now));
        schedule.watch(2, true, now);
        schedule.watch(1, false, now);
        assert!(schedule.take_due(due));
        schedule.watch(2, false, due);
        assert!(schedule.deadline().is_none());
        assert!(!schedule.take_due(due + CADENCE));
        schedule.watch(3, true, due);
        assert_eq!(schedule.deadline(), Some(due + CADENCE));
    }

    #[test]
    fn sample_labels_the_workspace_volume_and_available_memory() {
        let dir = tempfile::tempdir().unwrap();
        let sample = sample(dir.path()).unwrap();
        assert!(sample.cpu_count > 0);
        assert!(sample.mem_total_bytes > 0);
        assert!(sample.mem_used_bytes <= sample.mem_total_bytes);
        assert!(sample.disk_free_bytes <= sample.disk_total_bytes);
        assert_eq!(sample.disk_path, dir.path().display().to_string());
        assert!(sample.sampled_at > 0);
    }
}
