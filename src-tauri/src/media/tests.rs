// SPDX-License-Identifier: MIT
use super::*;

#[test]
fn fetch_rule_requires_auto_own_machine_and_allowed_host() {
    for mode in [Remote::Auto, Remote::Ask, Remote::Off] {
        for own in [true, false] {
            for host in ["i.giphy.com", "example.com"] {
                let config = Config {
                    remote: mode,
                    ..Config::default()
                };
                assert_eq!(
                    config.should_auto_fetch(host, own),
                    mode == Remote::Auto && own && host == "i.giphy.com"
                );
                let a = Attachment::remote(&format!("https://{host}/cat.gif"), "cat", &config, own)
                    .unwrap();
                assert_eq!(
                    a.state,
                    if mode == Remote::Off {
                        "blocked"
                    } else {
                        "pending"
                    }
                );
            }
        }
    }
    assert!(!Config::default().allows("giphy.com.evil.test"));
}

#[test]
fn checks_magic_dimensions_and_size() {
    let gif = b"GIF89a\x02\x00\x03\x00";
    assert_eq!(inspect(gif).unwrap(), ("image/gif", 2, 3));
    assert_eq!(
        inspect(b"<svg xmlns='http://www.w3.org/2000/svg'/>")
            .unwrap_err()
            .reason,
        "type"
    );
    assert_eq!(inspect(&vec![0; MAX_BYTES + 1]).unwrap_err().reason, "size");
    assert!(inspect(b"GIF89a\x00\x00\x00\x00").is_err());
}

#[test]
fn media_requires_alt_and_exactly_one_source() {
    let input = |url, path, alt: &str| MediaInput {
        url,
        path,
        alt: alt.into(),
    };
    assert!(input(None, None, "cat").validate().is_err());
    assert!(input(Some("https://i.giphy.com/cat.gif".into()), None, "")
        .validate()
        .is_err());
    assert!(
        input(Some("http://i.giphy.com/cat.gif".into()), None, "cat")
            .validate()
            .is_err()
    );
    assert!(
        input(Some("https://i.giphy.com/cat.gif".into()), None, "cat")
            .validate()
            .is_ok()
    );
}

#[test]
fn local_media_uses_sender_scope_when_ui_scope_is_parent_team() {
    use crate::agents::content_reader::ContentReader;
    use crate::context_repo::{caller_scope::CallerScope, inheritance};

    let (_temp, root) = inheritance::test_fixture();
    let parent_only = root.join("parent/content/parent.gif");
    let child_image = root.join("child/content/child.gif");
    let gif = b"GIF89a\x02\x00\x03\x00";
    std::fs::write(&parent_only, gif).unwrap();
    std::fs::write(&child_image, gif).unwrap();

    let caller = CallerScope::Team(inheritance::team_view(&root, "acme/ctx", "child"));
    let ui = CallerScope::Team(inheritance::team_view(&root, "acme/ctx", "parent"));
    assert!(ui.open(&parent_only).is_ok());

    let cache = tempfile::tempdir().unwrap();
    assert!(prepare_local_for_scope(&caller, &parent_only, "parent", cache.path()).is_err());
    let own = prepare_local_for_scope(&caller, &child_image, "child", cache.path()).unwrap();
    assert_eq!(own.state, "cached");
    assert_eq!(own.mime.as_deref(), Some("image/gif"));
    assert_eq!(
        read_cached(cache.path(), own.sha256.as_deref().unwrap()).unwrap(),
        gif
    );
}

