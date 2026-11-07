// SPDX-License-Identifier: MIT
//! Backend-only target data. The transport adapter must supply observations from
//! the fixed bootstrap on its already verified SSH connection.

use super::*;

const ROOT_SUFFIX: &str = ".codesense/remote-private/v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerMarker {
    version: u32,
    provider: String,
    owner_id: serde_json::Value,
    uid: u32,
    canonical_home: String,
    enrollment_id: String,
}

/// Produced by the bootstrap adapter after checking filesystem ownership, modes,
/// link counts and symlink components. There is deliberately no public/raw-root
/// constructor or deserialization implementation for this proof.
struct VerifiedPrivateAccount {
    account: PrivateAccount,
    host_key: String,
    lease: PrivateAuthorityLease,
}

impl VerifiedPrivateAccount {
    fn from_checked_observation(
        authority: &PrivateAuthoritySnapshot,
        uid: u32,
        canonical_home: &str,
        verified_host_key: &str,
        marker_bytes: &[u8],
    ) -> Result<Self, String> {
        authority.lease().validate()?;
        if marker_bytes.len() > 4096 || verified_host_key.is_empty() {
            return Err("private account unavailable".into());
        }
        let marker: OwnerMarker =
            serde_json::from_slice(marker_bytes).map_err(|_| "private account unavailable")?;
        if codesense_session_server::machine_owner::MachineOwner::from_marker(
            marker.version,
            &marker.provider,
            &marker.owner_id,
        )? != authority.machine_owner()
            || marker.uid != uid
            || marker.canonical_home != canonical_home
        {
            return Err("private account unavailable".into());
        }
        let account = PrivateAccount {
            uid,
            canonical_home: canonical_home.into(),
            root_version: 1,
            enrollment_id: marker.enrollment_id,
        };
        account
            .validate()
            .map_err(|_| "private account unavailable")?;
        Ok(Self {
            account,
            host_key: verified_host_key.into(),
            lease: authority.lease(),
        })
    }
}

/// Immutable, non-serializable backend authority; never a frontend/MCP DTO.
pub struct PrivateAccountTarget {
    lease: PrivateAuthorityLease,
    machine_id: String,
    declaration_revision: String,
    binding_revision: u64,
    account: PrivateAccount,
    host_key: String,
    server_id: Option<String>,
}

impl PrivateAccountTarget {
    pub(crate) fn from_bootstrap(
        registry: &PrivateRegistry,
        id: &str,
        authority: &PrivateAuthoritySnapshot,
        host_key: &str,
        observed: &codesense_session_server::private_account::VerifiedAccount,
    ) -> Result<Self, String> {
        Self::from_verified(
            registry,
            id,
            VerifiedPrivateAccount::from_checked_observation(
                authority,
                observed.uid(),
                observed.canonical_home(),
                host_key,
                observed.marker_bytes(),
            )?,
        )
    }

    fn from_verified(
        registry: &PrivateRegistry,
        id: &str,
        observation: VerifiedPrivateAccount,
    ) -> Result<Self, String> {
        registry.lease.validate()?;
        observation.lease.validate()?;
        if registry.legacy.principal != observation.lease.principal() {
            return Err("private account unavailable".into());
        }
        let (_, declaration, revision) =
            registry.capture(id)?.ok_or("private machine not found")?;
        let binding = registry
            .state()?
            .bindings
            .remove(id)
            .ok_or("private setup is incomplete")?;
        validate_private_binding_declaration(&binding, &declaration, &revision)?;
        if binding.host_key.as_deref() != Some(&observation.host_key) {
            return Err("private account unavailable".into());
        }
        let (account, server_id) = match &binding.setup {
            PrivateSetup::NeedsEnrollment => return Err("private setup is incomplete".into()),
            PrivateSetup::Enrolled { account } => (account, None),
            PrivateSetup::Ready { account, server_id } => (account, Some(server_id.clone())),
        };
        if account != &observation.account {
            return Err("private account unavailable".into());
        }
        observation.lease.validate()?;
        registry.lease.validate()?;
        Ok(Self {
            lease: observation.lease,
            machine_id: id.into(),
            declaration_revision: revision,
            binding_revision: binding.revision,
            account: observation.account,
            host_key: observation.host_key,
            server_id,
        })
    }

