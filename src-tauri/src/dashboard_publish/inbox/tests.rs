// SPDX-License-Identifier: MIT
use super::super::catalog::{StreamCall, StreamItem};
use super::super::state::Scope;
use super::*;
use crate::commands::agent_inbox::{KnownSession, KnownSessions};
use crate::weft::{inbox as store, WeftStore};
use futures_util::StreamExt;
use sha2::Digest as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct FakeInbox {
    dir: tempfile::TempDir,
    changed: AtomicUsize,
    cache: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
    /// Display names for handles that name themselves otherwise.
    names: std::sync::Mutex<std::collections::HashMap<String, String>>,
    read_gate: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}
impl FakeInbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            dir: tempfile::tempdir().unwrap(),
            changed: AtomicUsize::new(0),
            cache: Default::default(),
            names: Default::default(),
            read_gate: std::sync::Mutex::new(None),
        })
    }
    fn path(&self, team: &str) -> PathBuf {
        self.dir.path().join(team).join("weft.sqlite3")
    }
    /// Cache `bytes` as the content-addressed media cache would, returning its hash.
    pub(crate) fn cache_media(&self, bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let sha = format!("{:x}", Sha256::digest(bytes));
        self.cache.lock().unwrap().insert(sha.clone(), bytes.to_vec());
        sha
    }
    pub(crate) fn attach(&self, team: &str, id: i64, attachments: &[crate::media::Attachment]) {
        store::inbox_set_attachments(&WeftStore::open(self.path(team)).unwrap(), id, attachments)
            .unwrap();
    }
    pub(crate) fn insert(&self, team: &str, to: &str, text: &str) -> i64 {
        self.insert_from(team, "agent", to, text)
    }
    pub(crate) fn insert_from(&self, team: &str, from: &str, to: &str, text: &str) -> i64 {
        store::inbox_insert(
            &WeftStore::open(self.path(team)).unwrap(),
            from,
            to,
            text,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    }
}
#[async_trait::async_trait]
impl InboxHost for FakeInbox {
    async fn team_store(&self, team: &LaunchTeam) -> Result<PathBuf, String> {
        let gate = self.read_gate.lock().unwrap().take();
        if let Some((started, finish)) = gate {
            crate::commands::await_blocking("test inbox read gate", move || {
                let _ = started.send(());
                let _ = finish.recv();
                Ok::<_, String>(())
            })
            .await?;
        }
        Ok(self.path(&team.team))
    }
    async fn active_team(&self) -> Result<LaunchTeam, String> {
        Ok(LaunchTeam {
            org: "acme".into(),
            team: "other".into(),
        })
    }
    fn launch_team(&self, handle: &str) -> Option<LaunchTeam> {
        Some(LaunchTeam {
            org: "acme".into(),
            team: if handle == "foreign" { "other" } else { "emea" }.into(),
        })
    }
    fn handle_for_session(&self, _: &str) -> Option<String> {
        None
    }
    async fn sessions(&self) -> Result<KnownSessions, String> {
        Ok(KnownSessions::new(
            ["agent", "foreign"]
                .into_iter()
                .map(|h| KnownSession {
                    handle: h.into(),
                    session: None,
                    name: h.into(),
                    running: true,
                })
                .collect(),
        ))
    }
    fn has_handle(&self, handle: &str) -> bool {
        matches!(handle, "agent" | "foreign")
    }
    fn display_name(&self, handle: &str) -> Option<String> {
        Some(self.names.lock().unwrap().get(handle).cloned().unwrap_or_else(|| handle.into()))
    }
    fn inbox_changed(&self) {
        self.changed.fetch_add(1, Ordering::SeqCst);
    }
    fn cached_media(&self, sha: &str) -> Result<Vec<u8>, String> {
        self.cache.lock().unwrap().get(sha).cloned().ok_or("evicted".into())
    }
}

pub(crate) fn fixture() -> (Arc<FakeInbox>, LiveCtx, Call) {
    let (host, _, ctx, call) = fixture_with_agents();
    (host, ctx, call)
}

pub(crate) fn fixture_with_agents() -> (
    Arc<FakeInbox>,
    Arc<agents::tests::FakeHost>,
    LiveCtx,
    Call,
) {
    let host = FakeInbox::new();
    let agents = Arc::new(agents::tests::FakeHost::new());
    agents.launch("agent", Some(("acme", "emea")), "workspace", None);
    agents.launch("foreign", Some(("acme", "other")), "workspace", None);
    let ctx = LiveCtx {
        agents: Some(agents.clone()),
        inbox: Some(host.clone()),
        ..Default::default()
    };
    let call = Call {
        args: json!({}),
        folder: "/tmp/dashboard".into(),
        scope: Some(Scope {
            org: "acme".into(),
            team: "emea".into(),
        }),
        ..Default::default()
    };
    (host, agents, ctx, call)
}

