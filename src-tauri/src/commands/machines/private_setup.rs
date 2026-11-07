// SPDX-License-Identifier: MIT
//! Private setup never enters the legacy root/probe path.
use super::*;
use codesense_session_server::private_account::{Bootstrap, HelperUpload, VerifiedAccount};
use registry::private::{
    PrivateAccount, PrivateBinding, PrivateDeclaration, PrivateRegistry, PrivateSetup,
};

const REFUSED: &str = "private account unavailable; check account enrollment";
/// This profile has no machine access at all, which enrollment can't fix.
const NO_MACHINE_ACCESS: &str =
    "machine access is unavailable for this profile; set up machine access in Settings";
fn authority(
    env: &Env,
) -> Result<&crate::commands::auth::private_authority::PrivateAuthoritySnapshot, String> {
    let authority = env.private_authority.as_ref().ok_or(NO_MACHINE_ACCESS)?;
    authority.lease().validate()?;
    Ok(authority)
}
fn registry(env: &Env) -> Result<PrivateRegistry, String> {
    PrivateRegistry::new(env.registry.clone(), authority(env)?)
}
pub(super) fn legacy(declaration: &PrivateDeclaration) -> Declaration {
    let m = &declaration.machine;
    Declaration {
        machine_id: m.machine_id.clone(),
        label: m.label.clone(),
        host: m.host.clone(),
        port: m.port,
        jump_host: m.jump_host.clone(),
        default_base_dir: m.default_base_dir.clone(),
        pinned_host_key: m.pinned_host_key.clone(),
        pinned_server_id: None,
    }
}
fn account(observed: &VerifiedAccount) -> PrivateAccount {
    PrivateAccount {
        uid: observed.uid(),
        canonical_home: observed.canonical_home().into(),
        root_version: 1,
        enrollment_id: observed.enrollment_id().into(),
    }
}
fn prepare(
    env: &Env,
    store: &dyn CredentialStore,
    id: Option<&str>,
    input: &MachineDraftInput,
    upload: HelperUpload,
    progress: &OpenProgress,
) -> Result<(Reach, Bootstrap, HostKey), String> {
    authority(env)?;
    let saved = id.map(|id| registry(env)?.find(id)).transpose()?.flatten();
    if let Some((_, saved)) = &saved {
        if saved.machine.host != input.host
            || saved.machine.port != input.port
            || saved.machine.jump_host != input.jump_host
        {
            return Err("private machine address changed; create a new declaration".into());
        }
    }
    let declaration = saved.as_ref().map(|(_, d)| legacy(d));
    let pinned = pinned_key(declaration.as_ref(), input)?;
    // Retain throwaway pins for all operations in this invocation; Reach owns
    // the actual pinned file through its connector, so use stable saved pins
    // only after verified key acceptance below.
    let reach = reach(env, store, id.unwrap_or(DRAFT_ID), input)?;
    progress.enter("checking the host key");
    let key = check::verified_host_key(
        &reach.connector,
        trust(pinned.as_ref(), input),
        &env.scratch(),
    )?;
    reach.connector.pin(&key)?;
    let bootstrap = Bootstrap::prepare(
        &reach.ssh(Mux::Fresh),
        &env.artifacts,
        &env.shell_init,
        upload,
        &|step| progress.enter(step),
    )?;
    authority(env)?;
    Ok((reach, bootstrap, key))
}
/// The saved legacy declaration `id` names, refusing an address the caller
/// changed: switching keeps the machine as saved.
fn saved_legacy(
    env: &Env,
    id: Option<&str>,
    input: &MachineDraftInput,
) -> Result<Option<(Scope, Declaration)>, String> {
    let Some(id) = id else { return Ok(None) };
    super::identity::validate_saved(env, id, None)?;
    if claims(env, id)? {
        return Ok(None);
    }
    let Some((scope, old)) = env.registry.find(id)? else {
        return Ok(None);
    };
    if old.host != input.host || old.port != input.port || old.jump_host != input.jump_host {
        return Err(format!(
            "This machine is saved at {}:{}{}. Undo the address change to switch it to a private account.",
            old.host,
            old.port,
            old.jump_host.as_deref().map(|jump| format!(" via {jump}")).unwrap_or_default()
        ));
    }
    Ok(Some((scope, old)))
}
/// A saved legacy machine set up as a private account converts in place: it
/// keeps its id, label, address, verified host key and personal settings.
/// Only private accounts open terminal tabs, so this is the one way forward
/// for a legacy machine rather than adding a second record for the host. The
/// caller reaches this only by asking for a private account explicitly.
fn convert_if_legacy(env: &Env, id: Option<&str>, input: &MachineDraftInput) -> Result<(), String> {
    if input.mode != Some(MachineMode::PrivateAccount) {
        return Ok(());
    }
    if let (Some(id), Some((scope, _))) = (id, saved_legacy(env, id, input)?) {
        registry(env)?.convert_legacy(&scope, id)?;
    }
    adopt_legacy_binding(env, id)
}
/// Carry this person's legacy sign-in and settings onto a private machine
/// that has none yet: the publisher's own on conversion, and a teammate's
/// the first time they set up a team machine someone else converted.
fn adopt_legacy_binding(env: &Env, id: Option<&str>) -> Result<(), String> {
    let Some(id) = id else { return Ok(()) };
    if !claims(env, id)? {
        return Ok(());
    }
    super::identity::validate_saved(env, id, None)?;
    let registry = registry(env)?;
    let Some((_, declaration, revision)) = registry.capture(id)? else {
        return Ok(());
    };
    if registry.binding(id)?.is_some() {
        return Ok(());
    }
    let Some(legacy) = env.registry.bindings()?.remove(id) else {
        return Ok(());
    };
    registry.save_binding(
        id,
        PrivateBinding {
            username: legacy.username,
            key_path: legacy.key_path,
            credential_ref: legacy.credential_ref,
            setup: PrivateSetup::NeedsEnrollment,
            host_key: declaration.machine.pinned_host_key,
            declaration_revision: revision,
            revision: registry
                .binding_generation(id)?
                .checked_add(1)
                .ok_or(REFUSED)?,
            auto_connect: false,
            // Legacy machines have no agent-control grant to carry.
            remote_mcp_control: false,
            hidden: legacy.hidden,
            restore_auto_connect: legacy.auto_connect,
            open_tabs: false,
        },
        None,
    )
}
fn validate_observed(
    env: &Env,
    id: Option<&str>,
    observed: &VerifiedAccount,
) -> Result<(), String> {
    if let Some(id) = id {
        if let Some(binding) = registry(env)?.binding(id)? {
            let previous = match binding.setup {
                PrivateSetup::Enrolled { account } | PrivateSetup::Ready { account, .. } => {
                    Some(account)
                }
                PrivateSetup::NeedsEnrollment => None,
            };
            if previous.is_some_and(|previous| previous != account(observed)) {
                return Err(REFUSED.into());
            }
        }
    }
    authority(env)?;
    Ok(())
}
fn report(detail: &str, status: &'static str, next: Vec<&'static str>) -> MachineTestReportDto {
    MachineTestReportDto {
        steps: vec![MachineTestStepDto {
            id: "private-account",
            status,
            detail: detail.into(),
            next_steps: next,
            fingerprint: None,
        }],
        install_plan: None,
        file_view_only: false,
    }
}
pub(super) fn test(
    env: &Env,
    store: &dyn CredentialStore,
    id: Option<&str>,
    input: &MachineDraftInput,
) -> Result<MachineTestReportDto, String> {
    authority(env)?;
    // Testing never switches a legacy machine: it checks the host as a
    // private account against the saved pin, as a draft, writing nothing.
    if let Some((_, legacy)) = saved_legacy(env, id, input)? {
        let pinned = legacy
            .pinned_host_key
            .as_deref()
            .map(HostKey::parse)
            .transpose()?
            .ok_or("This machine has no verified host key")?;
        let draft = MachineDraftInput {
            trusted_host_key_fp: Some(pinned.fingerprint()),
            ..input.clone()
        };
        return test(env, store, None, &draft);
    }
    let registry = registry(env)?;
    let saved = id.map(|id| registry.find(id)).transpose()?.flatten();
    let pinned = saved
        .as_ref()
        .and_then(|(_, d)| d.machine.pinned_host_key.as_deref())
        .map(HostKey::parse)
        .transpose()?;
    if pinned.is_none() && input.trusted_host_key_fp.is_none() {
        let reach = reach(env, store, id.unwrap_or(DRAFT_ID), input)?;
        let key = reach
            .connector
            .scan(&env.scratch(), None)
            .map_err(|failure| unreachable_detail(input, &failure.to_string()))?;
        return Ok(MachineTestReportDto {
            steps: vec![MachineTestStepDto {
                id: "hostKey",
                status: "needsTrust",
                detail: format!(
                    "First time connecting to {}. Check its fingerprint before trusting it.",
                    input.host
                ),
                next_steps: vec![],
                fingerprint: Some(key.fingerprint()),
            }],
            install_plan: None,
            file_view_only: false,
        });
    }
    let (reach, bootstrap, _) = prepare(
        env,
        store,
        id,
        input,
        HelperUpload::Setup,
        &OpenProgress::default(),
    )?;
    let runtime_steps: Vec<_> = check::runtime_checks(&reach.ssh(Mux::Fresh))
        .into_iter()
        .map(|step| MachineTestStepDto {
            id: step_id(step.id),
            status: step_status(step.status),
            detail: step.detail,
            next_steps: vec![],
            fingerprint: None,
        })
        .collect();
    let Some(account) = bootstrap.inspect(authority(env)?.bootstrap_owner())? else {
        let mut result = report(
            "This Unix account needs explicit private enrollment",
            "warn",
            vec!["enroll-private-account"],
        );
        result.steps.extend(runtime_steps);
        return Ok(result);
    };
    validate_observed(env, id, &account)?;
    let installed = bootstrap.status(&account)?;
    authority(env)?;
    let mut result = if installed.is_some() {
        report("Private account and terminal server verified", "ok", vec![])
    } else {
        report(
            "Private account enrolled; install its terminal server",
            "warn",
            vec!["install-terminal-server"],
        )
    };
    result.steps.extend(runtime_steps);
    Ok(result)
}
pub(super) fn enroll(
    env: &Env,
    store: &dyn CredentialStore,
    id: Option<&str>,
    input: &MachineDraftInput,
) -> Result<MachineTestReportDto, String> {
    convert_if_legacy(env, id, input)?;
    let (_reach, bootstrap, key) = prepare(
        env,
        store,
        id,
        input,
        HelperUpload::Setup,
        &OpenProgress::default(),
    )?;
    let registry = registry(env)?;
    let prior = id.map(|id| registry.binding(id)).transpose()?.flatten();
    let observed = if prior
        .as_ref()
        .is_some_and(|binding| !matches!(binding.setup, PrivateSetup::NeedsEnrollment))
    {
        bootstrap
            .inspect(authority(env)?.bootstrap_owner())?
            .ok_or(REFUSED)?
    } else {
        bootstrap.enroll(authority(env)?.bootstrap_owner())?
    };
    validate_observed(env, id, &observed)?;
    if let Some(id) = id {
        registry.pin_draft(id, &key.to_line())?;
        let (_, _, revision) = registry.capture(id)?.ok_or(REFUSED)?;
        let setup = match prior.as_ref().map(|b| &b.setup) {
            Some(ready @ PrivateSetup::Ready { .. }) => ready.clone(),
            _ => PrivateSetup::Enrolled {
                account: account(&observed),
            },
        };
        let binding = PrivateBinding {
            username: input.username.clone(),
            key_path: input.sign_in.key_path().map(String::from),
            credential_ref: input.sign_in.credential_ref().cloned(),
            setup,
            host_key: Some(key.to_line()),
            declaration_revision: revision,
            revision: registry
                .binding_generation(id)?
                .checked_add(1)
                .ok_or(REFUSED)?,
            auto_connect: prior.as_ref().is_some_and(|b| b.auto_connect),
            remote_mcp_control: prior.as_ref().is_some_and(|b| b.remote_mcp_control),
            hidden: prior.as_ref().is_some_and(|b| b.hidden),
            restore_auto_connect: prior.as_ref().is_some_and(|b| b.restore_auto_connect),
            open_tabs: prior.as_ref().is_some_and(|b| b.open_tabs),
        };
        registry.save_binding(id, binding, prior.map(|b| b.revision))?;
    }
    Ok(report(
        "Private account enrolled",
        "warn",
        vec!["install-terminal-server"],
    ))
}
pub(super) fn install(
    env: &Env,
    store: &dyn CredentialStore,
    id: Option<&str>,
    input: &MachineDraftInput,
) -> Result<InstallTerminalServerResult, String> {
    convert_if_legacy(env, id, input)?;
    let (_reach, bootstrap, _) = prepare(
        env,
        store,
        id,
        input,
        HelperUpload::Setup,
        &OpenProgress::default(),
    )?;
    let observed = bootstrap
        .inspect(authority(env)?.bootstrap_owner())?
        .ok_or("Explicit private account enrollment is required")?;
    validate_observed(env, id, &observed)?;
    let version = bootstrap.install(&observed)?;
    authority(env)?;
    Ok(InstallTerminalServerResult::Installed {
        ok: true,
        version,
        logout_warning: None,
    })
}
/// The shared writer holds save_transaction(id) through validation, conversion
/// and this write. Taking that same lock here would re-enter it.
pub(super) fn save_locked(
    env: &Env,
    store: &dyn CredentialStore,
    id: &str,
    input: &MachineDraftInput,
    share: &Scope,
    connect: bool,
) -> Result<MachineSummary, String> {
    let authority = authority(env)?;
    convert_if_legacy(env, Some(id), input)?;
    if let Some(handle) = input.sign_in.credential_ref() {
        passphrase::load(store, handle)?;
        env.registry.update_issued_passphrases(|issued| {
            if let Some(at) = issued.get_mut(&handle.account) {
                *at = now_secs();
            }
        })?;
    }
    let registry = registry(env)?;
    let id = id.to_string();
    let existing = registry.capture(&id)?;
    // A caller may mint a new machine's id itself, so a save it retries after
    // a failure updates that machine instead of adding a second one.
    if existing.is_none() && !registry::is_machine_id(&id) {
        return Err(format!("{id:?} can't name a machine"));
    }
    let old = if existing.is_some() {
        registry.binding(&id)?
    } else {
        None
    };
    let mut key = existing
        .as_ref()
        .and_then(|(_, d, _)| d.machine.pinned_host_key.clone());
    let mut setup = PrivateSetup::NeedsEnrollment;
    if connect {
        let (_reach, bootstrap, verified_key) = prepare(
            env,
            store,
            existing.as_ref().map(|_| id.as_str()),
            input,
            HelperUpload::Setup,
            &OpenProgress::default(),
        )?;
        let observed = bootstrap
            .inspect(authority.bootstrap_owner())?
            .ok_or("Explicit private account enrollment is required")?;
        if let Some(old) = &old {
            let prior = match &old.setup {
                PrivateSetup::Enrolled { account } | PrivateSetup::Ready { account, .. } => {
                    Some(account)
                }
                _ => None,
            };
            if prior.is_some_and(|prior| *prior != account(&observed)) {
                return Err(REFUSED.into());
            }
        }
        let version = bootstrap
            .status(&observed)?
            .ok_or("Install the private terminal server first")?;
        let ssh = _reach.ssh(Mux::Fresh);
        let script = bootstrap.attach_script(&observed, &version);
        let server_id = check::read_server_id_script(&ssh, &script, check::GREETING_TIMEOUT)
            .map_err(|_| REFUSED)?;
        if let Some(PrivateBinding {
            setup:
                PrivateSetup::Ready {
                    server_id: expected,
                    ..
                },
            ..
        }) = &old
        {
            if expected != &server_id {
                return Err(REFUSED.into());
            }
        }
        setup = PrivateSetup::Ready {
            account: account(&observed),
            server_id,
        };
        key = Some(verified_key.to_line());
    }
    let declaration = PrivateDeclaration::from_legacy(Declaration {
        machine_id: id.clone(),
        label: if input.label.trim().is_empty() {
            input.host.clone()
        } else {
            input.label.clone()
        },
        host: input.host.clone(),
        port: input.port,
        jump_host: input.jump_host.clone(),
        default_base_dir: None,
        pinned_host_key: key.clone(),
        pinned_server_id: None,
    })?;
    Address::new(
        &input.host,
        input.port,
        input.username.as_deref(),
        input.jump_host.as_deref(),
    )?;
    let scope = existing
        .as_ref()
        .map(|(scope, _, _)| scope)
        .unwrap_or(share)
        .clone();
    if existing.is_none() {
        registry.publish(&scope, &declaration)?;
    } else if let Some(key) = &key {
        registry.pin_draft(&id, key)?;
    }
    let (_, saved, revision) = registry.capture(&id)?.ok_or(REFUSED)?;
    // Existing published reach/host identity is immutable through personal setup.
    if saved.machine.host != declaration.machine.host
        || saved.machine.port != declaration.machine.port
    {
        return Err(REFUSED.into());
    }
    let ready = matches!(setup, PrivateSetup::Ready { .. });
    let restore = old.as_ref().is_some_and(|b| b.restore_auto_connect);
    let binding = PrivateBinding {
        username: input.username.clone(),
        key_path: input.sign_in.key_path().map(String::from),
        credential_ref: input.sign_in.credential_ref().cloned(),
        setup,
        host_key: key,
        declaration_revision: revision,
        revision: registry
            .binding_generation(&id)?
            .checked_add(1)
            .ok_or(REFUSED)?,
        auto_connect: saved_auto_connect(old.as_ref(), ready, &scope),
        remote_mcp_control: old.as_ref().is_some_and(|b| b.remote_mcp_control),
        hidden: old.as_ref().is_some_and(|b| b.hidden),
        restore_auto_connect: restore && !ready,
        open_tabs: old.as_ref().is_some_and(|b| b.open_tabs),
    };
    registry.save_binding(&id, binding.clone(), old.map(|b| b.revision))?;
    authority.lease().validate()?;
    private_summary(env, scope, &saved, Some(&binding))
}
/// Why a first-sight host key scan found nothing, said with the address the
/// person typed so they can tell a typo from a closed port.
fn unreachable_detail(input: &MachineDraftInput, failure: &str) -> String {
    let said = failure.trim();
    if said.is_empty() {
        format!("Couldn't reach {}:{}", input.host, input.port)
    } else {
        format!("Couldn't reach {}:{}: {said}", input.host, input.port)
    }
}
/// A binding's background connect. Personal machines connect on launch by
/// default from the moment their setup first completes; a later explicit
/// choice is kept, and a team machine stays connect-on-select.
fn saved_auto_connect(old: Option<&PrivateBinding>, ready: bool, scope: &Scope) -> bool {
    let was_ready = old.is_some_and(|b| matches!(b.setup, PrivateSetup::Ready { .. }));
    if ready && !was_ready && matches!(scope, Scope::Personal) {
        return true;
    }
    old.is_some_and(|b| b.auto_connect || (ready && b.restore_auto_connect))
}
/// Whether the connection manager may connect this binding with nobody
/// watching, when it asks to auto-connect.
fn background_auto_connect(binding: &PrivateBinding) -> bool {
    binding.auto_connect && can_connect_unattended(binding)
}
/// Whether signing in with this binding can't raise a prompt: a key whose
/// passphrase lives in the keychain can raise a system dialog, and an agent
/// known to confirm each use (1Password) can raise Touch ID. Those connect
/// only when the person picks the machine.
fn can_connect_unattended(binding: &PrivateBinding) -> bool {
    binding.credential_ref.is_none() && !(agent_held(binding) && agent_confirms_use())
}
/// The key is the agent's: no key file at all, or a public key that pins one
/// of the agent's keys.
fn agent_held(binding: &PrivateBinding) -> bool {
    binding
        .key_path
        .as_deref()
        .is_none_or(|path| path.ends_with(".pub"))
}
/// Whether the SSH agent is one known to ask before each use. Only 1Password
/// is detectable cheaply (its socket, here or as the config's IdentityAgent);
/// a plain agent's `ssh-add -c` confirmation is not.
fn agent_confirms_use() -> bool {
    static CONFIRMS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CONFIRMS.get_or_init(|| {
        let names_1password = |text: &str| text.to_ascii_lowercase().contains("1password");
        std::env::var("SSH_AUTH_SOCK").is_ok_and(|sock| names_1password(&sock))
            || dirs::home_dir()
                .and_then(|home| std::fs::read_to_string(home.join(".ssh/config")).ok())
                .is_some_and(|config| {
                    config.lines().any(|line| {
                        let line = line.trim().to_ascii_lowercase();
                        line.starts_with("identityagent") && names_1password(&line)
                    })
                })
    })
}
/// Keep the "had open tabs" bit the connection manager saw flip. A machine
/// without a private binding has nothing to keep.
pub(super) fn set_open_tabs(env: &Env, id: &str, open_tabs: bool) -> Result<(), String> {
    let registry = registry(env)?;
    let Some(mut binding) = registry.binding(id)? else {
        return Ok(());
    };
    if binding.open_tabs == open_tabs {
        return Ok(());
    }
    let old_revision = binding.revision;
    binding.open_tabs = open_tabs;
    binding.revision = registry
        .binding_generation(id)?
        .checked_add(1)
        .ok_or(REFUSED)?;
    registry.save_binding(id, binding, Some(old_revision))
}
fn private_summary(
    env: &Env,
    scope: Scope,
    declaration: &PrivateDeclaration,
    binding: Option<&PrivateBinding>,
) -> Result<MachineSummary, String> {
    let mut legacy_binding = binding.map(|b| Binding {
        username: b.username.clone(),
        key_path: b.key_path.clone(),
        credential_ref: b.credential_ref.clone(),
        auto_connect: b.auto_connect,
        hidden: b.hidden,
        set_up: matches!(b.setup, PrivateSetup::Ready { .. }),
        observed_server_id: None,
        accepted_identity: None,
    });
    if let Some(mut accepted) =
        super::identity::binding_for_private(env, &declaration.machine.machine_id)?
    {
        if let Some(binding) = legacy_binding.as_mut() {
            binding.accepted_identity = accepted.accepted_identity;
        } else {
            accepted.set_up = false;
            legacy_binding = Some(accepted);
        }
    }
    let mut result = summary(env, scope, &legacy(declaration), legacy_binding.as_ref());
    let pending = result
        .declaration
        .as_ref()
        .is_some_and(|d| d.identity_change.is_some());
    if let Some(declaration) = &mut result.declaration {
        declaration.mode = MachineMode::PrivateAccount;
    }
    if !pending && binding.is_some_and(|b| matches!(b.setup, PrivateSetup::Ready { .. })) {
        result.status = MachineStatus::Idle;
    }
    result.remote_mcp_control = !pending && binding.is_some_and(|b| b.remote_mcp_control);
    // Its agents reach this app's MCP (inbox, roles, progress, hooks) over the reverse
    // relay a launch provisions only with remote MCP control on a ready private binding.
    result.capabilities.agent_callbacks = !pending && agent_callbacks(binding);
    if let (Some(dto), Some(b)) = (result.binding.as_mut(), binding) {
        dto.reconnect_at_launch = b.open_tabs && can_connect_unattended(b);
    }
    Ok(result)
}

