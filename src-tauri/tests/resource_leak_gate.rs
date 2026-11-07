// SPDX-License-Identifier: MIT
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

const WARMUPS: usize = 2;
const MEASURED_ITERATIONS: usize = 5;
const MAX_PHASE_ITERATIONS: usize = 100;
// Warmup makes both exercised lifecycles byte-for-byte stable on the measured
// retained counters. Keep the gate exact; total/peak allocation traffic remains
// diagnostic and is intentionally not bounded.
const RETAINED_BYTE_DELTA: usize = 0;
const RETAINED_BLOCK_DELTA: usize = 0;
const ALLOWED_FD_DELTA: usize = 0;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Verdict {
    Pass,
    Fail,
    CouldNotMeasure,
}

#[derive(Debug, Serialize)]
struct HeapSample {
    iteration: usize,
    current_bytes: usize,
    current_blocks: usize,
    total_bytes: u64,
    total_blocks: u64,
    max_bytes: usize,
    max_blocks: usize,
}

#[derive(Debug, Serialize)]
struct FdSample {
    iteration: usize,
    count: usize,
    unexpected: Vec<FdIdentity>,
}

#[derive(Debug, Serialize)]
struct ScenarioReport {
    scenario: String,
    verdict: Verdict,
    baseline_heap_bytes: usize,
    baseline_heap_blocks: usize,
    baseline_fds: usize,
    allowed_heap_byte_delta: usize,
    allowed_heap_block_delta: usize,
    allowed_fd_delta: usize,
    heap_trend_rule: &'static str,
    warmups: usize,
    measured_iterations: usize,
    samples: Vec<HeapSample>,
    fd_samples: Vec<FdSample>,
    final_fds: Option<usize>,
    leaked_fds: Vec<FdIdentity>,
    failure_resource: Option<&'static str>,
    process: Option<ProcessEvidence>,
    detail: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct FdIdentity {
    fd: i32,
    fd_type: u32,
}

#[derive(Debug, Serialize)]
struct ProcessEvidence {
    pid: u32,
    process_group: i32,
    started_seconds: Option<u64>,
    started_microseconds: Option<u64>,
    deadline_ms: u64,
    elapsed_ms: u128,
    alive_at_deadline: bool,
    reaped: bool,
}

#[derive(Debug)]
struct Config {
    artifact_dir: PathBuf,
    scenario: String,
    inject: Option<String>,
    warmups: usize,
    measured_iterations: usize,
}

impl Config {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let mut artifact_dir = None;
        let mut scenario = "all".to_string();
        let mut inject = None;
        let mut warmups = WARMUPS;
        let mut measured_iterations = MEASURED_ITERATIONS;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--artifact-dir" => artifact_dir = args.next().map(PathBuf::from),
                "--scenario" => scenario = args.next().ok_or("--scenario requires a value")?,
                "--inject" => inject = Some(args.next().ok_or("--inject requires a value")?),
                "--warmups" => {
                    warmups = parse_count("--warmups", args.next(), 1)?;
                }
                "--iterations" => {
                    measured_iterations = parse_count("--iterations", args.next(), 2)?;
                }
                _ => return Err(format!("unknown argument: {arg}")),
            }
        }
        if let Some(value) = inject.as_deref() {
            if !matches!(value, "heap" | "fd" | "child") {
                return Err(format!("unknown injector: {value}"));
            }
        }
        Ok(Self {
            artifact_dir: artifact_dir.ok_or("--artifact-dir is required")?,
            scenario,
            inject,
            warmups,
            measured_iterations,
        })
    }
}

fn parse_count(option: &str, value: Option<String>, minimum: usize) -> Result<usize, String> {
    let value = value.ok_or_else(|| format!("{option} requires a value"))?;
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{option} must be an integer >= {minimum}"))?;
    (minimum..=MAX_PHASE_ITERATIONS)
        .contains(&parsed)
        .then_some(parsed)
        .ok_or_else(|| {
            format!("{option} must be an integer from {minimum} through {MAX_PHASE_ITERATIONS}")
        })
}

