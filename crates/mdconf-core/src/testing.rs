use crate as mdconf_api;
use crate::codec::{NoCodecs, StageRunner};
use crate::{Configuration, EntrySource, LoadOptions, Result};

pub fn no_codecs() -> bool {
    std::env::var_os("MDCONF_TEST_NO_CODECS").is_some_and(|v| v == "1")
}

pub fn load(src: &dyn EntrySource, opts: &LoadOptions) -> Result<Configuration> {
    crate::codec::load_with(src, opts, &NoCodecs)
}

pub fn load_with(
    src: &dyn EntrySource,
    opts: &LoadOptions,
    runner: &dyn StageRunner,
) -> Result<Configuration> {
    if no_codecs() {
        return crate::codec::load_with(src, opts, &NoCodecs);
    }
    crate::codec::load_with(src, opts, runner)
}

#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#[path = "../../mdconf/tests/support/index_source.rs"]
mod index_source;
pub use index_source::Source as ManagedTestSource;
