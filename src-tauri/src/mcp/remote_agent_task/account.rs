// SPDX-License-Identifier: MIT
//! Native account selection; credential contents remain owned by the official CLI.
use crate::{domain::Principal, install::agent_account_store as accounts};
use std::path::PathBuf;

#[derive(Clone)]
pub(super) struct Account {
    root: PathBuf,
    principal: Principal,
    provider: accounts::AgentProvider,
    requested: Option<(String, u64)>,
    selected: accounts::ResolvedAgentAccount,
    pub(super) home: Option<PathBuf>,
}
impl Account {
    pub(super) fn resolve(
        root: PathBuf,
        principal: Principal,
        provider: accounts::AgentProvider,
        id: Option<String>,
        generation: Option<u64>,
    ) -> Result<Self, String> {
        let requested = match (id, generation) {
            (Some(id), Some(generation)) => Some((id, generation)),
            (None, None) => None,
            _ => return Err("accountId and accountGeneration must be supplied together".into()),
        };
        let selected = select(&root, &principal, provider, requested.as_ref())?;
        let home = selected
            .account_id
            .as_ref()
            .map(|id| {
                accounts::managed_home(&root, &principal, id)
                    .map_err(|_| "account home unavailable".to_string())
            })
            .transpose()?;
        Ok(Self {
            root,
            principal,
            provider,
            requested,
            selected,
            home,
        })
    }
    pub(super) fn validate(&self) -> Result<(), String> {
        let current = select(
            &self.root,
            &self.principal,
            self.provider,
            self.requested.as_ref(),
        )?;
        if current.account_id != self.selected.account_id
            || current.generation != self.selected.generation
            || current.provider != self.selected.provider
            || current.source != self.selected.source
        {
            return Err("selected account was retired or changed".into());
        }
        if self.home.as_ref().is_some_and(|home| !home.is_dir()) {
            return Err("selected account home unavailable".into());
        }
        Ok(())
    }
    pub(super) fn provenance(&self) -> serde_json::Value {
        serde_json::json!({"source":self.selected.source,"accountId":self.selected.account_id,"generation":self.selected.generation,"provider":self.provider.as_str()})
    }
}
fn select(
    root: &std::path::Path,
    principal: &Principal,
    provider: accounts::AgentProvider,
    requested: Option<&(String, u64)>,
) -> Result<accounts::ResolvedAgentAccount, String> {
    let profiles =
        accounts::list_profiles(root, principal).map_err(|_| "account profiles unavailable")?;
    let active = accounts::get_active_account(root, principal, provider)
        .map_err(|_| "active account unavailable")?;
    let selected = accounts::resolve_agent_account(
        accounts::AccountRequirement::Optional,
        provider,
        &profiles,
        active.as_ref(),
        requested.map(|v| v.0.as_str()),
        requested.map(|v| v.1),
    );
    if !selected.spawn_eligible {
        return Err("selected account unavailable; native login required".into());
    }
    Ok(selected)
}