fn main() {
    let config = Config::parse().unwrap_or_else(|error| {
        eprintln!("COULD_NOT_MEASURE: {error}");
        std::process::exit(2);
    });
    std::fs::create_dir_all(&config.artifact_dir).unwrap_or_else(|error| {
        eprintln!("COULD_NOT_MEASURE: create artifact directory: {error}");
        std::process::exit(2);
    });
    let _artifact_lock = ArtifactLock::acquire(&config.artifact_dir).unwrap_or_else(|error| {
        eprintln!("COULD_NOT_MEASURE: {error}");
        std::process::exit(2);
    });
    let profile_path = config.artifact_dir.join("dhat-heap.json");
    let _profiler = dhat::Profiler::builder()
        .testing()
        .file_name(&profile_path)
        .build();

    let reports = run(&config);
    let report_path = config.artifact_dir.join("run-report.json");
    if let Err(error) = write_json_atomically(&report_path, &reports) {
        eprintln!("COULD_NOT_MEASURE: {error}");
        std::process::exit(2);
    }
    if reports
        .iter()
        .any(|report| !matches!(report.verdict, Verdict::Pass))
    {
        dhat::assert!(false, "resource leak gate reported a non-pass verdict");
    }
}

fn run(config: &Config) -> Vec<ScenarioReport> {
    let selected: &[&str] = match config.scenario.as_str() {
        "all" => &["pty", "sync-wave"],
        "pty" => &["pty"],
        "sync-wave" => &["sync-wave"],
        other => return vec![unmeasured(other, format!("unknown scenario: {other}"))],
    };
    selected
        .iter()
        .map(|scenario| measure_scenario(scenario, config))
        .collect()
}

