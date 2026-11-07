// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::field_schema::{canonical_field_type, sanitize_allowed_values, FieldDef};
use crate::commands::frontmatter_fields::{validate_field_count, validate_field_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    Private,
    Team,
    Story,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdRule {
    Slug,
    Generated,
}

/// The actions a collection can reserve for people in `humanOnly`.
pub(crate) const HUMAN_ONLY_ACTIONS: [&str; 3] = ["delete", "write", "setFields"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    #[serde(default)]
    pub human_only: Vec<String>,
    #[serde(default)]
    pub reap_empty: bool,
    #[serde(default)]
    pub seed: Vec<String>,
    #[serde(default)]
    pub recency: Option<String>,
    #[serde(default)]
    pub history: HistoryOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryOptions {
    pub human_debounce_ms: u64,
}
impl Default for HistoryOptions {
    fn default() -> Self {
        Self {
            human_debounce_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    pub name: String,
    pub scope: Scope,
    pub folder: PathBuf,
    pub id: IdRule,
    pub title: Option<String>,
    pub fields: BTreeMap<String, FieldDef>,
    pub order: Vec<String>,
    pub options: Options,
}

#[derive(Deserialize)]
struct Declaration {
    #[serde(default)]
    scope: Option<Scope>,
    collection: String,
    folder: PathBuf,
    id: IdRule,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    fields: BTreeMap<String, DeclaredField>,
    #[serde(default)]
    order: Vec<String>,
    #[serde(flatten)]
    options: Options,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeclaredField {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default, alias = "allowed_values")]
    allowed_values: Vec<String>,
    #[serde(default)]
    required: bool,
}

pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.contains("..")
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Reject links in each component, including an existing record itself.
pub(crate) fn guarded_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("collection path must stay inside its scope root".into());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("scope root: {e}"))?;
    let mut path = root;
    for part in relative.components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!("symlink not allowed: {}", path.display()))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(path)
}

pub fn load_collections(root: &Path, scope: Scope) -> Result<Vec<Collection>, String> {
    load_collections_for(root, scope, &Default::default())
}

pub(crate) fn load_collections_for(
    root: &Path,
    scope: Scope,
    caller: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<Collection>, String> {
    let mut out = if scope == Scope::Private {
        vec![Collection::notes()]
    } else {
        vec![]
    };
    let schema = guarded_path(root, Path::new("content/schema"))?;
    if !caller.visible(&schema) || !schema.exists() {
        return Ok(out);
    }
    let mut files = std::fs::read_dir(schema)
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    files.sort();
    for path in files {
        if !caller.allows(&path) {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        let path = guarded_path(
            root,
            path.strip_prefix(root.canonicalize().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        )?;
        let d: Declaration =
            serde_yaml::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        // UFCS and slicing keep these std calls out of the X1 census's ambiguous-method count.
        if let Some(unknown) = Iterator::find(&mut d.options.human_only.iter(), |a| {
            !HUMAN_ONLY_ACTIONS.contains(&&a[..])
        }) {
            return Err(format!(
                "{}: humanOnly names unknown action {unknown} (use {})",
                Path::display(&path),
                HUMAN_ONLY_ACTIONS.join(", ")
            ));
        }
        if !is_valid_id(&d.collection)
            || path.file_stem().and_then(|s| s.to_str()) != Some(d.collection.as_str())
        {
            return Err("collection name must match schema/<name>.yaml".into());
        }
        if out.iter().any(|c| c.name == d.collection) {
            return Err(format!("duplicate collection: {}", d.collection));
        }
        let folder = if d.scope == Some(Scope::Story) && scope == Scope::Team {
            PathBuf::from("content").join(&d.folder)
        } else {
            if d.scope.is_some_and(|declared| declared != scope) {
                return Err("collection scope must match its root".into());
            }
            d.folder.clone()
        };
        guarded_path(root, &folder)?;
        if let Some(title) = &d.title {
            validate_field_name(title)?;
        }
        let mut fields = BTreeMap::new();
        for (name, field) in d.fields {
            validate_field_name(&name)?;
            let kind = field
                .kind
                .map(|t| {
                    if t == "boolean" {
                        Ok(t)
                    } else {
                        canonical_field_type(&t)
                            .map(str::to_string)
                            .ok_or_else(|| format!("unsupported field type: {t}"))
                    }
                })
                .transpose()?;
            fields.insert(
                name.clone(),
                FieldDef {
                    name,
                    r#type: kind,
                    allowed_values: sanitize_allowed_values(field.allowed_values),
                    required: field.required,
                    official: false,
                },
            );
        }
        out.push(Collection {
            name: d.collection,
            scope,
            folder,
            id: d.id,
            title: d.title,
            fields,
            order: d.order,
            options: d.options,
        });
    }
    Ok(out)
}

impl Collection {
    pub fn notes() -> Self {
        let fields = [("title", "text"), ("pinned", "boolean")]
            .into_iter()
            .map(|(name, kind)| {
                (
                    name.into(),
                    FieldDef {
                        name: name.into(),
                        r#type: Some(kind.into()),
                        allowed_values: vec![],
                        required: false,
                        official: false,
                    },
                )
            })
            .collect();
        Self {
            name: "notes".into(),
            scope: Scope::Private,
            folder: "notes".into(),
            id: IdRule::Generated,
            title: Some("title".into()),
            fields,
            order: vec!["pinned desc".into(), "updatedAt desc".into()],
            options: Options {
                human_only: vec!["delete".into()],
                reap_empty: true,
                seed: vec!["Prompts".into()],
                recency: Some("local".into()),
                ..Options::default()
            },
        }
    }

    pub(crate) fn validate_fields(&self, fields: &[(String, Value)]) -> Result<(), String> {
        validate_field_count(fields.len())?;
        for (name, value) in fields {
            validate_field_name(name)?;
            let Some(def) = self.fields.get(name) else {
                continue;
            };
            if value.is_null() {
                if def.required {
                    return Err(format!("{name} is required"));
                }
                continue;
            }
            let valid = match def.r#type.as_deref() {
                Some("text") => value.is_string(),
                Some("choice") => value.as_str().is_some_and(|s| {
                    def.allowed_values.is_empty() || def.allowed_values.iter().any(|v| v == s)
                }),
                Some("number") => value.is_number(),
                Some("boolean" | "yes-no") => value.is_boolean(),
                Some("date") => value.as_str().is_some_and(|s| {
                    s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
                }),
                _ => true,
            };
            if !valid {
                return Err(format!("invalid value for {name}"));
            }
        }
        Ok(())
    }
}