#[cfg(unix)]
#[test]
fn local_media_workspace_access_is_sender_bound_and_canonical() {
    use crate::context_repo::{caller_scope::CallerScope, inheritance};
    use std::os::unix::fs::symlink;

    let (_temp, root) = inheritance::test_fixture();
    let caller = CallerScope::Team(inheritance::team_view(&root, "acme/ctx", "child"));
    let workspace_parent = tempfile::tempdir().unwrap();
    let workspace = workspace_parent.path().join("sender-session-worktree");
    std::fs::create_dir_all(&workspace).unwrap();
    let other_workspace = tempfile::tempdir().unwrap();
    let arbitrary = tempfile::tempdir().unwrap();
    let image = workspace.join("meme.gif");
    let other_image = other_workspace.path().join("other.gif");
    let arbitrary_image = arbitrary.path().join("unowned.gif");
    let outside = root.join("parent/content/parent.gif");
    let escape = workspace.join("escape.gif");
    let traversal_target = workspace_parent.path().join("escape.gif");
    let gif = b"GIF89a\x02\x00\x03\x00";
    std::fs::write(&image, gif).unwrap();
    std::fs::write(&other_image, gif).unwrap();
    std::fs::write(&arbitrary_image, gif).unwrap();
    std::fs::write(&outside, gif).unwrap();
    std::fs::write(&traversal_target, gif).unwrap();
    symlink(&outside, &escape).unwrap();
    let cache = tempfile::tempdir().unwrap();

    let own = prepare_local_for_scope_or_workspace(
        &caller,
        &image,
        Some(&workspace),
        "own meme",
        cache.path(),
    );
    let other = prepare_local_for_scope_or_workspace(
        &caller,
        &other_image,
        Some(&workspace),
        "other meme",
        cache.path(),
    );
    let parent = prepare_local_for_scope_or_workspace(
        &caller,
        &outside,
        Some(&workspace),
        "parent only",
        cache.path(),
    );
    let symlink_escape = prepare_local_for_scope_or_workspace(
        &caller,
        &escape,
        Some(&workspace),
        "symlink escape",
        cache.path(),
    );
    let traversal_escape = prepare_local_for_scope_or_workspace(
        &caller,
        &workspace.join("../escape.gif"),
        Some(&workspace),
        "traversal escape",
        cache.path(),
    );
    let unowned = prepare_local_for_scope_or_workspace(
        &caller,
        &arbitrary_image,
        Some(&workspace),
        "unowned path",
        cache.path(),
    );
    let remote_workspace = prepare_local_for_scope_or_workspace(
        &caller,
        &image,
        None,
        "remote worktree",
        cache.path(),
    );
    let unknown = prepare_local_for_scope_or_workspace(
        &CallerScope::Unknown,
        &image,
        Some(&workspace),
        "unknown sender",
        cache.path(),
    );
    assert!(own.is_ok(), "sender can attach from its own workspace");
    assert!(other.is_err(), "another session's workspace is refused");
    assert!(parent.is_err(), "parent-only team content remains refused");
    assert!(
        symlink_escape.is_err(),
        "workspace symlink escape is refused"
    );
    assert!(
        traversal_escape.is_err(),
        "workspace traversal escape is refused"
    );
    assert!(
        unowned.is_err(),
        "arbitrary paths outside team content and sender workspace are refused"
    );
    assert!(
        remote_workspace.is_err(),
        "remote worktrees are not local sender workspaces"
    );
    assert!(unknown.is_err(), "unknown callers cannot use local media");
}

#[test]
fn off_masks_cached_remote_media_without_changing_the_stored_attachment() {
    let a = Attachment::remote(
        "https://i.giphy.com/cat.gif",
        "cat",
        &Config::default(),
        true,
    )
    .unwrap();
    let config = Config {
        remote: Remote::Off,
        ..Config::default()
    };
    let mut masked = vec![a.clone()];
    config.apply(&mut masked);
    assert_eq!(masked[0].state, "blocked");
    assert_ne!(a.state, "blocked");
}

