pub use platform_catalog::startup::{DEFAULT_BUILD, select};

pub fn active_build() -> &'static str {
    platform_catalog::catalog().platform()
}
