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

mod codec_state;
mod entries;
pub mod forms;
pub mod graph;
#[doc(hidden)]
pub mod index;
mod load;
pub mod model;
pub mod parts;
mod settings;
pub mod templates;
#[doc(hidden)]
pub mod types;
pub mod xml_origin;

#[doc(hidden)]
pub use codec_state::CodecState;
pub use entries::EntryData;
pub use load::{LoadProfile, Stages};
pub use mdschema::{Codec, MdKind};
pub use model::*;
pub use settings::{
    CompatibilityMode, ConfigSettings, DataLockControlMode, ObjectAutonumerationMode, ScriptVariant,
};

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod test_util;
