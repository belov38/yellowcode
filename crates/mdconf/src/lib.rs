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
mod facade;
pub mod startup;
pub mod xml_out;

pub use md_forms::forms;
pub use md_graphics::{graph, templates};
pub use mdconf_core::{model, parts};
pub mod xml_in;

pub mod entries {
    pub use mdconf_core::entries::{
        BudgetedEntries, CfFile, CfWriter, EntryData, EntryMeta, EntrySink, EntrySource, Inflated,
        ManagedEntries, ManagedEntriesError, ManagedEntrySource, MemEntries, decode_inflated,
        decode_stored, decode_stored_budgeted,
    };
}

pub mod xml {
    pub use mdconf_core::xml::{XmlAttr, XmlError, XmlNode, parse_xml};
}

pub use entries::{
    BudgetedEntries, CfFile, CfWriter, EntryData, EntryMeta, EntrySink, EntrySource, Inflated,
    ManagedEntries, ManagedEntriesError, ManagedEntrySource, MemEntries, decode_inflated,
    decode_stored, decode_stored_budgeted,
};
pub use facade::{CfFileExt, ConfigurationExt};
pub use md_model::*;
pub use mdconf_core::{
    CompatibilityMode, ConfigSettings, ConfigurationInspection, DataLockControlMode, Error,
    LoadMeter, LoadOptions, LoadProfile, ManagedRootError, MeterRefusal, ObjectAutonumerationMode,
    Result, RootErrorKind, RootInfo, ScriptVariant,
};
pub use mdschema::{Codec, MdKind};
