use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Destination {
    pub explicit: Option<PathBuf>,
    pub output: Option<PathBuf>,
}

impl std::fmt::Debug for Destination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Destination")
            .field("explicit", &self.explicit.is_some())
            .field("output", &self.output.is_some())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteFailure {
    pub os_error: Option<i32>,
    pub kind: &'static str,
}

impl WriteFailure {
    pub fn of(error: &io::Error) -> Self {
        WriteFailure {
            os_error: error.raw_os_error(),
            kind: io_kind_tag(error.kind()),
        }
    }
}

pub fn io_kind_tag(kind: io::ErrorKind) -> &'static str {
    use io::ErrorKind as K;
    match kind {
        K::NotFound => "not_found",
        K::PermissionDenied => "permission_denied",
        K::AlreadyExists => "already_exists",
        K::InvalidInput => "invalid_input",
        K::InvalidData => "invalid_data",
        K::TimedOut => "timed_out",
        K::WouldBlock => "would_block",
        K::BrokenPipe => "broken_pipe",
        K::UnexpectedEof => "unexpected_eof",
        K::WriteZero => "write_zero",
        K::Interrupted => "interrupted",
        K::OutOfMemory => "out_of_memory",
        K::StorageFull => "storage_full",
        K::ReadOnlyFilesystem => "read_only_filesystem",
        K::NotADirectory => "not_a_directory",
        K::IsADirectory => "is_a_directory",
        _ => "other",
    }
}

pub(crate) fn file_name(unix_seconds: i64, attempt: u32) -> String {
    let counter = if attempt == 0 {
        String::new()
    } else {
        format!("-{}", attempt + 1)
    };
    format!(
        "yellowcode-bak-failure-{}{counter}.log",
        crate::utc::compact(unix_seconds)
    )
}

pub(crate) fn candidate_dirs(output: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = output
        .and_then(Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    dirs.push(PathBuf::from("."));
    dirs.push(std::env::temp_dir());
    dirs.dedup();
    dirs
}

pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

pub(crate) fn write_in(
    dirs: &[PathBuf],
    unix_seconds: i64,
    text: &str,
) -> Result<PathBuf, WriteFailure> {
    write_in_with(dirs, unix_seconds, text, write_to)
}

pub(crate) fn write_in_with(
    dirs: &[PathBuf],
    unix_seconds: i64,
    text: &str,
    write: impl Fn(File, &str) -> io::Result<()>,
) -> Result<PathBuf, WriteFailure> {
    let mut last: Option<io::Error> = None;
    for dir in dirs {
        for attempt in 0..100 {
            let path = dir.join(file_name(unix_seconds, attempt));
            match create_new(&path) {
                Ok(file) => match write(file, text) {
                    Ok(()) => return Ok(std::path::absolute(&path).unwrap_or(path)),
                    Err(error) => {
                        let _ = std::fs::remove_file(&path);
                        last = Some(error);
                        break;
                    }
                },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = Some(error),
                Err(error) => {
                    last = Some(error);
                    break;
                }
            }
        }
    }
    Err(last.as_ref().map_or(
        WriteFailure {
            os_error: None,
            kind: "other",
        },
        WriteFailure::of,
    ))
}

pub(crate) fn write_to(mut file: File, text: &str) -> io::Result<()> {
    file.write_all(text.as_bytes())?;
    file.flush()
}
