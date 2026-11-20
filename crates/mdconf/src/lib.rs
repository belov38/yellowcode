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
pub mod external;
mod facade;
mod selected_codecs;
mod selected_form_facts;
pub mod startup;
pub mod xml_out;

pub use md_forms::forms;
pub use md_graphics::{graph, templates};
pub use mdconf_core::{
    configinfo, extension_children, extension_header, extension_objects, extension_reader, model,
    parts, root_facts,
};
pub mod xml_in;

pub mod entries {
    pub use mdconf_core::entries::{
        BudgetedEntries, CfFile, CfWriter, EntryBacking, EntryData, EntryList, EntryMeta,
        EntrySink, EntrySource, Inflated, ManagedCfFile, ManagedEntries, ManagedEntriesError,
        ManagedEntrySource, MemEntries, StoredBytes, decode_inflated, decode_stored,
        decode_stored_budgeted,
    };
}

pub mod xml {
    pub use mdconf_core::xml::{XmlAttr, XmlError, XmlNode, parse_xml};
}

pub use entries::{
    BudgetedEntries, CfFile, CfWriter, EntryBacking, EntryData, EntryList, EntryMeta, EntrySink,
    EntrySource, Inflated, ManagedCfFile, ManagedEntries, ManagedEntriesError, ManagedEntrySource,
    MemEntries, StoredBytes, decode_inflated, decode_stored, decode_stored_budgeted,
};
pub use facade::{CfFileExt, ConfigurationExt};
pub use md_model::*;
pub use mdconf_core::{
    CompatibilityMode, ConfigSettings, ConfigurationInspection, DataLockControlMode, Error,
    KeepObject, LoadMeter, LoadOptions, LoadProfile, ManagedRootError, MeterRefusal,
    ObjectAutonumerationMode, ObjectFilter, Result, RootErrorKind, RootInfo, ScriptVariant,
};
pub use mdschema::{Codec, ManagedSchemaError, MdKind};
