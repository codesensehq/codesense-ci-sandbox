// SPDX-License-Identifier: MIT
use super::{Actor, BodyEdit, Collection, Scope, Store};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Scope roots captured when the host mints a frame or resolves an agent session.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    #[serde(skip)]
    pub(crate) caller: crate::context_repo::caller_scope::CallerScope,
    pub profile: String,
    pub org: String,
    pub team: Option<String>,
    pub private_root: PathBuf,
    pub scratch_repo: PathBuf,
    pub team_root: Option<PathBuf>,
}
impl Binding {
    pub(crate) fn for_caller(
        mut self,
        caller: crate::context_repo::caller_scope::CallerScope,
    ) -> Self {
        self.caller = caller;
        self
    }

    pub fn from_context(
        home: &Path,
        profile: &crate::domain::Principal,
        org: &str,
        team: Option<&str>,
    ) -> Result<Self, String> {
        let private_root = crate::context_repo::scratch_space::ensure_org_space(home, profile, org)
            .map_err(|e| e.to_string())?
            .0
            .root();
        let team_root = team
            .map(|team| {
                let org_root = crate::context_repo::path_to_org_root(home, profile, org)?;
                let content = crate::context_repo::path_to_team_content_dir(&org_root, team);
                Ok::<_, String>(content.is_dir().then(|| org_root.join(team)))
            })
            .transpose()?
            .flatten();
        Ok(Self {
            caller: Default::default(),
            profile: profile.to_string(),
            org: org.into(),
            team: team.map(str::to_owned),
            private_root,
            scratch_repo: crate::context_repo::scratch_space::scratch_root(home, profile),
            team_root,
        })
    }
    pub fn collections(&self) -> Result<Vec<(PathBuf, Collection)>, String> {
        let mut out =
            super::schema::load_collections_for(&self.private_root, Scope::Private, &self.caller)?
                .into_iter()
                .map(|c| (self.private_root.clone(), c))
                .collect::<Vec<_>>();
        if let Some(root) = &self.team_root {
            out.extend(
                super::schema::load_collections_for(root, Scope::Team, &self.caller)?
                    .into_iter()
                    .map(|c| (root.clone(), c)),
            );
        }
        Ok(out)
    }
    pub fn resolve(&self, name: &str) -> Result<Store, String> {
        let store = self.resolve_declared(name)?;
        if store.collection.scope == Scope::Private && name == "notes" {
            super::notes::ensure_notes_migrated(&self.private_root)?;
        }
        Ok(store)
    }
    pub(crate) fn resolve_declared(&self, name: &str) -> Result<Store, String> {
        let mut matching = self
            .collections()?
            .into_iter()
            .filter(|(_, c)| c.name == name);
        let (root, collection) = matching
            .next()
            .ok_or_else(|| format!("unknown collection: {name}"))?;
        if matching.next().is_some() {
            return Err(format!(
                "ambiguous collection: {name}; give private and team collections distinct names"
            ));
        }
        Store::new(
            &root,
            collection.clone(),
            (collection.scope == Scope::Private).then(|| self.scratch_repo.clone()),
        )
        .map(|store| store.for_transport().with_caller(self.caller.clone()))
    }
    pub fn view_key(&self, store: &Store) -> String {
        let scope = if store.collection.scope == Scope::Private {
            "private"
        } else {
            "team"
        };
        let owner = if scope == "private" {
            self.org.clone()
        } else {
            format!("{}/{}", self.org, self.team.as_deref().unwrap_or(""))
        };
        let key = format!("records:{scope}:{owner}:{}", store.collection.name);
        if matches!(
            self.caller,
            crate::context_repo::caller_scope::CallerScope::Human
        ) {
            key
        } else {
            format!("{key}:{}", self.caller.cache_key())
        }
    }
}
pub fn capability(scope: Scope, write: bool) -> &'static str {
    match (scope == Scope::Private, write) {
        (true, false) => "records:read:own",
        (true, true) => "records:write:own",
        (false, false) => "records:read:team",
        (false, true) => "records:write:team",
    }
}
pub fn is_write(operation: &str) -> bool {
    matches!(
        operation,
        "create" | "write" | "setFields" | "delete" | "restore"
    )
}
fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .ok_or_else(|| format!("{key} is required"))
}
pub fn execute(
    binding: &Binding,
    operation: &str,
    args: &Value,
    actor: &Actor,
    index: Option<crate::context_query::ContextIndexService>,
    grants: Option<&[String]>,
) -> Result<Value, String> {
    if is_write(operation)
        && !matches!(
            binding.caller,
            crate::context_repo::caller_scope::CallerScope::Human
        )
    {
        // The write keeps main's policy. Its result may contain retained fields/body.
        let baseline = binding.clone().for_caller(Default::default());
        let result = execute(&baseline, operation, args, actor, index, grants);
        let store = binding.resolve(required(args, "collection")?)?;
        if let Some(id) = args["id"].as_str().or_else(|| {
            result
                .as_ref()
                .ok()
                .and_then(|value| value["record"]["id"].as_str())
        }) {
            store.require_record(id)?;
        }
        return result;
    }
    if ["org", "team", "profile", "githubUserId"]
        .iter()
        .any(|k| args.get(*k).is_some())
    {
        return Err("denied: scope is fixed by the caller grant".into());
    }
    if operation == "collections" {
        let collections = binding
            .collections()?
            .into_iter()
            .map(|(_, c)| c)
            .filter(|c| grants.is_none_or(|g| g.iter().any(|s| s == capability(c.scope, false))))
            .collect::<Vec<_>>();
        return serde_json::to_value(collections).map_err(|e| e.to_string());
    }
    let mut store = binding.resolve(required(args, "collection")?)?;
    if let Some(index) = index {
        store = store.with_index(index);
    }
    let request = args["requestId"].as_str();
    let fields = args["fields"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let id = || required(args, "id");
    let result = match operation {
        "list" => {
            let records = store.list()?;
            let rows = records
                .iter()
                .map(|r| super::projection::row(&store, r))
                .collect::<Vec<_>>();
            let start = args["window"][0].as_u64().unwrap_or(0) as usize;
            let count = (args["window"][1].as_u64().unwrap_or(200) as usize)
                .saturating_sub(start)
                .min(200);
            json!({"rows":rows.into_iter().skip(start).take(count).collect::<Vec<_>>(),"total":records.len()})
        }
        "read" => json!(store.read(id()?)?),
        "search" => {
            let query = required(args, "query")?;
            let limit = args["limit"].as_u64().unwrap_or(20).min(200) as usize;
            json!(store.search(query, limit)?)
        }
        "history" => json!(store.history(
            id()?,
            args["limit"].as_u64().unwrap_or(50).min(500) as usize
        )?),
        "readAt" => json!(store.read_at(id()?, required(args, "commit")?)?),
        "create" => json!(store.create(
            args["id"].as_str(),
            &fields,
            args["body"].as_str().unwrap_or(""),
            actor,
            request
        )?),
        "setFields" => json!(store.set_fields(id()?, &fields, actor, request)?),
        "delete" => json!(store.delete(id()?, actor, request)?),
        "restore" => json!(store.restore(
            id()?,
            required(args, "commit")?,
            args["undo"].as_bool().unwrap_or(false),
            actor,
            request
        )?),
        "write" => {
            let count = ["body", "append", "patch"]
                .iter()
                .filter(|k| args.get(**k).is_some())
                .count();
            if count > 1 {
                return Err("give one of body, append or patch".into());
            }
            let edit = if let Some(body) = args["body"].as_str() {
                match args["base"].as_str() {
                    Some(base) => BodyEdit::ReplaceFrom {
                        base: base.into(),
                        content: body.into(),
                    },
                    None => BodyEdit::Replace(body.into()),
                }
            } else if let Some(append) = args["append"].as_str() {
                BodyEdit::Append(append.into())
            } else if args["patch"].is_object() {
                BodyEdit::Patch {
                    find: required(&args["patch"], "find")?.into(),
                    replace: required(&args["patch"], "replace")?.into(),
                }
            } else if !fields.is_empty() {
                BodyEdit::Keep
            } else {
                return Err("give body, append, patch or fields".into());
            };
            json!(store.write(id()?, &edit, &fields, actor, request)?)
        }
        _ => return Err(format!("unknown records operation: {operation}")),
    };
    Ok(result)
}
