// SPDX-License-Identifier: MIT
use super::{builtin_registry, Class, Confirm, Policy, VerbSpec};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallerTeam {
    pub org: String,
    pub team: String,
}
/// Resolved frame identity plus its trusted mint-time caller classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Caller {
    #[serde(skip)]
    pub records_binding: Option<crate::records::binding::Binding>,
    pub kind: crate::dashboard_stream::frame::FrameCallerKind,
    pub frame_id: String,
    pub principal: String,
    pub dashboard: String,
    pub slot: String,
    pub team: Option<CallerTeam>,
    pub scope: String,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "status",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum Decision {
    Admit {
        call_id: String,
    },
    Deny {
        reason: String,
    },
    Pending {
        confirm_id: String,
        allow_always: bool,
    },
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Call {
    pub caller: Caller,
    pub verb: String,
    pub args: Value,
    pub call_id: String,
    pub allow_always: bool,
}
#[derive(Default)]
pub struct Dispatcher {
    flushing: std::collections::BTreeSet<String>,
    record_windows: BTreeMap<(String, String), Vec<(u64, String)>>,
    pub(super) windows: BTreeMap<(String, String, Class), Vec<(u64, String)>>,
    calls: BTreeMap<String, Call>,
    pub(super) pending: BTreeMap<String, Call>,
}
fn cap_free(verb: &str, a: &Value) -> bool {
    matches!(verb, "openTab" | "openFile")
        || (verb == "openWorkspace"
            && matches!(a["target"]["kind"].as_str(), Some("dir" | "epic"))
            && a["headed"] != false)
}
fn audited(spec: Option<&VerbSpec>) -> bool {
    spec.is_none_or(|s| s.audited)
}
pub(super) fn action_target(caller: &Caller, verb: &str, args: &Value) -> Option<String> {
    let string = |key: &str| args[key].as_str().map(str::to_owned);
    match verb {
        "setStatus" => args["path"]
            .as_array()
            .map(|paths| format!("{} docs", paths.len()))
            .or_else(|| string("path")),
        "setField" => {
            let path = args["path"].as_str()?;
            let keys: Vec<&str> = if let Some(fields) = args["fields"].as_object() {
                fields.keys().map(String::as_str).collect()
            } else {
                args["field"].as_str().into_iter().collect()
            };
            Some(if keys.is_empty() {
                path.into()
            } else {
                format!("{path} ({})", keys.join(", "))
            })
        }
        "openFile" => {
            let path = args["path"].as_str()?;
            Some(match args["team"].as_str() {
                Some(team) => format!("{team} · {path}"),
                None => path.into(),
            })
        }
        "routeToContext" => string("logicalName"),
        "draft" => string("from"),
        "openWorkspace" => {
            if let Some(story) = args["storyId"].as_str() {
                return Some(story.into());
            }
            let target = &args["target"];
            let kind = target["kind"].as_str()?;
            match kind {
                "story" if target["storyId"].is_string() => {
                    target["storyId"].as_str().map(str::to_owned)
                }
                "epic" if target["team"].is_string() && target["epic"].is_string() => {
                    let team = target["team"].as_str()?;
                    let epic = target["epic"].as_str()?;
                    let label = format!("{team} · {epic}");
                    let another_team = caller
                        .team
                        .as_ref()
                        .is_some_and(|current| team != format!("{}:{}", current.org, current.team));
                    Some(if another_team {
                        format!("{label} (another team)")
                    } else {
                        label
                    })
                }
                "branch" if target["repo"].is_string() && target["branch"].is_string() => {
                    Some(format!(
                        "{}#{}",
                        target["repo"].as_str()?,
                        target["branch"].as_str()?
                    ))
                }
                "dir"
                    if target["path"]
                        .as_str()
                        .is_some_and(|path| !path.trim().is_empty()) =>
                {
                    Some(target["path"].as_str()?.trim().into())
                }
                _ => Some(kind.into()),
            }
        }
        "openTab" => string("workspaceId"),
        "sendToWorkspaceTab" => {
            let session = args["sessionId"]
                .as_str()
                .filter(|session| !session.is_empty())?;
            Some(match args["mode"].as_str() {
                Some(mode) => format!("{mode} → {session}"),
                None => session.into(),
            })
        }
        "runPrompt" | "sendToSession" | "closeWorkspace" => string("target"),
        "runSkill" => string("skill"),
        "readWorkspace" => string("path"),
        "getWorkspaceStatus" => string("storyId"),
        "sendPrompt" | "stopSession" | "closeTerminalTab" => string("handle"),
        "sendAsHuman" => string("to"),
        "loadMedia" | "replyAsHuman" | "react" | "openMessage" => args["messageId"]
            .as_number()
            .map(|id| format!("message {id}")),
        "archive" | "markRead" | "markUnread" => args["messageIds"]
            .as_array()
            .map(|messages| format!("{} messages", messages.len())),
        "setHumanStatus" => string("status"),
        "routineResult" | "routineStatus" | "routineRefresh" | "routineCancel" => string("key"),
        "commands.run" => string("id"),
        _ => None,
    }
}

fn audit(call: &Call, spec: Option<&VerbSpec>, decision: &str, reason: &str) {
    if !audited(spec) {
        return;
    }
    let mut line = json!({"ts": chrono::Utc::now().to_rfc3339(), "verb":call.verb, "target":action_target(&call.caller, &call.verb, &call.args), "decision":decision, "reason":reason, "docPath":call.caller.dashboard});
    if spec.is_some_and(|s| s.class == Class::Read) {
        line["read"] = json!(true);
    }
    if call.verb == "runSkill" {
        let skill = call.args["skill"].as_str().unwrap_or("");
        let args = call.args["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let raw = format!("{skill} {args}").trim().to_owned();
        line["argPreview"] = json!(if raw.chars().count() > 512 {
            format!("{}…", raw.chars().take(512).collect::<String>())
        } else {
            raw
        });
    }
    log::info!(target: "dashboard-action", "{}", line);
}
impl From<&crate::dashboard_stream::frame::FrameRecord> for Caller {
    fn from(frame: &crate::dashboard_stream::frame::FrameRecord) -> Self {
        let grant = &frame.grant;
        Caller {
            records_binding: frame.records_binding.clone(),
            kind: frame.kind,
            frame_id: grant.frame_id.clone(),
            principal: grant.principal.clone(),
            dashboard: grant.dashboard.clone(),
            slot: grant.slot.clone(),
            team: grant.team.as_ref().map(|t| CallerTeam {
                org: t.org.clone(),
                team: t.team.clone(),
            }),
            scope: grant.scope.clone(),
        }
    }
}

/// Capability admission shared by IPC dispatch, direct HTTP reads and first paint.
pub fn check_frame_capability(
    frame: &crate::dashboard_stream::frame::FrameRecord,
    verb: &str,
    args: &Value,
) -> Result<(), &'static str> {
    let spec = builtin_registry().get(verb).ok_or("not-allowed")?;
    check_capability(
        &Caller::from(frame),
        &frame.effective_grant,
        spec,
        args,
        frame.policy.team_safe,
    )
}

fn check_capability(
    caller: &Caller,
    grant: &[String],
    spec: &VerbSpec,
    args: &Value,
    team_safe: bool,
) -> Result<(), &'static str> {
    let scope = match caller.kind {
        crate::dashboard_stream::frame::FrameCallerKind::Team => "team",
        crate::dashboard_stream::frame::FrameCallerKind::Message => "message",
        _ => &caller.scope,
    };
    let allowed =
        |cap: &str| !team_safe || super::capabilities::records_allows(&caller.slot, scope, cap);
    if !(spec.name == "records.collections"
        && grant
            .iter()
            .any(|g| matches!(g.as_str(), "records:read:own" | "records:read:team") && allowed(g)))
        && !grant
            .iter()
            .any(|g| g == spec.required_capability(caller, args) && allowed(g))
    {
        return Err("denied: grant");
    }
    if spec.inbox_target {
        // An omitted target means the bound team, never the active renderer team.
        let org = args["org"]
            .as_str()
            .or_else(|| caller.team.as_ref().map(|t| t.org.as_str()));
        let team = args["team"]
            .as_str()
            .or_else(|| caller.team.as_ref().map(|t| t.team.as_str()));
        let cross_team = org != caller.team.as_ref().map(|t| t.org.as_str())
            || team != caller.team.as_ref().map(|t| t.team.as_str());
        if cross_team && !grant.iter().any(|g| g == "inbox:read:all-teams") {
            return Err("denied: grant");
        }
    }
    Ok(())
}

