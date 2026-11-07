// SPDX-License-Identifier: MIT
//! Pure, bounded bootstrap data. Never host-protocol framing.
use serde::{Deserialize, Serialize};

/// The helper's refusal when the login's private root was enrolled under another
/// CodeSense identity: another profile, or this one before its identity changed.
/// The app shows this line as it is.
pub(crate) const OTHER_PROFILE: &str =
    "this machine login is already set up for a different CodeSense identity";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Marker {
    pub canonical_home: String,
    pub enrollment_id: String,
    pub owner_id: serde_json::Value,
    pub provider: String,
    pub uid: u32,
    pub version: u32,
}

impl Marker {
    pub fn machine_owner(&self) -> Result<crate::machine_owner::MachineOwner, String> {
        crate::machine_owner::MachineOwner::from_marker(
            self.version,
            &self.provider,
            &self.owner_id,
        )
    }
}

pub(super) fn encode(marker: &Marker) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(marker).map_err(|_| "private account unavailable")?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn parse(bytes: &[u8]) -> Result<Option<Marker>, String> {
    if bytes.len() > 4096 {
        return Err("private account unavailable".into());
    }
    let value: Option<Marker> =
        serde_json::from_slice(bytes).map_err(|_| "private account unavailable")?;
    if let Some(marker) = &value {
        if marker.machine_owner().is_err()
            || marker.canonical_home.len() > 2048
            || !marker.canonical_home.starts_with('/')
            || marker.enrollment_id.len() != 36
            || !marker.enrollment_id.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
                }
            })
            || marker.enrollment_id.bytes().all(|b| b == b'0' || b == b'-')
            || std::path::Path::new(&marker.canonical_home)
                .components()
                .any(|c| {
                    !matches!(
                        c,
                        std::path::Component::RootDir | std::path::Component::Normal(_)
                    )
                })
            || std::path::Path::new(&marker.canonical_home)
                .components()
                .collect::<std::path::PathBuf>()
                .as_os_str()
                != std::ffi::OsStr::new(&marker.canonical_home)
            || encode(marker)? != bytes
        {
            return Err("private account unavailable".into());
        }
    }
    Ok(value)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallData {
    pub version: String,
    pub binary_sha: String,
    pub files: Vec<InstallFile>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallFile {
    pub rel: String,
    pub sha256: String,
    pub len: u64,
}

pub(super) fn encode_install(data: &InstallData) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(data).map_err(|_| "private install unavailable")?;
    bytes.push(b'\n');
    if bytes.len() > 8192 {
        return Err("private install unavailable".into());
    }
    Ok(bytes)
}
#[cfg(target_os = "linux")]
pub(super) fn parse_install(bytes: &[u8]) -> Result<InstallData, String> {
    if bytes.len() > 8192 {
        return Err("private install unavailable".into());
    }
    serde_json::from_slice(bytes).map_err(|_| "private install unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_marker_rejects_noncanonical_or_non_uuid_enrollment() {
        let mut marker = Marker {
            canonical_home: "/home/dev".into(),
            enrollment_id: "12e45678-e89b-42d3-a456-426614174000".into(),
            owner_id: 41.into(),
            provider: "github".into(),
            uid: 1000,
            version: 1,
        };
        assert!(parse(&encode(&marker).unwrap()).is_ok());
        for invalid in [
            "00000000-0000-0000-0000-000000000000",
            "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
            "12E45678-e89b-42d3-a456-426614174000",
        ] {
            marker.enrollment_id = invalid.into();
            assert!(parse(&encode(&marker).unwrap()).is_err());
        }
    }
}
