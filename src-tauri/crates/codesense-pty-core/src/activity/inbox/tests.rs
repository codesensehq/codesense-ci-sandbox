// SPDX-License-Identifier: MIT
use super::*;
use crate::activity::ActivityService;
use crate::domain::InboxWaiterState;
use crate::inbox_host::InboxOutlet;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Outlet {
    told: Mutex<Vec<String>>,
    states: Mutex<Vec<InboxWaiterState>>,
    audience: Mutex<Option<String>>,
}

impl InboxOutlet for Outlet {
    fn fate(&self, _session: &str, token: &str, fate: crate::inbox_host::Fate) {
        if fate == crate::inbox_host::Fate::Told {
            self.told.lock().unwrap().push(token.to_string());
        }
    }
    fn waiter(&self, state: InboxWaiterState) {
        self.states.lock().unwrap().push(state);
    }
    fn audience(&self, _: &str) -> Option<String> {
        self.audience.lock().unwrap().clone()
    }
    fn has_tools(&self, _: &str) -> Option<bool> {
        Some(true)
    }
}

/// A host's activity owner and ingress, with an outlet that records its inbox reports.
struct Host {
    _root: tempfile::TempDir,
    owner: Arc<ActivityOwner>,
    service: ActivityService,
    outlet: Arc<Outlet>,
}

fn host() -> Host {
    host_with(ActivityOwner::new())
}

fn host_with(owner: ActivityOwner) -> Host {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let owner = Arc::new(owner);
    let outlet = Arc::new(Outlet::default());
    owner.set_inbox_outlet(outlet.clone());
    let service = ActivityService::start(root.path(), &owner).unwrap();
    Host {
        _root: root,
        owner,
        service,
        outlet,
    }
}

impl Host {
    /// A native Claude launch of `session` reserved (not committed): its writer token.
    fn prepare(&self, session: &str) -> String {
        self.owner
            .prepare_native(
                session,
                AgentCli::Claude,
                self.service.path().to_str().unwrap(),
            )
            .unwrap()
            .token
    }
}

fn me() -> u32 {
    std::process::id()
}

fn this() -> process::Process {
    process::inspect(me()).unwrap()
}

fn ask(event: &str, agent: u32) -> Ask {
    Ask {
        receipts: Vec::new(),
        event: event.into(),
        agent,
        prompt: None,
        session: None,
    }
}

/// A live child of this process, killed when dropped.
struct Child(std::process::Child);

impl Child {
    fn spawn() -> Self {
        Self(
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap(),
        )
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn process(&self) -> process::Process {
        process::inspect(self.pid()).unwrap()
    }
}

/// A process run under a shell this process runs, as an agent's tool command runs: ended
/// with the shell when dropped.
fn under_a_shell() -> Shell {
    use std::io::BufRead;
    let mut shell = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30 & echo $!; wait"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(shell.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let run = process::inspect(line.trim().parse().expect("the shell names its run")).unwrap();
    Shell(Child(shell), run)
}

/// A shell from [`under_a_shell`] and the process it runs.
struct Shell(Child, process::Process);

impl Shell {
    fn pid(&self) -> u32 {
        self.0.pid()
    }

