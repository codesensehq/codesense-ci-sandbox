// SPDX-License-Identifier: MIT
//! Durable pinned Git exports. Only committed objects from an owned W1 workspace leave the host.
use super::*;
use codesense_pty_core::remote_export::{
    ExportDescriptor, ExportIntent, ExportOperation, ExportReply, ExportRequest, WorkspaceHead,
};
use codesense_pty_core::remote_workspace::GitBundle;

#[derive(Serialize, Deserialize)]
struct ExportReceipt {
    intent: ExportIntent,
    bundle: Option<GitBundle>,
    released: bool,
}

fn same_intent(a: &ExportIntent, b: &ExportIntent) -> bool {
    a.request_id == b.request_id
        && a.expected_head == b.expected_head
        && a.prerequisite == b.prerequisite
        && a.target.owner_id == b.target.owner_id
        && a.target.origin_key == b.target.origin_key
        && a.target.handle == b.target.handle
}
fn valid_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn persist(root: &File, key: &str, receipt: &ExportReceipt, first: bool) -> Result<(), String> {
    let bytes = serde_json::to_vec(receipt).map_err(|_| "cannot encode export receipt")?;
    if first {
        write_new(root, key, &bytes)
    } else {
        replace(root, key, &bytes)
    }
}
fn descriptor(receipt: &ExportReceipt) -> Result<ExportDescriptor, String> {
    Ok(ExportDescriptor {
        request_id: receipt.intent.request_id.clone(),
        bundle: receipt
            .bundle
            .clone()
            .ok_or("export not yet complete; reconcile original request")?,
        released: receipt.released,
    })
}
fn unlink_payload(root: &File, leaf: &str) -> Result<(), String> {
    if !exists(root, leaf)? {
        return Ok(());
    }
    let file = open(root, leaf, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let meta = io(file.metadata())?;
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("unsafe export payload".into());
    }
    let leaf = name(leaf)?;
    // SAFETY: exact regular owned staging file below a held private directory.
    if unsafe { libc::unlinkat(root.as_raw_fd(), leaf.as_ptr(), 0) } != 0 {
        return Err("export payload cleanup pending".into());
    }
    io(super::durable::sync(root))
}
fn payload(root: &File, leaf: &str) -> Result<File, String> {
    let file = open(root, leaf, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let meta = io(file.metadata())?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.len() == 0
        || meta.len() > MAX_BUNDLE_BYTES
    {
        return Err("unsafe or oversized export payload".into());
    }
    Ok(file)
}
fn payload_path(root: &File, leaf: &str) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    let path = format!("/proc/{}/fd/{}", std::process::id(), root.as_raw_fd());
    #[cfg(not(target_os = "linux"))]
    let path = descriptor_path(root)?;
    Ok(format!("{path}/{leaf}"))
}
fn hash_payload(
    root: &File,
    leaf: &str,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<GitBundle, String> {
    let mut file = payload(root, leaf)?;
    let before = io(file.metadata())?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        if !alive() || Instant::now() >= deadline {
            return Err("export authority retired or deadline exceeded".into());
        }
        let n = io(file.read(&mut bytes))?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    let after = io(file.metadata())?;
    if before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("export payload changed during hashing".into());
    }
    Ok(GitBundle {
        digest: format!("{:x}", hash.finalize()),
        size: before.len(),
        commit: String::new(),
    })
}

// Git owns these staging names and publishes its lock by renaming it to pending.
// Inspect one snapshot so publication cannot invalidate a later open of the name.
fn staging_leaf_alive(ledger: &File, leaf: &str) -> bool {
    let Ok(leaf) = name(leaf) else {
        return false;
    };
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: writable stat storage, held directory descriptor, validated name.
    // AT_SYMLINK_NOFOLLOW inspects links themselves without opening special files.
    let rc = unsafe {
        libc::fstatat(
            ledger.as_raw_fd(),
            leaf.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT);
    }
    test_hold("export-staging-observed");
    // SAFETY: successful fstatat initialized the complete metadata snapshot.
    let stat = unsafe { stat.assume_init() };
    stat.st_mode & libc::S_IFMT == libc::S_IFREG
        && stat.st_nlink == 1
        && stat.st_size >= 0
        && stat.st_size as u64 <= MAX_BUNDLE_BYTES
}

