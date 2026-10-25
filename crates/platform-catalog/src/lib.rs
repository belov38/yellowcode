#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![forbid(unsafe_code)]

mod codec;
mod index;
mod installed;
pub mod messages;
mod model;
mod resource;
pub mod startup;

pub use index::{Catalog, is_platform_build};
pub use model::*;

static CATALOG_BIN: &[u8] = include_bytes!("../data/catalog.bin");

pub fn catalog() -> &'static Catalog {
    resource::active().catalog()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlatformVersion(String);

impl PlatformVersion {
    pub fn new(build: &str) -> PlatformVersion {
        PlatformVersion(build.to_owned())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlatformError {
    Unknown(PlatformVersion),
    Broken {
        build: PlatformVersion,
        path: std::path::PathBuf,
        reason: String,
    },
    OtherSelected {
        active: PlatformVersion,
        requested: PlatformVersion,
    },
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformError::Unknown(v) => write!(f, "no platform catalog for build {}", v.as_str()),
            PlatformError::Broken {
                build,
                path,
                reason,
            } => write!(
                f,
                "broken platform resource for build {} at {}: {reason}",
                build.as_str(),
                installed::display_path(path)
            ),
            PlatformError::OtherSelected { active, requested } => write!(
                f,
                "platform build {} is already in use in this process; cannot select {}",
                active.as_str(),
                requested.as_str()
            ),
        }
    }
}

impl std::error::Error for PlatformError {}

pub fn select(build: &PlatformVersion) -> Result<(), PlatformError> {
    resource::select(build)
}

pub fn for_platform(build: &PlatformVersion) -> Result<&'static Catalog, PlatformError> {
    select(build)?;
    Ok(catalog())
}
