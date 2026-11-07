// SPDX-License-Identifier: MIT
//! The handshake. Each side's first frame is its [`Hello`]; a host follows its
//! own with its [`Census`]. A peer at the same `proto_major` is compatible
//! whatever its `proto_minor`: fields a later minor adds default when absent
//! and unknown ones are ignored. A peer at another major is incompatible: the
//! host refuses to serve it, and the client reports a [`Skew`] that names how
//! many sessions a restart would end — never a silent fallback.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::census::Census;
use crate::frame::{read_frame, write_frame, Frame, Kind};

/// Bumped on an incompatible wire change. 1 was the newline-JSON protocol; 2
/// the first framed one, whose keystrokes rode JSON control calls; 3 kept the
/// terminal layout in the app, off the wire; 4 spawned a command the app had
/// fully resolved; 5 spawned it on the host; 6 dropped the live-session
/// enumeration (the host's layout snapshot answers it); 7 subscribes to the
/// host emulator's serialized terminal, orders resizes in the stream and
/// drops byte replay (the host keeps no raw output to resend).
pub const PROTO_MAJOR: u16 = 7;

/// The xterm.js version every terminal emulator of this build runs: the app's
/// displays (`@xterm/xterm`) and the host's emulator worker
/// (`@xterm/headless`). A host whose emulator runs another version cannot
/// rebuild a display exactly, so it is as incompatible as another major.
pub const XTERM_VERSION: &str = "6.0.0";

/// Bumped on a compatible wire change (a new optional field, kind or
/// capability). 1 added respawn; 2 added connection-owned writable attachments;
/// 7 added account home; 8 added workspace head; 9 added agent process
/// inspection and invocation-scoped activity; 10 added session wakes and
/// timers; 11 added the streamed workspace manifest and the extended batch
/// Apply (`ContextManifestV1`). 12 was first reserved for pipelined bundle
/// upload, which shipped as capabilities without a wire bump; 12 adds
/// machine-armed routines (`RoutineArmV1`); 13 adds per-routine
/// wake limits on deliveries, timers and arms, and the per-session backstop
/// ; 14 adds the desktop identity on `RoutineArmBindSecret`
/// (agents handed back to the desktop that relayed them); 15 adds
/// the zstd batch codec (`ContextBatchZstdV1`); 16 adds the host's
/// [`BuildStamp`] on its hello; 17 adds host-side inbox waiting
/// (`InboxWaitV1`); 18 adds a saved Agent's own variables on a spawn and in its
/// startup (`AgentEnvV1`).
pub const PROTO_MINOR: u16 = 18;

