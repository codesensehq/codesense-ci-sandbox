// SPDX-License-Identifier: MIT
//! The server half of the remote perf harness
//! (`scripts/perf/remote-explicit-sync.sh`). One batch Begin, its chunks,
//! Seal, Apply and Finish against a real prepared workspace on whatever disk
//! `TMPDIR` names, timed per step with the fsync count of each. Ignored in a
//! plain `cargo test`; the harness runs the built test binary on the machine
//! being measured.
//!
//! Shapes (environment):
//! - `XS26_BENCH_SOURCE=<dir>`: every regular file under it, as a real team
//!   context; otherwise `XS26_BENCH_FILES` text files (default 2,000) of
//!   `XS26_BENCH_TOTAL_BYTES` together (default 40 MB) in
//!   `XS26_BENCH_DIRS` directories (default 100).
//! - `XS26_BENCH_CODEC=identity|gzip|zstd` (default identity).
use super::tests::call;
use super::*;
use crate::remote_workspace::tests::{fixture, prepared};
use codesense_pty_core::remote_files::Expected;

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Markdown-like text: words from a fixed vocabulary in short lines, so a
/// compressor sees what agent context looks like (not random bytes).
fn text(seed: u64, length: usize) -> Vec<u8> {
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
        let word = WORDS[(next() % WORDS.len() as u64) as usize];
        out.extend_from_slice(word.as_bytes());
        if next() % 9 == 0 {
            out.extend_from_slice(format!(" `{}`", next() % 100_000).as_bytes());
        }
    }
    out.truncate(length);
    out
}

fn files() -> Vec<(String, Vec<u8>)> {
    if let Ok(source) = std::env::var("XS26_BENCH_SOURCE") {
        let root = Path::new(&source);
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    stack.push(entry.path());
                } else if kind.is_file() {
                    let bytes = std::fs::read(entry.path()).unwrap();
                    if bytes.len() > codesense_pty_core::remote_files::MAX_FILE_BYTES {
                        continue;
                    }
                    let relative = entry.path().strip_prefix(root).unwrap().to_owned();
                    found.push((format!("ctx/{}", relative.to_string_lossy()), bytes));
                }
            }
        }
        found.sort();
        return found;
    }
    let count = env_number("XS26_BENCH_FILES", 2_000);
    let total = env_number("XS26_BENCH_TOTAL_BYTES", 40_000_000);
    let directories = env_number("XS26_BENCH_DIRS", 100).max(1);
    (0..count)
        .map(|index| {
            (
                format!("ctx/d{:03}/f{index:05}.md", index % directories),
                text(index + 1, (total / count) as usize),
            )
        })
        .collect()
}

struct Step {
    name: &'static str,
    millis: u128,
    syncs: u64,
    flushes: u64,
    sync_millis: u128,
}
fn timed(steps: &mut Vec<Step>, name: &'static str, run: impl FnOnce()) {
    let started = Instant::now();
    let meter = durable::Meter::start();
    run();
    let syncs = meter.read();
    steps.push(Step {
        name,
        millis: started.elapsed().as_millis(),
        syncs: syncs.total(),
        flushes: syncs.flushes,
        sync_millis: syncs.spent.as_millis(),
    });
}

fn request_for(
    target: &FileTarget,
    members: &[(String, Vec<u8>)],
    expected: &dyn Fn(&str) -> Expected,
    codec: Codec,
) -> (BatchRequest, Vec<u8>) {
    let mut raw = Vec::new();
    let mut items = Vec::new();
    for (index, (path, bytes)) in members.iter().enumerate() {
        items.push(BatchItem {
            index,
            operation_id: uuid::Uuid::new_v4().to_string(),
            path: path.clone(),
            offset: raw.len() as u64,
            length: bytes.len() as u64,
            hash: *blake3::hash(bytes).as_bytes(),
            mode: 0o644,
            expected: expected(path),
            dependencies: vec![],
        });
        raw.extend_from_slice(bytes);
    }
    let encoded = match codec {
        Codec::Identity => raw.clone(),
        Codec::Gzip => {
            let mut encoder = flate2::GzBuilder::new()
                .mtime(0)
                .write(Vec::new(), flate2::Compression::new(6));
            encoder.write_all(&raw).unwrap();
            encoder.finish().unwrap()
        }
        Codec::Zstd => super::tests::zstd_encode(&raw),
    };
    let manifest = BatchManifest {
        request_id: uuid::Uuid::new_v4().to_string(),
        generation: 1,
        codec,
        raw_length: raw.len() as u64,
        encoded_length: encoded.len() as u64,
        raw_digest: *blake3::hash(&raw).as_bytes(),
        encoded_digest: *blake3::hash(&encoded).as_bytes(),
        items,
        removes: vec![],
        chmods: vec![],
        ensure_dirs: vec![],
        exclude_block: None,
    };
    let request = BatchRequest {
        version: VERSION,
        target: target.clone(),
        batch_id: uuid::Uuid::new_v4().to_string(),
        manifest_digest: manifest.digest().unwrap(),
        operation: BatchOperation::Begin { manifest },
    };
    (request, encoded)
}

