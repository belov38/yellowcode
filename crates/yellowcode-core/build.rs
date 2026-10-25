use std::{env, error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let source = manifest.join("../../desktop/tauri.conf.json");
    println!("cargo:rerun-if-changed={}", source.display());
    let config: serde_json::Value = serde_json::from_slice(&fs::read(source)?)?;
    let version = config
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing app version")?;
    println!("cargo:rustc-env=YELLOWCODE_VERSION={version}");
    Ok(())
}