/// A feature a side advertises in its [`Hello`]. A capability this build does
/// not know reads as [`Capability::Unsupported`]: the feature is hidden, never
/// an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Hosts terminal sessions (the terminal verbs and their pushes).
    Terminals,
    /// Architecture: canonical source usage includes openings, and warm handoff uses the removal fence.
    WorktreeSafetyV1,
    /// Authenticated, nonce-bound cooperative retirement for replacement.
    ShutdownForReplacement,
    /// Explicit connection-owned input, independent of output subscriptions.
    WritableAttachment,
    /// Preserve a reserved tab after its spawn reply is lost.
    RemoteTabRecovery,
    /// Authenticated session-bound reverse MCP on this connection.
    ReverseMcp,
    /// Session-bound selection of desktop-configured MCP servers.
    ReverseMcpServers,
    NativeRemoteLaunch,
    /// Saved-Agent args, generated settings, and notification wiring before native spawn.
    AgentStartup,
    /// Session-owned, supervised remote stdio MCP entries.
    RemoteStdioMcp,
    /// Connection-owned official Codex executor bound to a completed workspace.
    RemoteExecutor,
    /// Bounded selected context inspection and outbound identity/gzip transfers.
    SelectedContextBatchV1,
    /// Named-agent process evidence inspected on the owning host.
    AgentProcessV1,
    /// Invocation-scoped observation snapshots, revisions and explicit mutations.
    ActivityV1,
    /// Bounded native output selected by captured invocation/thread identity.
    SessionOutputV1,
    /// Answers the host account's home directory, so a person's new terminal
    /// opens there without anyone guessing the path.
    AccountHome,
    /// Answers whether a native agent CLI is installed for the host account,
    /// so a launch can say so before anything starts.
    NativeRuntimeProbe,
    /// Answers the commit and branch a managed workspace's checkout is on, so
    /// its committed work can be exported without naming the commit first.
    WorkspaceHead,
    /// Answers the usage spool's records past a sequence number, so the desktop can pull the usage
    /// its CLIs exported on this host. Gated by this capability alone (no protocol-minor bump).
    UsageSpoolV1,
    /// Answers a session's cwd, the account home and its Git checkout's
    /// status, read on this host, for the terminal caption's prompt segment.
    /// Gated by this capability alone (no protocol-minor bump).
    SessionPrompt,
    /// Idle-gated deliveries into a session and session timers, queued and
    /// fired by this host, with an outcome log for clients that were away.
    SessionWakeV1,
    /// An agent's inbox notices go through this host: the desktop pushes a session's
    /// mail notice here, and the host hands it to the agent (its waiter, or a submit
    /// at its prompt), reports how that went, and reports which sessions have a way
    /// to be told.
    InboxWaitV1,
    /// Versioned desktop spend decisions for automatic inbox wakes.
    InboxWakeHoldsV1,
    /// Per-session local MCP readiness, owned by the app's terminal connection.
    LocalMcpReadinessV1,
    /// The streamed, hash-cached `WorkspaceManifest`, and the batch
    /// Apply with removals, mode changes, directories, a per-workspace
    /// exclude block, per-entry pre-hashes and no 64-item / 16 MiB caps.
    ContextManifestV1,
    /// Pipelined bundle chunks (by index, acked out of order), a
    /// received-chunk bitmap for resume, and one seal with one flush.
    BundlePipelineV1,
    /// Move a prepared workspace to a newer base commit.
    WorkspaceBaseUpdateV1,
    /// `WorkspaceManifest` accepts a `workspaceId` and reports that
    /// workspace's own exclude block (`workspaceExclude`). A client sends the
    /// field only to a host advertising this: an older `ContextManifestV1`
    /// host rejects unknown request fields.
    ManifestWorkspaceExcludeV1,
    /// Marker-only cleanup for a completed workspace whose folder is gone.
    ForgetMissingWorkspaceV1,
    /// Answers the workspace editor's file operations — a one-level directory
    /// listing, a file's committed bytes, and the quick-open file list — so an
    /// editor on a remote checkout browses and diffs like a local one. Gated by
    /// this capability alone (no protocol-minor bump).
    WorkspaceEditorV1,
    /// Activity for every agent in a terminal: a CLI typed by hand in a shell
    /// claims an invocation by process ancestry, and the full hook set
    /// (notifications, compaction, error kinds, quiet-turn decay) is folded.
    ActivityV2,
    /// Pre-trusts the exact injected attribution checkpoint commands in final Codex argv.
    AttributionHookTrustV1,
    /// `WorkspaceManifest` accepts `read` (exact paths) and streams
    /// those files' bytes after the entries, so an explicit publish reads
    /// every changed file in ONE call. A client sends the field only to a
    /// host advertising this.
    ManifestReadV1,
    /// Persistent routines armed on this host (`RoutineArm`,
    /// `RoutineDisarm`, `RoutineArms`, `RoutineArmRuns`), fired on its own
    /// clock whether or not a desktop is connected, fenced by arm epoch.
    RoutineArmV1,
    /// A context batch's payload may be one zstd frame
    /// (`Codec::Zstd`). A client sends it only to a host advertising this.
    ContextBatchZstdV1,
    /// A saved Agent's own environment variables: `SpawnIntent::agent_env` and
    /// `AgentStartup::env`. A host before it drops the first and refuses the
    /// second, so a client sends either only to a host advertising this.
    AgentEnvV1,
    /// Emits again, on request, the exit of a session that ended but is still
    /// retained (`RedeliverExits`), so a client that missed it can end the
    /// session. Gated by this capability alone (no protocol-minor bump).
    ExitRedeliveryV1,
    /// Reaps at a client's exit only the sessions that client opened
    /// (`ReapOwnedNonPersisting`), so one app quitting never ends another
    /// app's sessions on the same server. Gated by this capability alone (no
    /// protocol-minor bump).
    ScopedReapV1,
    /// Answers what it holds (`ServerStatus`): its sessions, running
    /// agents, connected apps and the sessions no connected app holds. Gated
    /// by this capability alone (no protocol-minor bump).
    ServerStatusV1,
    /// Self-reported resources, by one-shot read or connection-owned watch.
    MachineStatsV1,
    #[serde(other)]
    Unsupported,
}

