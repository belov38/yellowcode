#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod answer;
pub mod auth;
pub mod body;
pub mod cred;
pub mod dcs;
pub mod dlist;
pub mod enums;
pub mod formdescr;
pub mod forms;
pub mod info;
mod json;
pub mod metadata;
pub mod modules;
pub mod native;
pub mod object;
pub mod refs;
pub mod report;
pub mod session;
pub mod spreadsheet;
pub mod tabular;
mod tls;
pub mod transport;
pub mod value;
pub mod wire;
pub mod write;
pub mod xml;

pub use auth::{AuthSettings, AuthSettingsError};
pub use dcs::{
    Available, Change, DcsError, DcsForm, DetailsItem, DetailsValue, SettingKind, SettingValue,
    UserSetting, Variant,
};
pub use dlist::{ListError, ListQuery, ListView};
pub use enums::{EnumPresentation, EnumPresentations};
pub use formdescr::{FieldType, FormData, FormField, FormKind, FormTable};
pub use info::{InfoError, ServerInfo};
pub use metadata::Metadata;
pub use modules::{
    FormMethod, FormModuleIds, ModuleDef, ModuleDefs, ModuleId, ModuleKind, ModuleSource,
    ModulesError,
};
pub use native::transport::{NativeOptions, NativeTarget, NativeTransport};
pub use object::{Cell, ObjectData, ObjectError, ObjectField, ObjectTable};
pub use refs::{Presentation, Presentations};
pub use report::{FormAttribute, OpenedForm, ReportError};
pub use session::{
    CallResult, Credentials, DcsOptions, DcsView, InfoOptions, InfoView, MetadataOptions,
    MetadataView, ReportOptions, ReportSheet, ReportView, Session, SessionError, Step,
};
pub use spreadsheet::{Details, Merge, Sheet, SheetError, SheetLimits};
pub use tabular::{
    AllowedLength, AllowedSign, Column, DateFractions, DateQualifiers, ListItem, NumberQualifiers,
    Picture, StringQualifiers, TreeRow, TypeDescription, ValueList, ValueTable, ValueTree,
};
pub use transport::{HttpTransport, TransportError};
pub use value::{
    Array, Date, Decimal, EnumRef, Kind, Map, Ref, Structure, TypeName, Uuid, Value, ValueError,
};
pub use wire::Wire;
pub use write::{
    Applied, Assign, Input, ObjectChange, Outcome, RowEdit, TableEdit, Target, UserMessage,
    WriteMode, WriteRequest, WriteResult,
};
pub use xml::CallValues;
