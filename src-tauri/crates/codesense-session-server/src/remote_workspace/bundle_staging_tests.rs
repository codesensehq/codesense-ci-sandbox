// SPDX-License-Identifier: MIT
//! The pipelined upload against server staging, on a virtual clock.
use super::*;
use codesense_pty_core::remote_workspace::pipeline::{send_missing, ChunkLink};
use codesense_pty_core::remote_workspace::UPLOAD_WINDOW;
use std::cell::{Cell, RefCell};
use std::collections::BinaryHeap;

/// An in-memory store that counts flushes.
#[derive(Default)]
struct CountingStore {
    data: RefCell<Vec<u8>>,
    records: RefCell<HashMap<u32, [u8; 32]>>,
    syncs: Cell<usize>,
}

impl ChunkStore for CountingStore {
    fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), String> {
        let mut data = self.data.borrow_mut();
        let end = offset as usize + bytes.len();
        if data.len() < end {
            data.resize(end, 0);
        }
        data[offset as usize..end].copy_from_slice(bytes);
        Ok(())
    }
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let data = self.data.borrow();
        let mut out = vec![0; len];
        let start = (offset as usize).min(data.len());
        let end = (offset as usize + len).min(data.len());
        out[..end - start].copy_from_slice(&data[start..end]);
        Ok(out)
    }
    fn write_record(&self, index: u32, hash: &[u8; 32]) -> Result<(), String> {
        self.records.borrow_mut().insert(index, *hash);
        Ok(())
    }
    fn read_record(&self, index: u32) -> Result<[u8; 32], String> {
        Ok(self
            .records
            .borrow()
            .get(&index)
            .copied()
            .unwrap_or([0; 32]))
    }
    fn sync(&self) -> Result<(), String> {
        self.syncs.set(self.syncs.get() + 1);
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn bundle(size: usize) -> Vec<u8> {
    let mut random = 17u32;
    (0..size)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect()
}

fn read_from(bytes: &[u8]) -> impl FnMut(u32) -> Result<Vec<u8>, String> + '_ {
    move |index| {
        let (offset, len) = chunk_range(bytes.len() as u64, index).unwrap();
        Ok(bytes[offset as usize..offset as usize + len].to_vec())
    }
}

/// A fake link on a virtual clock (microseconds): each chunk is serialized
/// onto a shared link at `bytes_per_us`, arrives `one_way_us` later, is staged
/// by the real staging code, and its ack is back another `one_way_us` (plus a
/// per-chunk server jitter, so acks come back out of order).
struct VirtualLink<'a> {
    key: String,
    store: &'a dyn ChunkStore,
    size: u64,
    now: u64,
    link_free: u64,
    one_way_us: u64,
    bytes_per_us: u64,
    acks: BinaryHeap<std::cmp::Reverse<(u64, u32)>>,
    in_flight: usize,
    max_in_flight: usize,
    order: Vec<u32>,
    submitted: Vec<u32>,
}

impl<'a> VirtualLink<'a> {
    fn new(key: &str, store: &'a dyn ChunkStore, size: u64) -> Self {
        Self {
            key: key.into(),
            store,
            size,
            now: 0,
            link_free: 0,
            one_way_us: 50_000,
            // 100 Mbit/s.
            bytes_per_us: 12,
            acks: BinaryHeap::new(),
            in_flight: 0,
            max_in_flight: 0,
            order: Vec::new(),
            submitted: Vec::new(),
        }
    }
}

impl ChunkLink for VirtualLink<'_> {
    fn submit(&mut self, index: u32, bytes: Vec<u8>) -> Result<(), String> {
        let start = self.now.max(self.link_free);
        self.link_free = start + bytes.len() as u64 / self.bytes_per_us;
        let arrive = self.link_free + self.one_way_us;
        stage(
            &self.key,
            self.store,
            self.size,
            index,
            &hex(&bytes),
            &bytes,
        )?;
        let jitter = (index as u64 * 7_919) % 40_000;
        self.acks.push(std::cmp::Reverse((
            arrive + jitter + self.one_way_us,
            index,
        )));
        self.in_flight += 1;
        self.max_in_flight = self.max_in_flight.max(self.in_flight);
        self.submitted.push(index);
        Ok(())
    }
    fn next_ack(&mut self) -> Result<u32, String> {
        let std::cmp::Reverse((at, index)) = self.acks.pop().ok_or("no chunk in flight")?;
        self.now = self.now.max(at);
        self.in_flight -= 1;
        self.order.push(index);
        Ok(index)
    }
}