#[tokio::test]
async fn team_read_is_projected_redacted_and_never_marks_read() {
    let (host, ctx, call) = fixture();
    host.insert("other", "human", "other team's secret");
    host.insert(
        "emea",
        "human",
        "Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789",
    );
    host.insert("emea", "agent", "session mail");
    let value = query(ctx.clone(), call.clone()).await.unwrap();
    assert_eq!(value["messages"].as_array().unwrap().len(), 2);
    assert!(!value.to_string().contains("other team's"));
    assert!(value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m.get("text").is_none() && m["unread"] == true));
    let mut expanded = call.clone();
    expanded.fields = vec!["text".into()];
    let value = query(ctx.clone(), expanded).await.unwrap();
    assert!(!value
        .to_string()
        .contains("abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(value["messages"][0].get("text").is_some());
    assert_eq!(
        query(ctx.clone(), call.clone()).await.unwrap()["humanUnread"],
        1
    );
    assert_eq!(host.changed.load(Ordering::SeqCst), 0);
    for bad in [
        json!({"team":"other"}),
        json!({"scope":"everything"}),
        json!({"limit":"500"}),
        json!({"kind":"update"}),
    ] {
        let mut bad_call = call.clone();
        bad_call.args = bad;
        assert!(matches!(
            query(ctx.clone(), bad_call).await,
            Err(QueryError::InvalidArgs(_))
        ));
    }
    let mut moved = call;
    moved.scope.as_mut().unwrap().team = "other".into();
    assert!(query(ctx, moved).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn inbox_stream_shares_polling_and_only_emits_changed_values() {
    let (host, ctx, call) = fixture();
    host.insert("emea", "human", "first");
    let mut items = agents::open_inbox(
        ctx,
        StreamCall {
            args: call.args,
            folder: call.folder,
            scope: call.scope,
            fields: vec!["text".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(items.next().await, Some(StreamItem::Value(_))));
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), items.next())
            .await
            .is_err()
    );
    host.insert("emea", "human", "second");
    let item = tokio::time::timeout(std::time::Duration::from_secs(3), items.next())
        .await
        .unwrap()
        .unwrap();
    let StreamItem::Value(value) = item else {
        panic!("expected value")
    };
    assert_eq!(value["messages"].as_array().unwrap().len(), 2);
    assert_eq!(value["humanUnread"], 2);
    assert!(value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["unread"] == true));
    assert_eq!(host.changed.load(Ordering::SeqCst), 0);
}

#[test]
fn send_args_are_plain_typed_and_bounded() {
    for value in [
        json!({"to":"human","text":""}),
        json!({"to":"human","text":42}),
        json!({"to":"human","text":"x".repeat(2001)}),
        json!({"to":"human","text":"hi","contentType":"html"}),
        json!({"to":"human","text":"hi","attachFile":"secret"}),
        json!({"to":"human","text":"hi","onClick":{}}),
        json!({"to":"role:human","text":"hi"}),
        json!({"to":"human","text":"hi","replyTo":-1}),
    ] {
        assert!(MessageArgs::parse(value).is_err());
    }
    assert!(MessageArgs::parse(json!({"to":"role:triage","text":"🦀".repeat(2000)})).is_ok());
}

#[tokio::test]
async fn owner_sends_and_replies_through_the_inbox_with_fixed_team_and_plain_content() {
    let (host, ctx, call) = fixture();
    let send = |value| MessageArgs::parse(value).unwrap();
    let result = deliver(
        &ctx,
        &call.folder,
        call.scope.as_ref(),
        send(json!({"to":"agent","text":"<b>literal</b>"})),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "sent");
    let db = WeftStore::open(host.path("emea")).unwrap();
    let rows = store::inbox_list(&db).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].from, "human");
    assert_eq!(rows[0].text, "<b>literal</b>");
    assert_eq!(rows[0].content_type, Some(store::MessageContentType::Plain));
    let original = host.insert("emea", "human", "question");
    deliver(
        &ctx,
        &call.folder,
        call.scope.as_ref(),
        send(json!({"to":"agent","text":"answer","replyTo":original})),
    )
    .await
    .unwrap();
    let rows = store::inbox_list(&db).unwrap();
    let reply = rows.iter().find(|r| r.text == "answer").unwrap();
    assert_eq!(reply.in_reply_to, Some(original));
    assert_eq!(reply.content_type, Some(store::MessageContentType::Plain));
    assert_eq!(
        deliver(
            &ctx,
            &call.folder,
            call.scope.as_ref(),
            send(json!({"to":"foreign","text":"forbidden"}))
        )
        .await
        .unwrap_err(),
        "not_found"
    );
    deliver(
        &ctx,
        &call.folder,
        call.scope.as_ref(),
        send(json!({"to":"human","text":"fixed team"})),
    )
    .await
    .unwrap();
    assert!(store::inbox_list(&db)
        .unwrap()
        .iter()
        .any(|r| r.text == "fixed team"));
    let result = deliver(
        &ctx,
        &call.folder,
        call.scope.as_ref(),
        send(json!({"to":"role:triage","text":"role question"})),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "sent");
    assert!(store::inbox_list(&db)
        .unwrap()
        .iter()
        .any(|r| r.text == "role question"));
    let value = query(ctx, call).await.unwrap();
    let role = value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["messageId"] == result["messageId"])
        .unwrap();
    assert_eq!(role["delivery"]["state"], "queued");
    assert!(role.get("text").is_none());
}

#[tokio::test]
async fn replies_follow_available_reply_to_after_sender_exits_and_leave_latest_window() {
    let (host, ctx, call) = fixture();
    let db = WeftStore::open(host.path("emea")).unwrap();
    let id = store::inbox_insert_with_reply_to(
        &db,
        "exited-agent",
        "human",
        "Question",
        None,
        None,
        None,
        None,
        None,
        Some("role:triage"),
        None,
        None,
    )
    .unwrap();
    let make =
        || MessageArgs::parse(json!({"to":"exited-agent", "text":"Answer", "replyTo":id})).unwrap();
    let result = deliver(&ctx, &call.folder, call.scope.as_ref(), make())
        .await
        .unwrap();
    assert_eq!(result["to"], "role:triage");
    let mut restricted = make();
    restricted.allowed_recipients = Some(vec!["human".into()]);
    assert_eq!(
        deliver(&ctx, &call.folder, call.scope.as_ref(), restricted)
            .await
            .unwrap_err(),
        "invalid_args"
    );
    for _ in 0..501 {
        host.insert("emea", "foreign-conversation", "Newer");
    }
    let mut filtered = call.clone();
    filtered.args = json!({"agentId":"exited-agent"});
    assert!(query(ctx.clone(), filtered).await.unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["messageId"] == id));
    assert_eq!(
        deliver(&ctx, &call.folder, call.scope.as_ref(), make())
            .await
            .unwrap()["status"],
        "sent"
    );
}

#[tokio::test]
async fn running_local_headless_sessions_can_receive_mail() {
    let (host, mut ctx, call) = fixture();
    let agents = Arc::new(agents::tests::FakeHost::new());
    agents.launch("agent", Some(("acme", "emea")), "workspace", None);
    agents.rows.lock().unwrap()[0].machine = None;
    ctx.agents = Some(agents);
    let args = MessageArgs::parse(json!({"to":"agent", "text":"Headless inbox"})).unwrap();
    assert_eq!(
        deliver(&ctx, &call.folder, call.scope.as_ref(), args)
            .await
            .unwrap()["status"],
        "sent"
    );
    assert_eq!(
        store::inbox_list(&WeftStore::open(host.path("emea")).unwrap()).unwrap()[0].to,
        "agent"
    );
}

#[tokio::test]
async fn ordinary_full_inbox_remains_within_the_live_answer_budget() {
    let (host, ctx, mut call) = fixture();
    for _ in 0..150 {
        host.insert("emea", "human", &"🦀".repeat(2000));
    }
    call.fields = vec!["text".into()];
    let value = query(ctx, call).await.unwrap();
    assert!(serde_json::to_vec(&value).unwrap().len() <= super::super::live::MAX_ANSWER_BYTES);
    assert_eq!(value["messages"][0]["text"], "🦀".repeat(2000));
    let returned = value["messages"].as_array().unwrap().len();
    assert!(returned < 150);
    // SV-INP-38: a cut answer says so, and how many rows it kept.
    assert_eq!(value["truncated"], true);
    assert_eq!(value["returned"], returned);
}

/// SV-ENG-61: a row dropped by the 500 cap or by the viewer's `limit` is marked like every other
/// hosted list: `truncated`, `returned` and `total`.
#[tokio::test]
async fn inbox_rows_dropped_by_the_limit_are_marked() {
    let (host, ctx, call) = fixture();
    for i in 0..600 {
        host.insert("emea", "human", &format!("m{i}"));
    }
    let value = query(ctx.clone(), call.clone()).await.unwrap();
    let returned = value["messages"].as_array().unwrap().len();
    assert_eq!(returned, 500);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["returned"], returned);
    assert_eq!(value["total"], 600);
    let narrow = Call { args: json!({"limit": 5}), ..call.clone() };
    let value = query(ctx.clone(), narrow).await.unwrap();
    assert_eq!(
        (&value["truncated"], &value["returned"], &value["total"]),
        (&json!(true), &json!(5), &json!(600))
    );
    let few = Call { args: json!({"limit": 1000}), ..call };
    let (host, ctx, call) = fixture();
    host.insert("emea", "human", "only");
    let value = query(ctx, Call { args: few.args, ..call }).await.unwrap();
    assert!(value.get("truncated").is_none() && value.get("total").is_none());
}

