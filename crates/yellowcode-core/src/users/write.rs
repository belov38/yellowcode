use super::Card;
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserDraft {
    pub name: String,
    pub full_name: String,
    pub roles: Vec<String>,
    pub show_in_list: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserPatch {
    pub name: Option<String>,
    pub full_name: Option<String>,
    pub roles: Option<Vec<String>>,
    pub show_in_list: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum UserAction {
    Create { draft: UserDraft },
    Update { identity: String, patch: UserPatch },
    ClearPassword { identity: String },
    Delete { identity: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserWriteRequest {
    pub revision: String,
    pub confirmed: bool,
    pub allow_last_user: bool,
    pub action: UserAction,
}
#[derive(Debug, Clone, Serialize)]
pub struct RoleChoice {
    pub id: String,
    pub name: String,
    pub administrative: bool,
}
#[derive(Debug, Serialize)]
pub struct UserWriteSnapshot {
    pub revision: String,
    pub users: Vec<Card>,
    pub roles: Vec<RoleChoice>,
    pub writable: bool,
    pub supported_actions: &'static [&'static str],
    pub reason: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct UserWriteOutcome {
    pub revision: String,
    pub user: Option<Card>,
    pub deleted_id: Option<String>,
    pub users_remaining: usize,
}
#[derive(Debug, thiserror::Error)]
pub enum UserWriteError {
    #[error("invalid user input: {0}")]
    InvalidInput(&'static str),
    #[error("only an existing local 1CD infobase can be changed")]
    UnsupportedSource,
    #[error("unsupported user storage layout: {0}")]
    UnsupportedLayout(&'static str),
    #[error("empty password policy, algorithm or recovery is not qualified")]
    UnsupportedPolicy,
    #[error("this user operation is not qualified on this platform")]
    UnsupportedPlatform,
    #[error("infobase source is busy")]
    SourceBusy,
    #[error("the source changed or this snapshot has already been used; refresh it")]
    StaleRevision,
    #[error("no such infobase user")]
    NotFound,
    #[error("several users have this name; select the UUID")]
    Ambiguous,
    #[error("explicit confirmation of the selected user is required")]
    ConfirmationRequired,
    #[error("separate confirmation of the sole remaining user is required")]
    LastUserConfirmationRequired,
    #[error("the remaining users must retain administrative access")]
    LastAdministrator,
    #[error("data-area users are read-only")]
    DataAreaReadOnly,
    #[error("storage failure before commit; no changes committed by this request")]
    StorageBeforeCommit,
    #[error("commit outcome is unknown; do not retry automatically")]
    CommitUnknown,
    #[error("changes committed but verification failed; do not retry automatically")]
    CommittedUnverified,
}

pub fn write_snapshot(base: &Path) -> Result<UserWriteSnapshot, UserWriteError> {
    super::snapshot::load(base)
}
pub fn apply(base: &Path, request: &UserWriteRequest) -> Result<UserWriteOutcome, UserWriteError> {
    super::mutation::apply(base, request)
}
