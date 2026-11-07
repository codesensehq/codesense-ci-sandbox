// SPDX-License-Identifier: MIT
pub mod attachment;
mod credential_ref;
mod current_user;
mod error;
mod loaded_org;
mod org;
mod person;
mod principal;
pub mod provider_reconcile;
mod raw_yaml;
pub(crate) use raw_yaml::is_content_relative;
mod source_repo_slug;
pub(crate) use source_repo_slug::validate_host as validate_source_repo_host;
mod tag_schema;
mod team;
pub mod user_path;
pub mod validation;
mod workspace_type;

pub use attachment::{Attachment, Attachments};
pub use credential_ref::CredentialRef;
pub use current_user::CurrentUser;
pub use error::DomainError;
pub(crate) use loaded_org::{descriptor_exhausted, retry_descriptor_io};
pub use loaded_org::{load_org, FileLoadError, FileLoadErrorKind, LoadedOrg, PartialOrg};
pub use org::{Org, OrgConfigFlags};
pub use person::{Identity, Person, PersonId};
pub use principal::{Principal, LOCAL_GUEST_USER_ID};
pub use provider_reconcile::{
    reconcile, repos_by_provider_from_raw, ReconcileEntry, ReconcileReport,
};
pub use raw_yaml::{
    parse_org_yml, parse_people_yml, parse_teams_yml, ParseError, RawIdentity, RawOrgComposition,
    RawOrgYml, RawPeopleYml, RawPersonEntry, RawSourceRepoEntry, RawTeamEntry, RawTeamsYml,
    RegistryFile,
};
pub use source_repo_slug::{RepositoryPathKey, SourceRepoSlug};
pub use tag_schema::{TagDef, TagSchema, TagType};
pub use team::Team;
pub use workspace_type::{
    derive_epic_id_from_doc_path, derive_epic_workspace_slug, derive_integration_branch,
    is_epic_workspace_slug, is_synthetic_reverse_live, WorkspaceType, EPIC_OWNER_SENTINEL,
    INTEGRATION_BRANCH_PREFIX,
};
