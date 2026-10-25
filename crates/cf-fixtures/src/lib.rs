use std::path::{Path, PathBuf};

pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../cf-testkit/fixtures")
        .join(name)
}
