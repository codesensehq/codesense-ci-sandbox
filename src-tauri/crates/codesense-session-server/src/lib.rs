// SPDX-License-Identifier: MIT
//! The session-server sidecar as a library: process detachment, the host of the
//! `codesense-pty-core` runtime, the socket server, and the `attach --stdio`
//! relay that reaches it from another machine — plus what a client needs to
//! put it on a machine over SSH (`install`) and to tell whether it survives
//! logout there (`survival`), how to reach it with the system `ssh` (`ssh`,
//! `check`) and how that `ssh` gets a key's passphrase (`askpass`). The
//! `codesense-session-server` binary is a thin `main` over this; tests drive
//! it in-process.

pub mod askpass;
pub mod attach;
pub mod check;
pub mod detach;
pub mod disconnect_log;
pub mod exit_with;
pub mod host;
pub mod install;
pub mod private_account;
mod private_account_data;
#[cfg(target_os = "linux")]
pub mod private_account_native;
#[cfg(target_os = "linux")]
pub mod replace;
#[cfg(any(target_os = "linux", test))]
mod replacement_data;
mod replacement_evidence;
pub mod routine_arm;
pub mod server;
pub mod session_wake;
pub mod ssh;
pub mod stop;
pub mod survival;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod usage_receiver;
pub mod usage_spool;

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub mod reverse_mcp;

mod remote_workspace;

#[cfg(unix)]
mod native_launch;

pub mod machine_owner;
mod machine_stats;
pub mod offline_mcp;
#[cfg(target_os = "linux")]
mod owned_supervisors;