fn run_batch(root: &Path, request: &BatchRequest, encoded: &[u8], steps: &mut Vec<Step>) {
    timed(steps, "begin", || {
        call(root, request, request.operation.clone()).unwrap();
    });
    timed(steps, "upload", || {
        for (index, chunk) in encoded.chunks(CHUNK_BYTES).enumerate() {
            call(
                root,
                request,
                BatchOperation::Chunk {
                    offset: (index * CHUNK_BYTES) as u64,
                    bytes: chunk.to_vec(),
                },
            )
            .unwrap();
        }
    });
    timed(steps, "seal", || {
        assert_eq!(
            call(root, request, BatchOperation::Seal).unwrap().phase,
            TransferPhase::Verified
        );
    });
    timed(steps, "apply", || {
        let status = call(root, request, BatchOperation::Apply).unwrap();
        assert!(
            status.items.iter().all(|item| successful(&item.outcome)),
            "every member applies"
        );
    });
    timed(steps, "finish", || {
        call(
            root,
            request,
            BatchOperation::Finish {
                disposition: FinishDisposition::Settled,
            },
        )
        .unwrap();
    });
}

fn report(shape: &str, files: usize, raw: usize, encoded: usize, steps: &[Step]) -> String {
    let steps: Vec<String> = steps
        .iter()
        .map(|step| {
            format!(
                "\"{}\":{{\"ms\":{},\"fsyncs\":{},\"flushes\":{},\"fsync_ms\":{}}}",
                step.name, step.millis, step.syncs, step.flushes, step.sync_millis
            )
        })
        .collect();
    format!(
        "XS26_BENCH {{\"shape\":\"{shape}\",\"files\":{files},\"raw_bytes\":{raw},\"encoded_bytes\":{encoded},{}}}",
        steps.join(",")
    )
}

#[test]
#[ignore = "SW-XS-26 perf harness: run by scripts/perf/remote-explicit-sync.sh"]
fn xs26_apply_bench() {
    let codec = match std::env::var("XS26_BENCH_CODEC").as_deref() {
        Ok("gzip") => Codec::Gzip,
        Ok("zstd") => Codec::Zstd,
        _ => Codec::Identity,
    };
    let (root, setup) = fixture();
    let prepared = prepared(root.path(), setup.clone());
    assert_eq!(prepared.status, "prepared", "{prepared:?}");
    let target = FileTarget {
        owner_id: 41,
        origin_key: setup.origin_key,
        request_id: setup.request_id,
        handle: prepared.handle.unwrap(),
    };
    let members = files();
    let raw: usize = members.iter().map(|(_, bytes)| bytes.len()).sum();
    // Cold: every file new.
    let (request, encoded) = request_for(&target, &members, &|_| Expected::Absent, codec);
    let mut steps = Vec::new();
    run_batch(root.path(), &request, &encoded, &mut steps);
    println!(
        "{}",
        report("cold", members.len(), raw, encoded.len(), &steps)
    );
    // A re-sent Apply of the settled batch's twin: nothing left to write.
    let mut steps = Vec::new();
    timed(&mut steps, "status", || {
        call(root.path(), &request, BatchOperation::Status).unwrap();
    });
    println!("{}", report("resend-status", members.len(), raw, 0, &steps));
    // Five edits.
    let edited: Vec<(String, Vec<u8>)> = members
        .iter()
        .take(5)
        .map(|(path, bytes)| {
            let mut next = bytes.clone();
            next.extend_from_slice(b"\nedited\n");
            (path.clone(), next)
        })
        .collect();
    let previous: std::collections::HashMap<&str, [u8; 32]> = members
        .iter()
        .take(5)
        .map(|(path, bytes)| (path.as_str(), *blake3::hash(bytes).as_bytes()))
        .collect();
    let (request, encoded) = request_for(
        &target,
        &edited,
        &|path| Expected::Hash {
            hash: previous[path],
        },
        codec,
    );
    let mut steps = Vec::new();
    run_batch(root.path(), &request, &encoded, &mut steps);
    let raw: usize = edited.iter().map(|(_, bytes)| bytes.len()).sum();
    println!(
        "{}",
        report("edit5", edited.len(), raw, encoded.len(), &steps)
    );
}
