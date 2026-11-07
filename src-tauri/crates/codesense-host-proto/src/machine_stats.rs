// SPDX-License-Identifier: MIT
//! Self-reported host resources. Samples are transient observations, not reservations.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineStats {
    pub load1: f64,
    pub cpu_count: u32,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub disk_free_bytes: u64,
    pub disk_total_bytes: u64,
    pub disk_path: String,
    /// Milliseconds since the Unix epoch, on the reporting server's clock.
    pub sampled_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capability, Hello};

    #[test]
    fn optional_capability_and_sample_round_trip() {
        let old: Hello = serde_json::from_str("{}").unwrap();
        assert!(!old.supports(&Capability::MachineStatsV1));
        let new: Hello =
            serde_json::from_str(r#"{"capabilities":["machine_stats_v1","future_capability"]}"#)
                .unwrap();
        assert!(new.supports(&Capability::MachineStatsV1));
        assert!(new.capabilities.contains(&Capability::Unsupported));
        let wire = r#"{"load1":3.04,"cpuCount":8,"memUsedBytes":12,"memTotalBytes":16,"diskFreeBytes":84,"diskTotalBytes":460,"diskPath":"/workspaces","sampledAt":1000}"#;
        let sample: MachineStats = serde_json::from_str(wire).unwrap();
        assert_eq!(sample.cpu_count, 8);
        assert_eq!(
            serde_json::to_value(&sample).unwrap(),
            serde_json::from_str::<serde_json::Value>(wire).unwrap()
        );
    }
}