    pub fn root(&self) -> Result<PathBuf, String> {
        self.lease.validate()?;
        Ok(Path::new(&self.account.canonical_home).join(ROOT_SUFFIX))
    }

    pub fn revalidate(&self, registry: &PrivateRegistry) -> Result<(), String> {
        self.lease.validate()?;
        if registry.legacy.principal != self.lease.principal() {
            return Err("private account unavailable".into());
        }
        let binding = registry
            .binding(&self.machine_id)?
            .ok_or("private binding changed")?;
        let (account, server_id) = match &binding.setup {
            PrivateSetup::Enrolled { account } => (account, None),
            PrivateSetup::Ready { account, server_id } => (account, Some(server_id)),
            PrivateSetup::NeedsEnrollment => return Err("private setup is incomplete".into()),
        };
        if binding.revision != self.binding_revision
            || binding.declaration_revision != self.declaration_revision
            || binding.host_key.as_deref() != Some(&self.host_key)
            || account != &self.account
            || server_id != self.server_id.as_ref()
        {
            return Err("private binding changed".into());
        }
        self.lease.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn private_target_requires_backend_owner_and_exact_observed_account() {
        let temp = tempfile::tempdir().unwrap();
        let (manager, registry, _) = super::super::tests::fixture(temp.path()).await;
        let authority = manager.snapshot().unwrap();
        let marker = serde_json::json!({"version":1,"provider":"github","owner_id":41,
            "uid":1001,"canonical_home":"/home/owner","enrollment_id":uuid::Uuid::new_v4().to_string()});
        let verify = |value: &serde_json::Value, uid, home| {
            VerifiedPrivateAccount::from_checked_observation(
                &authority,
                uid,
                home,
                "ssh-ed25519 test",
                &serde_json::to_vec(value).unwrap(),
            )
        };
        assert!(verify(&marker, 1001, "/home/owner").is_ok());
        for owner in [0, crate::domain::LOCAL_GUEST_USER_ID, 42] {
            let mut foreign = marker.clone();
            foreign["owner_id"] = owner.into();
            assert!(verify(&foreign, 1001, "/home/owner").is_err());
        }
        assert!(verify(&marker, 1002, "/home/owner").is_err());
        assert!(verify(&marker, 1001, "/home/other").is_err());
        let mut arbitrary_root = marker.clone();
        arbitrary_root["root"] = "/tmp/arbitrary".into();
        assert!(verify(&arbitrary_root, 1001, "/home/owner").is_err());
        registry
            .publish(
                &Scope::Personal,
                &super::super::tests::declaration("shared-host"),
            )
            .unwrap();
        let (_, _, revision) = registry.capture("shared-host").unwrap().unwrap();
        let observation = verify(&marker, 1001, "/home/owner").unwrap();
        let mut binding = super::super::tests::draft(revision);
        binding.setup = PrivateSetup::Enrolled {
            account: observation.account.clone(),
        };
        registry.save_binding("shared-host", binding, None).unwrap();
        let target =
            PrivateAccountTarget::from_verified(&registry, "shared-host", observation).unwrap();
        assert_eq!(
            target.root().unwrap(),
            Path::new("/home/owner/.codesense/remote-private/v1")
        );
        target.revalidate(&registry).unwrap();
        registry.remove_binding("shared-host", 1).unwrap();
        assert!(target.revalidate(&registry).is_err());
        manager.revoke();
        assert!(target.root().is_err());
        assert!(verify(&marker, 1001, "/home/owner").is_err());
    }
}
