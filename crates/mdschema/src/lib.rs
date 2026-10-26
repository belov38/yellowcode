#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod build;
pub mod fmt220;
pub mod forms;
pub mod forms_upgrade;
pub mod kind;
pub mod rights;
pub mod spec;
pub mod tables;
mod type_names;
pub use type_names::*;
pub mod templates;

pub use kind::MdKind;
pub use spec::*;
pub use tables::{Schema, SchemaError, schema};

mod static_view;
pub use static_view::{
    StaticConfigPath, StaticConfigProp, StaticFormContext, StaticFormProp, StaticFormTableValue,
    StaticFormTables, StaticKindSpec, StaticObjectIter, StaticPath, StaticPropSpec,
    StaticRightsTables, StaticSchema, StaticStrings, StaticTemplateTables, StaticValue,
    static_form_tables, static_rights_tables, static_schema, static_template_tables,
};

mod resource;
pub use resource::{ConfigFormat, ResourceError, SchemaResource, embedded_resource, for_format};