/// A side's first frame. `server_id`, `server_name` and `epoch` identify a
/// host (the persisted id, its display name, and this host lifetime) and `rev`
/// is its layout's revision as it greets; a client leaves them empty (`rev`
/// 0).
/// `viewer_id` names a client's viewer and is `None` from a host. `instance_id` is the app-data instance the side belongs to; `pid` is
/// the sending process, for diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hello {
    pub proto_major: u16,
    pub proto_minor: u16,
    pub agent_version: String,
    pub server_id: String,
    pub server_name: String,
    pub epoch: String,
    pub rev: u64,
    pub instance_id: String,
    pub capabilities: Vec<Capability>,
    pub viewer_id: Option<String>,
    pub pid: u32,
    /// A host's terminal emulator; `None` from a client.
    pub emulator: Option<EmulatorHello>,
    /// Since minor 16: the CodeSense release and commit the host was
    /// built from. `None` from a client and from a host older than minor 16.
    /// A malformed stamp reads as none: it never costs the greeting.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_build"
    )]
    pub build: Option<BuildStamp>,
    /// A client's identity for one app process, the same on every
    /// connection it makes, so a host can tell which sessions a connected app
    /// opened. `None` from a host and from an older client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

/// `build` as sent, or `None` when it is not a stamp this build can read.
fn lenient_build<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<BuildStamp>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Maybe {
        Stamp(BuildStamp),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Maybe>::deserialize(deserializer)? {
        Some(Maybe::Stamp(stamp)) => Some(stamp),
        _ => None,
    })
}

/// The build a terminal server came from: the CodeSense **app** release it
/// ships with (the session-server crate's own version never changes), the git
/// commit, and when it was built. The app compares it with its own to say
/// whether a running server is its build, an older one or a newer one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildStamp {
    /// The app release version, e.g. `0.0.23`.
    pub version: String,
    /// The commit (12 hex), when the build knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Build time, in milliseconds since the Unix epoch, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_at_ms: Option<u64>,
}

/// The terminal emulator a host runs its sessions' terminals in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmulatorHello {
    pub xterm_version: String,
}

impl EmulatorHello {
    /// This build's emulator.
    pub fn current() -> Self {
        Self {
            xterm_version: XTERM_VERSION.to_string(),
        }
    }
}

impl Hello {
    /// The [`Kind::HELLO`] frame carrying this hello.
    pub fn frame(&self) -> Result<Frame, String> {
        Frame::json(Kind::HELLO, self)
    }

    /// Whether this side advertised `capability`. Never true for
    /// [`Capability::Unsupported`].
    pub fn supports(&self, capability: &Capability) -> bool {
        *capability != Capability::Unsupported && self.capabilities.contains(capability)
    }
}

/// The frozen part of a hello: enough to tell a peer's major — and the process
/// that sent it, which every protocol's hello has named — even when the rest
/// of its hello is laid out differently.
#[derive(Deserialize)]
struct Major {
    proto_major: u16,
    pid: u32,
}

