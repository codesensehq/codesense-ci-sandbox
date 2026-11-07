// SPDX-License-Identifier: MIT
use super::*;
use registry::AcceptedIdentity;

pub(super) use registry::IDENTITY_CHANGED as REFUSAL;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityDto {
    host: String,
    port: u16,
    jump_host: Option<String>,
    host_key_fingerprint: Option<String>,
    server_id: Option<String>,
}

impl From<&AcceptedIdentity> for IdentityDto {
    fn from(identity: &AcceptedIdentity) -> Self {
        Self {
            host: identity.host.clone(),
            port: identity.port,
            jump_host: identity.jump_host.clone(),
            host_key_fingerprint: identity
                .host_key
                .as_deref()
                .and_then(|key| HostKey::parse(key).ok())
                .map(|key| key.fingerprint()),
            server_id: identity.server_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityChange {
    accepted: IdentityDto,
    declared: IdentityDto,
    revision: String,
    author: Option<String>,
    date: Option<String>,
    observed_server_id: Option<String>,
}

fn revision(binding: &Binding, declaration: &Declaration) -> String {
    let bytes = serde_json::to_vec(&(
        &binding.accepted_identity,
        AcceptedIdentity::from(declaration),
    ))
    .expect("identity is serializable");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn change(
    env: &Env,
    scope: &Scope,
    declaration: &Declaration,
    binding: Option<&Binding>,
) -> Option<IdentityChange> {
    let binding = binding?;
    let accepted = binding.accepted_identity.as_ref()?;
    if binding.validate_declaration(declaration).is_ok() {
        return None;
    }
    // Git metadata is evidence only. A locally edited file still needs review
    // when no commit has recorded it yet.
    let directory = if private_setup::claims(env, &declaration.machine_id).ok()? {
        registry::private::PrivateRegistry::dir_for(&env.registry, scope)
    } else {
        env.registry.dir(scope)
    };
    let metadata = directory.ok().and_then(|dir| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["log", "-1", "--format=%an%n%aI", "--"])
            .arg(format!("{}.yaml", declaration.machine_id))
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    });
    let mut lines = metadata.as_deref().unwrap_or_default().lines();
    Some(IdentityChange {
        accepted: IdentityDto::from(accepted),
        declared: IdentityDto::from(&AcceptedIdentity::from(declaration)),
        revision: revision(binding, declaration),
        author: lines.next().map(String::from),
        date: lines.next().map(String::from),
        observed_server_id: binding.observed_server_id.clone(),
    })
}

/// A legacy acceptance survives a team's conversion to private mode. Private
/// accounts have their own daemon identity, so only the destination pin carries.
pub(super) fn binding_for_private(env: &Env, id: &str) -> Result<Option<Binding>, String> {
    Ok(env.registry.bindings()?.remove(id).and_then(|mut binding| {
        binding.accepted_identity.as_mut()?.server_id = None;
        binding.observed_server_id = None;
        Some(binding)
    }))
}

fn saved_declaration(env: &Env, id: &str) -> Result<Option<(Declaration, bool)>, String> {
    if private_setup::claims(env, id)? {
        return match registry::private::PrivateRegistry::inventory_for(&env.registry)?.remove(id) {
            Some(registry::private::PrivateEntry::Ready(_, declaration, _)) => {
                Ok(Some((private_setup::legacy(&declaration), true)))
            }
            _ => Err(REFUSAL.into()),
        };
    }
    Ok(env
        .registry
        .find(id)?
        .map(|(_, declaration)| (declaration, false)))
}

pub(super) fn validate_saved(
    env: &Env,
    id: &str,
    input: Option<&MachineDraftInput>,
) -> Result<(), String> {
    let Some((declaration, private)) = saved_declaration(env, id)? else {
        return Ok(());
    };
    let binding = if private {
        binding_for_private(env, id)?
    } else {
        env.registry.bindings()?.remove(id)
    };
    validate_snapshot(&declaration, binding.as_ref(), input)
}

/// Validate captured values without consulting the filesystem again. Writers
/// must keep using this same declaration and binding after admission.
pub(super) fn validate_snapshot(
    declaration: &Declaration,
    binding: Option<&Binding>,
    input: Option<&MachineDraftInput>,
) -> Result<(), String> {
    if let Some(binding) = binding {
        binding.validate_declaration(declaration)?;
        if let (Some(accepted), Some(input)) = (&binding.accepted_identity, input) {
            if accepted.host != input.host.trim()
                || accepted.port != input.port
                || accepted.jump_host.as_deref()
                    != input
                        .jump_host
                        .as_deref()
                        .map(str::trim)
                        .filter(|host| !host.is_empty())
            {
                return Err(format!(
                    "{REFUSAL}. Save the address, then confirm it in Machines before connecting."
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn expected_server_id<'a>(binding: &'a Binding, id: &str) -> Result<&'a str, String> {
    binding
        .accepted_identity
        .as_ref()
        .and_then(|identity| identity.server_id.as_deref())
        .ok_or_else(|| format!("Machine {id:?} has no verified terminal-server identity"))
}

pub(super) fn verify_server(
    env: &Env,
    id: &str,
    binding: &Binding,
    observed: &str,
) -> Result<(), String> {
    let expected = expected_server_id(binding, id)?;
    if let Err(error) = check::check_server_id(Some(expected), observed) {
        env.registry.update_bindings(|all| {
            if let Some(current) = all.get_mut(id) {
                if current.accepted_identity == binding.accepted_identity {
                    current.observed_server_id = Some(observed.into());
                }
            }
        })?;
        return Err(format!("{REFUSAL}. {error}"));
    }
    Ok(())
}

pub(super) fn resolve(env: &Env, id: &str, reviewed: &str, accept: bool) -> Result<(), String> {
    let transaction = save_transaction(id);
    let _held = transaction
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (declaration, private) = saved_declaration(env, id)?.ok_or("no machine")?;
    let binding = env
        .registry
        .bindings()?
        .remove(id)
        .ok_or("Machine needs personal setup")?;
    let comparison = if private {
        binding_for_private(env, id)?.ok_or("Machine needs personal setup")?
    } else {
        binding.clone()
    };
    if revision(&comparison, &declaration) != reviewed {
        return Err("Machine identity changed again; review it in Machines".into());
    }
    if !accept {
        return Ok(());
    }
    let accepted = AcceptedIdentity::from(&declaration);
    // SSH's local pin file and the binding must agree before a connection can
    // resume. A failed binding write leaves a refused identity, never admission.
    if let Some(key) = &accepted.host_key {
        env.paths().pin(id, &HostKey::parse(key)?)?;
    }
    env.registry.update_bindings(|all| -> Result<(), String> {
        let current = all.get_mut(id).ok_or("Machine binding removed")?;
        if current.accepted_identity != binding.accepted_identity {
            return Err("Machine acceptance changed; review it again".into());
        }
        current.accepted_identity = Some(accepted);
        current.observed_server_id = None;
        Ok(())
    })??;
    Ok(())
}

/// MCP's receiving seam fixes caller authority here; agent arguments cannot
/// select the native human confirmation path.
pub(crate) fn resolve_for_agent(
    env: &Env,
    id: &str,
    reviewed: &str,
    accept: bool,
) -> Result<(), String> {
    use super::super::interactive_gate::InteractiveGate;
    if accept {
        InteractiveGate::admit_human(
            "resolve_machine_identity",
            crate::panes::commands::Actor::Agent,
        )
        .map_err(|error| error.to_string())?;
    } else {
        InteractiveGate::admit("resolve_machine_identity").map_err(|error| error.to_string())?;
    }
    resolve(env, id, reviewed, accept)
}

#[tauri::command]
pub async fn resolve_machine_identity(
    app: tauri::AppHandle,
    machine_id: String,
    revision: String,
    accept: bool,
) -> Result<(), String> {
    super::super::interactive_gate::InteractiveGate::admit_human(
        "resolve_machine_identity",
        crate::panes::commands::Actor::Human,
    )
    .map_err(|error| error.to_string())?;
    let env = env_of(&app).await?;
    let id = machine_id.clone();
    crate::commands::await_blocking("resolve_machine_identity", move || {
        resolve(&env, &id, &revision, accept)
    })
    .await?;
    notify_manager_mutation(&app, machine_id, None).await
}
