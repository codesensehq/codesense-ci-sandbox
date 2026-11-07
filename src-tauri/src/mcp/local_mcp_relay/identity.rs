// SPDX-License-Identifier: MIT
//! Kernel identities of the bridge children one captured relay actually spawned.
use codesense_pty_core::activity::peer::{self, PeerProcess};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub(crate) struct Identity {
    descriptor: PathBuf,
    session: String,
    epoch: String,
    valid: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    children: Mutex<Vec<(PeerProcess, PeerProcess)>>,
}

impl Identity {
    pub(super) fn new(
        directory: &std::path::Path,
        session: &str,
        epoch: String,
        valid: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            descriptor: directory.join("local-relay.json"),
            session: session.to_string(),
            epoch,
            valid,
            children: Mutex::default(),
        })
    }

    pub(super) fn ingress(&self, caller: PeerProcess) -> bool {
        crate::commands::mcp_scope::relay_ingress_proven(&self.session, caller)
    }

    pub(super) fn spawned(
        self: &Arc<Self>,
        pid: u32,
        ingress: PeerProcess,
    ) -> Result<Registration, String> {
        (self.valid)()?;
        if !self.ingress(ingress) {
            return Err("MCP relay ingress has no verified launch identity".into());
        }
        let process = peer::inspect(pid).ok_or("cannot identify the spawned MCP bridge")?;
        self.children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((process, ingress));
        Ok(Registration {
            identity: self.clone(),
            process,
        })
    }

    pub(crate) fn proves(&self, caller: PeerProcess) -> bool {
        if (self.valid)().is_err() {
            return false;
        }
        // The descriptor is only a revocation/rotation marker. Its contents never
        // authenticate a caller: the kernel must also prove a registered child.
        if !codesense_host_proto::scoped_mcp::read_descriptor(&self.descriptor)
            .is_ok_and(|descriptor| descriptor.capability == self.epoch)
        {
            return false;
        }
        self.children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(root, ingress)| self.ingress(*ingress) && peer::descends_from(caller, *root))
    }
}

pub(super) struct Registration {
    identity: Arc<Identity>,
    process: PeerProcess,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.identity
            .children
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(process, _)| *process != self.process);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_host_proto::scoped_mcp::{self, Descriptor};

    #[test]
    fn review_r1_only_registered_kernel_children_survive_current_scope_checks() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let parent = crate::commands::mcp_scope::mcp_scope_session_dir("unused")
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(&parent).unwrap();
        let scope = tempfile::Builder::new()
            .prefix("peer-proof-")
            .tempdir_in(parent)
            .unwrap();
        let session = scope
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let descriptor = Descriptor {
            version: scoped_mcp::VERSION,
            socket: "/tmp/peer-fixture".into(),
            capability: "a".repeat(64),
        };
        let path = scope.path().join("local-relay.json");
        scoped_mcp::write_descriptor(&path, &descriptor).unwrap();
        let identity = Identity::new(
            scope.path(),
            &session,
            descriptor.capability.clone(),
            Arc::new(|| Ok(())),
        );
        crate::commands::mcp_scope::bind_relay_identity(&session, &identity);
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let caller = peer::of_socket(&socket).unwrap();
        assert!(
            !crate::commands::mcp_scope::relay_peer_proven(&session, caller),
            "a readable descriptor is not a child registration"
        );
        crate::commands::mcp_scope::bind_fixture_relay_root(&session, caller);
        let registration = identity.spawned(std::process::id(), caller).unwrap();
        assert!(crate::commands::mcp_scope::relay_peer_proven(
            &session, caller
        ));
        assert!(!crate::commands::mcp_scope::relay_peer_proven(
            &format!("other-{session}"),
            caller
        ));
        let rotated = Descriptor {
            capability: "b".repeat(64),
            ..descriptor.clone()
        };
        scoped_mcp::write_descriptor(&path, &rotated).unwrap();
        assert!(!crate::commands::mcp_scope::relay_peer_proven(
            &session, caller
        ));
        scoped_mcp::write_descriptor(&path, &descriptor).unwrap();
        assert!(crate::commands::mcp_scope::relay_peer_proven(
            &session, caller
        ));
        drop(registration);
        assert!(!crate::commands::mcp_scope::relay_peer_proven(
            &session, caller
        ));
        let _registration = identity.spawned(std::process::id(), caller).unwrap();
        crate::commands::mcp_scope::remove_scope_config(&session);
        assert!(!crate::commands::mcp_scope::relay_peer_proven(
            &session, caller
        ));
        std::fs::create_dir_all(scope.path()).unwrap();
        scoped_mcp::write_descriptor(&path, &descriptor).unwrap();
        assert!(
            !crate::commands::mcp_scope::relay_peer_proven(&session, caller),
            "recreating a descriptor cannot restore a retired app registration"
        );
    }
}