/// Identity and restart evidence whose JSON field names are frozen across the
/// framed protocol lineage. This deliberately is not a full old `Hello`: a
/// host at another major is never admitted through accidental serde
/// compatibility, only identified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenHostHello {
    pub proto_major: u16,
    pub proto_minor: u16,
    pub agent_version: String,
    pub server_id: String,
    pub server_name: String,
    pub epoch: String,
    pub rev: u64,
    pub instance_id: String,
    pub capabilities: Vec<Capability>,
    pub viewer_id: Option<String>,
    pub pid: u32,
    /// Absent from a host older than protocol 7.
    #[serde(default)]
    pub emulator: Option<EmulatorHello>,
}

impl From<Hello> for FrozenHostHello {
    fn from(hello: Hello) -> Self {
        Self {
            proto_major: hello.proto_major,
            proto_minor: hello.proto_minor,
            agent_version: hello.agent_version,
            server_id: hello.server_id,
            server_name: hello.server_name,
            epoch: hello.epoch,
            rev: hello.rev,
            instance_id: hello.instance_id,
            capabilities: hello.capabilities,
            viewer_id: hello.viewer_id,
            pid: hello.pid,
            emulator: hello.emulator,
        }
    }
}

impl FrozenHostHello {
    /// Why this host cannot serve this build, if it cannot: another major, or
    /// an emulator other than this build's.
    pub fn skew_reason(&self) -> Option<SkewReason> {
        skew_reason(self.proto_major, self.emulator.as_ref())
    }
}

/// Why a host cannot serve this build: another protocol major, or (at this
/// major) a terminal emulator of another xterm version (or none named).
fn skew_reason(proto_major: u16, emulator: Option<&EmulatorHello>) -> Option<SkewReason> {
    if proto_major != PROTO_MAJOR {
        return Some(SkewReason::ProtoMajor { peer: proto_major });
    }
    match emulator {
        Some(emulator) if emulator.xterm_version == XTERM_VERSION => None,
        other => Some(SkewReason::EmulatorVersion {
            peer: other.map(|e| e.xterm_version.clone()),
        }),
    }
}

/// A host greeting judged against this build: one it can use, or the frozen
/// identity of one it cannot ([`FrozenHostHello::skew_reason`] says why).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostGreeting {
    Supported { hello: Hello },
    Unsupported(FrozenHostHello),
}

/// A peer's hello, judged against this build's [`PROTO_MAJOR`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerHello {
    Compatible(Box<Hello>),
    Incompatible { proto_major: u16, pid: u32 },
}

/// Read a peer's first frame, which must be its hello.
pub fn read_hello(reader: &mut impl Read) -> Result<PeerHello, String> {
    let frame = read_frame(reader)?.ok_or_else(|| "closed before its hello".to_string())?;
    let frame = frame.expect(Kind::HELLO)?;
    let Major { proto_major, pid } = frame.decode_json()?;
    if proto_major != PROTO_MAJOR {
        return Ok(PeerHello::Incompatible { proto_major, pid });
    }
    frame
        .decode_json()
        .map(|hello| PeerHello::Compatible(Box::new(hello)))
}

/// Read the two frames with which a host greets a connection. This is shared
/// by ordinary client-first connections and greeting-first remote admission,
/// so both connectors use the exact same framing and decoding rules.
pub fn read_host_greeting(reader: &mut impl Read) -> Result<(HostGreeting, Census), String> {
    let frame = read_frame(reader)?.ok_or_else(|| "closed before its hello".to_string())?;
    let frame = frame.expect(Kind::HELLO)?;
    let frozen: FrozenHostHello = frame.decode_json()?;
    let peer = match frozen.skew_reason() {
        None => HostGreeting::Supported {
            hello: frame.decode_json()?,
        },
        Some(_) => HostGreeting::Unsupported(frozen),
    };
    let census = read_frame(reader)?
        .ok_or_else(|| "closed before its census".to_string())
        .and_then(|frame| Census::from_frame(&frame))?;
    Ok((peer, census))
}

