// SPDX-License-Identifier: MIT
//! Agent hierarchy, workspace attribution and naming, resolved by the live hub.
#[cfg(feature = "bench-support")]
pub(crate) mod bench;
pub(crate) mod fields;
mod input;
mod order;
pub(crate) mod sessions;
pub(crate) mod spend;
mod views;
pub(super) use views::{replace as replace_view, replace_status};
mod names;
pub(crate) mod preferences;
pub(super) use input::resolve;
pub(super) use roster::{map_roster, union as union_roster};
// Unknown teams follow known ranks in key order until the viewer discovers them.
pub(super) fn team_sort_key(viewer: &Value, key: &str) -> (u64, String) {
    (
        viewer["agentTeamRanks"][key].as_u64().unwrap_or(u64::MAX),
        key.into(),
    )
}
pub(super) fn home_team(viewer: &Value, fallback: String) -> String {
    let mut teams: Vec<_> = viewer["openedTeams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|key| *key != "_no_team:_none")
        .collect();
    teams.sort_by_key(|key| team_sort_key(viewer, key));
    teams.first().map(|key| key.to_string()).unwrap_or(fallback)
}
pub(super) mod presentation;
mod roster;
mod rows;
mod tree;
pub(crate) use rows::AgentListRow;
mod workspace;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;

#[cfg(test)]
fn project_case(kind: &str, input: &Value) -> Value {
    match kind {
        "buildAgentTree" => json!(tree::build_tree(
            &serde_json::from_value(input.clone()).unwrap()
        )),
        "buildOtherTeamGroups" => json!(tree::other_groups(
            &serde_json::from_value(input.clone()).unwrap()
        )),
        "mapRosterRowToSession" => roster::map_roster(&input[0]),
        "unionRosterWithLiveSessions" => json!(roster::union(
            input[0].as_array().unwrap(),
            input[1].as_array().unwrap()
        )),
        "withHandStartedAgents" => {
            roster::with_hand_started(&input[0], input[1].as_array().unwrap(), &input[2])
        }
        "agentRowNames" => names::row_names(&input[0]),
        "paneWorkspaceLocationOf" => workspace::pane_workspace(&input[0], &input[1]),
        "groupByWorkspace" => json!(tree::by_workspace(
            input[0].as_array().unwrap(),
            input
                .get(1)
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
        )),
        "groupByManager" => json!(tree::by_manager(input[0].as_array().unwrap(), &input[1])),
        _ => panic!("unknown parity case: {kind}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_workspace_shell_requires_agent_evidence_even_when_addressable() {
        let live = crate::mcp::terminal_tabs::LiveTabs {
            tabs: vec![],
            offline: vec![],
            live: vec![],
            labels: Default::default(),
            activity: vec![],
        };
        let mut providers = crate::live_hub::projection::FieldProviders::new([] as [&str; 0]);
        crate::live_hub::effective_status::register(&mut providers);
        for detected in [false, true] {
            let processes = if detected {
                [("shell".into(), "claude".into())].into_iter().collect()
            } else {
                Default::default()
            };
            let mut resolved = input::resolve(
                &json!({}),
                vec![
                    json!({"handle":"folder","sessionId":"shell","kind":"registered","alive":true,"agentRevision":null}),
                ],
                &live,
                json!({}),
                &json!({}),
                &Default::default(),
                &processes,
                "org:team".into(),
                None,
                &|_| None,
            );
            let wire = sessions::project(&resolved.sessions, &[], &mut resolved.rows, &providers);
            assert_eq!(wire[0].1["kind"], "registered");
            assert_eq!(wire[0].1["hasAgent"], detected);
            assert_eq!(
                wire[0].1["model"],
                if detected {
                    json!("Claude")
                } else {
                    Value::Null
                }
            );
        }
    }

    #[test]
    fn a_plain_registered_shell_is_listed_only_while_an_agent_runs_in_it() {
        let live = crate::mcp::terminal_tabs::LiveTabs {
            tabs: vec![],
            offline: vec![],
            live: vec![],
            labels: Default::default(),
            activity: vec![],
        };
        let listed = |sessions: Vec<Value>,
                      processes: &std::collections::BTreeMap<String, String>| {
            input::resolve(
                &json!({}),
                sessions,
                &live,
                json!({}),
                &json!({}),
                &Default::default(),
                processes,
                "org:team".into(),
                None,
                &|_| None,
            )
            .rows
            .into_iter()
            .map(|row| row.session_ref)
            .collect::<Vec<_>>()
        };
        let shell = json!({"handle":"tab","sessionId":"shell","kind":"registered","alive":true,"agentRevision":null});
        let running: std::collections::BTreeMap<String, String> =
            [("shell".into(), "claude".into())].into_iter().collect();
        assert!(listed(vec![shell.clone()], &Default::default()).is_empty());
        assert_eq!(listed(vec![shell.clone()], &running), vec!["shell"]);
        let mut named = shell.clone();
        named["agentRevision"] = "worker@1".into();
        assert_eq!(listed(vec![named], &Default::default()), vec!["shell"]);
        // A plain shell that launched an agent stays as that agent's parent.
        let child = json!({"handle":"child","sessionId":"child","kind":"dashboard-launched","alive":true,"launchedBy":"tab"});
        let mut both = listed(vec![shell, child], &Default::default());
        both.sort();
        assert_eq!(both, vec!["child", "shell"]);
    }

    #[test]
    fn locale_ranks_keep_visible_order_and_unplaced_agent_home_together() {
        let viewer = json!({
            "openedTeams":["zoo:dev","eclair:dev"],
            "orgDisplayNames":{"eclair":"Éclair","zoo":"Zoo"},
            "agentTeamRanks":{"eclair:dev":0,"zoo:dev":1},
            "workspaces":{
                "eclair:dev":[{"id":"e","kind":"generic","status":"ready","cwd":"/e"}],
                "zoo:dev":[{"id":"z","kind":"generic","status":"ready","cwd":"/z"}]
            }
        });
        let live = crate::mcp::terminal_tabs::LiveTabs {
            tabs: vec![],
            offline: vec![],
            live: vec![],
            labels: Default::default(),
            activity: vec![],
        };
        let resolved = input::resolve(
            &viewer,
            vec![
                json!({"handle":"unplaced","kind":"dashboard-launched","alive":true}),
                json!({"handle":"zoo","kind":"dashboard-launched","alive":true,"workspace":"/z"}),
                json!({"handle":"eclair","kind":"dashboard-launched","alive":true,"workspace":"/e"}),
            ],
            &live,
            json!({}),
            &json!({}),
            &Default::default(),
            &Default::default(),
            home_team(&viewer, "fallback".into()),
            None,
            &|_| None,
        );
        assert_eq!(resolved.home_team, "eclair:dev");
        assert_eq!(resolved.team_order, vec!["eclair:dev", "zoo:dev"]);
        assert_eq!(
            resolved
                .rows
                .iter()
                .map(|r| r.team_key.as_str())
                .collect::<Vec<_>>(),
            vec!["eclair:dev", "eclair:dev", "zoo:dev"]
        );
        assert_eq!(
            resolved
                .rows
                .iter()
                .find(|r| r.session_ref == "unplaced")
                .unwrap()
                .team_key,
            "eclair:dev"
        );
    }

    #[test]
    fn missing_team_ranks_use_a_total_lexical_fallback() {
        let viewer = json!({"agentTeamRanks":{"zoo:dev":0},"openedTeams":["Éclair:dev","alpha:dev","zoo:dev"]});
        let mut teams = vec!["Éclair:dev", "alpha:dev", "zoo:dev"];
        teams.sort_by_key(|team| team_sort_key(&viewer, team));
        assert_eq!(teams, vec!["zoo:dev", "alpha:dev", "Éclair:dev"]);
        assert_eq!(home_team(&viewer, "fallback".into()), "zoo:dev");
        assert_eq!(
            home_team(
                &json!({"openedTeams":["zoo:dev","alpha:dev"]}),
                "fallback".into()
            ),
            "alpha:dev"
        );
    }

    #[test]
    fn scoped_views_filter_metadata_and_keep_independent_structural_revision_logs() {
        let input:tree::TreeInput=serde_json::from_value(json!({"teamKey":"org:a","sessions":[{"row":{"handle":"lead","alive":true,"kind":"dashboard-launched"}},{"row":{"handle":"worker","alive":true,"kind":"dashboard-launched"}}]})).unwrap();
        let mut rows = rows::rows(&input, &json!({}));
        let mut registry = crate::live_hub::projection::TopicRegistry::default();
        let meta = json!({"ready":true,"homeTeam":"org:b","teamOrder":["org:b","org:a"]});
        views::replace(&mut registry, "all", &rows, &meta).unwrap();
        views::replace(&mut registry, "team:org/a", &rows, &meta).unwrap();
        let all = registry.snapshot("agentList", "all").unwrap();
        let scoped = registry.snapshot("agentList", "team:org/a").unwrap();
        let state = scoped
            .snapshot
            .iter()
            .find(|row| row["rowType"] == "state")
            .unwrap();
        assert_eq!(state["teamOrder"], json!(["org:a"]));
        assert!(state["homeTeam"].is_null());
        rows[1].parent_id = Some(rows[0].id.clone());
        rows[1].parent_source = Some("chosen".into());
        let deltas = views::replace(&mut registry, "team:org/a", &rows, &meta).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].upserts.len(), 1);
        assert!(serde_json::to_vec(&deltas[0]).unwrap().len() < 2048);
        assert_eq!(registry.snapshot("agentList", "all").unwrap().rev, all.rev);
        assert_eq!(
            registry
                .subscribe("agentList", "team:org/a", Some(scoped.rev))
                .unwrap()
                .deltas
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn team_scoped_metadata_only_names_admitted_teams() {
        let mut registry = crate::live_hub::projection::TopicRegistry::default();
        let meta = json!({
            "homeTeam":"org:b", "teamOrder":["org:a","org:b"],
            "teamNames":{"org:a":{"org":"Acme","team":"Alpha"},"org:b":{"org":"Acme","team":"Beta"}},
            "workspaceRollups":{"org:a":{"ws-a":{"count":1}},"org:b":{"ws-b":{"count":2}}},
            "pane":{"workspaceTeam":"org:b","workspaceLabel":"Secret workspace","workspaceWait":{"title":"Waiting for private machine"},"openTeams":["org:a","org:b"],"rails":{"org:a":[],"org:b":[]},"railKeys":{"org:a":{},"org:b":{}}}
        });
        for key in ["team:org/a", "all"] {
            views::replace(&mut registry, key, &[], &meta).unwrap();
            let snapshot = registry.snapshot("agentList", key).unwrap();
            let state = snapshot
                .snapshot
                .iter()
                .find(|row| row["rowType"] == "state")
                .unwrap();
            assert_eq!(state["teamNames"]["org:a"], meta["teamNames"]["org:a"]);
            assert_eq!(
                state["workspaceRollups"]["org:a"],
                meta["workspaceRollups"]["org:a"]
            );
            if key == "all" {
                assert_eq!(state["pane"]["workspaceLabel"], "Secret workspace");
                assert_eq!(
                    state["pane"]["workspaceWait"],
                    meta["pane"]["workspaceWait"]
                );
                assert_eq!(state["teamNames"], meta["teamNames"]);
                assert_eq!(state["workspaceRollups"], meta["workspaceRollups"]);
            } else {
                assert!(state["teamNames"].get("org:b").is_none());
                assert!(state["workspaceRollups"].get("org:b").is_none());
                assert_eq!(state["pane"]["openTeams"], json!(["org:a"]));
                assert!(state["pane"].get("workspaceLabel").is_none());
                assert!(state["pane"].get("workspaceWait").is_none());
                assert!(state["pane"].get("workspaceTeam").is_none());
                assert!(state["pane"]["rails"].get("org:b").is_none());
                assert!(state["pane"]["railKeys"].get("org:b").is_none());
            }
        }
    }

    #[test]
    fn home_team_follows_mirrored_ranks_and_workspace_crew_uses_largest_root() {
        assert_eq!(
            home_team(
                &json!({"openedTeams":["acme:ops","zeta:core"],"agentTeamRanks":{"zeta:core":0,"acme:ops":1}}),
                "fallback".into()
            ),
            "zeta:core"
        );
        let input:tree::TreeInput=serde_json::from_value(json!({"teamKey":"org:team","sessions":[
            {"row":{"handle":"solo","kind":"dashboard-launched","alive":true,"startedAtMs":9},"workspace":{"id":"ws","label":"Work","kind":"folder"}},
            {"row":{"handle":"lead","kind":"dashboard-launched","alive":true,"startedAtMs":1},"workspace":{"id":"ws","label":"Work","kind":"folder"}},
            {"row":{"handle":"worker","kind":"dashboard-launched","alive":true,"launchedBy":"lead"},"workspace":{"id":"ws","label":"Work","kind":"folder"}}
        ]})).unwrap();
        let rows = rows::rows(&input, &json!({}));
        assert_eq!(
            rows.iter().find(|row| row.crew_lead).unwrap().session_ref,
            "lead"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.session_ref == "worker")
                .unwrap()
                .crew_parent_id,
            rows.iter()
                .find(|row| row.session_ref == "lead")
                .map(|row| row.id.clone())
        );
    }

    #[test]
    fn reference_rows_keep_session_and_inbox_fields_out() {
        let input: tree::TreeInput = serde_json::from_value(json!({"teamKey":"org:team","sessions":[
            {"row":{"handle":"lead","sessionId":"s1","kind":"dashboard-launched","alive":true,"status":"running","displayName":"Lead","unread":7}},
            {"row":{"handle":"worker","sessionId":"s2","kind":"dashboard-launched","alive":true,"launchedBy":"lead"}}
        ]})).unwrap();
        let rows = rows::rows(&input, &json!({}));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].parent_id.as_ref(), Some(&rows[0].id));
        let wire = serde_json::to_value(&rows[0]).unwrap();
        for field in [
            "status",
            "state",
            "title",
            "displayName",
            "unread",
            "names",
            "groupUnread",
        ] {
            assert!(
                wire.get(field).is_none(),
                "{field} belongs to another owner"
            );
        }
        let changed: tree::TreeInput = serde_json::from_value(json!({"teamKey":"org:team","sessions":[
            {"row":{"handle":"lead","sessionId":"s1","kind":"dashboard-launched","alive":true,"status":"idle","displayName":"New title","unread":0}},
            {"row":{"handle":"worker","sessionId":"s2","kind":"dashboard-launched","alive":true,"launchedBy":"lead"}}
        ]})).unwrap();
        assert_eq!(rows, rows::rows(&changed, &json!({})));
    }

    #[test]
    fn manager_grouping_keeps_filtered_parents_and_tree_order() {
        let nodes = json!([
            {"row":{"handle":"lead"},"children":[{"row":{"handle":"worker"},"children":[]}]},
            {"row":{"handle":"other"},"children":[]}
        ]);
        assert_eq!(
            tree::by_manager(
                nodes.as_array().unwrap(),
                &json!({"worker":"lead","other":"gone"})
            ),
            nodes.as_array().unwrap().clone()
        );
    }

    #[test]
    fn status_and_unread_changes_do_not_advance_agent_list_revision() {
        let base = json!({"teamKey":"org:team","sessions":[{"row":{"handle":"lead","kind":"dashboard-launched","alive":true,"status":"running","unread":0}},{"row":{"handle":"worker","kind":"dashboard-launched","alive":true,"launchedBy":"lead"}}]});
        let input: tree::TreeInput = serde_json::from_value(base.clone()).unwrap();
        let rows = rows::rows(&input, &json!({}));
        let mut registry = crate::live_hub::projection::TopicRegistry::default();
        let meta = json!({"ready":true});
        views::replace(&mut registry, "all", &rows, &meta).unwrap();
        let mut changed = base;
        changed["sessions"][0]["row"]["status"] = "awaiting-answer".into();
        changed["sessions"][0]["row"]["unread"] = 5.into();
        let changed: tree::TreeInput = serde_json::from_value(changed).unwrap();
        let changed = rows::rows(&changed, &json!({}));
        assert_eq!(changed, rows);
        assert!(views::replace(&mut registry, "all", &changed, &meta)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn names_keep_roster_title_precedence_when_runtime_display_differs() {
        let roster = vec![json!({"handle":"worker","sessionId":"s1","displayName":"Roster title"})];
        let runtime =
            vec![json!({"handle":"worker","sessionId":"s1","displayName":"Runtime title"})];
        let projected = sessions::project(
            &roster,
            &runtime,
            &mut [],
            &crate::live_hub::projection::FieldProviders::new(["id"]),
        );
        assert_eq!(projected[0].1["displayName"], "Runtime title");
        assert_eq!(projected[0].1["rowNames"], names::row_names(&roster[0]));
    }

    #[test]
    fn hub_notes_join_matching_sessions_and_titles_follow_effective_status_without_reordering() {
        let stream = crate::dashboard_stream::DashboardStreamHub::default();
        stream.on_session_progress("s1", "dev", None, None, Some("Writing tests".into()));
        stream.on_session_progress("other", "dev", None, None, Some("Other task".into()));
        let live = crate::mcp::terminal_tabs::LiveTabs {
            tabs: vec![],
            offline: vec![],
            live: vec![],
            labels: Default::default(),
            activity: vec![],
        };
        let resolve = || {
            input::resolve(
                &json!({"openedTeams":["org:team"]}),
                vec![
                    json!({"handle":"worker","sessionId":"s1","kind":"dashboard-launched","alive":true,"displayName":"Fix login"}),
                    json!({"handle":"other-worker","sessionId":"s2","kind":"dashboard-launched","alive":true,"displayName":"Other worker"}),
                ],
                &live,
                json!({}),
                &json!({}),
                &Default::default(),
                &Default::default(),
                "org:team".into(),
                None,
                &|sid| stream.progress_note_for_title(sid),
            )
        };
        let mut providers = crate::live_hub::projection::FieldProviders::new(["id"]);
        crate::live_hub::effective_status::register(&mut providers);
        let project = |resolved: &mut input::Resolved, runtime: Vec<Value>| {
            sessions::project(&resolved.sessions, &runtime, &mut resolved.rows, &providers)
                .into_iter()
                .find(|(_, row)| row["sessionId"] == "s1")
                .unwrap()
                .1
        };
        let mut resolved = resolve();
        assert_eq!(resolved.sessions[0]["names"]["progress"], "Writing tests");
        assert!(resolved.sessions[1]["names"]["progress"].is_null());
        let runtime = |outcome: Option<&str>, signals: Value| {
            let mut live = json!(crate::live_hub::effective_status::LiveSignals::default());
            live.as_object_mut()
                .unwrap()
                .extend(signals.as_object().unwrap().clone());
            vec![json!({
                "sessionId":"s1","handle":"worker","_managedOutcome":outcome,"_statusSignals":live
            })]
        };
        let working = project(&mut resolved, runtime(None, json!({})));
        assert_eq!(
            working["rowNames"],
            json!({"primary":"Writing tests","secondary":"Fix login","full":"Writing tests · Fix login","source":"progress"})
        );
        for (outcome, signals) in [
            (Some("done"), json!({})),
            (Some("needs-attention"), json!({})),
            (None, json!({"needsYou":"blocked"})),
            (None, json!({"agentEnded":true})),
        ] {
            let row = project(&mut resolved, runtime(outcome, signals));
            assert_eq!(
                row["rowNames"],
                json!({"primary":"Fix login","secondary":null,"full":"Fix login"})
            );
        }
        stream.on_session_progress(
            "s1",
            "blocked",
            None,
            None,
            Some("Waiting for credentials".into()),
        );
        let mut unblocked = resolve();
        assert_eq!(
            project(&mut unblocked, runtime(None, json!({})))["rowNames"]["primary"],
            "Fix login"
        );
        stream.on_session_progress("s1", "dev", None, None, Some("Writing tests".into()));
        stream.on_session_status("s1", "success", Some("Tests written".into()));
        let mut resumed = resolve();
        let mut live = crate::live_hub::effective_status::LiveSignals::default();
        live.new_turn_after_completion = true;
        assert_eq!(
            project(&mut resumed, runtime(Some("done"), json!(live)))["rowNames"]["primary"],
            "Fix login"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));

        let mut registry = crate::live_hub::projection::TopicRegistry::default();
        let meta = json!({"ready":true});
        views::replace(&mut registry, "all", &resolved.rows, &meta).unwrap();
        stream.on_session_progress("s1", "review", None, None, Some("Reviewing tests".into()));
        let mut changed = resolve();
        assert_eq!(
            project(&mut changed, runtime(None, json!({})))["rowNames"]["primary"],
            "Reviewing tests"
        );
        assert_eq!(resolved.rows, changed.rows);
        assert!(views::replace(&mut registry, "all", &changed.rows, &meta)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn mirrored_workspaces_keep_live_story_precedence_and_reproject_a_late_home() {
        use std::collections::BTreeMap;
        let live = crate::mcp::terminal_tabs::LiveTabs {
            tabs: vec![],
            offline: vec![],
            live: vec![],
            labels: BTreeMap::new(),
            activity: vec![],
        };
        let viewer = json!({"openedTeams":["org:home","org:other"],"workspaces":{"org:other":[{"id":"live","kind":"story","status":"ready","storyId":"ST-1","storyStatus":"in-progress","storyFilePath":"/story.md"}]},"softClosedWorkspaces":{"org:home":{"shadow":{"workspace":{"id":"shadow","kind":"story","status":"ready","storyId":"ST-1","storyStatus":"in-progress","storyFilePath":"/story.md"}}}}});
        let resolve = |viewer: &Value, row: Value| {
            input::resolve(
                viewer,
                vec![row],
                &live,
                json!({}),
                &json!({}),
                &BTreeMap::new(),
                &BTreeMap::new(),
                "org:home".into(),
                None,
                &|_| None,
            )
        };
        let resolved = resolve(
            &viewer,
            json!({"handle":"ST-1","kind":"dashboard-launched","alive":true,"sessionId":null}),
        );
        assert_eq!(resolved.rows[0].workspace_ref.as_deref(), Some("live"));
        assert_eq!(resolved.rows[0].team_key, "org:other");
        let row = json!({"handle":"worker","kind":"dashboard-launched","alive":true,"sessionId":null,"workspace":"/wt/new"});
        assert!(resolve(&viewer, row.clone()).rows[0]
            .workspace_ref
            .is_none());
        let mut opened = viewer;
        opened["workspaces"]["org:home"] =
            json!([{"id":"new","kind":"generic","status":"ready","cwd":"/wt/new"}]);
        assert_eq!(
            resolve(&opened, row).rows[0].workspace_ref.as_deref(),
            Some("new")
        );
    }

    #[test]
    fn references_join_process_ids_and_keep_two_teams_same_handle_distinct() {
        let input: tree::TreeInput=serde_json::from_value(json!({"teamKey":"org:a","sessions":[
            {"row":{"handle":"twin","sessionId":"sid-a","kind":"dashboard-launched","alive":true},"tabTeam":"org:a"},
            {"row":{"handle":"twin","sessionId":"sid-b","kind":"dashboard-launched","alive":true},"tabTeam":"org:b"}
        ]})).unwrap();
        let mut references = rows::rows(&input, &json!({}));
        let runtime = vec![
            json!({"handle":"twin","sessionId":"sid-a","team":{"org":"mail","team":"a"},"displayName":"A"}),
            json!({"handle":"twin","sessionId":"sid-b","team":{"org":"mail","team":"b"},"displayName":"B"}),
        ];
        let projected = sessions::project(
            &input
                .tree_sessions
                .iter()
                .map(|s| s.row.clone())
                .collect::<Vec<_>>(),
            &runtime,
            &mut references,
            &crate::live_hub::projection::FieldProviders::new(["id", "displayName"]),
        );
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].1["id"], "sid-a");
        assert_eq!(projected[1].1["id"], "sid-b");
        assert_eq!(projected[0].1["displayName"], "A");
        assert_eq!(projected[1].1["displayName"], "B");
        assert_eq!(
            references
                .iter()
                .find(|row| row.session_ref == "sid-a")
                .unwrap()
                .inbox_ref,
            "counts:team:mail/a"
        );
        assert_eq!(
            references
                .iter()
                .find(|row| row.session_ref == "sid-b")
                .unwrap()
                .inbox_ref,
            "counts:team:mail/b"
        );
    }

    // Inputs and expected results come from the existing inline TypeScript cases.
    // Keep these independent of the implementation as the old model is removed.
    #[test]
    fn inline_model_parity() {
        let cases: Vec<Value> = serde_json::from_str(include_str!("parity.json")).unwrap();
        for case in cases {
            assert_eq!(
                project_case(case["kind"].as_str().unwrap(), &case["input"]),
                case["output"],
                "{}: {}",
                case["kind"],
                case["test"]
            );
        }
    }
}

