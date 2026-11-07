// SPDX-License-Identifier: MIT
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordsParams {
    pub collection: Option<String>,
    /// Org containing the collection; defaults to the session's org.
    pub org: Option<String>,
    /// Team for a shared collection; defaults to the session's team.
    pub team: Option<String>,
    /// Identity comes from the verified connection; a supplied id must name the same caller.
    pub session_id: Option<String>,
    pub id: Option<String>,
    pub fields: Option<std::collections::BTreeMap<String, Value>>,
    pub body: Option<String>,
    pub base: Option<String>,
    pub append: Option<String>,
    /// Edits to apply to the record's body, as a JSON value.
    #[schemars(with = "Option<super::AnyJson>")]
    pub patch: Option<serde_json::Value>,
    pub request_id: Option<String>,
    pub query: Option<String>,
    pub limit: Option<usize>,
    pub window: Option<[usize; 2]>,
    pub commit: Option<String>,
    pub undo: Option<bool>,
}
impl RecordsParams {
    pub fn args(&self) -> Value {
        let mut args = serde_json::to_value(self).expect("record parameters are JSON");
        args.as_object_mut()
            .unwrap()
            .retain(|k, v| !matches!(k.as_str(), "org" | "team" | "sessionId") && !v.is_null());
        args
    }
}
pub fn run(
    binding: &crate::records::binding::Binding,
    operation: &str,
    params: &RecordsParams,
    actor: &crate::records::Actor,
    index: Option<crate::context_query::ContextIndexService>,
) -> Result<Value, String> {
    crate::records::binding::execute(binding, operation, &params.args(), actor, index, None)
}

/// Apply the named caller's context before consulting any ambient UI state.
pub(crate) fn bind_defaults(
    params: &mut RecordsParams,
    context: Option<&crate::commands::dashboard_launch::LaunchTeam>,
) {
    if let Some(context) = context {
        if params.org.is_none() {
            params.org = Some(context.org.clone());
        }
        if params.team.is_none()
            && params.org.as_deref() == Some(&context.org)
            && !context.team.is_empty()
        {
            params.team = Some(context.team.clone());
        }
    }
}
