// SPDX-License-Identifier: MIT
//! Machine enrollment identity is distinct from a desktop data namespace.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum MachineOwner {
    LegacyGithub { id: u64 },
    Local { id: String },
}
impl MachineOwner {
    pub fn validate(&self) -> Result<(), String> {
        let valid = match self {
            Self::LegacyGithub { id } => *id != 0,
            Self::Local { id } => valid_uuid(id),
        };
        if valid {
            Ok(())
        } else {
            Err("invalid machine owner".into())
        }
    }
    pub fn marker_parts(&self) -> (u32, &'static str, serde_json::Value) {
        match self {
            Self::LegacyGithub { id } => (1, "github", (*id).into()),
            Self::Local { id } => (2, "local", id.clone().into()),
        }
    }
    pub fn from_marker(
        version: u32,
        provider: &str,
        id: &serde_json::Value,
    ) -> Result<Self, String> {
        let owner = match (version, provider) {
            (1, "github") => Self::LegacyGithub {
                id: id.as_u64().ok_or("invalid machine owner")?,
            },
            (2, "local") => Self::Local {
                id: id.as_str().ok_or("invalid machine owner")?.into(),
            },
            _ => return Err("unsupported machine owner".into()),
        };
        owner.validate()?;
        Ok(owner)
    }
}
fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && id.bytes().any(|b| b != b'0' && b != b'-')
}

/// The namespace remains a session fence; it cannot manufacture enrollment identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapOwner {
    pub machine_owner: MachineOwner,
    pub principal: u64,
}
impl From<u64> for BootstrapOwner {
    fn from(id: u64) -> Self {
        Self {
            machine_owner: MachineOwner::LegacyGithub { id },
            principal: id,
        }
    }
}
impl std::fmt::Display for BootstrapOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.machine_owner {
            MachineOwner::LegacyGithub { id } if *id == self.principal => write!(f, "{id}"),
            MachineOwner::LegacyGithub { id } => write!(f, "github:{id}:{}", self.principal),
            MachineOwner::Local { id } => write!(f, "local:{id}:{}", self.principal),
        }
    }
}
impl std::str::FromStr for BootstrapOwner {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let result = if let Ok(id) = s.parse::<u64>() {
            Self::from(id)
        } else {
            let parts: Vec<_> = s.split(':').collect();
            if parts.len() != 3 {
                return Err("invalid machine authority".into());
            }
            let machine_owner = match parts[0] {
                "github" => MachineOwner::LegacyGithub {
                    id: parts[1].parse().map_err(|_| "invalid machine owner")?,
                },
                "local" => MachineOwner::Local {
                    id: parts[1].into(),
                },
                _ => return Err("invalid machine authority".into()),
            };
            Self {
                machine_owner,
                principal: parts[2].parse().map_err(|_| "invalid principal")?,
            }
        };
        result.machine_owner.validate()?;
        if result.principal == 0 {
            return Err("invalid principal".into());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marker_version_and_id_type_are_strict_and_round_trip() {
        let uuid = "12e45678-e89b-42d3-a456-426614174000";
        let local = MachineOwner::Local { id: uuid.into() };
        let (version, provider, id) = local.marker_parts();
        assert_eq!(
            MachineOwner::from_marker(version, provider, &id).unwrap(),
            local
        );
        assert!(MachineOwner::from_marker(1, "github", &serde_json::json!("41")).is_err());
        assert!(MachineOwner::from_marker(2, "local", &serde_json::json!(41)).is_err());
        assert!(MachineOwner::from_marker(1, "local", &id).is_err());
        assert!(MachineOwner::from_marker(2, "github", &serde_json::json!(41)).is_err());
        assert!(MachineOwner::from_marker(3, "local", &id).is_err());
        assert!(MachineOwner::Local {
            id: "00000000-0000-0000-0000-000000000000".into()
        }
        .validate()
        .is_err());
        let authority = BootstrapOwner {
            machine_owner: local,
            principal: 999999999999,
        };
        assert_eq!(
            authority.to_string().parse::<BootstrapOwner>().unwrap(),
            authority
        );
        assert_eq!(BootstrapOwner::from(41).to_string(), "41");
    }
}
