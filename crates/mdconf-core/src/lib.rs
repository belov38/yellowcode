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

pub(crate) mod allocation;
pub mod build;
#[doc(hidden)]
pub mod codec;
pub(crate) mod codec_base64;
pub mod entries;
mod error;
#[allow(hidden_glob_reexports)]
mod index;
mod load;
pub use md_model::model;
mod modules;
#[cfg_attr(not(test), allow(dead_code))]
mod objects;
pub mod parts;
#[allow(dead_code)]
mod predefined;
mod root;
pub mod startup;
pub use root::{ErrorKind as RootErrorKind, ManagedRootError, RootInfo};
mod settings;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;
#[allow(hidden_glob_reexports)]
mod types;
pub mod xml;
pub mod xml_in;
pub mod xml_out;

pub use entries::{
    BudgetedEntries, CfFile, CfWriter, EntryBacking, EntryData, EntryList, EntryMeta, EntrySink,
    EntrySource, Inflated, ManagedCfFile, ManagedEntries, ManagedEntriesError, ManagedEntrySource,
    MemEntries, StoredBytes, decode_inflated, decode_stored, decode_stored_budgeted,
};
pub use error::{Error, Result};
pub use load::{ConfigurationInspection, LoadMeter, LoadOptions, LoadProfile, MeterRefusal};
pub use md_model::*;
pub use mdschema::{Codec, MdKind};
pub use settings::{
    CompatibilityMode, ConfigSettings, DataLockControlMode, ObjectAutonumerationMode, ScriptVariant,
};

#[doc(hidden)]
pub fn state<T: 'static>(state: &dyn md_model::CodecState) -> Option<&T> {
    let value = state.as_any().downcast_ref::<T>();
    debug_assert!(value.is_some(), "codec state of another type");
    value
}
