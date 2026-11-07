// SPDX-License-Identifier: MIT
//! Declared folders of Markdown records, shared by native and agent callers.
pub mod binding;
pub(crate) mod commands;
pub(crate) mod edit;
pub(crate) mod history;
pub(crate) mod io;
pub(crate) mod notes;
pub(crate) mod projection;
pub(crate) mod schema;
mod store;

pub use edit::{BodyEdit, Overlap, Patch};
pub use history::{ActorKind, NoteActor as Actor, NoteRevision as Revision};
pub use schema::{
    is_valid_id, load_collections, Collection, HistoryOptions, IdRule, Options, Scope,
};
pub use store::{Record, SearchHit, Store, Written};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cp2_tests;
