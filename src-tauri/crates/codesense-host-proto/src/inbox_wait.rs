// SPDX-License-Identifier: MIT
//! What an agent's inbox waiter and its host share: the notice that wakes the agent,
//! how one wait ends, and the lines the two exchange. The waiter is the host's own
//! helper (`--inbox-wait`), run by an agent CLI's hook; every host (this Mac's and each
//! machine's) speaks these, so each exists once.

/// The "you have mail" notice for `session_id`: what its inbox waiter hands the agent,
/// which wakes it. It names the session, so the agent calls read_messages without first
/// running a shell command to learn its own `$CODESENSE_TAB_SESSION_ID`. Never carries a
/// message body.
pub fn mail_notice(count: u32, session_id: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!(
        "You have {count} new message{plural}. Call read_messages with session_id {session_id}."
    )
}

/// One notice can point to direct mail and shared workspace posts without copying either.
pub fn mail_and_chat_notice(count: u32, session: &str, chat: Option<(&str, u32)>) -> String {
    let Some((workspace, group_count)) = chat.filter(|(_, n)| *n > 0) else {
        return mail_notice(count, session);
    };
    let workspace = workspace.strip_prefix("story:").unwrap_or(workspace);
    let group = format!("{group_count} new group-chat message{} in {workspace}. Call workspace_chat_read with sessionId {session}.", if group_count == 1 { "" } else { "s" });
    if count == 0 {
        format!("You have {group}")
    } else {
        format!("{} You also have {group}", mail_notice(count, session))
    }
}

/// How an inbox wait ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InboxWait {
    /// New mail: the notice the waiter hands the agent, which wakes it (or reaches its
    /// running turn at its next model request).
    Mail(String),
    /// Nothing within the hold: the waiter asks again.
    Quiet,
    /// The waiter ends without waking the agent: a newer waiter of the same session took
    /// over, one was already waiting, the session ended, or no one can wait for it.
    Replaced,
}

/// A notice token as it travels to a waiter: letters, digits and dashes only, so it fits
/// the waiter's one-line answers.
pub fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The host's answer to a waiter, as its line(s): `mail <token>` then the notice's line,
/// `quiet`, or `replaced`. A notice is kept to one line.
pub fn answer_lines(outcome: &InboxWait, token: &str) -> String {
    match outcome {
        InboxWait::Mail(notice) => format!("mail {token}\n{}\n", notice.replace('\n', " ")),
        InboxWait::Quiet => "quiet\n".into(),
        InboxWait::Replaced => "replaced\n".into(),
    }
}

/// The first line of a host's answer read back: `Some(Ok(token))` for mail (its notice is
/// the next line), `Some(Err(outcome))` for quiet or replaced, `None` for anything else.
pub fn parse_answer(line: &str) -> Option<Result<&str, InboxWait>> {
    match line.trim_end() {
        "quiet" => Some(Err(InboxWait::Quiet)),
        "replaced" => Some(Err(InboxWait::Replaced)),
        answer => answer
            .strip_prefix("mail ")
            .filter(|token| valid_token(token))
            .map(Ok),
    }
}

/// What a waiter answers once it printed the notice reserved under `token`.
pub fn printed_line(token: &str) -> String {
    format!("printed {token}\n")
}

/// The longest line of the inbox waiter protocol (read with
/// [`read_message`](crate::reverse_mcp::read_message)): a notice line, with room to spare.
pub const MAX_LINE: usize = 4096;

/// Writes `text` (whole lines of the waiter protocol) to `stream`.
pub fn send_lines(stream: &mut impl std::io::Write, text: &str) -> std::io::Result<()> {
    stream.write_all(text.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiter_and_its_host_read_back_what_the_other_wrote() {
        let mail = answer_lines(&InboxWait::Mail("You have\nmail.".into()), "t-1");
        assert_eq!(mail, "mail t-1\nYou have mail.\n");
        assert_eq!(parse_answer(mail.lines().next().unwrap()), Some(Ok("t-1")));
        for outcome in [InboxWait::Quiet, InboxWait::Replaced] {
            assert_eq!(
                parse_answer(&answer_lines(&outcome, "")),
                Some(Err(outcome))
            );
        }
        assert_eq!(parse_answer("mail bad token"), None);
        assert_eq!(parse_answer("what"), None);
        assert_eq!(printed_line("t-1"), "printed t-1\n");
        assert!(valid_token("0a-B") && !valid_token("") && !valid_token("a b"));
    }

    #[test]
    fn the_notice_counts_the_mail_and_names_the_session_to_read_it_with() {
        assert_eq!(
            mail_notice(1, "S1"),
            "You have 1 new message. Call read_messages with session_id S1."
        );
        assert_eq!(
            mail_notice(3, "S1"),
            "You have 3 new messages. Call read_messages with session_id S1."
        );
    }
}