fn measure_scenario(name: &str, config: &Config) -> ScenarioReport {
    let mut samples = Vec::with_capacity(config.measured_iterations);
    let mut fd_samples = Vec::with_capacity(config.measured_iterations);
    let operation =
        || -> Result<Option<codesense_pty_core::leak_gate::LeakProcessEvidence>, String> {
            match name {
                "pty" => codesense_pty_core::leak_gate::leak_gate_iteration().map(Some),
                "sync-wave" => {
                    codesense_lib::leak_support::run_sync_wave_iteration(&config.artifact_dir)
                        .map(|()| None)
                }
                _ => Err(format!("unknown scenario: {name}")),
            }
        };
    for warmup in 0..config.warmups {
        if let Err(error) = operation() {
            return operation_failure(name, warmup, format!("warmup failed: {error}"), config);
        }
    }
    let baseline_fd_snapshot = match fd_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => return unmeasured(name, error),
    };
    let baseline_fds = baseline_fd_snapshot.len();
    let baseline_set: BTreeSet<_> = baseline_fd_snapshot.into_iter().collect();
    // Evidence containers are part of the harness, not the operation. Allocate
    // them before the heap baseline so their own retained bytes cannot be
    // mistaken for product leakage.
    let baseline_heap = dhat::HeapStats::get();

    let mut lifecycle_process = None;
    for iteration in 0..config.measured_iterations {
        match operation() {
            Ok(process) => lifecycle_process = process,
            Err(error) => {
                return operation_failure(name, iteration, error, config);
            }
        }
        if config.inject.as_deref() == Some("heap") && iteration == 0 {
            Box::leak(vec![0_u8; 1024 * 1024].into_boxed_slice());
        }
        let stats = dhat::HeapStats::get();
        samples.push(HeapSample {
            iteration,
            current_bytes: stats.curr_bytes,
            current_blocks: stats.curr_blocks,
            total_bytes: stats.total_bytes,
            total_blocks: stats.total_blocks,
            max_bytes: stats.max_bytes,
            max_blocks: stats.max_blocks,
        });
        let snapshot = match fd_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => return unmeasured(name, error),
        };
        let unexpected = snapshot
            .iter()
            .filter(|fd| !baseline_set.contains(*fd))
            .cloned()
            .collect();
        fd_samples.push(FdSample {
            iteration,
            count: snapshot.len(),
            unexpected,
        });
    }

    let mut leaked_fd = None;
    if config.inject.as_deref() == Some("fd") {
        match std::fs::File::open("/dev/null") {
            Ok(file) => leaked_fd = Some(file),
            Err(error) => return unmeasured(name, format!("fd injector failed: {error}")),
        }
    }
    let child_deadline = std::time::Duration::from_secs(2);
    let mut leaked_child = if config.inject.as_deref() == Some("child") {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 30"]);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        match command.spawn() {
            Ok(child) => Some(ChildGuard::new(child)),
            Err(error) => return unmeasured(name, format!("child injector failed: {error}")),
        }
    } else {
        None
    };
    let final_fd_snapshot = match fd_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => return unmeasured(name, error),
    };
    let final_fds = final_fd_snapshot.len();
    let heap_failure = samples.iter().find(|sample| {
        sample
            .current_bytes
            .saturating_sub(baseline_heap.curr_bytes)
            > RETAINED_BYTE_DELTA
            || sample
                .current_blocks
                .saturating_sub(baseline_heap.curr_blocks)
                > RETAINED_BLOCK_DELTA
    });
    let leaked_fds: Vec<_> = final_fd_snapshot
        .into_iter()
        .filter(|fd| !baseline_set.contains(fd))
        .collect();
    let fd_failure = fd_samples
        .iter()
        .find(|sample| sample.unexpected.len() > ALLOWED_FD_DELTA);
    let fd_growth = leaked_fds.len();
    let process_started = std::time::Instant::now();
    if leaked_child.is_some() {
        std::thread::sleep(child_deadline);
    }
    let child_detail = if let Some(guard) = leaked_child.as_mut() {
        match guard.child_mut().try_wait() {
            Ok(None) => Some(format!(
                "child process remained alive past lifecycle boundary: pid={}",
                guard.child_mut().id()
            )),
            Ok(Some(_)) => None,
            Err(error) => {
                return unmeasured(name, format!("child census failed: {error}"));
            }
        }
    } else {
        None
    };
    let failure_resource = if heap_failure.is_some() {
        Some("heap")
    } else if fd_failure.is_some() || fd_growth > ALLOWED_FD_DELTA {
        Some("fd")
    } else if child_detail.is_some() {
        Some("child")
    } else {
        None
    };
    let mut process = leaked_child.as_mut().map(|guard| {
        let pid = guard.child_mut().id();
        let process_group = i32::try_from(pid)
            .ok()
            .map(|pid| unsafe { libc::getpgid(pid) })
            .unwrap_or(-1);
        ProcessEvidence {
            pid,
            process_group,
            started_seconds: None,
            started_microseconds: None,
            deadline_ms: child_deadline.as_millis() as u64,
            elapsed_ms: process_started.elapsed().as_millis(),
            alive_at_deadline: child_detail.is_some(),
            reaped: false,
        }
    });
    if process.is_none() {
        process = lifecycle_process.map(|evidence| ProcessEvidence {
            pid: evidence.pid,
            process_group: evidence.process_group,
            started_seconds: evidence.started_seconds,
            started_microseconds: evidence.started_microseconds,
            deadline_ms: evidence.deadline_ms,
            elapsed_ms: evidence.elapsed_ms,
            alive_at_deadline: evidence.alive_at_deadline,
            reaped: evidence.reaped,
        });
    }
    let detail = heap_failure
        .map(|sample| {
            format!(
                "heap retention exceeded bound at iteration {}: bytes={} blocks={}",
                sample.iteration,
                sample
                    .current_bytes
                    .saturating_sub(baseline_heap.curr_bytes),
                sample
                    .current_blocks
                    .saturating_sub(baseline_heap.curr_blocks)
            )
        })
        .or_else(|| {
            fd_failure.map(|sample| {
                format!(
                    "fd growth exceeded bound at iteration {}: {} unexpected descriptors",
                    sample.iteration,
                    sample.unexpected.len()
                )
            })
        })
        .or_else(|| {
            (fd_growth > ALLOWED_FD_DELTA).then(|| format!("fd growth exceeded bound: {fd_growth}"))
        })
        .or(child_detail);
    drop(leaked_fd);
    if let Some(guard) = leaked_child.as_mut() {
        let reaped = guard.cleanup();
        if let Some(evidence) = process.as_mut() {
            evidence.elapsed_ms = process_started.elapsed().as_millis();
            evidence.reaped = reaped;
        }
    }
    ScenarioReport {
        scenario: name.to_string(),
        verdict: if detail.is_some() {
            Verdict::Fail
        } else {
            Verdict::Pass
        },
        baseline_heap_bytes: baseline_heap.curr_bytes,
        baseline_heap_blocks: baseline_heap.curr_blocks,
        baseline_fds,
        allowed_heap_byte_delta: RETAINED_BYTE_DELTA,
        allowed_heap_block_delta: RETAINED_BLOCK_DELTA,
        allowed_fd_delta: ALLOWED_FD_DELTA,
        heap_trend_rule: "every post-cleanup retained delta must be <= the named bound",
        warmups: config.warmups,
        measured_iterations: config.measured_iterations,
        samples,
        fd_samples,
        final_fds: Some(final_fds),
        leaked_fds,
        failure_resource,
        process,
        detail,
    }
}