#[cfg(test)]
mod pane_fields_tests {
    use super::*;
    #[test]
    fn custom_stage_roles_follow_the_host_taxonomy() {
        let live = crate::mcp::terminal_tabs::LiveTabs {
            tabs: vec![],
            offline: vec![],
            live: vec![],
            labels: Default::default(),
            activity: vec![],
        };
        let resolved = input::resolve(
            &json!({"openedTeams":["org:dev"],"agentStageStates":[{"id":"checking","role":"review","label":"Checking"}],
                "workspaces":{"org:dev":[{"id":"ws","kind":"story","status":"ready","storyStatus":"checking","storyId":"ST-1","cwd":"/ws"}]}}),
            vec![
                json!({"handle":"a","sessionId":"a","kind":"dashboard-launched","alive":true,"workspace":"/ws"}),
            ],
            &live,
            json!({}),
            &json!({}),
            &Default::default(),
            &Default::default(),
            "org:dev".into(),
            None,
            &|_| None,
        );
        assert_eq!(resolved.rows[0].workspace["stageRole"], "review");
        assert_eq!(resolved.rows[0].workspace["stageLabel"], "Checking");
    }
    #[test]
    fn workspace_presentation_is_projected_with_its_reference() {
        let input: tree::TreeInput = serde_json::from_value(json!({"teamKey":"org:dev","sessions":[
            {"row":{"handle":"lead","kind":"dashboard-launched","alive":true},"workspace":{"id":"ws","label":"Work","kind":"story","storyStatus":"review","machineId":"remote","sourceRepoSlug":"org/repo"}}
        ]})).unwrap();
        let row = serde_json::to_value(&rows::rows(&input, &json!({}))[0]).unwrap();
        assert_eq!(row["workspace"], input.tree_sessions[0].workspace);
    }
}
