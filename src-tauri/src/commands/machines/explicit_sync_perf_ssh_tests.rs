// SPDX-License-Identifier: MIT
// The end-to-end half of the remote perf harness (a maintainer script that
// starts the sshd test container). Real Step 2 / hydrate and publish runs over real SSH
// against the sshd container, each split into diff, transfer and apply from
// the engine's own progress phases, with the machine's per-step batch log
// lines (time and fsync count) attached. `XS26_LINK=slow` shapes the link
// to 20 Mbit/s each way with 50 ms RTT once setup is done (netem on both
// ends of the container's veth pair).
//
// Shapes: (a) a real team context (`XS26_CONTEXT_SOURCE`, every regular
// file under it), cold then no-op; (b) 2,000 text files of about 40 MB,
// cold then no-op; (d) five machine-side edits published back.

static XS26_PERF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[test]
#[ignore = "owned real SSH: SW-XS-26 remote explicit-sync perf harness"]
fn remote_explicit_sync_perf_over_real_ssh() {
    // Only the perf mode runs it: the shared-container default run would
    // otherwise share its fixture with every other container case.
    if std::env::var_os("XS26_LINK").is_none() {
        eprintln!("skipped: set XS26_LINK to run the remote perf case");
        return;
    }
    XS26_PERF.store(true, std::sync::atomic::Ordering::SeqCst);
    remote_hydration_fixture(false, false, false, false);
}

fn xs26_perf_requested() -> bool {
    XS26_PERF.load(std::sync::atomic::Ordering::SeqCst)
}

/// Text that compresses like agent context does (a fixed vocabulary in short
/// markdown lines), deterministic per seed.
fn xs26_text(seed: u64, length: usize) -> Vec<u8> {
    const WORDS: &[&str] = &[
        "the",
        "agent",
        "context",
        "story",
        "workspace",
        "machine",
        "sync",
        "explicit",
        "apply",
        "batch",
        "manifest",
        "receipt",
        "directory",
        "file",
        "launch",
        "prepare",
        "review",
        "spec",
        "acceptance",
        "criteria",
        "given",
        "when",
        "then",
        "remote",
        "server",
        "session",
        "terminal",
        "team",
        "skill",
        "hook",
        "settings",
        "invariant",
        "budget",
        "durable",
        "journal",
        "commit",
        "operation",
        "outcome",
        "status",
        "phase",
        "upload",
        "chunk",
        "seal",
        "verify",
        "hash",
        "digest",
        "mode",
        "exclude",
        "block",
        "path",
        "write",
        "remove",
        "conflict",
        "applied",
        "settled",
        "pending",
        "recover",
        "reconcile",
        "lock",
        "and",
        "of",
        "to",
        "in",
        "is",
        "a",
        "for",
        "on",
        "with",
        "that",
        "by",
        "it",
        "as",
    ];
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut out = Vec::with_capacity(length + 64);
    while out.len() < length {
        match next() % 12 {
            0 => out.extend_from_slice(b"\n## "),
            1 => out.extend_from_slice(b"\n- "),
            2 => out.extend_from_slice(b"\n\n"),
            _ => out.push(b' '),
        }
        out.extend_from_slice(WORDS[(next() % WORDS.len() as u64) as usize].as_bytes());
        if next() % 9 == 0 {
            out.extend_from_slice(format!(" `{}`", next() % 100_000).as_bytes());
        }
    }
    out.truncate(length);
    out
}

/// Where each engine phase began, from its progress events.
#[derive(Default)]
struct Xs26Phases {
    started: Option<std::time::Instant>,
    first: std::collections::BTreeMap<&'static str, std::time::Instant>,
    sent: u64,
}
impl Xs26Phases {
    fn observe(&mut self, progress: &crate::agents::prepare::PrepareProgress) {
        use crate::agents::prepare::PreparePhase;
        let name = match progress.phase {
            PreparePhase::Diff => "diff",
            PreparePhase::Transfer => {
                self.sent = self.sent.max(progress.total);
                "transfer"
            }
            PreparePhase::Apply => "apply",
            _ => "done",
        };
        self.first
            .entry(name)
            .or_insert_with(std::time::Instant::now);
    }
    /// Milliseconds per phase: each phase lasts until the next one began.
    fn split(&self, ended: std::time::Instant) -> String {
        let started = self.started.expect("started");
        let order = ["diff", "transfer", "apply", "done"];
        let mut marks: Vec<(&str, std::time::Instant)> = order
            .iter()
            .filter_map(|name| self.first.get(name).map(|at| (*name, *at)))
            .collect();
        marks.sort_by_key(|(_, at)| *at);
        let mut parts = Vec::new();
        let mut previous = ("setup", started);
        for (name, at) in marks.into_iter().chain([("end", ended)]) {
            if previous.0 != "done" {
                parts.push(format!(
                    "\"{}_ms\":{}",
                    previous.0,
                    at.duration_since(previous.1).as_millis()
                ));
            }
            previous = (name, at);
        }
        parts.join(",")
    }
}

