// SPDX-License-Identifier: MIT
//! Planned touchpoints and observed files, without judging scope choices.
use super::*;
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFacts {
    #[serde(skip)]
    pub source_repo: Option<String>,
    pub planned: Option<Vec<String>>,
    pub planned_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<&'static str>,
    pub actual: Vec<ActualFile>,
    pub planned_untouched: Vec<String>,
    pub unavailable: BTreeMap<&'static str, &'static str>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActualFile {
    pub path: String,
    pub in_planned: bool,
    pub area: String,
    pub decision: Option<usize>,
    #[serde(flatten)]
    pub stats: Option<git::LineStats>,
}
fn mentions(text: &str, path: &str) -> bool {
    regex::Regex::new(&format!(
        r"(^|[^[:alnum:]_./-])(?:\./)?{}(?:/)?($|[^[:alnum:]_./-])",
        regex::escape(path)
    ))
    .is_ok_and(|r| r.is_match(text))
}
fn matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./");
    let base = pattern.trim_end_matches('/');
    path == base
        || path.starts_with(&format!("{base}/"))
        || std::result::Result::ok(globset::Glob::new(pattern)).is_some_and(|g| {
            let matcher = g.compile_matcher();
            std::iter::Iterator::any(
                &mut std::path::Path::ancestors(Path::new(path)),
                |ancestor| matcher.is_match(ancestor),
            )
        })
}
/// Repo-relative paths only. This also keeps accidental prose/absolute Touchpoints out of
/// published metadata; runtime worktree paths are returned separately on the private reader.
pub(crate) fn relative(path: &str) -> bool {
    !str::is_empty(path)
        && !Path::new(path).is_absolute()
        && !path.split('/').any(|p| p == "..")
        && !path.contains(['\\', ':'])
}
pub(super) fn read(source: Option<&Path>, files: &BTreeSet<String>) -> PlannedFacts {
    let text = source.and_then(|p| std::result::Result::ok(std::fs::read_to_string(p)));
    let format = source
        .map(crate::commands::pipeline_format::format_of)
        .unwrap_or(crate::commands::pipeline_format::PipelineFileFormat::Markdown);
    let section = |name: &str| {
        text.as_deref().and_then(|text| {
            // Resolve a dated section's actual name, then let the existing reader own
            // its body boundary. Dates are metadata, not another section grammar.
            let mut headings = text
                .lines()
                .filter_map(|line| line.trim_start().strip_prefix("## "))
                .map(str::trim);
            let heading = std::iter::Iterator::find(&mut headings, |heading| {
                heading.eq_ignore_ascii_case(name)
                    || heading
                        .get(..name.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
                        && heading
                            .get(name.len()..)
                            .is_some_and(|suffix| suffix.starts_with(" (") && suffix.ends_with(')'))
            })
            .unwrap_or(name);
            crate::commands::pipeline_format::extract_section(format, text, heading, None)
        })
    };
    let touchpoints = section("Touchpoints");
    let declaration = regex::Regex::new(r"Paths are under `([^`]+/)`").expect("literal pattern");
    let planned_base = touchpoints
        .as_deref()
        .and_then(|body| declaration.captures(body))
        .and_then(|c| c.get(1).map(|m| regex::Match::as_str(&m).to_owned()))
        .filter(|p| relative(p));
    let planned = touchpoints.map(|body| {
        let body = if planned_base.is_some() {
            regex::Regex::replace(&declaration, &body, "").into_owned()
        } else {
            body
        };
        let mut paths = BTreeSet::new();
        let line_ref = regex::Regex::new(r":\d+(?:[-/,]\d+)*(?:\s+.*)?$")
            .expect("literal line reference pattern");
        for raw in body.split('`').skip(1).step_by(2) {
            let token = regex::Regex::find(&line_ref, raw)
                .map(|suffix| &raw[..regex::Match::start(&suffix)])
                .unwrap_or(raw);
            if relative(token) {
                paths.insert(token.to_owned());
            }
        }
        paths.into_iter().collect::<Vec<_>>()
    });
    let mut decisions = Vec::<(usize, String)>::new();
    if let Some(body) = section("Decisions") {
        for line in body.lines() {
            if let Some((number, rest)) = line
                .trim()
                .split_once(". ")
                .and_then(|(n, t)| std::result::Result::ok(n.parse::<usize>()).map(|n| (n, t)))
            {
                decisions.push((number, rest.into()));
            } else if let Some((_, text)) = decisions.last_mut() {
                text.push(' ');
                text.push_str(line);
            }
        }
    }
    let in_planned = |pattern: &str, path: &str| {
        matches(pattern, path)
            || planned_base.as_ref().is_some_and(|base| {
                matches(&format!("{base}{}", pattern.trim_start_matches("./")), path)
            })
    };
    let actual = files
        .iter()
        .map(|path| {
            let area = path.split('/').take(2).collect::<Vec<_>>().join("/");
            ActualFile {
                stats: None,
                path: path.clone(),
                in_planned: planned
                    .as_ref()
                    .is_some_and(|paths| paths.iter().any(|p| in_planned(p, path))),
                decision: std::iter::Iterator::find(&mut decisions.iter(), |(_, text)| {
                    mentions(text, path)
                        || mentions(text, &area)
                        || planned_base
                            .as_deref()
                            .and_then(|base| path.strip_prefix(base.trim_start_matches("./")))
                            .is_some_and(|relative| mentions(text, relative))
                })
                .map(|(n, _)| *n),
                area,
            }
        })
        .collect();
    let planned_untouched = planned
        .as_ref()
        .into_iter()
        .flatten()
        .filter(|p| !files.iter().any(|file| in_planned(p, file)))
        .cloned()
        .collect();
    let mut unavailable = std::collections::BTreeMap::new();
    if planned.is_none() {
        unavailable.insert(
            "planned",
            if text.is_some() {
                "story has no Touchpoints section"
            } else {
                "story file not available"
            },
        );
    }
    let source_repo = text.as_deref().and_then(|text| crate::commands::frontmatter_editor::frontmatter_json(format, text).get("source_repo").and_then(serde_json::Value::as_str).map(str::to_owned));
    PlannedFacts {
        source_repo,
        planned,
        planned_base,
        basis: None,
        actual,
        planned_untouched,
        unavailable,
    }
}