fn operation_failure(
    scenario: &str,
    iteration: usize,
    error: String,
    config: &Config,
) -> ScenarioReport {
    let mut report = unmeasured(scenario, format!("iteration {iteration} failed: {error}"));
    report.warmups = config.warmups;
    report.measured_iterations = config.measured_iterations;
    report
}

fn unmeasured(scenario: &str, detail: String) -> ScenarioReport {
    ScenarioReport {
        scenario: scenario.to_string(),
        verdict: Verdict::CouldNotMeasure,
        baseline_heap_bytes: 0,
        baseline_heap_blocks: 0,
        baseline_fds: 0,
        allowed_heap_byte_delta: RETAINED_BYTE_DELTA,
        allowed_heap_block_delta: RETAINED_BLOCK_DELTA,
        allowed_fd_delta: ALLOWED_FD_DELTA,
        heap_trend_rule: "every post-cleanup retained delta must be <= the named bound",
        warmups: 0,
        measured_iterations: 0,
        samples: Vec::new(),
        fd_samples: Vec::new(),
        final_fds: None,
        leaked_fds: Vec::new(),
        failure_resource: None,
        process: None,
        detail: Some(detail),
    }
}

#[cfg(target_os = "macos")]
fn fd_snapshot() -> Result<Vec<FdIdentity>, String> {
    let pid = std::process::id() as libc::c_int;
    for _ in 0..3 {
        let bytes =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if bytes <= 0 {
            return Err(format!(
                "proc_pidinfo size query failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let record_size = std::mem::size_of::<libc::proc_fdinfo>();
        let capacity = bytes as usize / record_size + 16;
        let mut records = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0
            };
            capacity
        ];
        let filled = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                records.as_mut_ptr().cast(),
                (records.len() * record_size) as libc::c_int,
            )
        };
        if filled <= 0 {
            return Err(format!(
                "proc_pidinfo fd query failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let filled = filled as usize;
        let buffer_size = records.len() * record_size;
        if filled == buffer_size {
            continue;
        }
        if filled > buffer_size || !filled.is_multiple_of(record_size) {
            return Err(format!("invalid proc_pidinfo fd payload: {filled} bytes"));
        }
        records.truncate(filled / record_size);
        return Ok(records
            .into_iter()
            .map(|record| FdIdentity {
                fd: record.proc_fd,
                fd_type: record.proc_fdtype,
            })
            .collect());
    }
    Err("proc_pidinfo fd list changed during three census attempts".to_string())
}

#[cfg(not(target_os = "macos"))]
fn fd_snapshot() -> Result<Vec<FdIdentity>, String> {
    Err("resource leak gate requires macOS fd census".to_string())
}

struct ChildGuard {
    child: Option<std::process::Child>,
    #[cfg(unix)]
    process_group: libc::pid_t,
}

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        #[cfg(unix)]
        let process_group = i32::try_from(child.id()).unwrap_or(-1);
        Self {
            child: Some(child),
            #[cfg(unix)]
            process_group,
        }
    }

    fn child_mut(&mut self) -> &mut std::process::Child {
        self.child
            .as_mut()
            .expect("child guard is always populated")
    }

    fn cleanup(&mut self) -> bool {
        let Some(mut child) = self.child.take() else {
            return true;
        };
        #[cfg(unix)]
        if self.process_group > 0 && self.process_group != unsafe { libc::getpgrp() } {
            unsafe { libc::kill(-self.process_group, libc::SIGKILL) };
        }
        let _ = child.kill();
        if child.wait().is_err() {
            return false;
        }
        #[cfg(unix)]
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                if unsafe { libc::kill(-self.process_group, 0) } != 0 {
                    return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                }
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        #[cfg(not(unix))]
        true
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

struct ArtifactLock(PathBuf);

impl ArtifactLock {
    fn acquire(directory: &Path) -> Result<Self, String> {
        let path = directory.join(".resource-leak-gate.lock");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("artifact directory is already in use: {error}"))?;
        Ok(Self(path))
    }
}

impl Drop for ArtifactLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