/// The machine's batch log lines since `from` (a line count).
fn xs26_server_lines(from: usize) -> (usize, Vec<String>) {
    let output = std::process::Command::new("docker")
        .args([
            "exec",
            &var("CODESENSE_REMOTE_HOST_CONTAINER"),
            "sh",
            "-c",
            "cat /home/dev/.codesense/remote-private/v1/logs/pty-server.log 2>/dev/null || true",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    let batch = lines
        .iter()
        .skip(from)
        .filter(|line| line.contains("context batch ") && !line.contains("context batch chunk"))
        .map(|line| {
            let at = line.find("context batch ").unwrap();
            line[at..].replace('"', "'")
        })
        .collect();
    (lines.len(), batch)
}

/// Shape the container's link both ways: its own egress (downloads) inside
/// the container, and its veth peer's egress in the Docker host's namespace
/// (uploads), through a privileged helper on the host network.
fn xs26_link(link: &str) {
    let container = var("CODESENSE_REMOTE_HOST_CONTAINER");
    let index = std::process::Command::new("docker")
        .args(["exec", &container, "cat", "/sys/class/net/eth0/iflink"])
        .output()
        .unwrap();
    let index = String::from_utf8_lossy(&index.stdout).trim().to_string();
    let image = std::process::Command::new("docker")
        .args(["inspect", "--format", "{{.Config.Image}}", &container])
        .output()
        .unwrap();
    let image = String::from_utf8_lossy(&image.stdout).trim().to_string();
    let shape = |device: &str| match link {
        "slow" => {
            format!("tc qdisc replace dev {device} root netem delay 25ms rate 20mbit limit 10000")
        }
        _ => format!("(tc qdisc del dev {device} root 2>/dev/null; true)"),
    };
    docker_as_root(&shape("eth0"));
    let peer = format!(
        "dev=$(ip -o link | awk -F': ' '$1 == \"{index}\" {{ sub(/@.*/, \"\", $2); print $2 }}'); \
         test -n \"$dev\" && {}",
        shape("\"$dev\"")
    );
    let status = std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "--privileged",
            "--net=host",
            &image,
            "sh",
            "-c",
            &peer,
        ])
        .status()
        .unwrap();
    assert!(status.success(), "the host-side shaping applied");
}

