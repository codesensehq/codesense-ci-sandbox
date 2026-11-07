// SPDX-License-Identifier: MIT
//! SW-WV-F6 — the render tripwire: a per-scope, user-configurable rule table
//! (`wv_render_config`) that decides *where a controller's reply appears* —
//! terminal chat text, a drill-down doc, or an artifact annotation — the same
//! way the dispatch tripwire decides *who does the work*.
//!
//! This module holds the PURE routing logic: [`evaluate_render_target`] takes the
//! message text, the sending worker's subclass, the caller's `has_artifacts`
//! flag, and a slice of rules loaded from `wv_render_config`, and returns a
//! [`RenderTarget`]. It performs no I/O and no `async` — the DB read that loads
//! the rules ([`super::sqlite::WeftStore::load_render_config`]) is the caller's
//! responsibility (the `post_agent_message` handler runs it inside
//! `spawn_blocking`). Keeping the decision pure means every AC-2/AC-6 case is a
//! plain `#[test]` with zero setup.

/// Where a controller reply is rendered. `Terminal` is the safe default — a
/// routing miss always falls back to plain chat text rather than a broken emit.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RenderTarget {
    Terminal,
    DrillDoc,
    Artifact,
}

/// One row of `wv_render_config`. `kind` is one of `message_len` | `subclass` |
/// `has_artifacts`; `threshold` is kind-specific (an int for `message_len`, a
/// subclass string for `subclass`, `'1'`/`'0'` for `has_artifacts`); `target` is
/// one of `terminal` | `drill_doc` | `artifact`; `priority` breaks ties (higher
/// wins).
pub struct RenderRule {
    pub kind: String,
    pub threshold: String,
    pub target: String,
    pub priority: i64,
}

/// Map a stored `target` string to the enum. An unknown string falls through to
/// [`RenderTarget::Terminal`] — a corrupt/unrecognized rule can never route a
/// reply to a nonexistent surface.
fn target_from_str(target: &str) -> RenderTarget {
    match target {
        "drill_doc" => RenderTarget::DrillDoc,
        "artifact" => RenderTarget::Artifact,
        _ => RenderTarget::Terminal,
    }
}

