use crate::{PlatformError, PlatformVersion, is_platform_build};

pub const DEFAULT_BUILD: &str = "8.5.1.1529";

pub fn select(build: &str) -> Result<(), String> {
    crate::select(&PlatformVersion::new(build)).map_err(|error| match error {
        PlatformError::Unknown(version) if !is_platform_build(version.as_str()) => format!(
            "unknown platform build {build:?}; expected a dot-separated numeric build (for example 8.5.1.1529)"
        ),
        PlatformError::Unknown(version) => {
            format!("platform build {} is not installed", version.as_str())
        }
        other => other.to_string(),
    })
}
