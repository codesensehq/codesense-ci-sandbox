// SPDX-License-Identifier: MIT
//! Shared teaching links and search vocabulary for the served MCP registry.
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Deserialize)]
pub(super) struct Catalog {
    #[cfg(test)]
    pub schema: u32,
    pub families: Vec<Family>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Family {
    pub id: String,
    pub title: String,
    pub docs: String,
    pub search_terms: Vec<String>,
    pub tools: Vec<String>,
    #[cfg(test)]
    pub calls: Vec<serde_json::Value>,
}
pub(super) fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_yaml::from_str(include_str!(
            "../../../codesense-docs/getting-started/content/_shared/examples.yml"
        ))
        .expect("valid MCP family catalog")
    })
}
pub(super) fn family(name: &str) -> Option<&'static Family> {
    catalog()
        .families
        .iter()
        .find(|f| f.tools.iter().any(|t| t == name))
}

/// Name and curated family vocabulary share the first tier; prose is secondary.
/// Catalog order resolves family ties, followed by the exact tool name.
pub(super) fn rank(name: &str, description: &str, terms: &[String]) -> Option<(i32, usize)> {
    let family = family(name);
    let name = name.to_ascii_lowercase();
    let description = description.to_ascii_lowercase();
    let vocabulary = family
        .map(|f| format!("{} {} {}", f.id, f.title, f.search_terms.join(" ")).to_ascii_lowercase())
        .unwrap_or_default();
    let mut score = 0;
    for term in terms {
        if name == *term {
            score += 200;
        } else if name.contains(term) || vocabulary.contains(term) {
            score += 100;
        } else if description.contains(term) {
            score += 1;
        } else {
            return None;
        }
    }
    let order = family
        .and_then(|f| f.tools.iter().position(|t| t == &name))
        .unwrap_or(usize::MAX);
    Some((score, order))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::handler::CodesenseHandler;
    #[test]
    fn registry_reference_export() {
        let tools = CodesenseHandler::served_tool_router().list_all();
        if let Ok(path) = std::env::var("MCP_REFERENCE_OUTPUT") {
            let mut sorted = tools.clone();
            sorted.sort_by(|a, b| a.name.cmp(&b.name));
            std::fs::write(
                path,
                serde_json::to_string_pretty(&serde_json::json!({"tools":sorted})).unwrap() + "\n",
            )
            .unwrap();
        }
        // The one `dashboard-publish` build also names the tools that feature gates, so the
        // exporter derives the default view instead of compiling the crate a second time.
        #[cfg(feature = "dashboard-publish")]
        if let Ok(path) = std::env::var("MCP_REFERENCE_GATED_OUTPUT") {
            let mut gated: Vec<_> = CodesenseHandler::publish_tool_router()
                .list_all()
                .into_iter()
                .map(|t| t.name)
                .collect();
            gated.sort();
            std::fs::write(path, serde_json::to_string(&gated).unwrap()).unwrap();
        }
        let catalog = catalog();
        assert_eq!(catalog.schema, 1);
        let docs_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../codesense-docs/getting-started/content");
        let mut seen = std::collections::HashSet::new();
        for f in &catalog.families {
            assert!(!f.calls.is_empty(), "{} needs an example", f.id);
            let (page, anchor) = f
                .docs
                .split_once('#')
                .map(|(p, a)| (p, Some(a)))
                .unwrap_or((&f.docs, None));
            let page = std::fs::read_to_string(docs_root.join(page))
                .unwrap_or_else(|e| panic!("{}: {e}", f.docs));
            if let Some(anchor) = anchor {
                assert!(
                    page.contains(&format!("id=\"{anchor}\""))
                        || page.contains(&format!("id='{anchor}'")),
                    "{}",
                    f.docs
                );
            }
            for name in &f.tools {
                assert!(seen.insert(name), "duplicate family for {name}");
            }
            for call in &f.calls {
                let name = call["tool"].as_str().expect("example tool");
                assert!(f.tools.iter().any(|t| t == name));
                if let Some(tool) = tools.iter().find(|t| t.name == name) {
                    let schema = serde_json::to_value(&tool.input_schema).unwrap();
                    let validator = jsonschema::validator_for(&schema).unwrap();
                    if let Err(e) = validator.validate(&call["arguments"]) {
                        panic!("{} example: {e}", f.id);
                    }
                }
            }
        }
        for tool in &tools {
            let f = family(&tool.name).unwrap_or_else(|| panic!("{} missing family", tool.name));
            let desc = tool.description.as_deref().unwrap();
            assert_eq!(
                desc.lines().filter(|l| l.starts_with("Docs: ")).count(),
                1,
                "{}",
                tool.name
            );
            assert!(
                desc.ends_with(&format!("Docs: {}", f.docs)),
                "{}",
                tool.name
            );
        }
    }
    #[test]
    fn dashboard_search_finds_pane_writer() {
        let mut hits: Vec<_> = CodesenseHandler::served_tool_router()
            .list_all()
            .into_iter()
            .filter_map(|t| {
                rank(
                    &t.name,
                    t.description.as_deref().unwrap_or(""),
                    &["dashboard".into()],
                )
                .map(|(score, order)| (score, order, t.name.to_string()))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        assert!(hits.iter().take(3).any(|t| t.2 == "pane_write"), "{hits:?}");
    }
}