/// Why a host cannot serve this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkewReason {
    /// It speaks another protocol major.
    ProtoMajor { peer: u16 },
    /// It speaks this major, but its terminal emulator runs another xterm
    /// version (`None`: it named none).
    EmulatorVersion { peer: Option<String> },
}

/// A host this build cannot use until it is restarted, which ends its
/// `live_sessions` (`None` when its census was unreadable). `host_pid` is the
/// host process, as its hello named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skew {
    pub reason: SkewReason,
    pub live_sessions: Option<u32>,
    pub host_pid: Option<u32>,
}

impl Skew {
    /// The host's protocol major.
    pub fn host_major(&self) -> u16 {
        match self.reason {
            SkewReason::ProtoMajor { peer } => peer,
            SkewReason::EmulatorVersion { .. } => PROTO_MAJOR,
        }
    }
}

impl std::fmt::Display for Skew {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ends = match self.live_sessions {
            Some(1) => "1 session".to_string(),
            Some(n) => format!("{n} sessions"),
            None => "its running sessions".to_string(),
        };
        match &self.reason {
            SkewReason::ProtoMajor { peer } => write!(
                f,
                "restart terminal server (ends {ends}): it speaks protocol {peer}, this app {PROTO_MAJOR}"
            ),
            SkewReason::EmulatorVersion { peer } => write!(
                f,
                "restart terminal server (ends {ends}): its terminal emulator is xterm {}, this app {XTERM_VERSION}",
                peer.as_deref().unwrap_or("(unknown)")
            ),
        }
    }
}

/// Why a client's handshake failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GreetError {
    /// The exchange itself failed: I/O, or a malformed hello or census.
    Failed(String),
    /// The host speaks another major, or runs another emulator.
    Incompatible(Skew),
}