/// A 40 MB bundle at 50 ms one-way latency: at most 8 chunks in
/// flight, acks out of order, simulated time ≤ 0.3 × the sequential upload,
/// and exactly one flush for the whole upload.
#[test]
fn xs9_pipelined_upload_is_windowed_out_of_order_and_flushes_once() {
    let bytes = bundle(40 * 1024 * 1024);
    let size = bytes.len() as u64;

    let sequential_store = CountingStore::default();
    let mut sequential = VirtualLink::new("xs9-seq", &sequential_store, size);
    let mut received = ReceivedChunks::for_size(size);
    send_missing(
        &mut sequential,
        &mut received,
        size,
        PIPELINED_CHUNK_BYTES,
        1,
        &mut read_from(&bytes),
        &mut |_, _| {},
    )
    .unwrap();

    let store = CountingStore::default();
    let mut link = VirtualLink::new("xs9-pipelined", &store, size);
    let mut received = ReceivedChunks::for_size(size);
    let mut reported = Vec::new();
    let sent = send_missing(
        &mut link,
        &mut received,
        size,
        PIPELINED_CHUNK_BYTES,
        UPLOAD_WINDOW,
        &mut read_from(&bytes),
        &mut |sent, total| reported.push((sent, total)),
    )
    .unwrap();
    seal("xs9-pipelined", &store, size).unwrap();
    seal("xs9-pipelined", &store, size).unwrap();

    assert_eq!(sent.chunks, 160);
    assert_eq!(
        link.max_in_flight, 8,
        "never more than the window in flight"
    );
    assert_eq!(sent.max_in_flight, 8);
    assert!(
        link.order.windows(2).any(|pair| pair[1] < pair[0]),
        "acks came back out of order"
    );
    let ratio = link.now as f64 / sequential.now as f64;
    println!(
        "xs9 AC1: pipelined {} ms vs sequential {} ms = {ratio:.3}",
        link.now / 1000,
        sequential.now / 1000
    );
    assert!(
        ratio <= 0.3,
        "pipelined {}us vs sequential {}us ({ratio:.3})",
        link.now,
        sequential.now
    );
    assert_eq!(store.syncs.get(), 1, "one flush per upload, at seal");
    assert_eq!(*store.data.borrow(), bytes);
    assert_eq!(reported.last(), Some(&(size, size)));
    assert!(reported.windows(2).all(|pair| pair[0].0 <= pair[1].0));
}

/// A seal before every chunk is held is refused and flushes nothing.
#[test]
fn xs9_seal_refuses_an_incomplete_upload() {
    let bytes = bundle(3 * PIPELINED_CHUNK_BYTES + 10);
    let size = bytes.len() as u64;
    let store = CountingStore::default();
    let mut read = read_from(&bytes);
    stage(
        "xs9-incomplete",
        &store,
        size,
        0,
        &hex(&read(0).unwrap()),
        &read(0).unwrap(),
    )
    .unwrap();
    assert!(seal("xs9-incomplete", &store, size)
        .unwrap_err()
        .contains("incomplete"));
    assert_eq!(store.syncs.get(), 0);
    // A chunk with other bytes than its hash, or a changed retry, is refused.
    let other = read(1).unwrap();
    assert!(stage("xs9-incomplete", &store, size, 1, &hex(b"x"), &other).is_err());
    let mut changed = read(0).unwrap();
    changed[0] ^= 1;
    assert!(
        stage("xs9-incomplete", &store, size, 0, &hex(&changed), &changed)
            .unwrap_err()
            .contains("changed prior bytes")
    );
}

