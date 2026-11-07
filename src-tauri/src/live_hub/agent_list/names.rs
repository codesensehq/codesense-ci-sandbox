// SPDX-License-Identifier: MIT
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

fn compact(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn sanitize(value: Option<&str>, literal: bool) -> Option<(String, String)> {
    static SKILL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([$/])[a-z_][a-z0-9_]*(?:[-:][a-z0-9_]+)+(?:\s+|$)").unwrap()
    });
    static READ: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^read\s+(.+?)\s+(?:and\s+)?follow\s+it\b.*$").unwrap());
    static PATH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:[A-Za-z]:)?[/\\]?[\w~ .-]+(?:[/\\][\w .-]+)+[/\\]?$").unwrap()
    });
    static CAPS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z]+/[A-Z]+$").unwrap());
    static FILE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^[\w.-]+\.(md|txt|markdown)$").unwrap());
    static GENERIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(claude( code)?|codex|terminal( \d+)?|zsh|bash)$").unwrap()
    });
    let value = value?;
    let multiline = value.contains('\n');
    for line in value.lines() {
        let mut title = compact(line);
        if !literal {
            if title.starts_with('$') || multiline {
                title = SKILL.replace(&title, "").into_owned();
            }
            let candidate = READ
                .captures(&title)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| title.clone());
            let path = if candidate.len() >= 2
                && ["`", "\"", "'"]
                    .iter()
                    .any(|q| candidate.starts_with(q) && candidate.ends_with(q))
            {
                &candidate[1..candidate.len() - 1]
            } else if candidate.chars().any(char::is_whitespace) {
                ""
            } else {
                &candidate
            };
            if (PATH.is_match(path) && !CAPS.is_match(path))
                || FILE.is_match(path)
                || GENERIC.is_match(&title)
            {
                continue;
            }
        }
        if !title.is_empty() {
            return Some((title, compact(value)));
        }
    }
    None
}
fn short_handle(handle: &str) -> &str {
    handle
        .rsplit("::")
        .next()
        .unwrap_or(handle)
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or(handle)
}
fn fallback(handle: &str) -> String {
    let trimmed = handle.trim();
    if trimmed.is_empty()
        || ["term-tab-", "pty-session-", "remote-tab-", "remote-shell-"]
            .iter()
            .any(|p| trimmed.starts_with(p))
    {
        return memorable_name(handle);
    }
    if !handle.contains('/') && !handle.contains("::") {
        if let Some((stem, label)) = handle
            .split_once('#')
            .filter(|(_, l)| !l.is_empty() && !l.contains('#'))
        {
            let title = label
                .split('-')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let mut c = s.chars();
                    c.next()
                        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" ");
            return format!("{} · {}", short_handle(stem), title);
        }
    }
    short_handle(handle).to_string()
}
pub(super) fn row_names(row: &Value) -> Value {
    let names = &row["names"];
    let saved = sanitize(names["saved"].as_str(), true);
    let renamed = sanitize(names["renamed"].as_str(), true);
    let has_rename = renamed.is_some();
    let selected = renamed
        .or_else(|| sanitize(names["live"].as_str(), false))
        .or_else(|| sanitize(row["displayName"].as_str(), false))
        .or_else(|| saved.clone())
        .or_else(|| sanitize(names["cli"].as_str(), true))
        .unwrap_or_else(|| {
            let s = fallback(row["handle"].as_str().unwrap_or_default());
            (s.clone(), s)
        });
    let working = serde_json::from_value::<crate::live_hub::effective_status::AgentStatus>(
        row["effectiveStatus"].clone(),
    )
    .is_ok_and(|status| status.rank() == 1);
    if working && !has_rename {
        if let Some(progress) = sanitize(names["progress"].as_str(), false) {
            return json!({"primary":progress.0,"secondary":selected.0,"full":format!("{} · {}",progress.1,selected.0),"source":"progress"});
        }
    }
    let secondary = saved.filter(|s| s.0 != selected.0);
    json!({"primary":selected.0,"secondary":secondary.as_ref().map(|s| &s.0),"full":secondary.map_or(selected.1.clone(),|s| format!("{} · {}",selected.1,s.1))})
}
fn memorable_name(input: &str) -> String {
    fn hash(input: &str, mut h: u32) -> usize {
        for c in input.encode_utf16() {
            h ^= u32::from(c) & 0xff;
            h = h.wrapping_mul(0x01000193);
        }
        h ^= h >> 16;
        h = h.wrapping_mul(0x85ebca6b);
        h ^= h >> 13;
        h = h.wrapping_mul(0xc2b2ae35);
        h ^= h >> 16;
        h as usize
    }
    let seeded = format!("codesense:memorable:v1\0{input}");
    format!(
        "{} {}",
        ADJECTIVES[hash(&seeded, 0x811c9dc5) % ADJECTIVES.len()],
        NOUNS[hash(&seeded, 0x9e3779b1) % NOUNS.len()]
    )
}

const ADJECTIVES: &[&str] = &[
    "Amber", "Azure", "Blue", "Bold", "Brave", "Breezy", "Bright", "Brisk", "Cheery", "Clever",
    "Cosmic", "Crimson", "Crystal", "Dapper", "Deep", "Dusty", "Eager", "Earthy", "Ember",
    "Emerald", "Fancy", "Fiery", "Fluffy", "Fond", "Frosty", "Gentle", "Giddy", "Gilded", "Golden",
    "Grand", "Happy", "Hazel", "Humble", "Icy", "Indigo", "Iron", "Ivory", "Jade", "Jolly", "Keen",
    "Kind", "Lemon", "Lively", "Lucky", "Lunar", "Merry", "Minty", "Misty", "Mossy", "Noble",
    "Opal", "Peach", "Pearl", "Plucky", "Polar", "Quick", "Quiet", "Radiant", "Rapid", "Rose",
    "Royal", "Ruby", "Rustic", "Shiny", "Silent", "Silky", "Silver", "Snowy", "Solar", "Steady",
    "Stormy", "Sunny", "Swift", "Tangy", "Tidal", "Topaz", "Velvet", "Vivid", "Warm", "Witty",
];

const NOUNS: &[&str] = &[
    "Acorn", "Anchor", "Arrow", "Badger", "Beacon", "Beetle", "Berry", "Blossom", "Breeze",
    "Brook", "Canyon", "Cedar", "Cliff", "Clover", "Comet", "Coral", "Cosmos", "Delta", "Dolphin",
    "Dragon", "Dune", "Eagle", "Echo", "Falcon", "Feather", "Fern", "Finch", "Fjord", "Forest",
    "Fox", "Frost", "Garden", "Glade", "Grove", "Harbor", "Hawk", "Heron", "Island", "Lagoon",
    "Lantern", "Lark", "Leaf", "Lion", "Lotus", "Lynx", "Maple", "Meadow", "Meteor", "Nebula",
    "Oak", "Ocean", "Octopus", "Orchid", "Otter", "Owl", "Panda", "Pearl", "Pebble", "Pine",
    "Planet", "Quasar", "Raven", "Reef", "Ridge", "River", "Rocket", "Sage", "Shore", "Sparrow",
    "Spire", "Star", "Stone", "Summit", "Thunder", "Tiger", "Tulip", "Valley", "Wave", "Whale",
    "Willow",
];
