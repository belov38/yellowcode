#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod answer;
pub mod auth;
pub mod body;
pub mod cred;
pub mod enums;
pub mod info;
mod json;
pub mod metadata;
pub mod modules;
pub mod refs;
pub mod session;
pub mod tabular;
pub mod transport;
pub mod value;
pub mod xml;

pub use auth::{AuthSettings, AuthSettingsError};
pub use enums::{EnumPresentation, EnumPresentations};
pub use info::{InfoError, ServerInfo};
pub use metadata::Metadata;
pub use modules::{
    FormMethod, FormModuleIds, ModuleDef, ModuleDefs, ModuleId, ModuleKind, ModuleSource,
    ModulesError,
};
pub use refs::{Presentation, Presentations};
pub use session::{
    CallResult, Credentials, InfoOptions, InfoView, MetadataOptions, MetadataView, Session,
    SessionError, Step,
};
pub use tabular::{
    AllowedLength, AllowedSign, Column, DateFractions, DateQualifiers, ListItem, NumberQualifiers,
    Picture, StringQualifiers, TreeRow, TypeDescription, ValueList, ValueTable, ValueTree,
};
pub use transport::{HttpTransport, TransportError};
pub use value::{
    Array, Date, Decimal, EnumRef, Kind, Map, Ref, Structure, TypeName, Uuid, Value, ValueError,
};
pub use xml::CallValues;
