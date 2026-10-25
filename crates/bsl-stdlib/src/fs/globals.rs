use super::file::new_file;
use super::{
    copy_error, delete_error, fs, fs_error, is_held, is_held_under, move_error, same_path,
};
use crate::bind::{arg_bool, arg_str, global, is_given};
use crate::collections::{new_array, reserve_values};
use bsl_vm::{CallResult, Execution, FsError, FsErrorKind, Registry, Value};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn session_tag() -> u32 {
    static TAG: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *TAG.get_or_init(|| 0x100 + std::process::id() % 0xEFF)
}

fn has_wildcard(s: &str) -> bool {
    s.contains(['*', '?'])
}

fn split_mask(path: &str, temp: &str) -> Option<(String, String)> {
    match path.rfind(['\\', '/']) {
        Some(cut) => {
            let (dir, mask) = (&path[..=cut], &path[cut + 1..]);
            (has_wildcard(mask) && !has_wildcard(dir)).then(|| (dir.to_owned(), mask.to_owned()))
        }
        None => has_wildcard(path).then(|| (temp.to_owned(), path.to_owned())),
    }
}

fn bytes_guard(ex: &mut Execution, n: u64) -> CallResult<()> {
    ex.charge(n / 64)?;
    ex.reserve(n)?;
    Ok(())
}

pub(super) fn register(reg: &mut Registry) {
    global(
        reg,
        "КаталогВременныхФайлов",
        "TempFilesDir",
        |ex, _| {
            let f = fs(ex, "")?;
            Ok(Value::from(f.get().temp_dir().as_str()))
        },
    );
    global(
        reg,
        "ПолучитьИмяВременногоФайла",
        "GetTempFileName",
        |ex, a| {
            let ext = if is_given(a.first()) {
                arg_str(ex, a.first())?
                    .to_string_lossy()
                    .trim_start_matches('.')
                    .to_owned()
            } else {
                "tmp".to_owned()
            };
            let ext = ext.as_str();
            let f = fs(ex, "")?;
            let dir = f.get().temp_dir();
            let _ = f.get().create_dir(&dir);
            let mut previous_key = None;
            loop {
                let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
                let name = format!("{dir}v8_{:X}_{n:x}.{ext}", session_tag());
                ex.charge(1)?;
                if f.get().stat(&name).map_err(fs_error)?.is_none() {
                    return Ok(Value::from(name.as_str()));
                }
                let key = f.get().path_key(&name);
                if key.is_some() && key == previous_key {
                    return Err(fs_error(FsError {
                        kind: FsErrorKind::AccessDenied,
                        path: name,
                    })
                    .into());
                }
                previous_key = key;
            }
        },
    );
    global(reg, "УдалитьФайлы", "DeleteFiles", |ex, a| {
        let path = arg_str(ex, a.first())?.to_string_lossy();
        let mask = if is_given(a.get(1)) {
            Some(arg_str(ex, a.get(1))?.to_string_lossy())
        } else {
            None
        };
        let f = fs(ex, &path)?;
        let (dir, mask) = match (mask, split_mask(&path, &f.get().temp_dir())) {
            (Some(m), _) if !m.is_empty() => (path, Some(m)),
            (_, Some((d, m))) => (d, Some(m)),
            _ => (path, None),
        };
        let held = match &mask {
            None => (is_held(f.get(), &dir) || is_held_under(f.get(), &dir)).then(|| dir.clone()),
            Some(m) => f
                .get()
                .list(&dir, m, false)
                .map_err(fs_error)?
                .into_iter()
                .find(|p| is_held(f.get(), p)),
        };
        if let Some(p) = held {
            return Err(delete_error(&p).into());
        }
        f.get().remove(&dir, mask.as_deref()).map_err(fs_error)?;
        Ok(Value::Undefined)
    });
    global(
        reg,
        "СоздатьКаталог",
        "CreateDirectory",
        |ex, a| {
            let path = arg_str(ex, a.first())?.to_string_lossy();
            let f = fs(ex, &path)?;
            f.get().create_dir(&path).map_err(fs_error)?;
            Ok(Value::Undefined)
        },
    );
    global(reg, "НайтиФайлы", "FindFiles", |ex, a| {
        let path = arg_str(ex, a.first())?.to_string_lossy();
        let mask = if is_given(a.get(1)) {
            arg_str(ex, a.get(1))?.to_string_lossy()
        } else {
            String::new()
        };
        let recursive = is_given(a.get(2)) && arg_bool(ex, a.get(2))?;
        let f = fs(ex, &path)?;
        let (dir, mask) = if !mask.is_empty() {
            (path, mask)
        } else if let Some(split) = split_mask(&path, &f.get().temp_dir()) {
            split
        } else if f
            .get()
            .stat(&path)
            .map_err(fs_error)?
            .is_some_and(|s| !s.is_dir)
        {
            match path.rfind(['\\', '/']) {
                Some(cut) => (path[..=cut].to_owned(), path[cut + 1..].to_owned()),
                None => (f.get().temp_dir(), path),
            }
        } else {
            (path, "*".into())
        };
        let found = f.get().list(&dir, &mask, recursive).map_err(fs_error)?;
        reserve_values(ex, found.len() as u64)?;
        let mut files = Vec::with_capacity(found.len());
        for p in found {
            ex.charge(1)?;
            files.push(new_file(&p));
        }
        Ok(new_array(files))
    });
    global(reg, "КопироватьФайл", "CopyFile", |ex, a| {
        transfer(ex, a, false)
    });
    global(
        reg,
        "ПереместитьФайл",
        "MoveFile",
        |ex, a| transfer(ex, a, true),
    );
}