fn remote_explicit_sync_perf(
    ctx: &crate::content_engine::ResolverContext,
    slug: &crate::domain::SourceRepoSlug,
    io: &dyn crate::content_engine::destination::DestinationIo,
    worktree: &str,
    content: &std::path::Path,
) {
    let link = std::env::var("XS26_LINK").unwrap_or_else(|_| "lan".into());
    let principal = ctx.principal();
    let target = crate::agents::ManifestTarget {
        context_root: None,
        org: ctx.org_slug.clone(),
        team: "default".into(),
        repo: Some(slug.clone()),
    };
    // Shape (a): a real team context.
    let mut real = 0usize;
    if let Ok(source) = std::env::var("XS26_CONTEXT_SOURCE") {
        let source = std::path::PathBuf::from(source);
        let mut stack = vec![source.clone()];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                let relative = entry.path().strip_prefix(&source).unwrap().to_owned();
                if kind.is_dir() {
                    stack.push(entry.path());
                } else if kind.is_file() {
                    let destination = content.join("perf-a").join(&relative);
                    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
                    std::fs::copy(entry.path(), destination).unwrap();
                    real += 1;
                }
            }
        }
    }
    // Shape (b): 2,000 files, about 40 MB, in 100 directories.
    let files = std::env::var("XS26_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2_000u64);
    let total = std::env::var("XS26_TOTAL_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(40_000_000u64);
    for index in 0..files {
        let path = content.join(format!("perf-b/d{:03}/f{index:05}.md", index % 100));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, xs26_text(index + 1, (total / files) as usize)).unwrap();
    }
    xs26_link(&link);
    let legacy = || Ok(Vec::new());
    let validate = || io.validate_authority();
    let mut log_from = xs26_server_lines(0).0;
    let mut operation = 0x100u32;
    let mut report = |shape: &str, sets: &[&str]| {
        let sets: Vec<String> = sets.iter().map(|set| set.to_string()).collect();
        let want = crate::agents::prepare::step3::want_for_selection(
            &ctx.codesense_home,
            &principal,
            &target,
            &[],
            &sets,
        )
        .unwrap();
        operation += 1;
        let id = format!("6f1e2c3a-0000-4000-8000-{operation:012x}");
        let phases = std::cell::RefCell::new(Xs26Phases::default());
        phases.borrow_mut().started = Some(std::time::Instant::now());
        let progress =
            |event: &crate::agents::prepare::PrepareProgress| phases.borrow_mut().observe(event);
        let outcome =
            crate::agents::prepare::step3::hydrate(crate::agents::prepare::step3::Hydrate {
                home: &ctx.codesense_home,
                principal: &principal,
                destination: crate::agents::prepare::Destination::Remote { io },
                want: &want,
                operation: &id,
                workspace: worktree.to_string(),
                agent: "perf".into(),
                prune: false,
                overwrite_modified: false,
                progress: &progress,
                validate: &validate,
                cancel: None,
                legacy: &legacy,
            })
            .unwrap();
        let ended = std::time::Instant::now();
        assert_eq!(
            outcome.status,
            crate::agents::prepare::PrepareStatus::Ready,
            "{outcome:?}"
        );
        let phases = phases.into_inner();
        let (next, server) = xs26_server_lines(log_from);
        log_from = next;
        println!(
            "XS26_E2E {{\"link\":\"{link}\",\"shape\":\"{shape}\",\"wanted\":{},\"written\":{},\"sent_bytes\":{},\"total_ms\":{},{},\"server\":{}}}",
            want.files.len(),
            outcome.written.len(),
            phases.sent,
            ended.duration_since(phases.started.unwrap()).as_millis(),
            phases.split(ended),
            serde_json::to_string(&server).unwrap()
        );
    };
    if real > 0 {
        report("a-real-context-cold", &["perf-a"]);
        report("a-real-context-noop", &["perf-a"]);
    }
    report("b-2000-files-cold", &["perf-b"]);
    report("c-2000-files-noop", &["perf-b"]);
    // Five desktop edits: Step 2 sends exactly those.
    for index in 0..5u64 {
        let path = content.join(format!("perf-b/d{:03}/f{index:05}.md", index % 100));
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"\nedited on the desktop\n");
        std::fs::write(path, bytes).unwrap();
    }
    report("d-5-edits-hydrate", &["perf-b"]);
    // Five machine edits, published back.
    for index in 5..10u64 {
        docker_as_dev(&format!(
            "printf '\\nedited on the machine\\n' >> '{worktree}/perf-b/d{:03}/f{index:05}.md'",
            index % 100
        ));
    }
    let expect = std::collections::BTreeMap::new();
    let paths = vec!["perf-b".to_string()];
    let started = std::time::Instant::now();
    let published = crate::agents::prepare::publish::publish(
        &crate::agents::prepare::publish::PublishRequest {
            home: &ctx.codesense_home,
            principal: &principal,
            destination: crate::agents::prepare::Destination::Remote { io },
            target: &target,
            paths: &paths,
            sets: &[],
            to: None,
            expect: &expect,
            include_deletes: false,
            legacy: None,
            operation: "6f1e2c3a-0000-4000-8000-0000000002ff",
            workspace: worktree.to_string(),
            agent: "perf".into(),
            progress: &|_| {},
            validate: &validate,
            cancel: None,
        },
    )
    .unwrap();
    assert_eq!(
        published.status,
        crate::agents::prepare::publish::PublishStatus::Published,
        "{published:?}"
    );
    println!(
        "XS26_E2E {{\"link\":\"{link}\",\"shape\":\"d-5-edits-publish\",\"total_ms\":{}}}",
        started.elapsed().as_millis()
    );
    xs26_link("lan");
}