/// SV-ENG-61 r1: when a size cut follows the limit, `total` is still what matched before either.
#[tokio::test]
async fn inbox_total_counts_matches_before_a_size_cut() {
    let (host, ctx, mut call) = fixture();
    for _ in 0..600 {
        host.insert("emea", "human", &"x".repeat(2000));
    }
    call.fields = vec!["text".into()];
    let value = query(ctx.clone(), call.clone()).await.unwrap();
    let returned = value["messages"].as_array().unwrap().len();
    assert!(returned < 500);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["returned"], returned);
    assert_eq!(value["total"], 600);
    let narrow = Call { args: json!({"limit": 20}), ..call };
    let value = query(ctx, narrow).await.unwrap();
    assert_eq!(value["returned"], value["messages"].as_array().unwrap().len());
    assert_eq!(value["total"], 600);
}

#[tokio::test]
async fn replies_preserve_existing_role_address_compatibility() {
    let (host, ctx, call) = fixture();
    let db = WeftStore::open(host.path("emea")).unwrap();
    for (address, expected) in [
        ("role:code.review", "role:code.review"),
        ("role:human", "human"),
    ] {
        let id = store::inbox_insert_with_reply_to(
            &db,
            "exited-agent",
            "human",
            "Question",
            None,
            None,
            None,
            None,
            None,
            Some(address),
            None,
            None,
        )
        .unwrap();
        let args =
            MessageArgs::parse(json!({"to":"exited-agent","text":"Answer","replyTo":id})).unwrap();
        let result = deliver(&ctx, &call.folder, call.scope.as_ref(), args).await;
        assert_eq!(result.unwrap()["to"], expected);
    }
}

#[tokio::test]
async fn cancelled_query_keeps_its_slot_until_blocking_read_finishes() {
    let (host, ctx, mut call) = fixture();
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    call.hold = Some(Arc::new(slots.clone().acquire_owned().await.unwrap()));
    let (started, reading) = tokio::sync::oneshot::channel();
    let (finish, blocked) = std::sync::mpsc::channel();
    *host.read_gate.lock().unwrap() = Some((started, blocked));
    let running = tokio::spawn(query(ctx, call));
    reading.await.unwrap();
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    let available_while_reading = slots.available_permits();
    finish.send(()).unwrap();
    assert_eq!(
        available_while_reading, 0,
        "a cancelled query must retain its blocking read slot"
    );
    let _released = tokio::time::timeout(std::time::Duration::from_secs(2), slots.acquire())
        .await
        .unwrap()
        .unwrap();
}

pub(crate) fn image(sha: &str, state: &str) -> crate::media::Attachment {
    crate::media::Attachment {
        kind: "image".into(),
        sha256: Some(sha.into()),
        mime: Some("image/gif".into()),
        width: Some(2),
        height: Some(3),
        bytes: Some(10),
        alt: "a cat".into(),
        source_url: Some("https://i.giphy.com/x.gif".into()),
        source_host: Some("i.giphy.com".into()),
        state: state.into(),
        reason: Some("host".into()),
        retryable: false,
    }
}

pub(crate) fn gif(len: usize) -> Vec<u8> {
    let mut bytes = b"GIF89a\x02\x00\x03\x00".to_vec();
    bytes.extend((0..len - 10).map(|i| (i % 251) as u8));
    bytes
}

fn media_call(call: &Call, args: Value) -> Call {
    Call {
        args,
        fields: vec!["media".into()],
        ..call.clone()
    }
}

async fn media(ctx: &LiveCtx, call: &Call, args: Value) -> Result<Value, QueryError> {
    media::query(ctx.clone(), media_call(call, args)).await
}

#[tokio::test]
async fn meta_and_media_fields_are_opt_in_and_hide_the_source() {
    let (host, ctx, call) = fixture();
    let sha = host.cache_media(&gif(64));
    let id = host.insert("emea", "human", "hi");
    host.attach("emea", id, &[image(&sha, "cached")]);
    let read = |fields: &[&str]| {
        let mut c = call.clone();
        c.fields = fields.iter().map(|f| f.to_string()).collect();
        let ctx = ctx.clone();
        async move { query(ctx, c).await.unwrap()["messages"][0].clone() }
    };
    let none = read(&[]).await;
    for key in ["to", "toName", "replyTo", "replyToName", "contentType", "attachments"] {
        assert!(none.get(key).is_none(), "{key} leaked");
    }
    let meta = read(&["meta"]).await;
    assert_eq!(meta["to"], "human");
    assert!(meta.get("toName").is_some() && meta.get("contentType").is_some());
    assert!(meta.get("attachments").is_none());
    let media_only = read(&["media"]).await;
    assert!(media_only.get("to").is_none());
    let a = &media_only["attachments"][0];
    assert_eq!(a["sha256"], sha);
    assert_eq!((a["state"].as_str(), a["alt"].as_str()), (Some("cached"), Some("a cat")));
    let text = media_only.to_string();
    for leak in ["giphy", "sourceUrl", "sourceHost", "reason", "retryable"] {
        assert!(!text.contains(leak), "{leak} leaked");
    }
}

#[tokio::test]
async fn media_returns_cached_bytes_in_chunks_that_reassemble() {
    use base64::Engine as _;
    let (host, ctx, call) = fixture();
    let bytes = gif(400 * 1024);
    let sha = host.cache_media(&bytes);
    let id = host.insert("emea", "human", "hi");
    host.attach("emea", id, &[image(&sha, "cached")]);
    let mut got = Vec::new();
    loop {
        let a = media(&ctx, &call, json!({"messageId": id, "sha256": sha, "offset": got.len()}))
            .await
            .unwrap();
        assert_eq!(a["mime"], "image/gif");
        assert_eq!(a["total"], bytes.len());
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(a["data"].as_str().unwrap())
            .unwrap();
        assert!(chunk.len() <= media::CHUNK_BYTES);
        got.extend(chunk);
        if a["done"] == true {
            break;
        }
    }
    assert_eq!(got, bytes);
    let past = media(&ctx, &call, json!({"messageId": id, "sha256": sha, "offset": bytes.len()})).await;
    assert!(matches!(past, Err(QueryError::InvalidArgs(_))));
}

/// The relay lets a viewer session ask 60 times a minute (K18); the Mac's answer is capped at
/// `MAX_ANSWER_BYTES`. A largest-allowed image must load in few enough chunks to fit both.
#[tokio::test]
async fn a_largest_allowed_image_loads_within_the_relay_budget() {
    use base64::Engine as _;
    const VIEWER_ASKS_PER_MINUTE: usize = 60;
    let (host, ctx, call) = fixture();
    let bytes = gif(media::MAX_MEDIA_BYTES);
    let sha = host.cache_media(&bytes);
    let id = host.insert("emea", "human", "big");
    let mut attachment = image(&sha, "cached");
    attachment.bytes = Some(bytes.len() as u64);
    host.attach("emea", id, &[attachment]);
    let (mut got, mut asks) = (Vec::new(), 0);
    loop {
        let a = media(&ctx, &call, json!({"messageId": id, "sha256": sha, "offset": got.len()}))
            .await
            .unwrap();
        asks += 1;
        // The shape the relay checks before it passes the answer on.
        let sent = serde_json::to_vec(&media::redact(a.clone())).unwrap();
        assert!(sent.len() <= super::super::live::MAX_ANSWER_BYTES, "chunk {asks} is {} bytes", sent.len());
        got.extend(base64::engine::general_purpose::STANDARD.decode(a["data"].as_str().unwrap()).unwrap());
        if a["done"] == true {
            break;
        }
    }
    assert_eq!(got, bytes);
    assert_eq!(asks, media::MAX_MEDIA_BYTES.div_ceil(media::CHUNK_BYTES));
    assert!(asks * 2 <= VIEWER_ASKS_PER_MINUTE, "{asks} asks leave no room for the inbox stream");
}