fn transfer(ex: &mut Execution, a: &mut [bsl_vm::Arg], remove_source: bool) -> CallResult<Value> {
    let src = arg_str(ex, a.first())?.to_string_lossy();
    let dst = arg_str(ex, a.get(1))?.to_string_lossy();
    let f = fs(ex, &src)?;
    let fsys = f.get();
    let missing = || FsError {
        kind: FsErrorKind::NotFound,
        path: src.clone(),
    };
    let denied = |path: &str| FsError {
        kind: FsErrorKind::AccessDenied,
        path: path.to_owned(),
    };
    let fail = |ex: &Execution, e: &FsError, is_dir: bool| -> bsl_vm::CallError {
        if remove_source {
            move_error(ex, &src, e, is_dir).into()
        } else {
            copy_error(ex, &src, &dst, e).into()
        }
    };
    let stat = match fsys.stat(&src) {
        Ok(Some(s)) => s,
        Ok(None) => return Err(fail(ex, &missing(), false)),
        Err(e) => return Err(fail(ex, &e, false)),
    };
    if stat.is_dir {
        return Err(fail(ex, &denied(&src), true));
    }
    if same_path(fsys, &src, &dst) {
        if remove_source {
            return Ok(Value::Undefined);
        }
        let why = crate::bind::msg::FILE_SHARING;
        return Err(crate::fs::copy_error_text(ex, &src, &dst, why).into());
    }
    if fsys.stat(&dst).ok().flatten().is_some_and(|s| s.is_dir) {
        return Err(fail(ex, &denied(&dst), false));
    }
    bytes_guard(ex, stat.size)?;
    let data = fsys.read(&src).map_err(|e| fail(ex, &e, false))?;
    if remove_source {
        fsys.remove(&src, None).map_err(|e| fail(ex, &e, false))?;
        if let Err(e) = fsys.write(&dst, &data, false) {
            let _ = fsys.write(&src, &data, false);
            return Err(fail(ex, &e, false));
        }
    } else {
        fsys.write(&dst, &data, false)
            .map_err(|e| fail(ex, &e, false))?;
    }
    Ok(Value::Undefined)
}