/// The client side of the handshake: send `ours`, then read the host's hello
/// and census. Returns the host's hello when it is compatible. A host at
/// another major, or running another emulator, is incompatible even when its
/// census cannot be read.
pub fn greet(
    reader: &mut impl Read,
    writer: &mut impl Write,
    ours: &Hello,
) -> Result<Hello, GreetError> {
    let failed = |e: String| GreetError::Failed(e);
    write_frame(writer, &ours.frame().map_err(failed)?).map_err(|e| failed(e.to_string()))?;
    let peer = read_hello(reader).map_err(failed)?;
    let census = read_frame(reader)
        .and_then(|frame| frame.ok_or_else(|| "closed before its census".to_string()))
        .and_then(|frame| Census::from_frame(&frame));
    let (reason, pid) = match peer {
        PeerHello::Compatible(hello) => {
            match skew_reason(hello.proto_major, hello.emulator.as_ref()) {
                None => return census.map(|_| *hello).map_err(failed),
                Some(reason) => (reason, hello.pid),
            }
        }
        PeerHello::Incompatible { proto_major, pid } => {
            (SkewReason::ProtoMajor { peer: proto_major }, pid)
        }
    };
    Err(GreetError::Incompatible(Skew {
        reason,
        live_sessions: census.ok().map(|c| c.live_sessions),
        host_pid: Some(pid).filter(|pid| *pid != 0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_hello() -> Hello {
        Hello {
            proto_major: PROTO_MAJOR,
            proto_minor: PROTO_MINOR,
            agent_version: "0.1.0".into(),
            server_id: "server-1".into(),
            server_name: "mac".into(),
            epoch: "epoch-1".into(),
            rev: 4,
            instance_id: "/root".into(),
            capabilities: vec![Capability::Terminals],
            viewer_id: None,
            pid: 7,
            emulator: Some(EmulatorHello::current()),
            build: None,
            client_id: None,
        }
    }

    /// A client's identity rides on its hello; a hello without one (a
    /// host, an older client) leaves it out and still reads.
    #[test]
    fn sv7_client_id_round_trips_and_is_optional() {
        let client = Hello {
            client_id: Some("app-1".into()),
            ..host_hello()
        };
        let json = serde_json::to_value(&client).unwrap();
        assert_eq!(json["client_id"], "app-1");
        assert_eq!(serde_json::from_value::<Hello>(json).unwrap(), client);
        let json = serde_json::to_value(host_hello()).unwrap();
        assert!(json.get("client_id").is_none());
        assert_eq!(
            serde_json::from_value::<Hello>(json).unwrap().client_id,
            None
        );
    }

    /// A host's build stamp rides on its hello and survives the
    /// greeting; a host older than minor 16 sends none and still greets.
    #[test]
    fn agr19_build_stamp_round_trips_and_is_optional() {
        let stamped = Hello {
            build: Some(BuildStamp {
                version: "0.0.23".into(),
                commit: Some("0123456789ab".into()),
                built_at_ms: Some(1_790_000_000_000),
            }),
            ..host_hello()
        };
        let wire = serde_json::to_value(&stamped).unwrap();
        assert_eq!(wire["build"]["version"], "0.0.23");
        assert_eq!(wire["build"]["built_at_ms"], 1_790_000_000_000_u64);
        let (result, _) = greet_host(&mut host_wire(&wire, 0));
        assert_eq!(result, Ok(stamped));

        let mut old = serde_json::to_value(host_hello()).unwrap();
        old["proto_minor"] = 15.into();
        assert!(
            old.get("build").is_none(),
            "an unstamped hello sends no field"
        );
        let (result, _) = greet_host(&mut host_wire(&old, 0));
        assert_eq!(result.unwrap().build, None);

        let bare: BuildStamp =
            serde_json::from_value(serde_json::json!({"version": "0.0.23"})).unwrap();
        assert_eq!(bare.commit, None);
        assert_eq!(bare.built_at_ms, None);
    }

    /// What a host sends: `hello` as raw JSON, then its census.
    fn host_wire(hello: &serde_json::Value, live_sessions: u32) -> std::io::Cursor<Vec<u8>> {
        let mut wire = Frame::json(Kind::HELLO, hello).unwrap().encode();
        wire.extend(Census { live_sessions }.frame().encode());
        std::io::Cursor::new(wire)
    }

    fn greet_host(wire: &mut std::io::Cursor<Vec<u8>>) -> (Result<Hello, GreetError>, Vec<u8>) {
        let mut sent = Vec::new();
        let ours = Hello {
            viewer_id: Some("viewer".into()),
            ..host_hello()
        };
        (greet(wire, &mut sent, &ours), sent)
    }

    /// The client's hello is its first frame, and a host at the same major
    /// is compatible with every field it sent.
    #[test]
    fn a_same_major_host_is_compatible() {
        let hello = host_hello();
        let (result, sent) = greet_host(&mut host_wire(&serde_json::to_value(&hello).unwrap(), 2));
        assert_eq!(result, Ok(hello));
        match read_hello(&mut sent.as_slice()) {
            Ok(PeerHello::Compatible(ours)) => {
                assert_eq!(ours.viewer_id.as_deref(), Some("viewer"))
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rm4c_current_greeting_is_atomic() {
        let hello = host_hello();
        let mut wire = host_wire(&serde_json::to_value(&hello).unwrap(), 9);
        let (peer, census) = read_host_greeting(&mut wire).unwrap();
        assert_eq!(peer, HostGreeting::Supported { hello });
        assert_eq!(census.live_sessions, 9);
        assert_eq!(wire.position() as usize, wire.get_ref().len());
    }

    /// A host one major back is not admitted: it is identified for its
    /// restart, never adapted.
    #[test]
    fn rm4c_older_majors_are_identified_not_admitted() {
        let mut previous = host_hello();
        previous.proto_major = PROTO_MAJOR - 1;
        previous.emulator = None;
        let (greeting, census) =
            read_host_greeting(&mut host_wire(&serde_json::to_value(&previous).unwrap(), 3))
                .unwrap();
        let HostGreeting::Unsupported(frozen) = greeting else {
            panic!("the previous major was admitted")
        };
        assert_eq!(
            frozen.skew_reason(),
            Some(SkewReason::ProtoMajor {
                peer: PROTO_MAJOR - 1
            })
        );
        assert_eq!(frozen.server_id, "server-1");
        assert_eq!(census.live_sessions, 3);

        let older = serde_json::json!({
            "proto_major": 4,
            "proto_minor": 7,
            "agent_version": "old",
            "server_id": "server-old",
            "server_name": "old host",
            "epoch": "old-epoch",
            "rev": 41,
            "instance_id": "/old/root",
            "capabilities": ["terminals"],
            "pid": 71,
        });
        let (greeting, census) =
            read_host_greeting(&mut host_wire(&older, 8)).expect("frozen framed greeting");
        let HostGreeting::Unsupported(frozen) = greeting else {
            panic!("older major was admitted")
        };
        assert_eq!(frozen.server_id, "server-old");
        assert_eq!(frozen.epoch, "old-epoch");
        assert_eq!(frozen.rev, 41);
        assert_eq!(census.live_sessions, 8);
    }

    /// A newer minor's extra fields and unknown capabilities are ignored; an
    /// older minor's missing fields default. Neither is an error.
    #[test]
    fn a_minor_mismatch_is_compatible() {
        let newer = serde_json::json!({
            "proto_major": PROTO_MAJOR,
            "proto_minor": PROTO_MINOR + 3,
            "pid": 41,
            "server_id": "server-1",
            "capabilities": ["terminals", "fs_read"],
            "emulator": {"xterm_version": XTERM_VERSION, "added_later": 1},
            "added_later": {"nested": true},
        });
        let hello = greet_host(&mut host_wire(&newer, 0)).0.expect("compatible");
        assert_eq!(hello.proto_minor, PROTO_MINOR + 3);
        assert_eq!(
            hello.capabilities,
            [Capability::Terminals, Capability::Unsupported]
        );
        assert!(hello.supports(&Capability::Terminals));
        assert!(
            !hello.supports(&Capability::Unsupported),
            "hidden, not an error"
        );

        let older = serde_json::json!({
            "proto_major": PROTO_MAJOR,
            "pid": 41,
            "emulator": {"xterm_version": XTERM_VERSION},
        });
        let hello = greet_host(&mut host_wire(&older, 0)).0.expect("compatible");
        assert_eq!(hello.server_id, "");
        assert!(hello.capabilities.is_empty());
        assert!(!hello.supports(&Capability::Terminals));
    }

    /// An incompatible host's hello still names its process, for a restart.
    #[test]
    fn a_major_mismatch_names_the_host_process() {
        let hello = serde_json::json!({"proto_major": PROTO_MAJOR - 1, "pid": 42});
        match greet_host(&mut host_wire(&hello, 1)).0 {
            Err(GreetError::Incompatible(skew)) => assert_eq!(skew.host_pid, Some(42)),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A host at another major is reported with the census count, even when
    /// the rest of its hello no longer parses as this build's `Hello`.
    #[test]
    fn a_major_mismatch_names_the_sessions_a_restart_ends() {
        for (hello, major) in [
            (
                serde_json::json!({"proto_major": PROTO_MAJOR + 1, "pid": 51}),
                PROTO_MAJOR + 1,
            ),
            (
                serde_json::json!({"proto_major": PROTO_MAJOR - 1, "pid": 51, "capabilities": 5}),
                PROTO_MAJOR - 1,
            ),
        ] {
            let skew = match greet_host(&mut host_wire(&hello, 3)).0 {
                Err(GreetError::Incompatible(skew)) => skew,
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(
                skew,
                Skew {
                    reason: SkewReason::ProtoMajor { peer: major },
                    live_sessions: Some(3),
                    host_pid: Some(51),
                }
            );
            assert!(skew
                .to_string()
                .starts_with("restart terminal server (ends 3 sessions)"));
        }
        let one = Skew {
            reason: SkewReason::ProtoMajor { peer: 1 },
            live_sessions: Some(1),
            host_pid: None,
        };
        assert!(one.to_string().contains("(ends 1 session)"));

        let mut no_census = std::io::Cursor::new(
            Frame::json(
                Kind::HELLO,
                &serde_json::json!({"proto_major": 1, "pid": 52}),
            )
            .unwrap()
            .encode(),
        );
        match greet_host(&mut no_census).0 {
            Err(GreetError::Incompatible(skew)) => {
                assert_eq!(skew.live_sessions, None);
                assert!(skew
                    .to_string()
                    .starts_with("restart terminal server (ends its running sessions)"));
            }
            other => panic!("an unreadable census still reports the skew: {other:?}"),
        }
    }

    /// A host at this major whose emulator runs another xterm (or names none)
    /// is refused like another major, by both the client handshake and the
    /// greeting reader.
    #[test]
    fn another_emulator_version_is_incompatible() {
        for (emulator, peer) in [
            (
                Some(EmulatorHello {
                    xterm_version: "5.5.0".into(),
                }),
                Some("5.5.0".to_string()),
            ),
            (None, None),
        ] {
            let hello = Hello {
                emulator,
                ..host_hello()
            };
            let json = serde_json::to_value(&hello).unwrap();
            let skew = match greet_host(&mut host_wire(&json, 2)).0 {
                Err(GreetError::Incompatible(skew)) => skew,
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(
                skew.reason,
                SkewReason::EmulatorVersion { peer: peer.clone() }
            );
            assert_eq!(skew.host_major(), PROTO_MAJOR);
            assert_eq!(skew.host_pid, Some(7));
            assert!(skew
                .to_string()
                .starts_with("restart terminal server (ends 2 sessions)"));
            let (greeting, _) = read_host_greeting(&mut host_wire(&json, 2)).unwrap();
            let HostGreeting::Unsupported(frozen) = greeting else {
                panic!("admitted a host running another emulator")
            };
            assert_eq!(
                frozen.skew_reason(),
                Some(SkewReason::EmulatorVersion { peer })
            );
        }
    }

    /// Hellos between this major and the one before are refused in both
    /// directions: a host reading a client's, and a client reading a host's.
    #[test]
    fn the_previous_major_is_refused_both_ways() {
        let old_client = serde_json::json!({"proto_major": PROTO_MAJOR - 1, "pid": 5});
        let wire = Frame::json(Kind::HELLO, &old_client).unwrap().encode();
        assert_eq!(
            read_hello(&mut wire.as_slice()),
            Ok(PeerHello::Incompatible {
                proto_major: PROTO_MAJOR - 1,
                pid: 5
            })
        );
        let old_host = serde_json::json!({"proto_major": PROTO_MAJOR - 1, "pid": 6});
        assert!(matches!(
            greet_host(&mut host_wire(&old_host, 0)).0,
            Err(GreetError::Incompatible(Skew {
                reason: SkewReason::ProtoMajor { .. },
                ..
            }))
        ));
    }

    /// The pinned version is the one the app and the worker are built with.
    #[test]
    fn xterm_version_matches_the_packages() {
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../package.json"),
            )
            .unwrap(),
        )
        .unwrap();
        for package in ["@xterm/xterm", "@xterm/headless"] {
            let pinned = manifest["dependencies"][package]
                .as_str()
                .or_else(|| manifest["devDependencies"][package].as_str())
                .unwrap_or_else(|| panic!("{package} is not in package.json"));
            assert_eq!(pinned, XTERM_VERSION, "{package}");
        }
    }

    #[test]
    fn rm4c_partial_or_mixed_greeting_fails_atomically() {
        let mut census_first = std::io::Cursor::new(Census { live_sessions: 1 }.frame().encode());
        assert!(matches!(
            greet_host(&mut census_first).0,
            Err(GreetError::Failed(_))
        ));
        let mut no_census = std::io::Cursor::new(host_hello().frame().unwrap().encode());
        assert!(matches!(
            greet_host(&mut no_census).0,
            Err(GreetError::Failed(_))
        ));
    }
}