/// The cap is defined once, in `media::MAX_MEDIA_BYTES`: the toolkit holds no size literal and
/// the served text names the same number.
#[test]
fn the_media_cap_is_defined_once() {
    let mb = media::MAX_MEDIA_BYTES / (1024 * 1024);
    assert_eq!(media::MAX_MEDIA_BYTES, mb * 1024 * 1024);
    let toolkit = include_str!("../toolkit/cs-live.js");
    assert!(!toolkit.contains("MEDIA_MAX"));
    assert!(toolkit.contains(&format!("the Mac's {mb} MB cap")));
    assert!(include_str!("../catalog.rs").contains(&format!("at most {mb} MB and a GIF")));
}

#[tokio::test]
async fn media_refuses_everything_else_and_never_fetches() {
    let (host, ctx, call) = fixture();
    let sha = host.cache_media(&gif(64));
    let id = host.insert("emea", "human", "hi");
    host.attach("emea", id, &[image(&sha, "cached")]);
    let args = |id: i64, sha: &str| json!({"messageId": id, "sha256": sha});
    let refused = |v: Result<Value, QueryError>| v.unwrap()["refused"].as_str().map(str::to_string);

    // The same hash on a message of another team, and that team's message id.
    let other = host.insert("other", "human", "secret");
    let foreign = host.cache_media(&gif(80));
    host.attach("other", other, &[image(&foreign, "cached")]);
    assert_eq!(refused(media(&ctx, &call, args(id, &foreign)).await).as_deref(), Some("not_found"));
    assert_eq!(refused(media(&ctx, &call, args(other + 5, &foreign)).await).as_deref(), Some("not_found"));
    // An unknown hash, and an unknown message.
    assert_eq!(refused(media(&ctx, &call, args(id, &"0".repeat(64))).await).as_deref(), Some("not_found"));
    assert_eq!(refused(media(&ctx, &call, args(id + 40, &sha)).await).as_deref(), Some("not_found"));
    // Not declared: no `media` in live.fields.inbox, or a bad hash.
    let undeclared = Call { fields: vec!["text".into(), "meta".into()], args: args(id, &sha), ..call.clone() };
    assert!(matches!(media::query(ctx.clone(), undeclared).await, Err(QueryError::NotApproved(_))));
    assert!(matches!(media(&ctx, &call, args(id, "ABC")).await, Err(QueryError::InvalidArgs(_))));
    assert!(matches!(media(&ctx, &call, json!({"messageId": id, "sha256": sha, "x": 1})).await, Err(QueryError::InvalidArgs(_))));

    // Not cached: pending, blocked, failed (all keep their source URL and are never fetched),
    // and a cached one evicted from the cache.
    for state in ["pending", "blocked", "failed"] {
        let hash = host.cache_media(&gif(90 + state.len()));
        let m = host.insert("emea", "human", state);
        host.attach("emea", m, &[image(&hash, state)]);
        assert_eq!(refused(media(&ctx, &call, args(m, &hash)).await).as_deref(), Some("not_cached"), "{state}");
    }
    let evicted = host.cache_media(&gif(120));
    let m = host.insert("emea", "human", "evicted");
    host.attach("emea", m, &[image(&evicted, "cached")]);
    host.cache.lock().unwrap().remove(&evicted);
    assert_eq!(refused(media(&ctx, &call, args(m, &evicted)).await).as_deref(), Some("not_cached"));

    // Not an image (SVG), and a file that no longer matches its hash.
    let svg = host.cache_media(b"<svg xmlns='http://www.w3.org/2000/svg'/>");
    let m = host.insert("emea", "human", "svg");
    host.attach("emea", m, &[image(&svg, "cached")]);
    assert_eq!(refused(media(&ctx, &call, args(m, &svg)).await).as_deref(), Some("not_cached"));
    let tampered = host.cache_media(&gif(130));
    host.cache.lock().unwrap().insert(tampered.clone(), gif(131));
    let m = host.insert("emea", "human", "tampered");
    host.attach("emea", m, &[image(&tampered, "cached")]);
    assert_eq!(refused(media(&ctx, &call, args(m, &tampered)).await).as_deref(), Some("not_cached"));

    // Over the cap, by the stored size or by the file itself.
    let big = host.cache_media(&gif(media::MAX_MEDIA_BYTES + 1));
    let m = host.insert("emea", "human", "big");
    let mut a = image(&big, "cached");
    a.bytes = Some(media::MAX_MEDIA_BYTES as u64 + 1);
    host.attach("emea", m, &[a.clone()]);
    assert_eq!(refused(media(&ctx, &call, args(m, &big)).await).as_deref(), Some("too_large"));
    a.bytes = Some(10);
    host.attach("emea", m, &[a]);
    assert_eq!(refused(media(&ctx, &call, args(m, &big)).await).as_deref(), Some("too_large"));
}

#[test]
fn media_answers_keep_base64_whole_but_redact_the_rest() {
    use base64::Engine as _;
    // A chunk whose base64 holds an AWS-key lookalike, which the secret scan would mask.
    let mut bytes = b"GIF89a\x01\x00\x01\x00\x00\x00".to_vec();
    bytes.extend(base64::engine::general_purpose::STANDARD.decode("AKIAABCDEFGHIJKLMNOP").unwrap());
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    assert!(data.contains("AKIAABCDEFGHIJKLMNOP"));
    assert!(crate::dashboard_publish::catalog::redact(json!({"data": data.clone()}))["data"] != json!(data));
    let kept = media::redact(json!({"mime": "image/gif", "data": data.clone(), "note": "ghp_abcdefghijklmnopqrstuvwxyz0123"}));
    assert_eq!(kept["data"], json!(data));
    assert!(!kept.to_string().contains("ghp_abcdefghijklmnopqrstuvwxyz0123"));
    // Anything that is not base64 is dropped, never passed through.
    assert!(media::redact(json!({"data": "not base64 !! ghp_abcdefghijklmnopqrstuvwxyz0123"})).get("data").is_none());
}

fn texts(value: &Value) -> Vec<String> {
    let mut texts: Vec<String> = value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_string())
        .collect();
    texts.sort();
    texts
}