impl Dispatcher {
    pub fn set_flushing(&mut self, frame_id: &str, flushing: bool) {
        if flushing {
            self.flushing.insert(frame_id.into());
        } else {
            self.flushing.remove(frame_id);
        }
    }
    pub fn dispatch_frame(
        &mut self,
        frame: &crate::dashboard_stream::frame::FrameRecord,
        verb: &str,
        args: Value,
        policy: &Policy,
        now: u64,
    ) -> Decision {
        if verb == "loadMedia"
            && (!frame.policy.inbox_media
                || frame.message.is_some()
                || frame.kind == crate::dashboard_stream::frame::FrameCallerKind::Message
                || frame.grant.slot != "right-pane")
        {
            return Decision::Deny {
                reason: "denied: loadMedia requires the Inbox pane".into(),
            };
        }
        let caller = Caller::from(frame);
        self.dispatch_with_grant(caller, verb, args, policy, now, &frame.effective_grant)
    }
    // Raw callers are a policy-test seam; production always dispatches a stored frame.
    #[cfg(test)]
    pub fn dispatch(
        &mut self,
        caller: Caller,
        verb: &str,
        args: Value,
        policy: &Policy,
        now: u64,
    ) -> Decision {
        self.dispatch_with_grant(
            caller,
            verb,
            args,
            policy,
            now,
            &super::capabilities::full(),
        )
    }
    fn dispatch_with_grant(
        &mut self,
        caller: Caller,
        verb: &str,
        args: Value,
        policy: &Policy,
        now: u64,
        grant: &[String],
    ) -> Decision {
        let registry = builtin_registry();
        let spec = registry.get(verb);
        let call_id = uuid::Uuid::new_v4().to_string();
        let mut call = Call {
            caller,
            verb: verb.into(),
            args,
            call_id: call_id.clone(),
            allow_always: false,
        };
        let deny = |reason: &str| {
            audit(&call, spec, "deny", reason);
            Decision::Deny {
                reason: reason.into(),
            }
        };
        let Some(spec) = spec else {
            return deny("not-allowed");
        };
        if let Err(reason) = check_capability(
            &call.caller,
            grant,
            spec,
            &call.args,
            policy.preset == super::Preset::TeamSafe,
        ) {
            return deny(reason);
        }
        if policy.denied(&call.caller, spec) {
            return deny("denied: message frame");
        }
        if spec.confirmable
            && self.pending.values().any(|p| {
                p.caller.frame_id == call.caller.frame_id
                    && p.caller.principal == call.caller.principal
            })
        {
            return deny("busy");
        }
        let record_write = verb.starts_with("records.") && spec.class == Class::Metadata;
        if record_write && !self.flushing.contains(&call.caller.frame_id) {
            let key = (
                call.caller.frame_id.clone(),
                call.args["collection"].as_str().unwrap_or("").to_owned(),
            );
            let window = self.record_windows.entry(key).or_default();
            window.retain(|(at, _)| now.saturating_sub(*at) < 10_000);
            if window.len() >= 30 {
                return deny("denied: write budget");
            }
            window.push((now, call_id.clone()));
        }
        let max = if record_write {
            0
        } else {
            match spec.class {
                Class::Metadata | Class::Send => 10,
                Class::Prefs => 30,
                Class::Spawn if cap_free(verb, &call.args) => 10,
                _ => 0,
            }
        };
        if max > 0 && !self.flushing.contains(&call.caller.frame_id) {
            let window = self
                .windows
                .entry((
                    call.caller.principal.clone(),
                    call.caller.frame_id.clone(),
                    spec.class,
                ))
                .or_default();
            window.retain(|(at, _)| now.saturating_sub(*at) < 10_000);
            if window.len() >= max {
                let class = if spec.class == Class::Metadata {
                    "write"
                } else if spec.class == Class::Prefs {
                    "prefs"
                } else if spec.class == Class::Send {
                    "send"
                } else {
                    "spawn"
                };
                return deny(&format!("denied: {class} budget"));
            }
            window.push((now, call_id.clone()));
        }
        let confirm = policy.confirm(&call.caller, spec);
        // A headed dir/epic opens a terminal without launching an agent.
        let confirm = if verb == "openWorkspace"
            && cap_free(verb, &call.args)
            && call.args["agent"].is_null()
            && call.args["target"]["agent"].is_null()
        {
            Confirm::Never
        } else {
            confirm
        };
        if confirm != Confirm::Never {
            call.allow_always = confirm == Confirm::PromptOnce;
            let allow_always = call.allow_always;
            self.pending.insert(call_id.clone(), call);
            Decision::Pending {
                confirm_id: call_id,
                allow_always,
            }
        } else {
            self.calls.insert(call_id.clone(), call);
            Decision::Admit { call_id }
        }
    }
    pub fn answer(&mut self, id: &str, allow: bool) -> Result<Call, String> {
        let call = self
            .pending
            .remove(id)
            .ok_or("unavailable: no such confirmation")?;
        if allow {
            self.calls.insert(id.into(), call.clone());
        } else {
            if call.verb.starts_with("records.") {
                if let Some(window) = self.record_windows.get_mut(&(
                    call.caller.frame_id.clone(),
                    call.args["collection"].as_str().unwrap_or("").to_owned(),
                )) {
                    window.retain(|(_, key)| key != id);
                }
            }
            audit(
                &call,
                builtin_registry().get(&call.verb),
                "deny",
                "cancelled",
            );
        }
        Ok(call)
    }
    pub fn pending_call(&self, id: &str) -> Option<&Call> {
        self.pending.get(id)
    }
    pub fn settle(&mut self, id: &str, landed: bool, reason: &str) -> Result<(), String> {
        let call = self.calls.remove(id).ok_or("unavailable: no such call")?;
        let registry = builtin_registry();
        let spec = registry.get(&call.verb);
        if !landed && call.verb.starts_with("records.") {
            if let Some(window) = self.record_windows.get_mut(&(
                call.caller.frame_id.clone(),
                call.args["collection"].as_str().unwrap_or("").to_owned(),
            )) {
                window.retain(|(_, key)| key != id);
            }
        }
        if !landed && spec.is_some_and(|s| s.class == Class::Metadata) {
            if let Some(window) = self.windows.get_mut(&(
                call.caller.principal.clone(),
                call.caller.frame_id.clone(),
                Class::Metadata,
            )) {
                window.retain(|(_, key)| key != id);
            }
        }
        if !landed || !call.verb.starts_with("records.") {
            audit(&call, spec, if landed { "allow" } else { "deny" }, reason);
        }
        Ok(())
    }
}
