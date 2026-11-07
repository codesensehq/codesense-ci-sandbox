// SPDX-License-Identifier: MIT
use serde_json::{json, Value};
/// Describes the page binding; handlers retain their existing validation.
pub fn args_schema(name: &str) -> Value {
    if name == "openDiff" {
        return json!({"type":"object","properties":{"repo":{"type":"string"},"rev":{"type":"string"},"path":{"type":"string"},"storyId":{"type":"string"},"range":{"type":"object","properties":{"start":{"type":"integer","minimum":1},"end":{"type":"integer","minimum":1},"side":{"enum":["after"]}},"required":["start","end"],"additionalProperties":false}},"required":["repo","rev","path"],"additionalProperties":false});
    }
    if name == "storyChanges" {
        return serde_json::to_value(rmcp::schemars::schema_for!(crate::session_usage::story_changes::Params)).expect("story changes schema");
    }
    if name == "storyTotals" {
        return serde_json::to_value(rmcp::schemars::schema_for!(crate::session_usage::story_totals::Params)).expect("story totals schema");
    }
    if name == "whyLine" {
        return serde_json::to_value(rmcp::schemars::schema_for!(crate::attribution::why_line::Params)).expect("why line schema");
    }
    if name == "sessionRead" {
        return serde_json::to_value(rmcp::schemars::schema_for!(crate::session_usage::session_read::Params)).expect("session read schema");
    }
    let string = || json!({"type":"string"});
    if name.starts_with("records.") {
        let mut required = if name == "records.collections" {
            vec![]
        } else if matches!(name, "records.list" | "records.search" | "records.create") {
            vec!["collection"]
        } else {
            vec!["collection", "id"]
        };
        if name == "records.search" {
            required.push("query");
        }
        if matches!(name, "records.readAt" | "records.restore") {
            required.push("commit");
        }
        if name == "records.setFields" {
            required.push("fields");
        }
        return json!({"type":"object","properties":{"collection":string(),"id":string(),"query":string(),"body":string(),"base":string(),"append":string(),"patch":{"type":"object","properties":{"find":string(),"replace":string()},"required":["find","replace"]},"fields":{"type":"object"},"commit":string(),"undo":{"type":"boolean"},"requestId":{"type":"string","maxLength":64},"limit":{"type":"integer"},"window":{"type":"array","items":{"type":"integer"},"minItems":2,"maxItems":2}},"required":required,"additionalProperties":false});
    }
    let properties = match name {
        "files.list" => {
            json!({"scope":{"enum":["team","private","story","agent"]},"team":string(),"storyId":string(),"agentId":string(),"dir":string()})
        }
        "files.read" => json!({"path":string(),"maxBytes":{"type":"integer","minimum":0}}),
        "files.move" => json!({"from":string(),"to":string()}),
        "files.delete" => json!({"path":string()}),
        "files.create" => {
            json!({"path":string(),"content":string(),"onCollision":{"enum":["keep-both","fail"]}})
        }
        "dismiss_agent_mark" => {
            json!({"sessionId":string(),"crashId":string(),"requestId":string()})
        }
        "subscribe" => {
            json!({"topic":string(),"collection":string(),"scope":string(),"team":string(),"storyId":string(),"agentId":string(),"dir":string(),"sinceRev":{"type":"integer"},"window":{"type":"array","items":{"type":"integer","minimum":0},"minItems":2,"maxItems":2}})
        }
        "page" => json!({"topic":string(),"collection":string(),"cursor":string()}),
        "markUnread" => {
            json!({"messageIds":{"type":"array","items":{"type":"integer","minimum":1,"maximum":9_007_199_254_740_991_i64},"minItems":1},"org":string(),"team":string()})
        }
        "inbox" => {
            json!({"conversation":{"type":"string","minLength":1},"before":{"type":"integer","minimum":1}})
        }
        "unsubscribe" => json!({"topic":string(),"collection":string()}),
        "setUiState" => json!({"topic":string(),"patch":{"type":"object"},"requestId":string()}),
        "stopSession" | "closeTerminalTab" | "openAgentInbox" => json!({"handle":string()}),
        "openLauncher" => json!({"task":string(),"org":string(),"team":string()}),
        "openRoutineDraft" => {
            json!({"name":string(),"task":string(),"when":string(),"agent":string()})
        }
        "launchUnder" => json!({"handle":string(),"task":string()}),
        "gotoAgent" => json!({"agentId":string(),"preserveFocus":{"type":"boolean"}}),
        "moveWorkspace" => {
            json!({"teamKey":string(),"workspaceId":string(),"before":string(),"after":string()})
        }
        "showMenu" | "showPopover" => json!({
            "anchorRect":{"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"},"width":{"type":"number"},"height":{"type":"number"}},"required":["x","y","width","height"]},
            "target":{"enum":["row","text","frame"]},
            "text":{"type":"object","properties":{"editable":{"type":"boolean"},"hasSelection":{"type":"boolean"},"hasText":{"type":"boolean"},"password":{"type":"boolean"}}},
            "anchorId":string(),"row":{"type":"object"},"items":{"type":"array","items":{"$ref":"#/$defs/menuItem"}},"kind":string(),"sessionId":string()
        }),
        "focusOut" => json!({"direction":{"enum":["forward","backward"]}}),
        "sendToSession" => json!({"target":string(),"text":string()}),
        "sendPrompt" => json!({"handle":string(),"text":string(),"submit":{"type":"boolean"}}),
        "sendToWorkspaceTab" => {
            json!({"sessionId":string(),"text":string(),"mode":{"enum":["prompt","command"]}})
        }
        "sendAsHuman" | "postMessage" => {
            json!({"to":string(),"text":string(),"team":{"type":"object"}})
        }
        "loadMedia" => {
            json!({"messageId":{"type":"integer","minimum":1},"org":string(),"team":string()})
        }
        "replyAsHuman" => json!({"messageId":{"type":"integer"},"text":string()}),
        "react" => {
            json!({"messageId":{"type":"integer"},"emoji":string(),"remove":{"type":"boolean"}})
        }
        "runPrompt" => json!({"target":string(),"prompt":string()}),
        "runSkill" => json!({"skill":string(),"args":{"type":"array","items":string()}}),
        "setStatus" => {
            json!({"path":{"oneOf":[string(),{"type":"array","items":string()}]},"status":string()})
        }
        "setField" => {
            json!({"path":string(),"field":string(),"value":{},"fields":{"type":"object"}})
        }
        "closeWorkspace" => {
            json!({"target":string(),"removeWorktree":{"type":"boolean"},"discardUnsaved":{"type":"boolean"}})
        }
        "openWorkspace" => {
            json!({"storyId":string(),"target":{"type":"object","properties":{"kind":{"enum":["story","epic","branch","dir"]},"storyId":string(),"path":string()}},"headed":{"type":"boolean"},"initialPrompt":string(),"sessionMode":{"enum":["new","focus"]}})
        }
        "openTab" => json!({"workspaceId":string(),"agent":{"type":"boolean"}}),
        "openFile" => {
            json!({"path":string(),"team":string(),"newTab":{"type":"boolean"},"preview":{"type":"boolean"}})
        }
        "readWorkspace" => json!({"path":string(),"team":string()}),
        "routineResult" | "routineStatus" | "routineRefresh" | "routineCancel" | "routine" => {
            json!({"key":string()})
        }
        "getWorkspaceStatus" => json!({"storyId":string()}),
        "commands.register" => json!({"ids":{"type":"array","items":string()}}),
        "commands.run" => json!({"id":string(),"args":{}}),
        _ => json!({}),
    };
    let required: &[&str] = match name {
        "loadMedia" => &["messageId"],
        "files.read" | "files.delete" => &["path"],
        "files.move" => &["from", "to"],
        "files.create" => &["path", "onCollision"],
        "markUnread" => &["messageIds"],
        "dismiss_agent_mark" => &["sessionId", "crashId"],
        "showMenu" => &["anchorRect"],
        "showPopover" => &["kind", "anchorRect"],
        "subscribe" | "unsubscribe" => &["topic"],
        "page" => &["topic", "cursor"],
        "setUiState" => &["topic", "patch"],
        "stopSession" | "closeTerminalTab" | "launchUnder" | "openAgentInbox" => &["handle"],
        "openLauncher" => &["task"],
        "openRoutineDraft" => &["name", "task"],
        "gotoAgent" => &["agentId"],
        "moveWorkspace" => &["teamKey", "workspaceId"],
        "sendToSession" => &["target", "text"],
        "sendPrompt" => &["handle", "text"],
        "sendToWorkspaceTab" => &["sessionId", "text"],
        "runPrompt" => &["target", "prompt"],
        "runSkill" => &["skill"],
        "setStatus" => &["path", "status"],
        "setField" => &["path"],
        "openFile" | "readWorkspace" => &["path"],
        "closeWorkspace" => &["target"],
        "commands.register" => &["ids"],
        "commands.run" => &["id"],
        _ => &[],
    };
    let mut schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":true});
    if matches!(name, "showMenu" | "showPopover") {
        schema["$defs"] = json!({
            "menuGlyph":{"oneOf":[{"type":"string"},
                {"type":"object","properties":{"kind":{"const":"stageRail"},"role":{"enum":["not_started","active","review","blocked","done"]},"ready":{"type":"boolean"}},"required":["kind"]},
                {"type":"object","properties":{"kind":{"const":"statusMark"},"mark":{"type":"string"},"size":{"enum":[6,8]},"still":{"type":"boolean"}},"required":["kind","mark"]},
                {"type":"object","properties":{"kind":{"const":"fileKindIcon"},"fileKind":{"type":"string"}},"required":["kind","fileKind"]}]},
            "menuItem":{"oneOf":[{"type":"string"},{"type":"object","properties":{
                "id":{"type":"string"},"label":{"type":"string"},"icon":{"$ref":"#/$defs/menuGlyph"},"count":{"type":"number"},
                "checked":{"type":"boolean"},"enabled":{"type":"boolean"},"separator":{"type":"boolean"},"radioGroup":{"type":"string"},
                "submenu":{"type":"array","items":{"$ref":"#/$defs/menuItem"}},
                "segmented":{"type":"array","items":{"$ref":"#/$defs/menuItem"}}
            },"required":["id"]}]}
        });
    }
    if name == "moveWorkspace" {
        schema["oneOf"] = json!([{"required":["before"]},{"required":["after"]}]);
    }
    schema
}
pub fn result_schema(name: &str) -> Value {
    if name == "openDiff" {
        return json!({"oneOf":[{"type":"object","properties":{"status":{"const":"opened"}},"required":["status"]},{"type":"object","properties":{"status":{"const":"commitNotLocal"},"reason":{"type":"string"},"fetchCommand":{"type":"string"}},"required":["status","reason","fetchCommand"]},{"type":"object","properties":{"status":{"enum":["worktreeRemoved","refused","unavailable"]},"reason":{"type":"string"}},"required":["status","reason"]}]});
    }
    match name {
        "files.list" => {
            json!({"type":"object","properties":{"rows":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"path":{"type":"string"},"name":{"type":"string"},"kind":{"enum":["dir","file","story","epic"]},"size":{"type":["number","null"]},"mtime":{"type":["number","null"]},"inheritedFrom":{"type":"string"},"gitState":{"type":"string"}},"required":["id","path","name","kind"]}}},"required":["rows"]})
        }
        "files.read" => {
            json!({"type":"object","properties":{"text":{"type":"string"},"bytesUrl":{"type":"string"},"size":{"type":"number"},"truncated":{"type":"boolean"},"sha256":{"type":"string"}},"required":["size","truncated","sha256"]})
        }
        "files.create" => {
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
        }
        "files.delete" => {
            json!({"type":"object","properties":{"deletedPath":{"type":"string"}},"required":["deletedPath"]})
        }
        "files.move" => {
            json!({"type":"object","properties":{"fromPath":{"type":"string"},"toPath":{"type":"string"}},"required":["fromPath","toPath"]})
        }
        "markUnread" => {
            json!({"type":"object","properties":{"marked":{"type":"integer","minimum":0}},"required":["marked"]})
        }
        "openLauncher" | "openRoutineDraft" => {
            json!({"type":"object","properties":{"opened":{"type":"boolean"}},"required":["opened"]})
        }
        "moveWorkspace" => {
            json!({"type":"object","properties":{"moved":{"type":"boolean"}},"required":["moved"]})
        }
        "showMenu" => {
            json!({"oneOf":[{"type":"object","properties":{"closed":{"const":true}},"required":["closed"]},{"type":"object","properties":{"itemId":{"type":"string"},"row":{},"builtin":{"type":"boolean"}},"required":["itemId","row"]}]})
        }
        "showPopover" => {
            json!({"type":"object","properties":{"closed":{"const":true}},"required":["closed"]})
        }
        "focusOut" | "focusTerminal" => {
            json!({"type":"object","properties":{"focused":{"type":"boolean"}},"required":["focused"]})
        }
        "getDocDir" | "getEpicHandle" => json!({"type":["string","null"]}),
        "commands.list" => {
            json!({"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"label":{"type":"string"},"scope":{"enum":["global","slot-visible","slot-focused"]},"binding":{"type":"string"},"dashboard":{"type":"string"},"active":{"type":"boolean"},"humanOnly":{"type":"boolean"}},"required":["id","label","scope","binding","dashboard","active","humanOnly"]}})
        }
        "commands.run" => {
            json!({"type":"object","properties":{"requested":{"const":true}},"required":["requested"]})
        }
        "commands.register" => {
            json!({"type":"object","properties":{"active":{"type":"array","items":{"type":"string"}}},"required":["active"]})
        }
        "closeTerminalTab" => {
            json!({"type":"object","properties":{"closed":{"type":"boolean"}},"required":["closed"]})
        }
        "setStatus" => {
            json!({"type":"array","items":{"type":"object","properties":{"path":{"type":"string"},"ok":{"type":"boolean"}}}})
        }
        "sessions" | "agents" | "worktrees" | "machines" | "routines" => {
            json!({"type":"array","items":{"type":"object"}})
        }
        _ => {
            json!({"description":"JSON result of the existing host handler; see get_live_data_contract for its fields."})
        }
    }
}