/// Pure, deterministic routing. No I/O, no `async`.
///
/// Rules are evaluated highest-`priority`-first; the first matching rule's target
/// wins. Precedence (via the default priorities) is
/// `has_artifacts` > `subclass` > `message_len`. If no rule matches, the safe
/// default [`RenderTarget::Terminal`] is returned.
///
/// - `has_artifacts` matches when `has_artifacts == (threshold == "1")`.
/// - `subclass` matches when `subclass == Some(threshold)`.
/// - `message_len` matches when the message's **Unicode scalar count**
///   (`text.chars().count()`, NOT byte length) is `<=` the parsed threshold.
///   A non-parseable threshold is treated as `usize::MAX` (always matches),
///   keeping a corrupt rule from silently dropping every message.
pub fn evaluate_render_target(
    text: &str,
    subclass: Option<&str>,
    has_artifacts: bool,
    rules: &[RenderRule],
) -> RenderTarget {
    // Defensive re-sort: the loader returns rows ORDER BY priority DESC, but a
    // hand-inserted override could arrive out of order. Sort a borrowed index so
    // the caller's slice is untouched.
    let mut order: Vec<&RenderRule> = rules.iter().collect();
    order.sort_by(|a, b| b.priority.cmp(&a.priority));

    // Unicode scalar count — see the doc comment (perceived length, not bytes).
    let char_len = text.chars().count();

    for rule in order {
        let matched = match rule.kind.as_str() {
            "has_artifacts" => has_artifacts == (rule.threshold == "1"),
            "subclass" => subclass == Some(rule.threshold.as_str()),
            "message_len" => {
                let limit = rule.threshold.parse::<usize>().unwrap_or(usize::MAX);
                char_len <= limit
            }
            _ => false,
        };
        if matched {
            return target_from_str(&rule.target);
        }
    }
    RenderTarget::Terminal
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 5 default rules the store seeds for `scope='root'` — mirrored here so
    /// the pure-function tests exercise the exact shipped precedence without a DB.
    fn default_rules() -> Vec<RenderRule> {
        vec![
            RenderRule {
                kind: "has_artifacts".into(),
                threshold: "1".into(),
                target: "artifact".into(),
                priority: 30,
            },
            RenderRule {
                kind: "subclass".into(),
                threshold: "review".into(),
                target: "artifact".into(),
                priority: 20,
            },
            RenderRule {
                kind: "subclass".into(),
                threshold: "research".into(),
                target: "drill_doc".into(),
                priority: 20,
            },
            RenderRule {
                kind: "message_len".into(),
                threshold: "300".into(),
                target: "terminal".into(),
                priority: 10,
            },
            RenderRule {
                kind: "message_len".into(),
                threshold: "9999".into(),
                target: "drill_doc".into(),
                priority: 10,
            },
        ]
    }

    // AC-2 — priority: has_artifacts wins regardless of length or subclass.
    #[test]
    fn has_artifacts_beats_everything() {
        let rules = default_rules();
        assert_eq!(
            evaluate_render_target("short", Some("review"), true, &rules),
            RenderTarget::Artifact
        );
        let long = "x".repeat(5000);
        assert_eq!(
            evaluate_render_target(&long, Some("research"), true, &rules),
            RenderTarget::Artifact
        );
    }

    // AC-2 — subclass=review → Artifact.
    #[test]
    fn review_subclass_routes_to_artifact() {
        let rules = default_rules();
        assert_eq!(
            evaluate_render_target("anything", Some("review"), false, &rules),
            RenderTarget::Artifact
        );
    }

    // AC-2 — subclass=research + long text → DrillDoc.
    #[test]
    fn research_subclass_routes_to_drill_doc() {
        let rules = default_rules();
        let long = "y".repeat(1000);
        assert_eq!(
            evaluate_render_target(&long, Some("research"), false, &rules),
            RenderTarget::DrillDoc
        );
    }

    // AC-2 — short text, no subclass, no artifacts → Terminal.
    #[test]
    fn short_text_routes_to_terminal() {
        let rules = default_rules();
        assert_eq!(
            evaluate_render_target("a quick status update", None, false, &rules),
            RenderTarget::Terminal
        );
        // Boundary: exactly 300 chars is still Terminal (<= 300).
        let exactly_300 = "z".repeat(300);
        assert_eq!(
            evaluate_render_target(&exactly_300, None, false, &rules),
            RenderTarget::Terminal
        );
    }

    // AC-2 — long text, no subclass, no artifacts → DrillDoc.
    #[test]
    fn long_text_routes_to_drill_doc() {
        let rules = default_rules();
        // 301 chars: past the terminal threshold, caught by the 9999 drill_doc rule.
        let just_over = "w".repeat(301);
        assert_eq!(
            evaluate_render_target(&just_over, None, false, &rules),
            RenderTarget::DrillDoc
        );
    }

    // Unicode: 10 emoji (4 bytes each in UTF-8 = 40 bytes, but 10 scalar values)
    // must count as 10 chars, so a short emoji message stays Terminal rather than
    // tripping the drill-doc threshold on byte length.
    #[test]
    fn multibyte_message_counts_by_scalar_not_bytes() {
        let rules = default_rules();
        let emoji = "😀".repeat(10); // 10 chars, 40 bytes
        assert_eq!(emoji.len(), 40);
        assert_eq!(emoji.chars().count(), 10);
        assert_eq!(
            evaluate_render_target(&emoji, None, false, &rules),
            RenderTarget::Terminal
        );
    }

    // AC-6 — a user override at the same (scope, kind, threshold) with higher
    // priority wins. A message_len=300 rule retargeted to `artifact` at
    // priority=40 makes a 250-char message route to Artifact.
    #[test]
    fn custom_priority_rule_overrides_default() {
        let mut rules = default_rules();
        rules.push(RenderRule {
            kind: "message_len".into(),
            threshold: "300".into(),
            target: "artifact".into(),
            priority: 40,
        });
        let msg = "m".repeat(250);
        assert_eq!(
            evaluate_render_target(&msg, None, false, &rules),
            RenderTarget::Artifact
        );
    }

    // Empty rule set → safe default Terminal (never panics, never mis-routes).
    #[test]
    fn empty_rules_default_to_terminal() {
        assert_eq!(
            evaluate_render_target("hello", Some("review"), true, &[]),
            RenderTarget::Terminal
        );
    }

    // An unknown target string in a rule falls through to Terminal.
    #[test]
    fn unknown_target_falls_through_to_terminal() {
        let rules = vec![RenderRule {
            kind: "message_len".into(),
            threshold: "9999".into(),
            target: "carrier-pigeon".into(),
            priority: 10,
        }];
        assert_eq!(
            evaluate_render_target("x", None, false, &rules),
            RenderTarget::Terminal
        );
    }
}
