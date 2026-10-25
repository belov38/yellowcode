use crate::bind::{key_error, msg, with_other};
use bsl_vm::{
    AppHost, Cause, Execution, FileSystem, FsError, FsErrorKind, FsStat, GuardedOp, HostError,
    MsgRef,
};
use platform_catalog::messages::text_key;
use std::sync::Arc;

const DIR_NOT_FOUND: &str = "core85:IDS_FERROR_DIRNOTFOUND";
const FILE_INFO: &str = "ext:IDS_ERR_FILEINFO";

pub(crate) fn fs_error(e: FsError) -> HostError {
    let (key, suffix) = match e.kind {
        FsErrorKind::DirNotFound => (DIR_NOT_FOUND, format!(" '{}'", e.path)),
        FsErrorKind::NotFound | FsErrorKind::AccessDenied | FsErrorKind::TooLarge => {
            (FILE_INFO, e.path)
        }
    };
    let mut h = HostError::key(key);
    if let MsgRef::Key(k) = &mut h.msg {
        k.suffix = suffix;
    }
    h
}

pub(crate) struct Fs {
    app: Arc<dyn AppHost>,
}

pub(crate) fn fs(ex: &Execution, path: &str) -> Result<Fs, HostError> {
    let app = ex.runtime().app().clone();
    app.check(ex, GuardedOp::File)?;
    if app.file_system().is_none() {
        return Err(fs_error(FsError {
            kind: FsErrorKind::AccessDenied,
            path: path.to_owned(),
        }));
    }
    Ok(Fs { app })
}

struct NoFs;

impl FileSystem for NoFs {
    fn temp_dir(&self) -> String {
        String::new()
    }
    fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        Err(denied(path))
    }
    fn write(&self, path: &str, _: &[u8], _: bool) -> Result<(), FsError> {
        Err(denied(path))
    }
    fn stat(&self, path: &str) -> Result<Option<FsStat>, FsError> {
        Err(denied(path))
    }
    fn remove(&self, path: &str, _: Option<&str>) -> Result<(), FsError> {
        Err(denied(path))
    }
    fn create_dir(&self, path: &str) -> Result<(), FsError> {
        Err(denied(path))
    }
    fn list(&self, dir: &str, _: &str, _: bool) -> Result<Vec<String>, FsError> {
        Err(denied(dir))
    }
}

fn denied(path: &str) -> FsError {
    FsError {
        kind: FsErrorKind::AccessDenied,
        path: path.to_owned(),
    }
}

impl Fs {
    pub(crate) fn app(&self) -> &Arc<dyn AppHost> {
        &self.app
    }

    pub(crate) fn get(&self) -> &dyn FileSystem {
        self.app.file_system().unwrap_or(&NoFs)
    }
}

fn shown(path: &str) -> &str {
    if path.len() > 3 {
        path.trim_end_matches(['\\', '/'])
    } else {
        path
    }
}

fn named(key: &'static str, path: &str) -> HostError {
    with_other(key_error(key, Vec::new(), format!(" '{path}'")), true)
}

pub(crate) fn file_not_found(path: &str) -> HostError {
    named(msg::FILE_NOT_FOUND, path)
}

pub(crate) fn file_sharing(path: &str) -> HostError {
    named(msg::FILE_SHARING, path)
}

pub(crate) fn stream_not_found(path: &str) -> HostError {
    let one = || key_error(msg::STREAM_FILE_NOT_FOUND, Vec::new(), format!(": {path}"));
    with_other(one(), true).caused_by(Cause::Host(Box::new(one())))
}

fn text(ex: &Execution, key: &str) -> String {
    text_key(key, ex.lang()).unwrap_or_default().to_owned()
}

fn chained(head: &'static str, cause: HostError) -> HostError {
    with_other(HostError::key(head), false).caused_by(Cause::Host(Box::new(cause)))
}

pub(crate) fn copy_error(ex: &Execution, src: &str, dst: &str, why: &FsError) -> HostError {
    let reason = match why.kind {
        FsErrorKind::NotFound => msg::FILE_NOT_FOUND,
        FsErrorKind::DirNotFound => "core85:IDS_FERROR_DIRNOTFOUND",
        FsErrorKind::AccessDenied | FsErrorKind::TooLarge => msg::FILE_ACCESS,
    };
    copy_error_text(ex, src, dst, reason)
}

pub(crate) fn copy_error_text(ex: &Execution, src: &str, dst: &str, reason: &str) -> HostError {
    let args = vec![
        text(ex, msg::FILE_COPY),
        shown(src).to_owned(),
        shown(dst).to_owned(),
        text(ex, reason),
    ];
    chained(
        msg::COPY_FILES,
        with_other(key_error(msg::FILE_COPY_MOVE, args, String::new()), false),
    )
}

pub(crate) fn move_error(ex: &Execution, src: &str, why: &FsError, is_dir: bool) -> HostError {
    let cause = if is_dir {
        let tail = format!(". {}", text(ex, msg::FILE_IS_DIR));
        key_error(
            msg::FILE_BAD_PATH,
            Vec::new(),
            format!(" '{}'{tail}", shown(src)),
        )
    } else {
        match why.kind {
            FsErrorKind::NotFound => key_error(
                msg::FILE_NOT_FOUND,
                Vec::new(),
                format!(" '{}'", shown(src)),
            ),
            _ => fs_error(why.clone()),
        }
    };
    chained(msg::MOVE_FILES, with_other(cause, false))
}

pub(crate) fn delete_error(path: &str) -> HostError {
    const OS: &str = "32(0x00000020): The process cannot access the file because it is being used by another process. ";
    let cause = key_error(msg::FILE_SHARING, Vec::new(), format!(" '{path}'. {OS}"));
    chained(msg::DELETE_FILES, with_other(cause, false))
}
