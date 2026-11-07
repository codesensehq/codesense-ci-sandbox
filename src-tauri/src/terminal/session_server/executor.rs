// SPDX-License-Identifier: MIT
//! Dedicated bounded executor byte channels; never interpreted as MCP.
use super::*;
use codesense_host_proto::{executor as wire, Kind};
use codesense_pty_core::remote_executor::{ExecutorStarted, StartExecutor};
use tokio::sync::mpsc;
#[derive(Default)]
pub(super) struct Streams {
    entries: Mutex<HashMap<u32, mpsc::Sender<Vec<u8>>>>,
    next: AtomicU64,
}
impl Streams {
    pub(super) fn receive(&self, frame: Frame) {
        let mut entries = lock(&self.entries);
        if frame.kind == Kind::EXECUTOR_CLOSE || frame.payload.len() > wire::CHUNK_BYTES {
            entries.remove(&frame.channel);
            return;
        }
        if entries
            .get(&frame.channel)
            .is_some_and(|s| s.try_send(frame.payload).is_err())
        {
            entries.remove(&frame.channel);
        }
    }
    pub(super) fn retire(&self) {
        lock(&self.entries).clear()
    }
}
impl SessionServerClient {
    pub(crate) fn start_executor(
        &self,
        task_id: String,
        target: codesense_pty_core::remote_files::FileTarget,
    ) -> Result<(ExecutorStarted, mpsc::Receiver<Vec<u8>>), String> {
        self.private_lease()
            .ok_or("private executor authority unavailable")?
            .validate()?;
        if !self.hello.supports(&Capability::RemoteExecutor) {
            return Err("capabilityUnavailable: remote executor support unavailable".into());
        }
        let n = self.executor_streams.next.fetch_add(1, Ordering::AcqRel);
        let channel = u32::try_from(n)
            .ok()
            .and_then(|n| n.checked_add(2))
            .ok_or("executor channels exhausted")?;
        let (sender, receiver) = mpsc::channel(wire::QUEUE_CHUNKS);
        {
            let mut entries = lock(&self.executor_streams.entries);
            if entries.len() >= wire::MAX_EXECUTORS + wire::MAX_CONNECTIONS {
                return Err("executor channel limit reached".into());
            }
            entries.insert(channel, sender);
        }
        let result = match self.call(
            Request::StartRemoteExecutor {
                request: Box::new(StartExecutor {
                    task_id: task_id.clone(),
                    target,
                    channel,
                }),
            },
            Duration::from_secs(30),
        ) {
            Ok(Reply::RemoteExecutorStarted { result })
                if result.task_id == task_id && result.channel == channel =>
            {
                Ok(result)
            }
            Err(CallError::Failed(error)) => Err(error),
            _ => Err("remote executor start unavailable or outcome unknown".to_string()),
        };
        match result {
            Ok(result) => Ok((result, receiver)),
            Err(error) => {
                lock(&self.executor_streams.entries).remove(&channel);
                let _ = self.post(Request::StopRemoteExecutor { task_id });
                Err(error)
            }
        }
    }
    pub(crate) fn open_executor_channel(
        &self,
        task_id: String,
    ) -> Result<(u32, mpsc::Receiver<Vec<u8>>), String> {
        self.private_lease()
            .ok_or("private executor authority unavailable")?
            .validate()?;
        let n = self.executor_streams.next.fetch_add(1, Ordering::AcqRel);
        let channel = u32::try_from(n)
            .ok()
            .and_then(|n| n.checked_add(2))
            .ok_or("executor channels exhausted")?;
        let (sender, receiver) = mpsc::channel(wire::QUEUE_CHUNKS);
        {
            let mut entries = lock(&self.executor_streams.entries);
            if entries.len() >= wire::MAX_EXECUTORS + wire::MAX_CONNECTIONS {
                return Err("executor channel limit reached".into());
            }
            entries.insert(channel, sender);
        }
        match self.call(
            Request::OpenRemoteExecutor { task_id, channel },
            Duration::from_secs(5),
        ) {
            Ok(Reply::Done) => Ok((channel, receiver)),
            _ => {
                self.close_executor_channel(channel);
                Err("executor connection unavailable".into())
            }
        }
    }
    pub(crate) fn close_executor_channel(&self, channel: u32) {
        lock(&self.executor_streams.entries).remove(&channel);
        let _ = self.writer.try_send(&Frame {
            kind: Kind::EXECUTOR_CLOSE,
            channel,
            payload: vec![],
        });
    }
    pub(crate) fn executor_send(&self, channel: u32, bytes: &[u8]) -> Result<(), String> {
        self.private_lease()
            .ok_or("private executor authority unavailable")?
            .validate()?;
        for chunk in bytes.chunks(wire::CHUNK_BYTES) {
            self.writer
                .try_send(&Frame {
                    kind: Kind::EXECUTOR_DATA,
                    channel,
                    payload: chunk.into(),
                })
                .map_err(|_| "executor write outcome unknown")?;
        }
        Ok(())
    }
    pub(crate) fn stop_executor(&self, task_id: String, channel: u32) -> Result<(), String> {
        lock(&self.executor_streams.entries).remove(&channel);
        match self.call(
            Request::StopRemoteExecutor { task_id },
            Duration::from_secs(5),
        ) {
            Ok(Reply::Done) => Ok(()),
            _ => Err("executor stop outcome unknown".into()),
        }
    }
}
