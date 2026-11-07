// SPDX-License-Identifier: MIT
//! `feedback_send`: an agent's beta feedback. Nothing sends it now: the install token
//! it was posted with is gone, so every call is refused (`availability`). The sender
//! behind it is removed too; whether the tool stays is still undecided.

use super::tool_envelope::ToolEnvelope;

pub(crate) const DESCRIPTION: &str = "Send beta feedback to the CodeSense team, the same way the person does from Help → Send beta feedback. Use it to report a CodeSense bug you hit (a tool that errors or returns something wrong, a screen that misbehaves), an idea that would make CodeSense better, or a question about how CodeSense works. The CodeSense team reads every one and replies to the person by email; it is not a support chat and you get no answer here. Args: {kind} \"bug\" | \"idea\" | \"question\"; {text} 1 to 5000 characters, plain words: what you did, what you expected, what happened instead, and the CodeSense tool or screen involved. Do not paste code, file contents, repository or file names, terminal output, secrets or personal details into {text}: say what kind of thing happened, not the user's work. {include_logs} optional, default false: true attaches the last 200 KB of the CodeSense app log with keys, tokens, email addresses and home folder names removed (the same log the person can choose to attach); set it only for a bug the log may explain. {session_id} optional: identity comes from the verified connection; if supplied, it must name the same caller. An unbound MCP caller is refused. The feedback is marked as from an agent and names your tab (see who_am_i). No screenshots. Beta builds only: in other builds it sends nothing and says so. Limits: 5 submissions an hour from this Mac, person and agents together. Returns {status:\"ok\", data:{id, kind, from, includedLogs}} or {status:\"validation\", data:{message}} (bad kind, empty or too long text, not a beta build, this Mac not activated, the hourly limit, or the server not reachable).";

/// The beta's refusal. Feedback was posted with the beta license's install token, and no build
/// has a license any more, so there is nothing to send it with.
pub(crate) const FEEDBACK_REFUSED: &str =
    "Feedback can't be sent from this build of CodeSense, so nothing was sent.";

/// Refuses in every build.
pub(crate) fn availability() -> Result<(), String> {
    if cfg!(cs_channel = "internal") {
        Err("Beta feedback is only in CodeSense beta builds. This build is not one, so nothing was sent.".into())
    } else {
        Err(FEEDBACK_REFUSED.into())
    }
}

/// A caller reaching this Mac from another machine, as its authority knows it.
pub(crate) struct RemoteCaller<'a> {
    /// The session of the tab it runs in, when it runs in one.
    pub tab_session: Option<&'a str>,
    /// The remote agent task it serves, when it is one.
    pub task: Option<&'a str>,
    pub machine: &'a str,
}

/// Who the feedback is from, before the tab is looked up.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Caller {
    /// Named by this session's tab.
    Session(String),
    /// A remote caller with no tab, named by what its authority knows.
    Label(String),
    /// Not known: still marked as from an agent.
    Unknown,
}

/// Attribution uses the connection or captured remote authority, never a claim.
pub(crate) fn caller(
    _claimed: Option<&str>,
    connection: Option<String>,
    remote: Option<RemoteCaller<'_>>,
) -> Caller {
    let clean = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(remote) = remote {
        if let Some(session) = remote.tab_session.and_then(clean) {
            return Caller::Session(session);
        }
        return Caller::Label(match remote.task.and_then(clean) {
            Some(task) => format!("remote agent task {task} on machine {}", remote.machine),
            None => format!("remote agent on machine {}", remote.machine),
        });
    }
    match connection.as_deref().and_then(clean) {
        Some(session) => Caller::Session(session),
        None => Caller::Unknown,
    }
}

/// How a feedback caller is labelled: its tab name, and its agent when known.
pub(crate) fn agent_label(display_name: &str, agent: Option<&str>) -> String {
    match agent.map(str::trim).filter(|a| !a.is_empty()) {
        Some(agent) if agent != display_name => format!("{display_name} ({agent})"),
        _ => display_name.to_string(),
    }
}

fn invalid(message: String) -> ToolEnvelope<serde_json::Value> {
    ToolEnvelope::Validation { message }
}

/// Refuses every agent submission with a validation envelope; nothing is read or sent.
pub(crate) async fn send(
    _app: &tauri::AppHandle,
    _kind: &str,
    _text: &str,
    _include_logs: bool,
    _agent: Option<String>,
) -> ToolEnvelope<serde_json::Value> {
    invalid(availability().unwrap_err())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(cs_channel = "internal")]
    #[test]
    fn outside_the_beta_the_tool_refuses_and_says_why() {
        let err = availability().unwrap_err();
        assert!(err.contains("only in CodeSense beta builds"));
        assert!(err.contains("nothing was sent"));
    }

    /// The beta has no install token to send with, so it refuses too.
    #[cfg(not(cs_channel = "internal"))]
    #[test]
    fn in_the_beta_the_tool_refuses_and_says_why() {
        let err = availability().unwrap_err();
        assert!(err.contains("can't be sent from this build"));
        assert!(err.contains("nothing was sent"));
    }

    #[test]
    fn the_description_tells_an_agent_what_to_send_and_what_not_to() {
        for words in [
            "bug",
            "idea",
            "question",
            "Do not paste code, file contents, repository or file names, terminal output",
            "include_logs",
            "Beta builds only",
            "sends nothing",
        ] {
            assert!(DESCRIPTION.contains(words), "{words}");
        }
    }

    #[test]
    fn the_connections_own_tab_wins_over_a_claimed_session() {
        let session = |s: &str| Caller::Session(s.into());
        assert_eq!(
            caller(Some("someone-else"), Some("mine".into()), None),
            session("mine")
        );
        assert_eq!(caller(Some(" claimed "), None, None), Caller::Unknown);
        assert_eq!(
            caller(Some("claimed"), Some("  ".into()), None),
            Caller::Unknown
        );
        assert_eq!(caller(None, None, None), Caller::Unknown);
        assert_eq!(caller(Some(" "), None, None), Caller::Unknown);
    }

    #[test]
    fn a_remote_caller_is_its_tab_or_its_remote_label_never_a_claimed_session() {
        let remote = |tab_session, task| RemoteCaller {
            tab_session,
            task,
            machine: "m-1",
        };
        assert_eq!(
            caller(
                Some("someone-else"),
                None,
                Some(remote(Some("its-tab"), None))
            ),
            Caller::Session("its-tab".into())
        );
        assert_eq!(
            caller(
                Some("someone-else"),
                None,
                Some(remote(None, Some("task-7")))
            ),
            Caller::Label("remote agent task task-7 on machine m-1".into())
        );
        assert_eq!(
            caller(
                Some("someone-else"),
                Some("local".into()),
                Some(remote(None, None))
            ),
            Caller::Label("remote agent on machine m-1".into())
        );
    }

    #[test]
    fn the_admin_label_names_the_tab_and_the_agent() {
        assert_eq!(agent_label("Amelia", Some("claude")), "Amelia (claude)");
        assert_eq!(agent_label("claude", Some("claude")), "claude");
        assert_eq!(agent_label("Amelia", None), "Amelia");
        assert_eq!(agent_label("Amelia", Some(" ")), "Amelia");
    }
}