fn build(
    ledger: &File,
    key: &str,
    worktree: &File,
    repo_path: &str,
    receipt: &mut ExportReceipt,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<(), String> {
    if receipt.bundle.is_some() {
        return Ok(());
    }
    let stem = key.strip_suffix(".json").ok_or("invalid export key")?;
    let reference = format!("refs/codesense/exports/{stem}");
    // A stable private ref retains this exact commit across remote HEAD movement and GC.
    // Refs live in the shared clone: the short refs lock keeps this from
    // racing another workspace's prepare on Git's ref locks.
    {
        let _refs = locks::refs(repo_path, alive)?;
        git(
            worktree,
            &["update-ref", &reference, &receipt.intent.expected_head],
            alive,
            deadline,
        )?;
    }
    let pending = format!("{stem}.pending");
    let ready = format!("{stem}.bundle");
    let lock = format!("{pending}.lock");
    if !exists(ledger, &ready)? {
        // Only pre-publication staging belonging to this durable exact intent is reclaimed.
        unlink_payload(ledger, &pending)?;
        unlink_payload(ledger, &lock)?;
        headroom(ledger, 256 * 1024 * 1024)?;
        let path = payload_path(ledger, &pending)?;
        let mut args = vec!["bundle", "create", path.as_str(), reference.as_str()];
        let exclusion = receipt
            .intent
            .prerequisite
            .as_ref()
            .map(|value| format!("^{value}"));
        if let Some(exclusion) = &exclusion {
            args.push(exclusion);
        }
        // The output is disk-backed. The existing supervised Git runner bounds runtime;
        // monitor available space and output size throughout the operation as well.
        let bounded_alive = || {
            alive()
                && headroom(ledger, 0).is_ok()
                && [&pending, &lock]
                    .iter()
                    .all(|leaf| staging_leaf_alive(ledger, leaf))
        };
        git(worktree, &args, &bounded_alive, deadline)?;
        let file = payload(ledger, &pending)?;
        io(super::durable::sync(&file))?;
        let src = name(&pending)?;
        let dst = name(&ready)?;
        // SAFETY: names under the same held private ledger; this export holds its staging lock.
        if unsafe {
            libc::renameat(
                ledger.as_raw_fd(),
                src.as_ptr(),
                ledger.as_raw_fd(),
                dst.as_ptr(),
            )
        } != 0
        {
            return Err("export staging commit interrupted".into());
        }
        io(super::durable::sync(ledger))?;
    }
    let path = payload_path(ledger, &ready)?;
    git(worktree, &["bundle", "verify", &path], alive, deadline)?;
    let heads = git(worktree, &["bundle", "list-heads", &path], alive, deadline)?;
    if heads.lines().count() != 1
        || heads != format!("{} {}", receipt.intent.expected_head, reference)
    {
        return Err("export bundle does not contain the pinned commit".into());
    }
    let mut bundle = hash_payload(ledger, &ready, alive, deadline)?;
    bundle.commit = receipt.intent.expected_head.clone();
    receipt.bundle = Some(bundle);
    persist(ledger, key, receipt, false)
}

/// The commit and branch the owned, completed workspace's checkout is on. Only
/// names leave the host; the caller pins the commit in its export intent.
pub(crate) fn head(
    root: &Path,
    owner: u64,
    target: &codesense_pty_core::remote_files::FileTarget,
    alive: impl Fn() -> bool,
) -> Result<WorkspaceHead, String> {
    // Lock-free: HEAD is read by Git (atomic ref files), and no file Apply
    // moves it, so a long Apply or another workspace's prepare never delays it.
    let (_, _, workspace) = resolve_completed_target(root, owner, target, &alive)?;
    let worktree = open_directory(&workspace.intent.worktree_path, false)?;
    owner_directory(&worktree)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let commit = git(&worktree, &["rev-parse", "HEAD"], &alive, deadline)?;
    if !valid_commit(&commit) {
        return Err("managed worktree HEAD is not a commit".into());
    }
    // Empty on a detached HEAD.
    let branch = git(&worktree, &["branch", "--show-current"], &alive, deadline)?;
    let branch = (!branch.is_empty()).then_some(branch);
    if !alive() {
        return Err("export authority retired".into());
    }
    Ok(WorkspaceHead { commit, branch })
}

pub(crate) fn handle(
    root: &Path,
    owner: u64,
    request: ExportRequest,
    alive: impl Fn() -> bool,
) -> Result<ExportReply, String> {
    if uuid::Uuid::parse_str(&request.intent.request_id).is_err()
        || !valid_commit(&request.intent.expected_head)
        || request
            .intent
            .prerequisite
            .as_ref()
            .is_some_and(|value| !valid_commit(value) || value == &request.intent.expected_head)
    {
        return Err("invalid immutable export intent".into());
    }
    let (_, _, workspace) = resolve_completed_target(root, owner, &request.intent.target, &alive)?;
    let worktree = open_directory(&workspace.intent.worktree_path, false)?;
    owner_directory(&worktree)?;
    let private = io(File::open(root))?;
    let ledger = directory(&private, "remote-workspace-exports", true)?;
    owner_directory(&ledger)?;
    let key = format!(
        "{}.json",
        digest(
            format!(
                "{}:{}:{}",
                owner, request.intent.target.origin_key, request.intent.request_id
            )
            .as_bytes()
        )
    );
    // An export pins its commit in its own intent and stages its own bundle,
    // so it takes no workspace lock: only its own staging lock, so retries of
    // this one export never interleave.
    let _staging = locks::staging(&format!("export:{}:{key}", root.display()), &alive)?;
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    let mut receipt = if exists(&ledger, &key)? {
        let receipt: ExportReceipt =
            serde_json::from_slice(&read(&ledger, &key)?).map_err(|_| "export receipt invalid")?;
        if !same_intent(&receipt.intent, &request.intent) {
            return Err("export request belongs to another immutable intent".into());
        }
        receipt
    } else {
        if !matches!(request.operation, ExportOperation::Prepare) {
            return Err("export receipt unavailable; no download or cleanup authorized".into());
        }
        if git(&worktree, &["rev-parse", "HEAD"], &alive, deadline)? != request.intent.expected_head
        {
            return Err("managed worktree HEAD differs from expectedHead".into());
        }
        git(
            &worktree,
            &[
                "cat-file",
                "-e",
                &format!("{}^{{commit}}", request.intent.expected_head),
            ],
            &alive,
            deadline,
        )?;
        if let Some(base) = &request.intent.prerequisite {
            git(
                &worktree,
                &[
                    "merge-base",
                    "--is-ancestor",
                    base,
                    &request.intent.expected_head,
                ],
                &alive,
                deadline,
            )?;
        }
        let receipt = ExportReceipt {
            intent: request.intent.clone(),
            bundle: None,
            released: false,
        };
        persist(&ledger, &key, &receipt, true)?;
        receipt
    };
    if !alive() {
        return Err("export authority retired".into());
    }
    let leaf = format!("{}.bundle", key.trim_end_matches(".json"));
    let reply = match request.operation {
        ExportOperation::Prepare => {
            build(
                &ledger,
                &key,
                &worktree,
                &workspace.intent.repo_path,
                &mut receipt,
                &alive,
                deadline,
            )?;
            ExportReply::Prepared {
                descriptor: descriptor(&receipt)?,
            }
        }
        ExportOperation::Read { offset } => {
            let descriptor = descriptor(&receipt)?;
            if receipt.released || offset > descriptor.bundle.size {
                return Err("export payload released or offset outside pinned bundle".into());
            }
            let mut file = payload(&ledger, &leaf)?;
            if io(file.metadata())?.len() != descriptor.bundle.size {
                return Err("export payload length changed".into());
            }
            io(file.seek(SeekFrom::Start(offset)))?;
            let mut bytes =
                vec![0; (descriptor.bundle.size - offset).min(BUNDLE_CHUNK_BYTES as u64) as usize];
            io(file.read_exact(&mut bytes))?;
            ExportReply::Chunk { offset, bytes }
        }
        ExportOperation::Release { bundle } => {
            if descriptor(&receipt)?.bundle != bundle {
                return Err("verified import acknowledgement does not match export".into());
            }
            // Retain settlement before deleting bytes; retries need only the small receipt.
            if !receipt.released {
                receipt.released = true;
                persist(&ledger, &key, &receipt, false)?;
            }
            unlink_payload(&ledger, &leaf)?;
            let reference = format!("refs/codesense/exports/{}", key.trim_end_matches(".json"));
            let current = git(
                &worktree,
                &["for-each-ref", "--format=%(objectname)", &reference],
                &alive,
                deadline,
            )?;
            if !current.is_empty() {
                if current != receipt.intent.expected_head {
                    return Err("export retention ref changed; cleanup refused".into());
                }
                // Deleting the retention ref writes the shared clone's refs.
                let _refs = locks::refs(&workspace.intent.repo_path, &alive)?;
                git(
                    &worktree,
                    &[
                        "update-ref",
                        "-d",
                        &reference,
                        &receipt.intent.expected_head,
                    ],
                    &alive,
                    deadline,
                )?;
            }
            ExportReply::Released {
                descriptor: descriptor(&receipt)?,
            }
        }
    };
    if !alive() {
        return Err("export result authority retired; reconcile original request".into());
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesense_pty_core::remote_files::FileTarget;
    #[test]
    fn staging_liveness_survives_git_lock_publication() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = File::open(temp.path()).unwrap();
        let pending = temp.path().join("export.pending");
        let lock = temp.path().join("export.pending.lock");
        std::fs::write(&lock, b"completed Git bundle").unwrap();
        let published = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = published.clone();
        let published_pending = pending.clone();
        let published_lock = lock.clone();
        arm_test_hold("export-staging-observed", move || {
            std::fs::rename(published_lock, published_pending).unwrap();
            observed.set(true);
        });

        let alive = ["export.pending", "export.pending.lock"]
            .iter()
            .all(|leaf| staging_leaf_alive(&ledger, leaf));

        assert!(
            published.get(),
            "publication must occur at the observation seam"
        );
        assert!(!lock.exists());
        assert_eq!(std::fs::read(&pending).unwrap(), b"completed Git bundle");
        assert!(alive, "normal Git lock publication must preserve liveness");
    }

    #[test]
    fn staging_liveness_accepts_absence_and_bounded_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = File::open(temp.path()).unwrap();
        assert!(staging_leaf_alive(&ledger, "missing"));
        let file = File::create(temp.path().join("regular")).unwrap();
        assert!(staging_leaf_alive(&ledger, "regular"));
        file.set_len(MAX_BUNDLE_BYTES).unwrap();
        assert!(staging_leaf_alive(&ledger, "regular"));
        file.set_len(MAX_BUNDLE_BYTES + 1).unwrap();
        assert!(!staging_leaf_alive(&ledger, "regular"));
    }

    #[test]
    fn staging_liveness_rejects_nonregular_and_linked_files() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = File::open(temp.path()).unwrap();
        std::fs::create_dir(temp.path().join("directory")).unwrap();
        let fifo = name("fifo").unwrap();
        // SAFETY: held directory descriptor and a validated, terminated leaf name.
        assert_eq!(
            unsafe { libc::mkfifoat(ledger.as_raw_fd(), fifo.as_ptr(), 0o600) },
            0
        );
        let _socket = std::os::unix::net::UnixListener::bind(temp.path().join("socket")).unwrap();
        std::fs::write(temp.path().join("regular"), b"bundle").unwrap();
        std::fs::hard_link(temp.path().join("regular"), temp.path().join("hardlink")).unwrap();
        for (target, leaf) in [
            ("regular", "symlink"),
            ("missing", "dangling-symlink"),
            ("fifo", "fifo-symlink"),
        ] {
            std::os::unix::fs::symlink(target, temp.path().join(leaf)).unwrap();
        }
        for leaf in [
            "directory",
            "fifo",
            "socket",
            "regular",
            "hardlink",
            "symlink",
            "dangling-symlink",
            "fifo-symlink",
        ] {
            assert!(!staging_leaf_alive(&ledger, leaf), "accepted {leaf}");
        }
    }

    #[test]
    fn staging_liveness_rejects_filesystem_errors_other_than_absence() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = File::open(temp.path()).unwrap();
        let regular = File::create(temp.path().join("regular")).unwrap();
        // A regular-file parent produces ENOTDIR, independently of user privileges.
        assert!(!staging_leaf_alive(&regular, "child"));
        // A single oversized component produces ENAMETOOLONG, not ENOENT.
        assert!(!staging_leaf_alive(&ledger, &"x".repeat(4096)));
    }

    fn commit(path: &str, text: &str) -> String {
        std::fs::write(Path::new(path).join("committed.txt"), text).unwrap();
        for args in [
            vec!["add", "committed.txt"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                text,
            ],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap()
                .status
                .success());
        }
        String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(path)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .into()
    }
    fn setup() -> (tempfile::TempDir, PrepareWorkspace, ExportIntent) {
        let (temp, request) = super::super::tests::fixture();
        let prepared = super::super::tests::prepared(temp.path(), request.clone());
        assert_eq!(prepared.status, "prepared");
        let expected_head = commit(&request.worktree_path, "chosen");
        let intent = ExportIntent {
            target: FileTarget {
                owner_id: 41,
                origin_key: request.origin_key.clone(),
                request_id: request.request_id.clone(),
                handle: prepared.handle.unwrap(),
            },
            request_id: uuid::Uuid::new_v4().to_string(),
            expected_head,
            prerequisite: Some(request.bundle.commit.clone()),
        };
        (temp, request, intent)
    }
    fn call(
        root: &Path,
        intent: &ExportIntent,
        operation: ExportOperation,
    ) -> Result<ExportReply, String> {
        handle(
            root,
            41,
            ExportRequest {
                intent: intent.clone(),
                operation,
            },
            || true,
        )
    }
    fn prepare(root: &Path, intent: &ExportIntent) -> ExportDescriptor {
        match call(root, intent, ExportOperation::Prepare).unwrap() {
            ExportReply::Prepared { descriptor } => descriptor,
            reply => panic!("{reply:?}"),
        }
    }
    #[test]
    fn pinned_export_survives_head_advance_and_release_with_exact_incremental_objects() {
        let (temp, workspace, intent) = setup();
        std::fs::write(
            Path::new(&workspace.worktree_path).join("dirty-secret.txt"),
            "not committed",
        )
        .unwrap();
        let descriptor = prepare(temp.path(), &intent);
        commit(&workspace.worktree_path, "later");
        assert_eq!(prepare(temp.path(), &intent), descriptor);
        let mut bytes = Vec::new();
        while bytes.len() < descriptor.bundle.size as usize {
            match call(
                temp.path(),
                &intent,
                ExportOperation::Read {
                    offset: bytes.len() as u64,
                },
            )
            .unwrap()
            {
                ExportReply::Chunk {
                    offset,
                    bytes: chunk,
                } => {
                    assert_eq!(offset, bytes.len() as u64);
                    assert!(chunk.len() <= BUNDLE_CHUNK_BYTES);
                    bytes.extend(chunk);
                }
                reply => panic!("{reply:?}"),
            }
        }
        assert_eq!(digest(&bytes), descriptor.bundle.digest);
        let bundle = temp.path().join("return.bundle");
        std::fs::write(&bundle, bytes).unwrap();
        let source = temp.path().join("source-offline");
        assert!(Command::new("git")
            .args(["bundle", "verify", bundle.to_str().unwrap()])
            .current_dir(&source)
            .output()
            .unwrap()
            .status
            .success());
        assert!(Command::new("git")
            .args(["fetch", bundle.to_str().unwrap(), &intent.expected_head])
            .current_dir(&source)
            .output()
            .unwrap()
            .status
            .success());
        let tree = Command::new("git")
            .args(["ls-tree", "-r", &intent.expected_head])
            .current_dir(&source)
            .output()
            .unwrap();
        assert!(!String::from_utf8(tree.stdout)
            .unwrap()
            .contains("dirty-secret"));
        call(
            temp.path(),
            &intent,
            ExportOperation::Release {
                bundle: descriptor.bundle.clone(),
            },
        )
        .unwrap();
        let repeated = prepare(temp.path(), &intent);
        assert!(repeated.released);
        assert_eq!(repeated.bundle, descriptor.bundle);
        call(
            temp.path(),
            &intent,
            ExportOperation::Release {
                bundle: descriptor.bundle,
            },
        )
        .unwrap();
        assert!(call(temp.path(), &intent, ExportOperation::Read { offset: 0 }).is_err());
        assert_eq!(
            std::fs::read_dir(temp.path().join("remote-workspace-exports"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".bundle"))
                .count(),
            0
        );
    }
    #[test]
    fn head_names_the_checkout_commit_and_branch_only_for_its_owner() {
        let (temp, workspace, intent) = setup();
        let head = head(temp.path(), 41, &intent.target, || true).unwrap();
        assert_eq!(head.commit, intent.expected_head);
        assert_eq!(head.branch.as_deref(), Some(workspace.branch.as_str()));
        assert!(super::head(temp.path(), 42, &intent.target, || true).is_err());
        assert!(super::head(temp.path(), 41, &intent.target, || false).is_err());
        let mut other = intent.target.clone();
        other.handle = format!("remote-workspace-{}", "0".repeat(64));
        assert!(super::head(temp.path(), 41, &other, || true).is_err());
        assert!(Command::new("git")
            .args(["checkout", "--detach", "HEAD"])
            .current_dir(&workspace.worktree_path)
            .output()
            .unwrap()
            .status
            .success());
        // A checkout off its prepared branch is no longer the managed workspace.
        assert!(super::head(temp.path(), 41, &intent.target, || true).is_err());
    }
    #[test]
    fn interrupted_export_rebuilds_only_reserved_commit_and_rejects_changed_intent() {
        let (temp, workspace, intent) = setup();
        let ledger = directory(
            &File::open(temp.path()).unwrap(),
            "remote-workspace-exports",
            true,
        )
        .unwrap();
        let stem =
            digest(format!("41:{}:{}", intent.target.origin_key, intent.request_id).as_bytes());
        let key = format!("{stem}.json");
        persist(
            &ledger,
            &key,
            &ExportReceipt {
                intent: intent.clone(),
                bundle: None,
                released: false,
            },
            true,
        )
        .unwrap();
        write_new(
            &ledger,
            &format!("{stem}.pending"),
            b"interrupted incomplete bundle",
        )
        .unwrap();
        write_new(
            &ledger,
            &format!("{stem}.pending.lock"),
            b"interrupted Git lock staging",
        )
        .unwrap();
        commit(&workspace.worktree_path, "later");
        assert_eq!(
            prepare(temp.path(), &intent).bundle.commit,
            intent.expected_head
        );
        let mut changed = intent.clone();
        changed.expected_head = workspace.bundle.commit.clone();
        assert!(call(temp.path(), &changed, ExportOperation::Prepare).is_err());
        changed = intent.clone();
        changed.request_id = uuid::Uuid::new_v4().to_string();
        assert!(
            call(temp.path(), &changed, ExportOperation::Prepare).is_err(),
            "first selection checks HEAD"
        );
        changed = intent.clone();
        changed.target.owner_id += 1;
        assert!(call(temp.path(), &changed, ExportOperation::Prepare).is_err());
        assert!(handle(
            temp.path(),
            41,
            ExportRequest {
                intent,
                operation: ExportOperation::Prepare
            },
            || false
        )
        .is_err());
    }
}
