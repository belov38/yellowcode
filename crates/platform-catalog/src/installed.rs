use crate::{PlatformError, PlatformVersion};
use std::path::{Path, PathBuf};

const OVERRIDE: &str = "YELLOWCODE_PLATFORM_RESOURCES";

pub(crate) fn broken(
    build: &PlatformVersion,
    path: &Path,
    reason: impl std::fmt::Display,
) -> PlatformError {
    PlatformError::Broken {
        build: build.clone(),
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

pub(crate) fn find(build: &PlatformVersion) -> Result<Option<PathBuf>, PlatformError> {
    let roots = if let Some(root) = std::env::var_os(OVERRIDE) {
        if root.is_empty() {
            return Err(broken(
                build,
                Path::new(""),
                format_args!("{OVERRIDE} must name a platform resource directory"),
            ));
        }
        vec![PathBuf::from(root)]
    } else {
        let exe = std::env::current_exe().map_err(|e| broken(build, Path::new(""), e))?;
        roots(&exe)
    };
    for root in roots {
        let dir = root.join(build.as_str());
        match std::fs::metadata(&dir) {
            Ok(_) => return Ok(Some(dir)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(broken(build, &dir, e)),
        }
    }
    Ok(None)
}

fn roots(exe: &Path) -> Vec<PathBuf> {
    let Some(parent) = exe.parent() else {
        return Vec::new();
    };
    let mut roots = vec![parent.join("resources").join("platform")];
    if cfg!(target_os = "macos") && parent.file_name().is_some_and(|p| p == "MacOS") {
        if let Some(contents) = parent
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "Contents"))
        {
            roots.push(
                contents
                    .join("Resources")
                    .join("resources")
                    .join("platform"),
            );
        }
    } else if cfg!(target_os = "linux") {
        if let Some(prefix) = parent.parent() {
            roots.push(
                prefix
                    .join("lib")
                    .join("Yellowcode")
                    .join("resources")
                    .join("platform"),
            );
        }
    }
    roots
}

pub(crate) fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }
}