#[tokio::test]
async fn scope_human_returns_only_mail_to_or_from_the_human_for_query_and_stream() {
    let (host, ctx, call) = fixture();
    host.insert_from("emea", "agent", "human", "to the human");
    host.insert_from("emea", "human", "agent", "from the human");
    host.insert_from("emea", "agent", "foreign", "agent to agent");
    host.insert_from("emea", "agent", "role:reviewer", "agent to role");
    host.insert_from("other", "agent", "human", "other team");
    let expected = vec!["from the human".to_string(), "to the human".to_string()];
    let human = Call {
        args: json!({"scope": "human"}),
        fields: vec!["text".into()],
        ..call.clone()
    };
    assert_eq!(texts(&query(ctx.clone(), human.clone()).await.unwrap()), expected);
    // `all` still has the agent mail, and an unknown scope is still refused.
    let all = Call { args: json!({"scope": "all"}), ..human.clone() };
    assert!(texts(&query(ctx.clone(), all).await.unwrap()).contains(&"agent to agent".to_string()));
    let bad = Call { args: json!({"scope": "humans"}), ..human.clone() };
    assert!(matches!(query(ctx.clone(), bad).await, Err(QueryError::InvalidArgs(_))));
    // The limit counts human rows only: the newest agent mail cannot crowd them out.
    let limited = Call { args: json!({"scope": "human", "limit": 1}), ..human.clone() };
    host.insert_from("emea", "agent", "foreign", "newer agent mail");
    assert_eq!(texts(&query(ctx.clone(), limited).await.unwrap()).len(), 1);

    let mut items = agents::open_inbox(
        ctx,
        StreamCall {
            args: human.args,
            folder: human.folder,
            scope: human.scope,
            fields: human.fields,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let Some(StreamItem::Value(value)) = items.next().await else {
        panic!("expected a value")
    };
    assert_eq!(texts(&value), expected);
}

#[tokio::test(start_paused = true)]
async fn inbox_stream_gates_meta_and_media_and_redacts_their_free_text() {
    let (host, ctx, call) = fixture();
    let sha = host.cache_media(&gif(64));
    let id = host.insert_from("emea", "agent", "human", "hi");
    let mut a = image(&sha, "cached");
    a.alt = "key ghp_abcdefghijklmnopqrstuvwxyz0123 here".into();
    host.attach("emea", id, &[a]);
    let first = |fields: &[&str]| {
        let (ctx, call) = (ctx.clone(), call.clone());
        let fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
        async move {
            let mut items = agents::open_inbox(
                ctx,
                StreamCall {
                    args: call.args,
                    folder: call.folder,
                    scope: call.scope,
                    fields,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let Some(StreamItem::Value(value)) = items.next().await else {
                panic!("expected a value")
            };
            value["messages"][0].clone()
        }
    };
    let none = first(&[]).await;
    for key in ["to", "toName", "contentType", "attachments"] {
        assert!(none.get(key).is_none(), "{key} leaked");
    }
    let meta = first(&["meta"]).await;
    assert!(meta.get("to").is_some() && meta.get("attachments").is_none());
    let media_row = first(&["media"]).await;
    assert!(media_row.get("to").is_none());
    assert_eq!(media_row["attachments"][0]["sha256"], sha);
    let text = media_row.to_string();
    assert!(!text.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"), "{text}");
    assert!(text.contains("[redacted]"), "{text}");
    for leak in ["giphy", "sourceUrl", "reason"] {
        assert!(!text.contains(leak), "{leak} leaked");
    }
}

#[tokio::test]
async fn reactions_project_counts_and_mine_only_behind_their_field_and_refresh_the_stream() {
    use crate::commands::agent_inbox::react_from_human;
    use crate::weft::reactions::reaction_react;
    let (host, ctx, call) = fixture();
    let id = host.insert("emea", "human", "meme");
    let team = LaunchTeam { org: "acme".into(), team: "emea".into() };
    let store = WeftStore::open(host.path("emea")).unwrap();
    reaction_react(&store, id, "agent", "😂", None).unwrap();
    reaction_react(&store, id, "other-agent", "😂", None).unwrap();
    let read = |fields: &[&str]| {
        let (ctx, call) = (ctx.clone(), call.clone());
        let fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
        async move { query(ctx, Call { fields, ..call }).await.unwrap()["messages"][0].clone() }
    };
    // Undeclared (even with every other field declared): absent.
    assert!(read(&["text", "meta", "media"]).await.get("reactions").is_none());
    assert_eq!(read(&["reactions"]).await["reactions"], json!([{"emoji": "😂", "count": 2, "mine": false}]));

    let mut items = agents::open_inbox(
        ctx.clone(),
        StreamCall { args: call.args.clone(), folder: call.folder.clone(), scope: call.scope.clone(), fields: vec!["reactions".into()], ..Default::default() },
    )
    .await
    .unwrap();
    let Some(StreamItem::Value(first)) = items.next().await else { panic!("expected a value") };
    assert_eq!(first["messages"][0]["reactions"], json!([{"emoji": "😂", "count": 2, "mine": false}]));
    // The human's reaction through the app's own path pushes a new value, with `mine`.
    react_from_human(&*host, &team, id, "😂", None).await.unwrap();
    async fn next(items: &mut catalog::Items) -> StreamItem {
        tokio::time::timeout(std::time::Duration::from_secs(3), items.next()).await.unwrap().unwrap()
    }
    let StreamItem::Value(v) = next(&mut items).await else { panic!("expected a value") };
    assert_eq!(v["messages"][0]["reactions"], json!([{"emoji": "😂", "count": 3, "mine": true}]));
    react_from_human(&*host, &team, id, "😂", None).await.unwrap();
    let StreamItem::Value(v) = next(&mut items).await else { panic!("expected a value") };
    assert_eq!(v["messages"][0]["reactions"], json!([{"emoji": "😂", "count": 2, "mine": false}]));

    // No reactor identity in any projection.
    react_from_human(&*host, &team, id, "🔥", None).await.unwrap();
    for row in [read(&["reactions"]).await, read(&["text", "meta", "media", "reactions"]).await] {
        for group in row["reactions"].as_array().unwrap() {
            let mut keys: Vec<_> = group.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, ["count", "emoji", "mine"]);
        }
        let text = row.to_string();
        for name in ["reactors", "other-agent"] {
            assert!(!text.contains(name), "{name} leaked: {text}");
        }
    }
}

// ---- SV-INP-30: agent faces ----

fn solid_png(side: u32, rgb: [u8; 3]) -> Vec<u8> {
    let pixels = (0..side * side).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect();
    crate::avatars::raster::encode_png(&crate::avatars::raster::Rgba { width: side, height: side, pixels }).unwrap()
}

fn face_call(call: &Call, fields: &[&str], args: Value) -> Call {
    Call { args, fields: fields.iter().map(|f| f.to_string()).collect(), ..call.clone() }
}

async fn read_rows(ctx: &LiveCtx, call: &Call, fields: &[&str]) -> Vec<Value> {
    let answer = query(ctx.clone(), face_call(call, fields, json!({}))).await.unwrap();
    answer["messages"].as_array().unwrap().clone()
}

async fn face_media(ctx: &LiveCtx, call: &Call, fields: &[&str], id: i64, sha: &str) -> Result<Value, QueryError> {
    media::query(ctx.clone(), face_call(call, fields, json!({"messageId": id, "sha256": sha}))).await
}

fn set_avatar(agents: &agents::tests::FakeHost, handle: &str, avatar: Option<&str>) {
    for row in agents.rows.lock().unwrap().iter_mut().filter(|r| r.handle == handle) {
        row.avatar = avatar.map(str::to_string);
    }
}

fn not_found() -> Value {
    json!({"refused": "not_found"})
}

#[tokio::test]
async fn faces_are_opt_in_and_only_for_senders_of_the_rows() {
    let (host, agents, ctx, call) = fixture_with_agents();
    set_avatar(&agents, "agent", Some("builtin-funny:coffee-bot"));
    set_avatar(&agents, "foreign", Some("builtin-chess:king"));
    host.insert("emea", "human", "hi");
    // A message in another team's store, from a sender who also wears a face.
    host.insert_from("other", "foreign", "human", "elsewhere");
    for fields in [&[][..], &["text", "meta", "media"][..]] {
        for row in read_rows(&ctx, &call, fields).await {
            assert!(row.get("sender").is_none(), "{fields:?} leaked a face");
        }
    }
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    assert_eq!(rows.len(), 1);
    let face = &rows[0]["sender"]["face"];
    assert_eq!(face["mime"], "image/png");
    assert_eq!((face["width"].as_u64(), face["height"].as_u64()), (Some(96), Some(96)));
    let sha = face["sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64);
    // Nothing about the other team's sender or about anyone absent from the rows.
    let text = serde_json::to_string(&rows).unwrap();
    for leak in ["foreign", "builtin-", "coffee-bot", "king"] {
        assert!(!text.contains(leak), "{leak} leaked");
    }
    assert_eq!(rows[0]["sender"].as_object().unwrap().len(), 1);
}

#[tokio::test]
async fn faces_are_images_emoji_or_colours_with_the_session_first() {
    let (host, agents, ctx, call) = fixture_with_agents();
    agents.faces.lock().unwrap().agent_avatars.insert("builtin/claude".into(), "#336699".into());
    agents.faces.lock().unwrap().human = Some("🙂".into());
    host.insert("emea", "human", "from the agent");
    host.insert_from("emea", "human", "agent", "from the human");
    let face_of = |rows: &[Value], from: &str| {
        rows.iter().find(|r| r["from"] == from).and_then(|r| r.get("sender").map(|s| s["face"].clone()))
    };
    let cases = [
        // The session's own avatar wins over its Agent's default.
        (Some("🦊"), json!({"emoji": "🦊"})),
        (Some("#ABC"), json!({"colour": "#aabbcc"})),
        (Some("#A1B2C3"), json!({"colour": "#a1b2c3"})),
        // A pack this Mac lacks falls through to the Agent's default face.
        (Some("nopack:missing"), json!({"colour": "#336699"})),
        (None, json!({"colour": "#336699"})),
    ];
    for (avatar, expected) in cases {
        set_avatar(&agents, "agent", avatar);
        let rows = read_rows(&ctx, &call, &["faces"]).await;
        assert_eq!(face_of(&rows, "agent"), Some(expected), "{avatar:?}");
    }
    // A name that is not a face (too long, markup) is no face of its own: the sender is dealt
    // one, by its Agent's key, exactly as the agents list deals it.
    let dealt = |key: &str| {
        let pool = crate::avatars::pool::builtin_refs(crate::avatars::pool::FALLBACK_AVATAR_SET);
        let reference = crate::avatars::pool::deal(&pool, key).unwrap();
        let bytes = crate::http::routes::dashboard_kit::builtin_face(
            reference.split_once(':').unwrap().0,
            reference.split_once(':').unwrap().1,
        )
        .unwrap();
        crate::avatars::hosted::normalize(crate::avatars::FaceFormat::Svg, bytes).unwrap().sha256.clone()
    };
    for bad in ["not a face", "<b>", "#12", "#ggg", ""] {
        set_avatar(&agents, "agent", Some(bad));
        agents.faces.lock().unwrap().agent_avatars.clear();
        let rows = read_rows(&ctx, &call, &["faces"]).await;
        let face = face_of(&rows, "agent").unwrap();
        assert_eq!(face["sha256"], dealt("builtin/claude"), "{bad:?}");
    }
    // The human's own face rides the same resolver.
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    assert_eq!(face_of(&rows, "human"), Some(json!({"emoji": "🙂"})));
    // The human is never dealt a face: with none of their own they show initials.
    agents.faces.lock().unwrap().human = None;
    assert_eq!(face_of(&read_rows(&ctx, &call, &["faces"]).await, "human"), None);
    agents.faces.lock().unwrap().human = Some("🙂".into());
    agents.faces.lock().unwrap().human = Some("builtin-chess:king".into());
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    assert!(face_of(&rows, "human").unwrap()["sha256"].is_string());
}

#[tokio::test]
async fn team_avatar_set_and_uploaded_faces_are_drawn_to_pngs() {
    use base64::Engine as _;
    let (host, agents, ctx, call) = fixture_with_agents();
    let red = solid_png(200, [200, 10, 10]);
    agents.faces.lock().unwrap().packs.insert(("crew".into(), "red".into()), (crate::avatars::FaceFormat::Png, red));
    set_avatar(&agents, "agent", Some("crew:red"));
    let blue = solid_png(70, [10, 10, 200]);
    let upload = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&blue));
    agents.faces.lock().unwrap().human = Some(upload);
    host.insert("emea", "human", "a");
    host.insert_from("emea", "human", "agent", "b");
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    let dims = |from: &str| {
        let f = &rows.iter().find(|r| r["from"] == from).unwrap()["sender"]["face"];
        (f["width"].as_u64().unwrap(), f["height"].as_u64().unwrap())
    };
    // A 200 px team face is scaled down; a 70 px upload is never scaled up.
    assert_eq!(dims("agent"), (96, 96));
    assert_eq!(dims("human"), (70, 70));
}

#[tokio::test]
async fn media_serves_a_face_to_whoever_the_rows_name_and_nothing_else() {
    use base64::Engine as _;
    let (host, agents, ctx, call) = fixture_with_agents();
    set_avatar(&agents, "agent", Some("builtin-funny:coffee-bot"));
    agents.faces.lock().unwrap().human = Some("builtin-chess:king".into());
    let id = host.insert("emea", "human", "from the agent");
    let from_human = host.insert_from("emea", "human", "agent", "from the human");
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    let sha_of = |from: &str| {
        rows.iter().find(|r| r["from"] == from).unwrap()["sender"]["face"]["sha256"].as_str().unwrap().to_string()
    };
    let (agent_sha, human_sha) = (sha_of("agent"), sha_of("human"));
    assert_ne!(agent_sha, human_sha);

    // The bytes: one answer, a PNG whose hash is the one named.
    let answer = face_media(&ctx, &call, &["faces"], id, &agent_sha).await.unwrap();
    assert_eq!((answer["mime"].as_str(), answer["done"].as_bool()), (Some("image/png"), Some(true)));
    let png = base64::engine::general_purpose::STANDARD.decode(answer["data"].as_str().unwrap()).unwrap();
    assert_eq!(format!("{:x}", sha2::Sha256::digest(&png)), agent_sha);
    assert_eq!((answer["width"].as_u64(), answer["height"].as_u64()), (Some(96), Some(96)));
    // The human's face is served on the message the human sent, and only on it.
    assert!(face_media(&ctx, &call, &["faces"], from_human, &human_sha).await.unwrap().get("data").is_some());
    assert_eq!(face_media(&ctx, &call, &["faces"], id, &human_sha).await.unwrap(), not_found());
    assert_eq!(face_media(&ctx, &call, &["faces"], from_human, &agent_sha).await.unwrap(), not_found());
}

#[tokio::test]
async fn a_face_hash_outside_the_projection_is_the_same_not_found_as_any_miss() {
    let (host, agents, ctx, call) = fixture_with_agents();
    set_avatar(&agents, "agent", Some("builtin-funny:coffee-bot"));
    set_avatar(&agents, "foreign", Some("builtin-chess:king"));
    agents.launch("lurker", Some(("acme", "emea")), "workspace", None);
    set_avatar(&agents, "lurker", Some("builtin-chess:queen"));
    let id = host.insert("emea", "human", "hi");
    let other = host.insert_from("other", "foreign", "human", "secret");
    // The faces of a sender in another team, and of a session of this team who sent nothing.
    let sha_of_face = |reference: &str| {
        let bytes = crate::http::routes::dashboard_kit::builtin_face(
            reference.split_once(':').unwrap().0,
            reference.split_once(':').unwrap().1,
        )
        .unwrap();
        crate::avatars::hosted::normalize(crate::avatars::FaceFormat::Svg, bytes).unwrap().sha256.clone()
    };
    let (foreign_sha, lurker_sha) = (sha_of_face("builtin-chess:king"), sha_of_face("builtin-chess:queen"));
    let own = read_rows(&ctx, &call, &["faces"]).await[0]["sender"]["face"]["sha256"].as_str().unwrap().to_string();
    let miss = not_found();
    for sha in [&foreign_sha, &lurker_sha, &"0".repeat(64), &"f".repeat(64)] {
        assert_eq!(face_media(&ctx, &call, &["faces"], id, sha).await.unwrap(), miss, "{sha}");
        // The other team's own message id is not a message of this team.
        assert_eq!(face_media(&ctx, &call, &["faces"], other + 7, sha).await.unwrap(), miss);
    }
    // The same hash against an unknown message, and the right hash against one.
    assert_eq!(face_media(&ctx, &call, &["faces"], id + 40, &own).await.unwrap(), miss);
    // Only `faces` serves a face: `media` alone is the same miss, and so is an attachment hash
    // under `faces` alone.
    assert_eq!(face_media(&ctx, &call, &["media"], id, &own).await.unwrap(), miss);
    let gif_sha = host.cache_media(&gif(64));
    host.attach("emea", id, &[image(&gif_sha, "cached")]);
    assert_eq!(face_media(&ctx, &call, &["faces"], id, &gif_sha).await.unwrap(), miss);
    assert!(face_media(&ctx, &call, &["media"], id, &gif_sha).await.unwrap().get("data").is_some());
    // With both declared each is served.
    assert!(face_media(&ctx, &call, &["media", "faces"], id, &own).await.unwrap().get("data").is_some());
    assert!(face_media(&ctx, &call, &["media", "faces"], id, &gif_sha).await.unwrap().get("data").is_some());
    // Neither declared: not approved, whatever the hash.
    for fields in [&[][..], &["text", "meta"][..]] {
        assert!(matches!(face_media(&ctx, &call, fields, id, &own).await, Err(QueryError::NotApproved(_))));
    }
    // A bad hash is a bad argument, as for attachments.
    assert!(matches!(face_media(&ctx, &call, &["faces"], id, "ABC").await, Err(QueryError::InvalidArgs(_))));
}

#[tokio::test]
async fn a_face_changed_on_the_mac_is_not_served_under_its_old_hash() {
    let (host, agents, ctx, call) = fixture_with_agents();
    set_avatar(&agents, "agent", Some("builtin-funny:coffee-bot"));
    let id = host.insert("emea", "human", "hi");
    let old = read_rows(&ctx, &call, &["faces"]).await[0]["sender"]["face"]["sha256"].as_str().unwrap().to_string();
    set_avatar(&agents, "agent", Some("builtin-chess:king"));
    assert_eq!(face_media(&ctx, &call, &["faces"], id, &old).await.unwrap(), not_found());
    let new = read_rows(&ctx, &call, &["faces"]).await[0]["sender"]["face"]["sha256"].as_str().unwrap().to_string();
    assert_ne!(old, new);
    assert!(face_media(&ctx, &call, &["faces"], id, &new).await.unwrap().get("data").is_some());
}

#[tokio::test]
async fn the_inbox_stream_carries_faces_only_when_declared() {
    let (host, agents, ctx, call) = fixture_with_agents();
    set_avatar(&agents, "agent", Some("🦊"));
    host.insert("emea", "human", "hi");
    for (fields, expected) in [(vec![], false), (vec!["faces".to_string()], true)] {
        let mut items = agents::open_inbox(
            ctx.clone(),
            StreamCall {
                args: call.args.clone(),
                folder: call.folder.clone(),
                scope: call.scope.clone(),
                fields,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let Some(StreamItem::Value(first)) = items.next().await else { panic!("no first value") };
        assert_eq!(first["messages"][0].get("sender").is_some(), expected);
    }
}

#[tokio::test]
async fn an_agent_with_no_face_is_dealt_one_by_its_key_name_or_handle() {
    let (host, agents, ctx, call) = fixture_with_agents();
    // Sessions: one with a saved Agent (dealt by its key), one without (by its name), and a
    // sender no session is listed for (by its handle).
    agents.launch("named", Some(("acme", "emea")), "workspace", None);
    for row in agents.rows.lock().unwrap().iter_mut().filter(|r| r.handle == "named") {
        row.agent = None;
        row.display_name = "Calm Otter".into();
    }
    host.insert_from("emea", "agent", "human", "a");
    host.insert_from("emea", "named", "human", "b");
    host.insert_from("emea", "ghost", "human", "c");
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    let sha = |from: &str| rows.iter().find(|r| r["from"] == from).unwrap()["sender"]["face"]["sha256"].as_str().unwrap().to_string();
    let drawn = |reference: &str| {
        let (pack, id) = reference.split_once(':').unwrap();
        let bytes = crate::http::routes::dashboard_kit::builtin_face(pack, id).unwrap();
        crate::avatars::hosted::normalize(crate::avatars::FaceFormat::Svg, bytes).unwrap().sha256.clone()
    };
    let funny = crate::avatars::pool::builtin_refs("builtin-funny");
    let deal = |key: &str| drawn(crate::avatars::pool::deal(&funny, key).unwrap());
    assert_eq!(sha("agent"), deal("builtin/claude"));
    assert_eq!(sha("named"), deal("Calm Otter"));
    assert_eq!(sha("ghost"), deal("ghost"));
    // A sender with no session is dealt by its display name, trimmed, as the in-app Inbox
    // does, and the projection and the proof agree on it.
    host.names.lock().unwrap().insert("ghost".into(), "  Spooky Ghost ".into());
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    let ghost = rows.iter().find(|r| r["from"] == "ghost").unwrap()["sender"]["face"]["sha256"].as_str().unwrap().to_string();
    assert_eq!(ghost, deal("Spooky Ghost"));
    let ghost_id = rows.iter().find(|r| r["from"] == "ghost").unwrap()["messageId"].as_i64().unwrap();
    assert!(face_media(&ctx, &call, &["faces"], ghost_id, &ghost).await.unwrap().get("data").is_some());
    // The team's default pack replaces the pool.
    agents.faces.lock().unwrap().pool = Some(vec!["builtin-chess:king".into(), "builtin-chess:queen".into()]);
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    let sha = |from: &str| rows.iter().find(|r| r["from"] == from).unwrap()["sender"]["face"]["sha256"].as_str().unwrap().to_string();
    let chess = ["builtin-chess:king".to_string(), "builtin-chess:queen".to_string()];
    for (from, key) in [("agent", "builtin/claude"), ("named", "Calm Otter"), ("ghost", "Spooky Ghost")] {
        assert_eq!(sha(from), drawn(crate::avatars::pool::deal(&chess, key).unwrap()), "{from}");
    }
    // A dealt face is served like any other, and only for its own sender.
    let id = host.insert_from("emea", "named", "human", "d");
    let own = sha("named");
    assert!(face_media(&ctx, &call, &["faces"], id, &own).await.unwrap().get("data").is_some());
    assert_eq!(face_media(&ctx, &call, &["faces"], id, &sha("agent")).await.unwrap(), not_found());
}

#[tokio::test]
async fn a_team_or_uploaded_svg_face_is_never_drawn_and_leaves_the_sender_with_no_face() {
    use base64::Engine as _;
    let (host, agents, ctx, call) = fixture_with_agents();
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><rect width="4" height="4" fill="#0f0"/></svg>"##;
    agents.faces.lock().unwrap().packs.insert(("crew".into(), "logo".into()), (crate::avatars::FaceFormat::Svg, svg.to_vec()));
    // The session's avatar is a team SVG; its Agent's default is a colour that would otherwise
    // be next: the search ends, so the sender shows initials, not the colour or a dealt face.
    agents.faces.lock().unwrap().agent_avatars.insert("builtin/claude".into(), "#336699".into());
    set_avatar(&agents, "agent", Some("crew:logo"));
    // The human uploaded an SVG.
    agents.faces.lock().unwrap().human = Some(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)));
    host.insert("emea", "human", "from the agent");
    host.insert_from("emea", "human", "agent", "from the human");
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    for row in &rows {
        assert!(row.get("sender").is_none(), "{} got a face", row["from"]);
    }
    // A shipped pack's SVG is still drawn.
    set_avatar(&agents, "agent", Some("builtin-chess:king"));
    let rows = read_rows(&ctx, &call, &["faces"]).await;
    assert!(rows.iter().find(|r| r["from"] == "agent").unwrap()["sender"]["face"]["sha256"].is_string());
}

#[tokio::test]
async fn marking_read_from_a_dashboard_flips_unread_on_the_stream_through_the_apps_own_path() {
    let (host, ctx, call) = fixture();
    let one = host.insert("emea", "human", "one");
    let two = host.insert("emea", "human", "two");
    let mut items = agents::open_inbox(
        ctx.clone(),
        StreamCall { args: call.args.clone(), folder: call.folder.clone(), scope: call.scope.clone(), ..Default::default() },
    )
    .await
    .unwrap();
    async fn next(items: &mut catalog::Items) -> StreamItem {
        tokio::time::timeout(std::time::Duration::from_secs(3), items.next()).await.unwrap().unwrap()
    }
    let Some(StreamItem::Value(first)) = items.next().await else { panic!("expected a value") };
    assert_eq!(first["humanUnread"], 2);
    let args = super::MarkReadArgs::parse(json!({"messageIds": [one]})).unwrap();
    let done = super::mark_read(&ctx, &call.folder, call.scope.as_ref(), args).await.unwrap();
    assert_eq!(done["marked"], 1);
    let StreamItem::Value(v) = next(&mut items).await else { panic!("expected a value") };
    assert_eq!(v["humanUnread"], 1);
    let unread: Vec<_> = v["messages"].as_array().unwrap().iter().filter(|m| m["unread"] == true).map(|m| m["messageId"].clone()).collect();
    assert_eq!(unread, vec![json!(two)]);
}

/// A real context index, reset while the hosted inbox stream is open: the reset empties the index
/// until the walk finishes, and the stream is not ended for it (it was ended `no_team` before).
/// It goes on once the walk is done. A folder in no team's context is still refused `no_team`.
#[tokio::test]
async fn a_context_index_reset_does_not_end_an_open_inbox_stream() {
    use crate::context_query::service::{Footprint, ContextIndexService};
    use crate::context_query::{index::IndexRoot, model::Layer};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("acme/emea/content");
    let folder = root.join("dashboards/phone");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(root.join("note.md"), "# note\n").unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let walking = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (flag, dir) = (walking.clone(), root.clone());
    let service = Arc::new(ContextIndexService::new());
    service.start(
        Arc::new(move || {
            flag.load(Ordering::SeqCst).then(|| Footprint {
                owner: 1,
                roots: vec![IndexRoot {
                    dir: dir.clone(),
                    org: Some("acme".into()),
                    team: Some("emea".into()),
                    layer: Layer::Team,
                }],
                summary_headings: Vec::new(),
            })
        }),
        Arc::new(|_| {}),
    );
    let ready = |want: bool| {
        let service = service.clone();
        async move {
            for _ in 0..200 {
                if service.snapshot().ready == want {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            panic!("the index never became ready={want}");
        }
    };
    ready(true).await;

    let (host, agents, ctx, call) = fixture_with_agents();
    *agents.index.lock().unwrap() = Some(service.clone());
    host.insert("emea", "human", "first");
    let open = |folder: PathBuf| {
        let (ctx, call) = (ctx.clone(), call.clone());
        async move {
            agents::open_inbox(
                ctx,
                StreamCall { args: call.args, folder, scope: call.scope, ..Default::default() },
            )
            .await
        }
    };
    let mut items = open(folder.clone()).await.unwrap();
    assert!(matches!(items.next().await, Some(StreamItem::Value(_))));

    // The footprint is not available again and the index is reset: no roots, not ready, for as
    // long as the stream polls twice over (it ended `no_team` after two polls).
    walking.store(false, Ordering::SeqCst);
    service.reset();
    ready(false).await;
    assert!(service.snapshot().roots.is_empty());
    let waited = tokio::time::timeout(agents::LIST_EVERY * 4, items.next()).await;
    assert!(waited.is_err(), "the stream was ended or sent while the index was reset: {waited:?}");
    // An open during the reset is a failed open, never a refusal about the team.
    assert_eq!(
        open(folder.clone()).await.err(),
        Some(QueryError::Failed(agents::INDEXING.into()))
    );

    // The walk finishes: the stream goes on with the next change.
    walking.store(true, Ordering::SeqCst);
    service.request_reconcile();
    ready(true).await;
    host.insert("emea", "human", "second");
    let item = tokio::time::timeout(agents::LIST_EVERY * 6, items.next())
        .await
        .expect("the stream resumes")
        .unwrap();
    let StreamItem::Value(value) = item else { panic!("expected a value, got {item:?}") };
    assert_eq!(value["messages"].as_array().unwrap().len(), 2);

    // With the index ready, a folder in no team's context repo is still `no_team`.
    assert_eq!(open(elsewhere).await.err(), Some(QueryError::Refused("no_team")));
}

/// While the context index is not ready, the owner's send, react and mark-read answer `failed`
/// (try again), not `not_found`; with it ready and the folder in no team they are `not_found`.
#[tokio::test]
async fn owner_actions_answer_failed_while_the_index_is_not_ready() {
    use std::sync::atomic::Ordering;
    let (host, agents, ctx, call) = fixture_with_agents();
    let id = host.insert("emea", "human", "question");
    *agents.team.lock().unwrap() = None;
    agents.not_ready.store(true, Ordering::SeqCst);
    let send = MessageArgs::parse(json!({"to":"agent","text":"hi"})).unwrap();
    assert_eq!(
        deliver(&ctx, &call.folder, call.scope.as_ref(), send).await.unwrap_err(),
        "failed"
    );
    let reacted = react(&ctx, &call.folder, call.scope.as_ref(), ReactArgs::parse(json!({"messageId": id, "emoji": "👍"})).unwrap()).await;
    assert_eq!(reacted.err(), Some("failed"));
    let marked = mark_read(&ctx, &call.folder, call.scope.as_ref(), MarkReadArgs::parse(json!({"messageIds": [id]})).unwrap()).await;
    assert_eq!(marked.err(), Some("failed"));
    agents.not_ready.store(false, Ordering::SeqCst);
    let send = MessageArgs::parse(json!({"to":"agent","text":"hi"})).unwrap();
    assert_eq!(
        deliver(&ctx, &call.folder, call.scope.as_ref(), send).await.unwrap_err(),
        "not_found"
    );
}