#[tokio::test]
async fn fetch_transport_omits_private_headers_rejects_redirect_and_caps_stream() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gif"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "tracker=private")
                .set_body_bytes(b"GIF89a\x02\x00\x03\x00"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/redirect"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://example.com/tracker.gif"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/large"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0; MAX_BYTES + 1]))
        .mount(&server)
        .await;
    let client = client_builder(&Config::default()).build().unwrap();
    for _ in 0..2 {
        let bytes = read_response(
            client
                .get(format!("{}/gif", server.uri()))
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(inspect(&bytes).unwrap(), ("image/gif", 2, 3));
    }
    Mock::given(path("/same-host"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/gif", server.uri())),
        )
        .mount(&server)
        .await;
    // The HTTP fixture substitutes only redirect admission; production still enforces HTTPS.
    let following = client_builder(&Config::default())
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .unwrap();
    read_response(
        following
            .get(format!("{}/same-host", server.uri()))
            .send()
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let requests = server.received_requests().await.unwrap();
    for request in &requests {
        assert!(request.headers.get("cookie").is_none());
        assert!(request.headers.get("referer").is_none());
        assert_eq!(
            request.headers.get("user-agent").unwrap(),
            "ImageFetcher/1.0"
        );
    }
    assert!(client
        .get(format!("{}/redirect", server.uri()))
        .send()
        .await
        .unwrap_err()
        .is_redirect());
    assert_eq!(
        read_response(
            client
                .get(format!("{}/large", server.uri()))
                .send()
                .await
                .unwrap()
        )
        .await
        .unwrap_err()
        .reason,
        "size"
    );
    // A body with unknown length must also stop while streaming, rather than buffering the whole file.
    use axum::{body::Body, routing::get, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/",
        get(|| async {
            Body::from_stream(futures_util::stream::iter(
                (0..10).map(|_| Ok::<_, std::io::Error>(vec![0; 1024 * 1024])),
            ))
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let response = client
        .get(format!("http://{address}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.content_length(), None);
    assert_eq!(read_response(response).await.unwrap_err().reason, "size");
    task.abort();
}

#[test]
fn cache_round_trips_verified_bytes_and_all_supported_headers() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = Attachment::empty("cat");
    let bytes = b"GIF89a\x02\x00\x03\x00";
    store_bytes(dir.path(), bytes, &mut a).unwrap();
    assert_eq!(
        read_cached(dir.path(), a.sha256.as_deref().unwrap()).unwrap(),
        bytes
    );
    assert_eq!(a.state, "cached");
    let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    png.extend_from_slice(&2u32.to_be_bytes());
    png.extend_from_slice(&3u32.to_be_bytes());
    assert_eq!(inspect(&png).unwrap(), ("image/png", 2, 3));
    let mut webp = vec![0u8; 30];
    webp[..4].copy_from_slice(b"RIFF");
    webp[8..12].copy_from_slice(b"WEBP");
    webp[12..16].copy_from_slice(b"VP8X");
    webp[24] = 1;
    webp[27] = 2;
    assert_eq!(inspect(&webp).unwrap(), ("image/webp", 2, 3));
    let jpeg = b"\xff\xd8\xff\xc0\x00\x07\x08\x00\x03\x00\x02";
    assert_eq!(inspect(jpeg).unwrap(), ("image/jpeg", 2, 3));
}

#[test]
fn config_reads_edits_without_polling_or_a_cached_policy() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(Config::load(dir.path()).remote, Remote::Auto);
    std::fs::write(dir.path().join("inbox.yml"), "media: {remote: off}").unwrap();
    assert_eq!(Config::load(dir.path()).remote, Remote::Off);
    std::fs::write(
        dir.path().join("inbox.yml"),
        "media: {remote: ask, hosts: [images.example.com]}",
    )
    .unwrap();
    let config = Config::load(dir.path());
    assert_eq!(config.remote, Remote::Ask);
    assert!(config.allows("images.example.com"));
    assert!(!config.allows("i.giphy.com"));
}

#[test]
fn canonical_config_edits_emit_the_existing_inbox_domain_event() {
    use tauri::{Listener, Manager};
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.manage(crate::live_hub::LiveHub::default());
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    app.listen(crate::live_hub::inbox_events::INBOX_EVENT, move |e| {
        captured.lock().unwrap().push(e.payload().to_string());
    });
    let team = std::path::Path::new("/ctx/team");
    note_config_path(
        app.handle(),
        team,
        "org",
        "team",
        &team.join("config/other.yml"),
    );
    assert!(events.lock().unwrap().is_empty());
    note_config_path(
        app.handle(),
        team,
        "org",
        "team",
        &team.join("config/inbox.yml"),
    );
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);
    let event: crate::live_hub::inbox_events::InboxEvent =
        serde_json::from_str(&events[0]).unwrap();
    assert_eq!(event.team.org, "org");
    assert_eq!(event.team.team, "team");
}

#[test]
fn evicted_remote_images_offer_retry_and_local_images_report_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let mut local = Attachment::empty("cat");
    store_bytes(dir.path(), b"GIF89a\x02\x00\x03\x00", &mut local).unwrap();
    let mut remote = local.clone();
    remote.source_url = Some("https://i.giphy.com/cat.gif".into());
    std::fs::remove_file(dir.path().join(local.sha256.as_ref().unwrap())).unwrap();
    let mut images = vec![local, remote];
    cached_presence(dir.path(), &mut images);
    assert_eq!(images[0].state, "failed");
    assert_eq!(images[0].reason.as_deref(), Some("cache"));
    assert!(!images[0].retryable);
    assert!(images[1].retryable);
}

#[tokio::test]
async fn review_02_dns_loopback_is_refused() {
    assert_eq!(public_dns("localhost").await.unwrap_err().reason, "address");
}
#[tokio::test]
async fn review_02_private_redirect_is_refused_before_connecting() {
    use wiremock::{matchers::path, Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(path("/redirect"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://127.0.0.1:1/image.gif"),
        )
        .mount(&server)
        .await;
    let config = Config {
        hosts: vec!["127.0.0.1".into()],
        ..Config::default()
    };
    let error = client_builder(&config)
        .build()
        .unwrap()
        .get(format!("{}/redirect", server.uri()))
        .send()
        .await
        .unwrap_err();
    assert!(
        error.is_redirect(),
        "private redirect must fail admission: {error:?}"
    );
}
#[test]
fn review_03_decoded_dimensions_are_bounded() {
    assert_eq!(
        inspect(b"GIF89a\x01\x10\x00\x10").unwrap_err().reason,
        "pixels"
    );
}

#[tokio::test]
async fn review_02_nonpublic_address_families_are_refused() {
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.0.1",
        "100.64.0.1",
        "169.254.0.1",
        "0.0.0.0",
        "224.0.0.1",
        "::1",
        "::",
        "fe80::1",
        "fc00::1",
        "ff02::1",
        "::ffff:10.0.0.1",
        "::ffff:8.8.8.8",
        // NAT64 local-use (RFC 8215) can translate to any IPv4, including private ranges.
        "64:ff9b:1::a00:1",
        // NAT64 well-known prefix and 6to4 carrying private IPv4.
        "64:ff9b::a00:1",
        "2002:a00:1::1",
        // Teredo and documentation ranges.
        "2001::1",
        "2001:db8::1",
    ] {
        assert!(public_dns(ip).await.is_err(), "{ip} must not be fetched");
    }
    assert!(public_dns("8.8.8.8").await.is_ok());
    // Translation prefixes carrying a public IPv4 stay usable (DNS64 networks).
    for ip in ["64:ff9b::808:808", "2002:808:808::1"] {
        assert!(
            public_ip(ip.parse().unwrap()),
            "{ip} embeds a public address"
        );
    }
}