/// An interrupted upload with holes resumes sending only the
/// chunks the machine lacks.
#[test]
fn xs9_resume_sends_only_the_missing_indices() {
    let bytes = bundle(12 * PIPELINED_CHUNK_BYTES + 99);
    let size = bytes.len() as u64;
    let store = CountingStore::default();
    let mut read = read_from(&bytes);
    let held = [0u32, 2, 3, 7, 12];
    for index in held {
        let chunk = read(index).unwrap();
        stage("xs9-resume", &store, size, index, &hex(&chunk), &chunk).unwrap();
    }
    let mut received = state("xs9-resume", &store, size).unwrap().received;
    let missing = received.missing();
    assert_eq!(missing, vec![1, 4, 5, 6, 8, 9, 10, 11]);
    let mut link = VirtualLink::new("xs9-resume", &store, size);
    send_missing(
        &mut link,
        &mut received,
        size,
        PIPELINED_CHUNK_BYTES,
        UPLOAD_WINDOW,
        &mut read,
        &mut |_, _| {},
    )
    .unwrap();
    let mut submitted = link.submitted.clone();
    submitted.sort_unstable();
    assert_eq!(submitted, missing, "held chunks are never sent again");
    seal("xs9-resume", &store, size).unwrap();
    assert_eq!(*store.data.borrow(), bytes);
}

/// After a server restart the held set comes from the per-chunk hashes
/// on disk; a chunk whose bytes or record did not survive is sent again.
#[test]
fn xs9_restart_recomputes_the_held_chunks_from_disk() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = bundle(6 * PIPELINED_CHUNK_BYTES + 5);
    let size = bytes.len() as u64;
    let open = |name: &str| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(temp.path().join(name))
            .unwrap()
    };
    let store = FileStore {
        data: open("data"),
        records: open("records"),
    };
    store.data.set_len(size).unwrap();
    let key = format!("xs9-restart:{}", temp.path().display());
    let mut read = read_from(&bytes);
    for index in 0..5 {
        let chunk = read(index).unwrap();
        stage(&key, &store, size, index, &hex(&chunk), &chunk).unwrap();
    }
    // Acked before the crash: 0..5. The page cache lost chunk 1's bytes and
    // chunk 3's record.
    use std::os::unix::fs::FileExt;
    store
        .data
        .write_all_at(
            &vec![0; PIPELINED_CHUNK_BYTES],
            PIPELINED_CHUNK_BYTES as u64,
        )
        .unwrap();
    store.records.write_all_at(&[0; 32], 3 * 32).unwrap();
    assert!(
        state(&key, &store, size).unwrap().received.contains(1),
        "the same process still trusts its own staging"
    );
    forget_all();
    let after = state(&key, &store, size).unwrap();
    assert_eq!(after.received.missing(), vec![1, 3, 5, 6]);
    assert!(!after.sealed);
    let mut received = after.received;
    let mut link = VirtualLink::new(&key, &store, size);
    send_missing(
        &mut link,
        &mut received,
        size,
        PIPELINED_CHUNK_BYTES,
        UPLOAD_WINDOW,
        &mut read,
        &mut |_, _| {},
    )
    .unwrap();
    let mut submitted = link.submitted.clone();
    submitted.sort_unstable();
    assert_eq!(submitted, vec![1, 3, 5, 6]);
    seal(&key, &store, size).unwrap();
    assert_eq!(std::fs::read(temp.path().join("data")).unwrap(), bytes);
}

/// A digest mismatch drops the in-memory state and restarts
/// the upload instead of wedging until the server restarts.
#[test]
fn xs9_a_digest_mismatch_restarts_the_upload_instead_of_wedging() {
    let bytes = bundle(3 * PIPELINED_CHUNK_BYTES);
    let size = bytes.len() as u64;
    let store = CountingStore::default();
    let mut read = read_from(&bytes);
    for index in 0..3 {
        let chunk = read(index).unwrap();
        stage("xs9-mismatch", &store, size, index, &hex(&chunk), &chunk).unwrap();
    }
    let wrong = format!("{:x}", Sha256::digest(b"another bundle"));
    assert!(verify("xs9-mismatch", &store, size, &wrong)
        .unwrap_err()
        .contains("digest mismatch"));
    let after = state("xs9-mismatch", &store, size).unwrap();
    assert_eq!(
        after.received.missing(),
        vec![0, 1, 2],
        "every chunk is sent again"
    );
    assert!(!after.sealed && !after.verified);
    // Resent, it verifies against the right digest.
    let mut received = after.received;
    let mut link = VirtualLink::new("xs9-mismatch", &store, size);
    send_missing(
        &mut link,
        &mut received,
        size,
        PIPELINED_CHUNK_BYTES,
        UPLOAD_WINDOW,
        &mut read,
        &mut |_, _| {},
    )
    .unwrap();
    verify("xs9-mismatch", &store, size, &hex(&bytes)).unwrap();
}
