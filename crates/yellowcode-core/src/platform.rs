pub use platform_catalog::startup::{DEFAULT_BUILD, managed_message, select};
pub use platform_catalog::{
    DiscoveryRoots, DiscoverySnapshot, ManagedSelectError, catalog_with_memory,
    select_from_roots_with_memory, select_with_memory,
};

pub fn active_build() -> &'static str {
    platform_catalog::catalog().platform()
}
