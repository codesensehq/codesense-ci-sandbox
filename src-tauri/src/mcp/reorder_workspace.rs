// SPDX-License-Identifier: MIT
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReorderWorkspaceParams {
    pub team_key: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

impl ReorderWorkspaceParams {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.team_key.trim().is_empty() || self.workspace_id.trim().is_empty() {
            return Err("teamKey and workspaceId must be non-empty strings");
        }
        match (&self.before, &self.after) {
            (Some(anchor), None) | (None, Some(anchor)) if !anchor.trim().is_empty() => Ok(()),
            _ => Err("exactly one non-empty before or after workspace id is required"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reorder_workspace_validates_args_and_preserves_page_field_names() {
        for place in [json!({"before":"a"}), json!({"after":"a"})] {
            let mut args = json!({"teamKey":"acme:core","workspaceId":"b"});
            args.as_object_mut()
                .unwrap()
                .extend(place.as_object().unwrap().clone());
            let params: ReorderWorkspaceParams = serde_json::from_value(args.clone()).unwrap();
            assert!(params.validate().is_ok());
            assert_eq!(serde_json::to_value(params).unwrap(), args);
        }
        for args in [
            json!({"teamKey":"acme:core","workspaceId":"b"}),
            json!({"teamKey":"acme:core","workspaceId":"b","before":"a","after":"c"}),
            json!({"teamKey":"","workspaceId":"b","before":"a"}),
            json!({"teamKey":"acme:core","workspaceId":"","before":"a"}),
            json!({"teamKey":"acme:core","workspaceId":"b","before":""}),
        ] {
            assert!(serde_json::from_value::<ReorderWorkspaceParams>(args)
                .unwrap()
                .validate()
                .is_err());
        }
        assert!(serde_json::from_value::<ReorderWorkspaceParams>(
            json!({"workspaceId":"b","before":"a"})
        )
        .is_err());
        let router = crate::mcp::handler::CodesenseHandler::served_tool_router();
        let tool = router
            .map
            .get("reorder_workspace")
            .expect("reorder tool registered");
        assert_eq!(tool.attr.input_schema.get("type"), Some(&json!("object")));
        for name in ["teamKey", "workspaceId", "before", "after"] {
            assert!(tool.attr.input_schema["properties"].get(name).is_some());
        }
        assert!(tool
            .attr
            .description
            .as_deref()
            .unwrap()
            .contains("machine"));
    }
}
