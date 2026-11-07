// SPDX-License-Identifier: MIT
//! A completed remote workspace (real Git clone and worktree,
//! prepared through the real bundle upload and prepare) whose workspace
//! requests are answered by the production handlers, frame for frame as the
//! host answers them. For the app's wire-level tests; never in a release.
use super::*;
use codesense_host_proto::test_support::strip_attribution_env;
use codesense_host_proto::Frame;
use codesense_pty_core::protocol::{Message, Reply, Request};
use codesense_pty_core::remote_files::FileTarget;

pub struct RemoteWorkspaceFixture {
    root: tempfile::TempDir,
    pub target: FileTarget,
    pub worktree: std::path::PathBuf,
    pub repo: std::path::PathBuf,
    /// The next batch Apply waits for this gate (one slow Apply; a retry
    /// behind it then replays, as on a real host).
    apply_gate: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// Each batch Apply is cut (its connection retires) once this exists.
    cut_apply_when: std::sync::Mutex<Option<std::path::PathBuf>>,
}

fn git(dir: &Path, args: &[&str]) {
    let status = strip_attribution_env(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

impl RemoteWorkspaceFixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let canonical = root.path().canonicalize().expect("canonical root");
        let source = canonical.join("source");
        std::fs::create_dir(&source).expect("source");
        git(&source, &["init", "-b", "main"]);
        git(
            &source,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
        git(&source, &["branch", "codesense-prepared"]);
        let bundle_path = canonical.join("input.bundle");
        git(
            &source,
            &[
                "bundle",
                "create",
                bundle_path.to_str().expect("utf8"),
                "refs/heads/codesense-prepared",
            ],
        );
        let bytes = std::fs::read(&bundle_path).expect("bundle");
        let commit = String::from_utf8(
            strip_attribution_env(&mut Command::new("git"))
                .args(["rev-parse", "HEAD"])
                .current_dir(&source)
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_string();
        let request = codesense_pty_core::remote_workspace::PrepareWorkspace {
            request_id: uuid::Uuid::new_v4().to_string(),
            owner_id: 41,
            origin_key: "b".repeat(64),
            repository_key: "github:example/fixture".into(),
            clone_url: source.to_string_lossy().into_owned(),
            repo_path: canonical.join("clone").to_string_lossy().into_owned(),
            worktree_path: canonical.join("work").to_string_lossy().into_owned(),
            base_ref: "main".into(),
            branch: "work-branch".into(),
            bundle: codesense_pty_core::remote_workspace::GitBundle {
                size: bytes.len() as u64,
                digest: digest(&bytes),
                commit,
            },
        };
        for (index, chunk) in bytes.chunks(BUNDLE_CHUNK_BYTES).enumerate() {
            upload(
                root.path(),
                41,
                BundleChunk {
                    request: request.clone(),
                    offset: (index * BUNDLE_CHUNK_BYTES) as u64,
                    bytes: chunk.to_vec(),
                },
                || true,
            )
            .expect("bundle upload");
        }
        let prepared = prepare(root.path(), 41, request.clone(), || true);
        assert_eq!(prepared.status, "prepared", "{prepared:?}");
        Self {
            target: FileTarget {
                owner_id: 41,
                origin_key: request.origin_key,
                request_id: request.request_id,
                handle: prepared.handle.expect("handle"),
            },
            worktree: request.worktree_path.into(),
            repo: request.repo_path.into(),
            root,
            apply_gate: std::sync::Mutex::new(None),
            cut_apply_when: std::sync::Mutex::new(None),
        }
    }

    /// Recreate a forgotten workspace at its original path through preparation.
    pub fn prepare_again(&self) -> FileTarget {
        let (_, _, receipt) =
            captured_receipt(self.root.path(), 41, &self.target, &|| true).unwrap();
        let mut request = receipt.intent;
        request.request_id = uuid::Uuid::new_v4().to_string();
        // The clone already has the imported commit; no bundle crosses again.
        request.bundle.size = 0;
        request.bundle.digest = digest(&[]);
        let result = prepare(self.root.path(), 41, request.clone(), || true);
        assert_eq!(result.status, "prepared", "{result:?}");
        FileTarget {
            owner_id: 41,
            origin_key: request.origin_key,
            request_id: request.request_id,
            handle: result.handle.unwrap(),
        }
    }

    /// Hold the next batch Apply until the returned sender sends (or drops).
    pub fn hold_next_apply(&self) -> std::sync::mpsc::Sender<()> {
        let (release, gate) = std::sync::mpsc::channel();
        *self.apply_gate.lock().unwrap() = Some(gate);
        release
    }

    /// Cut every batch Apply as soon as `path` exists (None: never).
    pub fn cut_apply_when(&self, path: Option<std::path::PathBuf>) {
        *self.cut_apply_when.lock().unwrap() = path;
    }

    /// The host's private root.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// Answer call `rid` as the host does: each streamed part, then the reply,
    /// as control frames handed to `send` in order. `None` for a request this
    /// fixture does not serve.
    pub fn answer(&self, rid: u64, request: Request, send: &mut dyn FnMut(Frame)) -> Option<()> {
        let root = self.root.path();
        let result = match request {
            Request::WorkspaceManifest { request } => {
                manifest::handle(root, 41, *request, || true, &mut |part| {
                    manifest::part_frame(rid, part).map(&mut *send).is_some()
                })
                .map(|result| Reply::WorkspaceManifest {
                    result: Box::new(result),
                })
            }
            Request::WorkspaceContextBatch { request } => {
                let applying = matches!(
                    request.operation,
                    codesense_pty_core::context_batch::BatchOperation::Apply
                );
                let cut = if applying {
                    let gate = self.apply_gate.lock().unwrap().take();
                    if let Some(gate) = gate {
                        let _ = gate.recv();
                    }
                    self.cut_apply_when.lock().unwrap().clone()
                } else {
                    None
                };
                context_batch::handle(root, 41, *request, || {
                    cut.as_ref().is_none_or(|path| !path.exists())
                })
                .map(|result| Reply::WorkspaceContextBatch {
                    result: Box::new(result),
                })
            }
            Request::ForgetMissingWorkspace { request } => {
                forget_missing_workspace(root, 41, request, || true).map(|()| Reply::Done)
            }
            Request::WorkspaceFile { request } => {
                files::handle(root, 41, *request, || true).map(|result| Reply::WorkspaceFile {
                    result: Box::new(result),
                })
            }
            _ => return None,
        };
        send(Frame::control(&Message::Reply { rid, result }).expect("reply frame"));
        Some(())
    }
}

impl Default for RemoteWorkspaceFixture {
    fn default() -> Self {
        Self::new()
    }
}