    fn run(&self) -> process::Process {
        self.1
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        // SAFETY: signalling the run this shell started, before the shell is reaped.
        unsafe {
            libc::kill(self.1.pid as i32, libc::SIGKILL);
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn admitted(admission: InboxAdmission) -> Option<String> {
    match admission {
        InboxAdmission::Admitted { session, .. } => Some(session),
        _ => None,
    }
}

#[test]
fn only_a_waiter_run_by_the_invocation_its_host_named_is_admitted() {
    let host = host();
    let token = host.prepare("S1");
    host.owner.register("S1", Some(me()));
    // The agent's own waiter: it names the invocation and runs under it.
    assert_eq!(
        admitted(host.owner.inbox_admission(&token, this(), me(), None)),
        Some("S1".into())
    );
    // A `claude` run inside the agent's turn, under the shell its tool runs: its waiter
    // names that nested process.
    let nested = under_a_shell();
    assert!(matches!(
        host.owner
            .inbox_admission(&token, nested.run(), nested.run().pid, None),
        InboxAdmission::Refused
    ));
    // A waiter outside the agent (detached from it) naming the agent.
    let invocation = Child::spawn();
    let detached_token = host.prepare("S2");
    host.owner.register("S2", Some(invocation.pid()));
    assert!(matches!(
        host.owner
            .inbox_admission(&detached_token, this(), invocation.pid(), None),
        InboxAdmission::Refused
    ));
    // A token no invocation of this host holds.
    assert!(matches!(
        host.owner.inbox_admission("foreign", this(), me(), None),
        InboxAdmission::Refused
    ));
    assert!(matches!(
        host.owner.inbox_admission("", this(), me(), None),
        InboxAdmission::Refused
    ));
}

#[test]
fn a_waiter_that_asks_before_the_commit_is_held_then_admitted_or_refused() {
    let host = host();
    let token = host.prepare("S1");
    assert!(matches!(
        host.owner.inbox_admission(&token, this(), me(), None),
        InboxAdmission::Pending { .. }
    ));
    host.owner.register("S1", Some(me()));
    assert_eq!(
        admitted(host.owner.inbox_admission(&token, this(), me(), None)),
        Some("S1".into())
    );
    // Committed to another process, the early waiter is refused, never ranked.
    let other = Child::spawn();
    let token = host.prepare("S2");
    host.owner.register("S2", Some(other.pid()));
    assert!(matches!(
        host.owner.inbox_admission(&token, this(), me(), None),
        InboxAdmission::Refused
    ));
}

#[test]
fn a_restarted_invocation_takes_over_and_an_ended_one_is_refused() {
    let host = host();
    let first = host.prepare("S1");
    let incarnation = host.owner.register("S1", Some(me())).unwrap();
    assert!(admitted(host.owner.inbox_admission(&first, this(), me(), None)).is_some());
    host.owner.exit(
        "S1",
        &incarnation,
        crate::activity::Exit {
            code: Some(0),
            signal: None,
            requested: false,
        },
    );
    assert!(matches!(
        host.owner.inbox_admission(&first, this(), me(), None),
        InboxAdmission::Refused
    ));
    host.owner.remove("S1", &incarnation);
    let second = host.prepare("S1");
    host.owner.register("S1", Some(me()));
    assert!(admitted(host.owner.inbox_admission(&second, this(), me(), None)).is_some());
    assert!(matches!(
        host.owner.inbox_admission(&first, this(), me(), None),
        InboxAdmission::Refused
    ));
}

/// The helper's side of one wait, run on a thread: the notice it was handed.
fn helper(host: &Host, token: String, ask: Ask) -> std::thread::JoinHandle<Option<String>> {
    let socket = host.service.path().to_path_buf();
    let parent = std::os::unix::process::parent_id();
    std::thread::spawn(move || {
        let (notice, notice_token, mut stream) = ask_once(&socket, &token, ask, parent).ok()?;
        wire::send_lines(&mut stream, &wire::printed_line(&notice_token)).unwrap();
        Some(notice)
    })
}

fn until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_waiter_admitted_through_the_ingress_is_handed_the_pushed_notice_and_it_is_told() {
    let host = host();
    let token = host.prepare("S1");
    host.owner.register("S1", Some(me()));
    let waiting = helper(&host, token, ask("Stop", me()));
    until(|| host.owner.inbox().state("S1").waiting);
    // Its desktop heard it is waiting, which makes it check the session's inbox.
    assert!(host.outlet.states.lock().unwrap().iter().any(|s| s.waiting));
    host.owner
        .inbox_mail("S1", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    assert_eq!(
        waiting.join().unwrap().as_deref(),
        Some("You have 1 new message.")
    );
    until(|| *host.outlet.told.lock().unwrap() == ["t1"]);
    // Pushed again (its report was lost on the way): told again, never handed twice.
    host.owner
        .inbox_mail("S1", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    assert_eq!(*host.outlet.told.lock().unwrap(), ["t1", "t1"]);
    // A notice that would not fit the waiter's one line, or a token that would not, is
    // refused.
    assert!(host
        .owner
        .inbox_mail("S1", "x".repeat(wire::MAX_LINE), "t2".into())
        .is_err());
    assert!(host
        .owner
        .inbox_mail("S1", "n".into(), "bad token".into())
        .is_err());
}

#[test]
fn an_early_waiter_waits_through_the_commit_and_a_nested_one_ends_unhanded() {
    let host = host();
    let token = host.prepare("S1");
    let early = helper(&host, token.clone(), ask("SessionStart", me()));
    std::thread::sleep(Duration::from_millis(300));
    host.owner.register("S1", Some(me()));
    until(|| host.owner.inbox().state("S1").waiting);
    let nested = under_a_shell();
    let refused = helper(&host, token, ask("Stop", nested.run().pid));
    assert_eq!(refused.join().unwrap(), None);
    // The nested waiter did not take the agent's place.
    assert!(host.owner.inbox().state("S1").waiting);
    host.owner
        .inbox_mail("S1", "n".into(), "t1".into())
        .unwrap();
    assert_eq!(early.join().unwrap().as_deref(), Some("n"));
}

#[test]
fn a_waiter_of_a_session_whose_invocation_never_commits_is_refused() {
    let host = host();
    let token = host.prepare("S1");
    let other = Child::spawn();
    let waiting = helper(&host, token, ask("Stop", me()));
    std::thread::sleep(Duration::from_millis(200));
    host.owner.register("S1", Some(other.pid()));
    assert_eq!(waiting.join().unwrap(), None);
    assert!(!host.owner.inbox().state("S1").waiting);
}

#[test]
fn a_claude_waiter_asks_again_under_a_tool_calls_event_which_defers() {
    let delivery = crate::agent_cli::adapter(AgentCli::Claude).inbox();
    assert_eq!(reask_event(delivery), Some("PostToolUse"));
    let InboxDelivery::Waiter { replace_events, .. } = delivery else {
        panic!("a waiter");
    };
    assert_eq!(
        WaitArm::from_hook("PostToolUse", replace_events),
        WaitArm::IfAbsent
    );
}

/// No pause long enough to slow a test.
const QUICK: Pauses = Pauses {
    retry: Duration::from_millis(200),
    retry_max: Duration::from_millis(200),
    min_ask: Duration::ZERO,
};

/// The events a fake host was asked under, and the lines the helper sent back.
type Served = std::thread::JoinHandle<(Vec<String>, Vec<String>)>;

/// A host's ingress that answers each ask as `answers` says, in turn, and records the
/// events asked under and the lines the helper sent back.
fn fake_host(dir: &Path, answers: Vec<&'static str>) -> (PathBuf, Served) {
    let socket = dir.join("activity.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let served = std::thread::spawn(move || {
        let (mut events, mut back) = (Vec::new(), Vec::new());
        for answer in answers {
            let (mut stream, _) = listener.accept().unwrap();
            let mut length = [0; 4];
            stream.read_exact(&mut length).unwrap();
            let mut envelope = vec![0; u32::from_be_bytes(length) as usize];
            stream.read_exact(&mut envelope).unwrap();
            let envelope: serde_json::Value = serde_json::from_slice(&envelope).unwrap();
            events.push(envelope["inbox"]["event"].as_str().unwrap().to_string());
            wire::send_lines(&mut stream, answer).unwrap();
            if answer.starts_with("mail ") {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                back.push(
                    codesense_host_proto::reverse_mcp::read_message(
                        &mut stream,
                        wire::MAX_LINE,
                        |_| Ok(()),
                    )
                    // A helper that went away says nothing.
                    .unwrap_or_default(),
                );
            }
        }
        (events, back)
    });
    (socket, served)
}

#[test]
fn a_waiter_whose_hold_ended_quiet_asks_again_deferring_and_is_handed_its_mail() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, served) = fake_host(dir.path(), vec!["quiet\n", "mail t-1\nYou have mail.\n"]);
    let parent = std::os::unix::process::parent_id();
    let reask = reask_event(crate::agent_cli::adapter(AgentCli::Claude).inbox()).unwrap();
    let (notice, token, mut stream) = wait(&socket, "token", parent, QUICK, |first| {
        ask(if first { "Stop" } else { reask }, me())
    })
    .expect("handed its mail");
    assert_eq!((notice.as_str(), token.as_str()), ("You have mail.", "t-1"));
    wire::send_lines(&mut stream, &wire::printed_line(&token)).unwrap();
    let (events, back) = served.join().unwrap();
    // Its first ask takes the place; the next only re-arms one with no waiter waiting.
    assert_eq!(events, ["Stop", "PostToolUse"]);
    assert_eq!(back, [wire::printed_line("t-1")]);
}

#[test]
fn of_waiters_retrying_an_unreachable_host_only_the_newest_keeps_on() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("activity.sock");
    let parent = std::os::unix::process::parent_id();
    let waiting = {
        let socket = socket.clone();
        std::thread::spawn(move || {
            wait(&socket, "token", parent, QUICK, |_| ask("Stop", me())).is_none()
        })
    };
    let retrier = retrier_path(&socket, "token");
    until(|| retrier.exists());
    // A newer waiter of the same invocation records itself: the older one gives up.
    std::fs::write(&retrier, "a newer waiter").unwrap();
    assert!(waiting.join().unwrap());
    assert_eq!(std::fs::read_to_string(&retrier).unwrap(), "a newer waiter");
}

#[test]
fn a_retrying_waiter_that_reaches_its_host_again_gives_up_its_record() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("activity.sock");
    let parent = std::os::unix::process::parent_id();
    let retrier = retrier_path(&socket, "token");
    let waiting = {
        let socket = socket.clone();
        std::thread::spawn(move || wait(&socket, "token", parent, QUICK, |_| ask("Stop", me())))
    };
    until(|| retrier.exists());
    let (_, served) = fake_host(dir.path(), vec!["replaced\n"]);
    assert!(waiting.join().unwrap().is_none());
    served.join().unwrap();
    assert!(!retrier.exists(), "no record is left behind");
}

#[test]
fn a_codex_prompt_check_is_handed_the_held_notice_once_and_its_turn_end_is_no_check() {
    let host = host();
    let token = host
        .owner
        .prepare_native("S1", AgentCli::Codex, host.service.path().to_str().unwrap())
        .unwrap()
        .token;
    host.owner.register("S1", Some(me()));
    // On no engine (none is bound), its host starts it nothing: the notice is held for a
    // check.
    host.owner
        .inbox_mail("S1", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    assert!(host.owner.inbox().holds("S1"));
    // A turn end runs no check of Codex's, and a nested `codex` is not the invocation:
    // neither is handed it.
    let stop = helper(&host, token.clone(), ask("Stop", me()));
    assert_eq!(stop.join().unwrap(), None);
    let nested = under_a_shell();
    let refused = helper(
        &host,
        token.clone(),
        ask("UserPromptSubmit", nested.run().pid),
    );
    assert_eq!(refused.join().unwrap(), None);
    assert!(host.owner.inbox().holds("S1"));
    assert!(host.outlet.told.lock().unwrap().is_empty());
    // Its next prompt's check is handed it, and it is told once.
    let prompt = Ask {
        prompt: Some("fix the build".into()),
        ..ask("UserPromptSubmit", me())
    };
    let checked = helper(&host, token.clone(), prompt);
    assert_eq!(
        checked.join().unwrap().as_deref(),
        Some("You have 1 new message.")
    );
    until(|| *host.outlet.told.lock().unwrap() == ["t1"]);
    assert!(!host.owner.inbox().holds("S1"));
    // The next tool call's check finds nothing held: it looks once and ends.
    let tool = helper(&host, token, ask("PostToolUse", me()));
    assert_eq!(tool.join().unwrap(), None);
    assert_eq!(*host.outlet.told.lock().unwrap(), ["t1"]);
}

#[test]
fn a_one_shot_codex_is_never_submitted_to_nor_reads_as_reached() {
    let host = host();
    // This test process is the hand-typed `codex exec`; its parent stands in for the shell.
    let shell = unsafe { libc::getppid() } as u32;
    host.owner.register("typed", Some(shell));
    let (_, writer) = host
        .owner
        .claim_shell(this(), AgentCli::Codex, "unused", true)
        .unwrap();
    // Even on an engine, with its trusted hooks showing it idle.
    host.owner
        .bind_engine_for_test(
            &writer,
            host.service.path().with_file_name("engine-test.sock"),
        )
        .unwrap();
    for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
        let input = serde_json::json!({"hook_event_name": event, "session_id": "exec-1", "turn_id": "turn-1"});
        let report = crate::agent_cli::adapter(AgentCli::Codex)
            .normalize(&serde_json::to_vec(&input).unwrap(), true)
            .unwrap();
        host.owner.report_for_test(&writer, me(), report).unwrap();
    }
    host.owner.decay(Instant::now() + Duration::from_secs(6));
    let state = host.owner.inbox().state("typed");
    assert!(!state.waiting && state.grace_ms.is_none(), "{state:?}");
    host.owner
        .inbox_mail("typed", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    // Its own checks may still look, as any one-shot run's hooks do.
    assert!(host.owner.inbox().holds("typed"));
}

#[test]
fn a_one_shot_run_is_admitted_to_look_only() {
    let host = host();
    // This test process is the hand-typed `claude -p`; its parent stands in for the shell.
    let shell = unsafe { libc::getppid() } as u32;
    host.owner.register("typed", Some(shell));
    let (_, writer) = host
        .owner
        .claim_shell(this(), AgentCli::Claude, "unused", true)
        .unwrap();
    assert!(matches!(
        host.owner
            .inbox_admission(&writer.token, this(), me(), None),
        InboxAdmission::Admitted { batch: true, .. }
    ));
}

/// What a check reads of `input`, as its hook helper does.
fn check_input(input: &mut impl Read) -> crate::agent_cli::CheckInput {
    crate::agent_cli::read_check(input, MAX_PROMPT)
}

#[test]
fn a_check_reads_its_prompt_and_whose_hook_ran_it_and_drains_its_input() {
    let read = |input: &str| check_input(&mut std::io::Cursor::new(input.as_bytes().to_vec()));
    let prompt = |input: &str| read(input).prompt;
    // Only an input read whole that names no subagent is the agent's own: a subagent's,
    // one that names a subagent oddly, or one that can't be read is not.
    assert!(read(r#"{"tool_name":"Bash"}"#).main);
    assert!(!read(r#"{"tool_name":"Bash","agent_id":""}"#).main);
    assert!(read(r#"{"tool_name":"Bash","agent_id":null}"#).main);
    assert!(!read(r#"{"tool_name":"Bash","agent_id":"child"}"#).main);
    assert!(!read(r#"{"tool_name":"Bash","agent_id":7}"#).main);
    assert!(!read("not json").main);
    assert!(!read("[]").main);
    assert!(!read("42").main);
    assert!(!read(r#"{"agent_id":null,"agent_id":"child"}"#).main);
    assert_eq!(
        prompt(r#"{"prompt":"[inbox t1] hi","session_id":"s"}"#).as_deref(),
        Some("[inbox t1] hi")
    );
    assert_eq!(prompt(r#"{"tool_name":"Bash"}"#), None);
    // The CLI session it names, when it names one as a string.
    let session = |input: &str| read(input).session;
    assert_eq!(
        session(r#"{"session_id":"thread-1"}"#).as_deref(),
        Some("thread-1")
    );
    assert_eq!(session(r#"{"session_id":7}"#), None);
    assert!(read(r#"{"session_id":7}"#).main);
    assert_eq!(session(r#"{"tool_name":"Bash"}"#), None);
    assert_eq!(prompt("not json"), None);
    // A long prompt is cut on a character boundary.
    let long = format!(r#"{{"prompt":"{}"}}"#, "é".repeat(MAX_PROMPT));
    let cut = prompt(&long).unwrap();
    assert!(cut.len() <= MAX_PROMPT && cut.chars().all(|c| c == 'é'));
}

/// A reader that hands its bytes over `step` at a time, as a pipe may.
struct Trickle {
    bytes: Vec<u8>,
    at: usize,
    step: usize,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let end = (self.at + self.step.min(buf.len())).min(self.bytes.len());
        let read = end - self.at;
        buf[..read].copy_from_slice(&self.bytes[self.at..end]);
        self.at = end;
        Ok(read)
    }
}

#[test]
fn a_check_with_a_big_input_reads_its_top_level_fields_as_they_stream_by() {
    // A tool call's big result, with `head` and `tail` around it at the top level.
    let big = |head: &str, tail: &str| {
        let filler = "x".repeat(1024 * 1024);
        format!(
            r#"{{{head}"tool_response":"{filler} \"agent_id\":\"quoted\"","tool_input":{{"agent_id":"nested"}}{tail}}}"#
        )
    };
    // Every split of the input across reads gives the same answer, read to its end.
    let main = |input: String| {
        let answers: Vec<crate::agent_cli::CheckInput> = [1, 7, 8192]
            .into_iter()
            .map(|step| {
                let mut reader = Trickle {
                    bytes: input.clone().into_bytes(),
                    at: 0,
                    step,
                };
                let answer = check_input(&mut reader);
                assert_eq!(reader.at, reader.bytes.len(), "read to its end");
                answer
            })
            .collect();
        assert!(answers.iter().all(|answer| answer.prompt.is_none()));
        assert!(
            answers.windows(2).all(|pair| pair[0] == pair[1]),
            "{answers:?}"
        );
        answers[0].main
    };
    // A key quoted inside a string, or one in a nested object, is not the hook's own.
    assert!(main(big("", "")));
    // A top-level one is, however it is spaced or placed.
    assert!(!main(big("", r#","agent_id":"child""#)));
    assert!(!main(big(r#""agent_id" : "child","#, "")));
    assert!(!main(big("", r#", "agent_id":7"#)));
    assert!(!main(big("", r#","agent_id":"""#)));
    assert!(main(big("", r#","agent_id": null"#)));
    // Only the key itself counts, not one that starts like it or that it starts like.
    assert!(main(big(
        "",
        r#","agent_idx":"child","agent_i":"child","agent_id_x":"child""#
    )));
    // Any other value names one: a string with an escape, or no string.
    for value in [r#""\u0063""#, r#""\"x""#, "true", "false", "{}", "[1]"] {
        assert!(
            !main(big("", &format!(r#","agent_id":{value}"#))),
            "{value}"
        );
    }
    // An escaped key is read as the key it spells.
    assert!(!main(big(r#""agent\u005fid":"child","#, "")));
    // One it can't read clearly is not known to be the hook's own: a key given twice, one
    // cut short, or one that is no object.
    assert!(!main(big(r#""agent_id":null,"#, r#","agent_id":"child""#)));
    let cut = big("", "");
    assert!(!main(cut[..cut.len() - 1].to_string()));
    assert!(!main(format!("[{}]", big("", ""))));
    assert!(!main(format!("{} {{}}", big("", ""))));
    // A big prompt still carries its head.
    let prompt = format!(r#"{{"prompt":"[inbox t1]\n{}"}}"#, "y".repeat(1024 * 1024));
    let head = check_input(&mut std::io::Cursor::new(prompt.into_bytes()))
        .prompt
        .unwrap();
    assert!(head.starts_with("[inbox t1]\n") && head.len() == MAX_PROMPT);
}

/// A reader that fails partway.
struct Broken(usize);

impl Read for Broken {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.0 == 0 {
            return Err(io::Error::other("broken"));
        }
        let read = self.0.min(buf.len());
        buf[..read].fill(b' ');
        self.0 -= read;
        Ok(read)
    }
}

#[test]
fn a_check_whose_input_breaks_is_not_known_to_be_the_agents_own() {
    assert_eq!(check_input(&mut Broken(10)), Default::default());
}

#[test]
fn a_notice_is_handed_as_its_cli_takes_it() {
    assert_eq!(
        handed("You have mail.", "Stop", HandOver::Rewake),
        ("You have mail.".to_string(), true, 2)
    );
    let (line, on_stderr, status) =
        handed("You have mail.", "PostToolUse", HandOver::AdditionalContext);
    assert_eq!((on_stderr, status), (false, 0));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": "You have mail.",
        }})
    );
}

#[test]
fn the_hosts_own_sweep_tells_a_session_bound_to_another_desktop_meanwhile() {
    let host = host();
    let token = host.prepare("S1");
    host.owner.register("S1", Some(me()));
    let _waiting = helper(&host, token, ask("Stop", me()));
    until(|| host.owner.inbox().state("S1").waiting);
    let told = || host.outlet.states.lock().unwrap().len();
    let before = told();
    // Bound to another desktop with no waiter arming or ending: the ingress's periodic
    // sweep tells it.
    *host.outlet.audience.lock().unwrap() = Some("other-mac".into());
    until(|| told() > before);
    assert!(host.outlet.states.lock().unwrap().last().unwrap().waiting);
    host.owner
        .inbox_mail("S1", "n".into(), "t1".into())
        .unwrap();
}

#[test]
fn a_hook_that_names_no_known_cli_event_or_agent_does_nothing() {
    let args =
        |values: &[&str]| -> Vec<std::ffi::OsString> { values.iter().map(Into::into).collect() };
    for bad in [
        args(&["nope", "Stop", "1"]),
        args(&["claude", "PreToolUse", "1"]),
        args(&["claude", "Stop", "not-a-pid"]),
        args(&["codex", "Stop", "1"]),
        args(&[]),
    ] {
        assert_eq!(wait_from_hook(&bad), 0, "{bad:?}");
    }
}

#[test]
fn a_one_shot_runs_hook_ends_at_once_and_takes_no_place() {
    let host = host();
    let shell = unsafe { libc::getppid() } as u32;
    host.owner.register("typed", Some(shell));
    let (_, writer) = host
        .owner
        .claim_shell(this(), AgentCli::Claude, "unused", true)
        .unwrap();
    let asked = Instant::now();
    assert_eq!(
        helper(&host, writer.token, ask("SessionStart", me()))
            .join()
            .unwrap(),
        None
    );
    assert!(
        asked.elapsed() < Duration::from_secs(2),
        "it never holds the run"
    );
    assert!(!host.owner.inbox().state("typed").waiting);
}

#[test]
fn a_waiter_whose_agent_went_away_prints_nothing_and_says_nothing_printed() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, served) = fake_host(dir.path(), vec!["mail t-1\nYou have mail.\n"]);
    // Asked for an agent that is not its parent (any more).
    assert_eq!(
        ask_once(&socket, "token", ask("Stop", me()), 1).map(|_| ()),
        Err(Step::End)
    );
    let (_, back) = served.join().unwrap();
    assert_eq!(back, [String::new()], "no printed line");
}

#[test]
fn a_host_submitted_clis_check_hook_is_handed_held_mail_at_once_and_other_hooks_are_refused() {
    use crate::agent_cli::orion::{adapters, ORION};
    let host = host_with(ActivityOwner::with_adapters(adapters));
    let token = host
        .owner
        .prepare_native("S1", ORION, host.service.path().to_str().unwrap())
        .unwrap()
        .token;
    host.owner.register("S1", Some(me()));
    // The outlet gives no port, so the notice is held, never submitted.
    host.owner
        .inbox_mail("S1", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    assert!(host.owner.inbox().holds("S1"));
    // A hook it does not check at is refused at once.
    assert_eq!(
        helper(&host, token.clone(), ask("Prompt", me()))
            .join()
            .unwrap(),
        None
    );
    // Its check hook (after a tool call) asks once and is handed the held notice.
    let handed = helper(&host, token, ask("ToolDone", me()));
    assert_eq!(
        handed.join().unwrap().as_deref(),
        Some("You have 1 new message.")
    );
    until(|| *host.outlet.told.lock().unwrap() == ["t1"]);
}

#[test]
fn a_host_submitted_clis_check_before_the_commit_waits_for_it() {
    use crate::agent_cli::orion::{adapters, ORION};
    let host = host_with(ActivityOwner::with_adapters(adapters));
    let (writer, incarnation) = host
        .owner
        .prepare_batch("run", ORION, host.service.path().to_str().unwrap())
        .unwrap();
    host.owner
        .inbox_mail("run", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    let checking = helper(&host, writer.token, ask("ToolDone", me()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(host.owner.commit("run", &incarnation, me()));
    assert_eq!(
        checking.join().unwrap().as_deref(),
        Some("You have 1 new message.")
    );
}

#[test]
fn a_desktop_one_shot_run_is_handed_mail_pushed_before_it_started_and_never_held() {
    let host = host();
    let (writer, incarnation) = host
        .owner
        .prepare_batch(
            "run",
            AgentCli::Claude,
            host.service.path().to_str().unwrap(),
        )
        .unwrap();
    // The desktop pushes the mail already waiting before the run starts.
    host.owner
        .inbox_mail("run", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    // Its boot look asks before the desktop commits its PID, and waits for the commit.
    let looking = helper(&host, writer.token.clone(), ask("SessionStart", me()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(host.owner.commit("run", &incarnation, me()));
    assert_eq!(
        looking.join().unwrap().as_deref(),
        Some("You have 1 new message.")
    );
    until(|| *host.outlet.told.lock().unwrap() == ["t1"]);
    // With nothing held, a later look ends at once and takes no place.
    let asked = Instant::now();
    assert_eq!(
        helper(&host, writer.token.clone(), ask("Stop", me()))
            .join()
            .unwrap(),
        None
    );
    assert!(
        asked.elapsed() < Duration::from_secs(2),
        "it never holds the run"
    );
    assert!(!host.owner.inbox().state("run").waiting);
    // Removed once the run ends: its token is refused, and a late commit adds nothing.
    host.owner.remove("run", &incarnation);
    assert!(matches!(
        host.owner
            .inbox_admission(&writer.token, this(), me(), None),
        InboxAdmission::Refused
    ));
    assert!(!host.owner.commit("run", &incarnation, me()));
    assert!(host.owner.snapshot("run").is_none());
}

/// A launched Codex `S1`, committed as this process: its host and writer.
fn codex_committed() -> (Host, crate::activity::Writer) {
    let host = host();
    let writer = host
        .owner
        .prepare_native("S1", AgentCli::Codex, host.service.path().to_str().unwrap())
        .unwrap();
    host.owner.register("S1", Some(me()));
    (host, writer)
}

/// [`codex_committed`], with its own session `thread-1` seen.
fn codex_in_its_session() -> (Host, crate::activity::Writer) {
    let (host, writer) = codex_committed();
    let input = serde_json::json!({"hook_event_name": "SessionStart", "session_id": "thread-1"});
    let report = crate::agent_cli::adapter(AgentCli::Codex)
        .normalize(&serde_json::to_vec(&input).unwrap(), true)
        .unwrap();
    host.owner.report_for_test(&writer, me(), report).unwrap();
    (host, writer)
}

#[test]
fn a_program_under_the_agent_is_the_agent_only_by_the_agents_own_session() {
    let (host, writer) = codex_in_its_session();
    let admit = |run: process::Process, session| {
        host.owner
            .inbox_admission(&writer.token, run, run.pid, session)
    };
    // The program it runs in its place runs its hooks (npm's `codex` script runs its binary
    // as its child; other launchers nest deeper): they name its own session. A CLI its tool
    // runs under it, with the token it inherits, names its own session, or none.
    let child = Child::spawn();
    let deeper = under_a_shell();
    for run in [child.process(), deeper.run()] {
        assert_eq!(admitted(admit(run, Some("thread-1"))), Some("S1".into()));
        assert!(matches!(
            admit(run, Some("nested-1")),
            InboxAdmission::Refused
        ));
        assert!(matches!(admit(run, None), InboxAdmission::Refused));
    }
    // The agent itself needs no session named.
    assert_eq!(admitted(admit(this(), None)), Some("S1".into()));
}

#[test]
fn a_check_run_by_the_program_its_engine_runs_is_the_agents_by_its_own_session() {
    let (host, writer) = codex_in_its_session();
    // Its engine (npm's `codex` script, standing in as the shell) runs its binary as its
    // child, which runs the hooks.
    let engine = under_a_shell();
    host.owner
        .bind_engine(
            &writer.token,
            this(),
            engine.pid(),
            host.service.path().with_file_name("engine-test.sock"),
            None,
        )
        .unwrap();
    let binary = engine.run();
    let admit = |session| {
        host.owner
            .inbox_admission(&writer.token, binary, binary.pid, session)
    };
    assert_eq!(admitted(admit(Some("thread-1"))), Some("S1".into()));
    assert!(matches!(admit(Some("nested-1")), InboxAdmission::Refused));
    assert!(matches!(admit(None), InboxAdmission::Refused));
}

#[test]
fn a_programs_check_before_the_agents_session_was_seen_is_refused() {
    let (host, writer) = codex_committed();
    let child = Child::spawn();
    assert!(matches!(
        host.owner.inbox_admission(
            &writer.token,
            child.process(),
            child.pid(),
            Some("thread-1")
        ),
        InboxAdmission::Refused
    ));
}

#[test]
fn waiter_renews_before_its_finite_hook_timeout_including_elapsed_wall_time() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    let renewal = start + Duration::from_secs(crate::agent_cli::INBOX_WAIT_TIMEOUT_SECS - 300);
    assert!(!renewal_due(
        Some(renewal),
        renewal - Duration::from_secs(1)
    ));
    assert!(renewal_due(Some(renewal), renewal));
    assert!(renewal_due(
        Some(renewal),
        renewal + Duration::from_secs(86400)
    ));
    assert!(crate::agent_cli::INBOX_WAIT_TIMEOUT_SECS * 1000 <= i32::MAX as u64);
    let dir = tempfile::tempdir().unwrap();
    let result = wait_with_deadline(
        &dir.path().join("absent.sock"),
        "token",
        std::os::unix::process::parent_id(),
        QUICK,
        Some(start),
        |_| ask("Stop", me()),
    );
    assert!(
        matches!(result, Err(())),
        "deadline exits through rewake, never silently"
    );
}

#[test]
fn a_claude_prompt_check_confirms_the_recovery_key_without_arming_or_delivering_again() {
    use crate::agent_cli::{
        orion::{serve, Orion},
        AgentCliAdapter, SubmitOutcome,
    };
    let endpoint = tempfile::tempdir().unwrap();
    let path = endpoint.path().join("engine.sock");
    let server = serve(&path);
    server.answer(&[SubmitOutcome::Uncertain]);
    let host = host();
    let token = host.prepare("S1");
    host.owner.register("S1", Some(me())).unwrap();
    host.owner
        .inbox_mail("S1", "You have 1 new message.".into(), "recovery".into())
        .unwrap();
    host.owner.inbox().submit(&*host.outlet, "S1", &Orion, || {
        (Orion.control().channel.unwrap())(&path, None, None).ok()
    });
    assert!(host.outlet.told.lock().unwrap().is_empty());
    let mut confirmation = ask("UserPromptSubmit", me());
    confirmation.prompt = Some("[inbox recovery]\nYou have 1 new message.".into());
    assert_eq!(helper(&host, token, confirmation).join().unwrap(), None);
    until(|| *host.outlet.told.lock().unwrap() == ["recovery"]);
    assert!(!host.owner.inbox().state("S1").waiting);
    assert_eq!(server.handed(), ["recovery"]);
}

/// An OpenCode launch of `session`, as its plugin's waiter is admitted by it.
fn opencode_prepared(host: &Host, session: &str) -> String {
    host.owner
        .prepare_native(
            session,
            AgentCli::OpenCode,
            host.service.path().to_str().unwrap(),
        )
        .unwrap()
        .token
}

#[test]
fn an_opencode_plugin_waiter_is_handed_the_pushed_notice_and_it_is_told_once() {
    let host = host();
    let token = opencode_prepared(&host, "S1");
    host.owner.register("S1", Some(me()));
    let waiting = helper(&host, token, ask("SessionStart", me()));
    until(|| host.owner.inbox().state("S1").waiting);
    host.owner
        .inbox_mail("S1", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    assert_eq!(
        waiting.join().unwrap().as_deref(),
        Some("You have 1 new message.")
    );
    until(|| *host.outlet.told.lock().unwrap() == ["t1"]);
    assert!(!host.owner.inbox().state("S1").waiting);
}

#[test]
fn an_opencode_tool_calls_waiter_defers_to_the_one_waiting_and_a_turn_end_replaces_it() {
    let host = host();
    let token = opencode_prepared(&host, "S1");
    host.owner.register("S1", Some(me()));
    let first = helper(&host, token.clone(), ask("SessionStart", me()));
    until(|| host.owner.inbox().state("S1").waiting);
    // A tool call's waiter ends unhanded and leaves the first in its place.
    let deferred = helper(&host, token.clone(), ask("PostToolUse", me()));
    assert_eq!(deferred.join().unwrap(), None);
    assert!(host.owner.inbox().state("S1").waiting);
    // A turn end takes the place: the first ends unhanded, the newer is handed the mail.
    let newer = helper(&host, token, ask("Stop", me()));
    assert_eq!(first.join().unwrap(), None);
    until(|| host.owner.inbox().state("S1").waiting);
    host.owner
        .inbox_mail("S1", "n".into(), "t1".into())
        .unwrap();
    assert_eq!(newer.join().unwrap().as_deref(), Some("n"));
}

#[test]
fn a_one_shot_opencode_run_takes_no_place() {
    let host = host();
    let shell = unsafe { libc::getppid() } as u32;
    host.owner.register("typed", Some(shell));
    let (_, writer) = host
        .owner
        .claim_shell(this(), AgentCli::OpenCode, "unused", true)
        .unwrap();
    let asked = Instant::now();
    assert_eq!(
        helper(&host, writer.token, ask("SessionStart", me()))
            .join()
            .unwrap(),
        None
    );
    assert!(asked.elapsed() < Duration::from_secs(2));
    assert!(!host.owner.inbox().state("typed").waiting);
}

/// What `hand` does with a notice, confirmed on `confirmation` for a waiter that asks it:
/// the hook's exit status, and what it told the host (empty when it said nothing).
fn handing(
    hand_over: HandOver,
    confirmation: impl Read + Send + 'static,
    wait: Duration,
) -> (i32, String) {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let status = hand(
        "You have mail.",
        "t-1",
        ours,
        "Stop",
        hand_over,
        confirmation,
        wait,
    );
    let mut told = String::new();
    theirs.read_to_string(&mut told).unwrap();
    (status, told)
}

#[test]
fn a_confirmed_waiter_tells_the_host_only_once_its_cli_confirms_the_delivery() {
    let printed = wire::printed_line("t-1");
    let confirmed = format!("{}\n", crate::agent_cli::WAITER_CONFIRMATION_LINE);
    let (status, told) = handing(
        HandOver::RewakeConfirmed,
        std::io::Cursor::new(confirmed.into_bytes()),
        Duration::from_secs(5),
    );
    assert_eq!(status, 2);
    assert!(told.contains(&printed), "{told:?}");
    // Nothing confirmed: it ends with the stream closed and nothing said, so the host
    // keeps the notice for the next waiter.
    for silent in [Vec::new(), b"not delivered\n".to_vec()] {
        let (status, told) = handing(
            HandOver::RewakeConfirmed,
            std::io::Cursor::new(silent),
            Duration::from_secs(5),
        );
        assert_eq!((status, told.as_str()), (0, ""));
    }
}

#[test]
fn a_confirmed_waiter_that_hears_nothing_in_time_says_nothing() {
    // The CLI's end stays open and silent.
    let (quiet, _keep_open) = UnixStream::pair().unwrap();
    let asked = Instant::now();
    let (status, told) = handing(HandOver::RewakeConfirmed, quiet, Duration::from_millis(200));
    assert_eq!((status, told.as_str()), (0, ""));
    assert!(asked.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_plain_rewake_tells_the_host_it_printed_without_waiting_for_anyone() {
    let (status, told) = handing(
        HandOver::Rewake,
        std::io::Cursor::new(Vec::new()),
        Duration::from_secs(5),
    );
    assert_eq!(status, 2);
    assert!(told.contains(&wire::printed_line("t-1")), "{told:?}");
}

#[test]
fn a_notice_its_waiter_never_confirmed_is_handed_again_to_the_next_waiter() {
    let host = host();
    let token = host.prepare("S1");
    host.owner.register("S1", Some(me()));
    let socket = host.service.path().to_path_buf();
    let parent = std::os::unix::process::parent_id();
    // This waiter is handed the notice and ends without saying it printed it.
    let first = {
        let (socket, token) = (socket.clone(), token.clone());
        std::thread::spawn(move || {
            ask_once(&socket, &token, ask("SessionStart", me()), parent)
                .ok()
                .map(|(notice, _, stream)| {
                    drop(stream);
                    notice
                })
        })
    };
    until(|| host.owner.inbox().state("S1").waiting);
    host.owner
        .inbox_mail("S1", "You have 1 new message.".into(), "t1".into())
        .unwrap();
    assert_eq!(
        first.join().unwrap().as_deref(),
        Some("You have 1 new message.")
    );
    assert!(host.outlet.told.lock().unwrap().is_empty(), "not told");
    // The next waiter is handed the same notice, and it is told once.
    let second = helper(&host, token, ask("Stop", me()));
    assert_eq!(
        second.join().unwrap().as_deref(),
        Some("You have 1 new message.")
    );
    until(|| *host.outlet.told.lock().unwrap() == ["t1"]);
}

#[test]
fn a_hermes_check_is_admitted_under_its_gateway_and_never_from_outside_the_cli() {
    let host = host_with(ActivityOwner::new());
    let writer = host
        .owner
        .prepare_native(
            "S1",
            AgentCli::Hermes,
            host.service.path().to_str().unwrap(),
        )
        .unwrap();
    host.owner.register("S1", Some(me()));
    // The agent's session is seen when a main turn is submitted, as Hermes reports it.
    let input = serde_json::json!({"hook_event_name": "pre_llm_call", "session_id": "gateway-1",
        "extra": {"platform": "cli", "turn_id": "t1"}});
    let report = crate::agent_cli::adapter(AgentCli::Hermes)
        .normalize(&serde_json::to_vec(&input).unwrap(), true)
        .unwrap();
    host.owner.report_for_test(&writer, me(), report).unwrap();
    // Its gateway is the engine, run under the CLI; its hooks run beneath the gateway.
    let gateway = under_a_shell();
    host.owner
        .bind_engine(
            &writer.token,
            this(),
            gateway.pid(),
            host.service.path().with_file_name("engine-test.sock"),
            None,
        )
        .unwrap();
    let hook = gateway.run();
    // The CLI's tree, and a process of the harness that is not in it.
    let outside = process::inspect(std::os::unix::process::parent_id()).unwrap();
    for (who, run, session, admitted_as) in [
        (
            "the gateway's hook, naming the agent's session",
            hook,
            Some("gateway-1"),
            Some("S1"),
        ),
        (
            "the gateway's hook, naming another session",
            hook,
            Some("nested-1"),
            None,
        ),
        ("the gateway's hook, naming none", hook, None, None),
        (
            "a process outside the CLI, naming the session",
            outside,
            Some("gateway-1"),
            None,
        ),
    ] {
        let admission = host
            .owner
            .inbox_admission(&writer.token, run, run.pid, session);
        assert_eq!(admitted(admission).as_deref(), admitted_as, "{who}");
    }
}
