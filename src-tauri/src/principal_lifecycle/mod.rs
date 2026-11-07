// SPDX-License-Identifier: MIT
//! Principal lifecycle contract spine (SW-PRN-0).
//!
//! This module locks the *shape* of the durable-Principal migration and the
//! app-wide Principal switch before either feature is built. It is pure domain
//! logic plus I/O *ports* (traits): the catalog and active-pointer model
//! ([`catalog`]), the versioned migration state machine and its copy/verify/
//! retire driver ([`migration`]), and the one lifecycle-owner registry with
//! generation fencing ([`lifecycle`]).
//!
//! SW-PRN-1 supplies the real keychain/filesystem adapters behind the ports and
//! wires the driver into bootstrap; SW-PRN-2 registers process-global owners and
//! drives [`lifecycle::LifecycleRegistry::switch`]. Until those consumers land
//! the spine has no production caller, so it carries a module-wide
//! `#![allow(dead_code)]` — the same reader-side-API-ahead-of-its-writer pattern
//! used elsewhere in this crate.
#![allow(dead_code)]

pub mod catalog;
pub mod lifecycle;
pub mod migration;