/// Whether agents launched on this private machine can call back into this app: the
/// binding half of the gate [`authorize_remote_mcp_control`] applies to every reverse MCP
/// launch and call (which also validates this owner's lease at call time).
pub(super) fn agent_callbacks(binding: Option<&PrivateBinding>) -> bool {
    binding.is_some_and(|b| matches!(b.setup, PrivateSetup::Ready { .. }) && b.remote_mcp_control)
}

pub(super) fn claims(env: &Env, id: &str) -> Result<bool, String> {
    PrivateRegistry::claims_reserved(&env.registry, id)
}
/// Whether this owner's private binding for `id` is ready to connect.
pub(super) fn ready(env: &Env, id: &str) -> Result<bool, String> {
    super::identity::validate_saved(env, id, None)?;
    Ok(registry(env)?
        .binding(id)?
        .is_some_and(|b| matches!(b.setup, PrivateSetup::Ready { .. })))
}
pub(super) fn generation(env: &Env, id: &str) -> Result<String, String> {
    let registry = registry(env)?;
    let declaration = registry.capture(id)?.ok_or(REFUSED)?;
    // Open tabs coming and going is no reason to drop a connection.
    let binding = registry.binding(id)?.map(|mut b| {
        b.open_tabs = false;
        b
    });
    let accepted = super::identity::binding_for_private(env, id)?.and_then(|b| b.accepted_identity);
    let bytes = serde_json::to_vec(&(
        declaration,
        binding,
        accepted,
        authority(env)?.lease().epoch(),
    ))
    .map_err(|_| REFUSED)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
pub(super) fn open(
    env: &Env,
    store: &dyn CredentialStore,
    id: &str,
    progress: &OpenProgress,
) -> Result<PinnedSshTransport, String> {
    let registry = registry(env)?;
    let (_, declaration, _) = registry.capture(id)?.ok_or(REFUSED)?;
    let binding = registry.binding(id)?.ok_or(REFUSED)?;
    let PrivateSetup::Ready {
        account: expected,
        server_id,
    } = &binding.setup
    else {
        return Err("private setup is incomplete".into());
    };
    let initial_generation = generation(env, id)?;
    let personal = Binding {
        username: binding.username.clone(),
        key_path: binding.key_path.clone(),
        credential_ref: binding.credential_ref.clone(),
        ..Default::default()
    };
    let input = MachineDraftInput {
        mode: Some(MachineMode::PrivateAccount),
        label: declaration.machine.label.clone(),
        host: declaration.machine.host.clone(),
        port: declaration.machine.port,
        username: binding.username.clone(),
        jump_host: declaration.machine.jump_host.clone(),
        sign_in: stored_sign_in(&personal)?,
        trusted_host_key_fp: None,
    };
    let (reach, bootstrap, key) = prepare(
        env,
        store,
        Some(id),
        &input,
        HelperUpload::Connect,
        progress,
    )?;
    if binding.host_key.as_deref() != Some(&key.to_line()) {
        return Err(REFUSED.into());
    }
    progress.enter("verifying the private account");
    let observed = bootstrap
        .inspect(authority(env)?.bootstrap_owner())?
        .ok_or(REFUSED)?;
    if account(&observed) != *expected {
        return Err(REFUSED.into());
    }
    let target = registry::private::target::PrivateAccountTarget::from_bootstrap(
        &registry,
        id,
        authority(env)?,
        &key.to_line(),
        &observed,
    )?;
    progress.enter("reading the installed terminal server");
    let version = bootstrap.status(&observed)?.ok_or(REFUSED)?;
    target.revalidate(&registry)?;
    progress.enter("starting the private terminal server relay");
    let ssh = reach.ssh(Mux::Fresh);
    let script = bootstrap.attach_script(&observed, &version);
    let (reader, writer, mut child_guard) = spawn_pinned_script(ssh, &script, reach.askpass)?;
    child_guard.private_bootstrap = Some(bootstrap);
    if generation(env, id)? != initial_generation {
        drop(reader);
        drop(writer);
        child_guard.close_and_reap()?;
        return Err(REFUSED.into());
    }
    target.revalidate(&registry)?;
    Ok(PinnedSshTransport {
        reader,
        writer,
        child_guard,
        private_lease: Some(authority(env)?.lease()),
        expected_server_id: server_id.clone(),
        generation: initial_generation,
    })
}
pub(super) fn record(
    env: &Env,
    id: &str,
) -> Result<Option<super::super::host_connection_manager::MachineManagerRecord>, String> {
    if !claims(env, id)? {
        return Ok(None);
    }
    let registry = registry(env)?;
    let Some((scope, _, _)) = registry.capture(id).ok().flatten() else {
        return Ok(None);
    };
    let binding = registry.binding(id)?;
    Ok(Some(
        super::super::host_connection_manager::MachineManagerRecord {
            machine_id: id.into(),
            scope: Some(scope),
            revision: None,
            fingerprint: generation(env, id)?,
            set_up: super::identity::validate_saved(env, id, None).is_ok()
                && binding
                    .as_ref()
                    .is_some_and(|b| matches!(b.setup, PrivateSetup::Ready { .. })),
            auto_connect: binding.as_ref().is_some_and(background_auto_connect),
            open_tabs: binding
                .as_ref()
                .is_some_and(|b| b.open_tabs && can_connect_unattended(b)),
        },
    ))
}
pub(super) fn summaries(env: &Env) -> Result<Vec<MachineSummary>, String> {
    if env.private_authority.is_none() {
        let mut result = Vec::new();
        for entry in PrivateRegistry::inventory_for(&env.registry)?.into_values() {
            if let registry::private::PrivateEntry::Ready(scope, declaration, _) = entry {
                let mut summary = private_summary(env, scope, &declaration, None)?;
                summary.status = MachineStatus::ConnectionFailed {
                    last_error: "Machine access is unavailable for this profile. Set up machine access in Settings or recover its historical identity metadata.".into(),
                };
                result.push(summary);
            }
        }
        return Ok(result);
    }
    let registry = registry(env)?;
    let mut result = Vec::new();
    for (id, entry) in registry.inventory()? {
        if let registry::private::PrivateEntry::Ready(scope, declaration, _) = entry {
            result.push(private_summary(
                env,
                scope,
                &declaration,
                registry.binding(&id)?.as_ref(),
            )?);
        }
    }
    Ok(result)
}

pub(super) fn credential_accounts(env: &Env) -> Result<std::collections::BTreeSet<String>, String> {
    if env.private_authority.is_none() {
        return Ok(Default::default());
    }
    let registry = registry(env)?;
    let mut accounts = std::collections::BTreeSet::new();
    for (id, entry) in registry.inventory()? {
        if matches!(entry, registry::private::PrivateEntry::Ready(..)) {
            if let Some(handle) = registry.binding(&id)?.and_then(|b| b.credential_ref) {
                accounts.insert(handle.account);
            }
        }
    }
    Ok(accounts)
}

fn prepared_saved(
    env: &Env,
    store: &dyn CredentialStore,
    id: &str,
    upload: HelperUpload,
) -> Result<(Reach, Bootstrap, VerifiedAccount, PrivateBinding, String), String> {
    let registry = registry(env)?;
    let (_, declaration, _) = registry.capture(id)?.ok_or(REFUSED)?;
    let binding = registry.binding(id)?.ok_or(REFUSED)?;
    if !matches!(binding.setup, PrivateSetup::Ready { .. }) {
        return Err(REFUSED.into());
    }
    let initial_generation = generation(env, id)?;
    let personal = Binding {
        username: binding.username.clone(),
        key_path: binding.key_path.clone(),
        credential_ref: binding.credential_ref.clone(),
        ..Default::default()
    };
    let input = MachineDraftInput {
        mode: Some(MachineMode::PrivateAccount),
        label: declaration.machine.label.clone(),
        host: declaration.machine.host.clone(),
        port: declaration.machine.port,
        username: binding.username.clone(),
        jump_host: declaration.machine.jump_host.clone(),
        sign_in: stored_sign_in(&personal)?,
        trusted_host_key_fp: None,
    };
    let (reach, bootstrap, key) = prepare(
        env,
        store,
        Some(id),
        &input,
        upload,
        &OpenProgress::default(),
    )?;
    let observed = bootstrap
        .inspect(authority(env)?.bootstrap_owner())?
        .ok_or(REFUSED)?;
    validate_observed(env, Some(id), &observed)?;
    registry::private::target::PrivateAccountTarget::from_bootstrap(
        &registry,
        id,
        authority(env)?,
        &key.to_line(),
        &observed,
    )?
    .revalidate(&registry)?;
    Ok((reach, bootstrap, observed, binding, initial_generation))
}
/// How a private machine's host is reached for work outside its private
/// root: its saved address, this person's sign-in and the host key it pinned.
pub(super) fn reach_input(env: &Env, id: &str) -> Result<(MachineDraftInput, String), String> {
    let registry = registry(env)?;
    let (_, declaration, _) = registry.capture(id)?.ok_or(REFUSED)?;
    let binding = registry.binding(id)?.ok_or(REFUSED)?;
    let pinned = binding
        .host_key
        .clone()
        .ok_or_else(|| format!("Machine {id:?} has no verified host key"))?;
    let personal = Binding {
        username: binding.username.clone(),
        key_path: binding.key_path.clone(),
        credential_ref: binding.credential_ref.clone(),
        ..Default::default()
    };
    let input = MachineDraftInput {
        mode: Some(MachineMode::PrivateAccount),
        label: declaration.machine.label.clone(),
        host: declaration.machine.host.clone(),
        port: declaration.machine.port,
        username: binding.username.clone(),
        jump_host: declaration.machine.jump_host.clone(),
        sign_in: stored_sign_in(&personal)?,
        trusted_host_key_fp: None,
    };
    Ok((input, pinned))
}
pub(super) fn proof(
    env: &Env,
    store: &dyn CredentialStore,
    id: &str,
) -> Result<Option<LegacyHostProof>, String> {
    // Proof runs inside a connect, which has its own deadline.
    let (_reach, bootstrap, observed, binding, initial_generation) =
        prepared_saved(env, store, id, HelperUpload::Connect)?;
    let PrivateSetup::Ready { server_id, .. } = binding.setup else {
        return Err(REFUSED.into());
    };
    let proof = parse_legacy_host_proof(&bootstrap.proof(&observed)?, &server_id).ok_or(REFUSED)?;
    if proof.uid != observed.uid()
        || proof.root != format!("{}/.codesense/remote-private/v1", observed.canonical_home())
        || generation(env, id)? != initial_generation
    {
        return Err(REFUSED.into());
    }
    Ok(Some(proof))
}
pub(super) fn replace(
    env: &Env,
    store: &dyn CredentialStore,
    id: &str,
    expected: &LegacyHostProof,
    nonce: &str,
    epoch: &str,
) -> Result<String, ReplacementDispatchFailure> {
    let fail = |message| ReplacementDispatchFailure {
        old_stopped: false,
        message,
    };
    let (_reach, bootstrap, observed, binding, initial_generation) =
        prepared_saved(env, store, id, HelperUpload::Setup).map_err(fail)?;
    let PrivateSetup::Ready { server_id, .. } = binding.setup else {
        return Err(fail(REFUSED.into()));
    };
    let proof = parse_legacy_host_proof(&bootstrap.proof(&observed).map_err(fail)?, &server_id)
        .ok_or_else(|| fail(REFUSED.into()))?;
    revalidate_replacement_process_proof(expected, &proof).map_err(fail)?;
    if generation(env, id).map_err(fail)? != initial_generation {
        return Err(fail(REFUSED.into()));
    }
    let version = bootstrap.install(&observed).map_err(fail)?;
    if generation(env, id).map_err(fail)? != initial_generation {
        return Err(fail(REFUSED.into()));
    }
    bootstrap
        .replace(
            &observed,
            &version,
            &proof.authorization_record(nonce, epoch),
        )
        .map_err(|error| ReplacementDispatchFailure {
            old_stopped: error.old_stopped,
            message: error.message,
        })?;
    if generation(env, id).map_err(|message| ReplacementDispatchFailure {
        old_stopped: true,
        message,
    })? != initial_generation
    {
        return Err(ReplacementDispatchFailure {
            old_stopped: true,
            message: REFUSED.into(),
        });
    }
    Ok(initial_generation)
}

pub(super) fn preferences(
    env: &Env,
    id: &str,
    hidden: Option<bool>,
    auto_connect: Option<bool>,
) -> Result<(), String> {
    let registry = registry(env)?;
    let mut binding = registry
        .binding(id)?
        .ok_or("Set up this private machine first")?;
    if auto_connect == Some(true) && !matches!(binding.setup, PrivateSetup::Ready { .. }) {
        return Err("Set up this private machine first".into());
    }
    let old_revision = binding.revision;
    if let Some(hidden) = hidden {
        binding.hidden = hidden;
    }
    if let Some(auto_connect) = auto_connect {
        binding.auto_connect = auto_connect;
    }
    binding.revision = registry
        .binding_generation(id)?
        .checked_add(1)
        .ok_or(REFUSED)?;
    registry.save_binding(id, binding, Some(old_revision))
}
pub(super) fn remove(env: &Env, store: &dyn CredentialStore, id: &str) -> Result<(), String> {
    let registry = registry(env)?;
    let prior = registry.remove_declaration(id)?;
    if let Some(handle) = prior.and_then(|binding| binding.credential_ref) {
        passphrase::forget(store, &handle)?;
    }
    match std::fs::remove_file(env.paths().pin_file(id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
pub(super) fn unshare(env: &Env, id: &str) -> Result<MachineSummary, String> {
    registry(env)?.unshare(id)?;
    summary_of(env, id)
}

/// Personal permission is valid only for this owner’s ready private binding.
pub(super) fn authorize_remote_mcp_control(env: &Env, id: &str) -> Result<(), String> {
    super::identity::validate_saved(env, id, None)?;
    let binding = registry(env)?.binding(id)?.ok_or(REFUSED)?;
    if !agent_callbacks(Some(&binding)) {
        return Err("Remote MCP control is not enabled for this private machine".into());
    }
    authority(env)?.lease().validate()
}

pub(super) fn set_remote_mcp_control(env: &Env, id: &str, enabled: bool) -> Result<(), String> {
    let registry = registry(env)?;
    let mut binding = registry
        .binding(id)?
        .ok_or("Set up this private machine first")?;
    if !matches!(binding.setup, PrivateSetup::Ready { .. }) {
        return Err("Set up this private machine first".into());
    }
    if binding.remote_mcp_control == enabled {
        return authority(env)?.lease().validate();
    }
    let old_revision = binding.revision;
    binding.remote_mcp_control = enabled;
    binding.revision = registry
        .binding_generation(id)?
        .checked_add(1)
        .ok_or(REFUSED)?;
    registry.save_binding(id, binding, Some(old_revision))
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    fn binding(setup: PrivateSetup, auto_connect: bool) -> PrivateBinding {
        PrivateBinding {
            username: Some("dev".into()),
            key_path: None,
            credential_ref: None,
            setup,
            host_key: None,
            declaration_revision: String::new(),
            revision: 1,
            auto_connect,
            remote_mcp_control: false,
            hidden: false,
            restore_auto_connect: false,
            open_tabs: false,
        }
    }

    fn ready() -> PrivateSetup {
        PrivateSetup::Ready {
            account: PrivateAccount {
                uid: 1000,
                canonical_home: "/home/dev".into(),
                root_version: 1,
                enrollment_id: "e".into(),
            },
            server_id: "server".into(),
        }
    }

    fn team() -> Scope {
        Scope::Team {
            org_slug: "acme".into(),
            team: "core".into(),
        }
    }

    /// Agents on a machine call back into this app only over the reverse relay, which
    /// needs a ready private binding with remote MCP control on.
    #[test]
    fn agent_callbacks_follow_remote_mcp_control_on_a_ready_binding() {
        assert!(!agent_callbacks(None));
        let mut on = binding(ready(), false);
        assert!(!agent_callbacks(Some(&on)), "remote MCP control off");
        on.remote_mcp_control = true;
        assert!(agent_callbacks(Some(&on)));
        let mut unfinished = binding(PrivateSetup::NeedsEnrollment, false);
        unfinished.remote_mcp_control = true;
        assert!(!agent_callbacks(Some(&unfinished)), "setup not ready");
    }

    #[test]
    fn a_new_personal_machine_connects_on_launch_by_default() {
        assert!(saved_auto_connect(None, true, &Scope::Personal));
        let draft = binding(PrivateSetup::NeedsEnrollment, false);
        assert!(saved_auto_connect(Some(&draft), true, &Scope::Personal));
    }

    #[test]
    fn a_team_machine_or_an_unfinished_setup_stays_connect_on_select() {
        assert!(!saved_auto_connect(None, true, &team()));
        assert!(!saved_auto_connect(None, false, &Scope::Personal));
    }

    #[test]
    fn a_later_save_keeps_the_persons_choice() {
        let off = binding(ready(), false);
        assert!(!saved_auto_connect(Some(&off), true, &Scope::Personal));
        let on = binding(ready(), true);
        assert!(saved_auto_connect(Some(&on), true, &team()));
    }

    #[test]
    fn a_passphrase_machine_never_connects_with_nobody_watching() {
        let mut passphrase = binding(ready(), true);
        assert!(background_auto_connect(&passphrase));
        passphrase.credential_ref = Some(CredentialRef {
            provider: "ssh".into(),
            account: "k".into(),
        });
        assert!(!background_auto_connect(&passphrase));
    }

    #[test]
    fn an_unreachable_first_scan_names_the_address() {
        let input = MachineDraftInput {
            mode: Some(MachineMode::PrivateAccount),
            label: String::new(),
            host: "203.0.113.10".into(),
            port: 22,
            username: Some("root".into()),
            jump_host: None,
            sign_in: MachineSignIn::Agent,
            trusted_host_key_fp: None,
        };
        assert_eq!(
            unreachable_detail(&input, "Operation timed out\n"),
            "Couldn't reach 203.0.113.10:22: Operation timed out"
        );
        assert_eq!(
            unreachable_detail(&input, ""),
            "Couldn't reach 203.0.113.10:22"
        );
    }
}
