use super::{fs, fs_error};
use crate::bind::{arg_str, msg as bmsg};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::enums::type_by_id;
use crate::globals::tz::{server_zone, to_local_in};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, FsError, FsErrorKind, HostError, HostObject, MemberId, Registry,
    TypeId, TypeIdx, TypeImpl, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
fn file_type() -> TypeIdx {
    static T: OnceLock<TypeIdx> = OnceLock::new();
    *T.get_or_init(|| type_by_id("File").unwrap_or_else(|| panic!("catalog type File")))
}

pub(crate) fn new_file(path: &str) -> Value {
    Value::Object(Rc::new(FileObj {
        full: path.to_owned(),
    }))
}

pub(crate) struct FileObj {
    full: String,
}

impl FileObj {
    fn split(&self) -> (&str, &str) {
        let (dir, name) = match self.full.rfind(['\\', '/']) {
            Some(i) => (&self.full[..=i], &self.full[i + 1..]),
            None => ("", &self.full[..]),
        };
        let trimmed = name.trim_end_matches('.');
        (dir, if trimmed.is_empty() { name } else { trimmed })
    }

    fn extension(name: &str) -> &str {
        name.rfind('.').map_or("", |i| &name[i..])
    }

    fn stat(&self, ex: &Execution) -> CallResult<Option<bsl_vm::FsStat>> {
        let f = fs(ex, &self.full)?;
        Ok(f.get().stat(&self.full).map_err(fs_error)?)
    }

    fn existing(&self, ex: &Execution) -> CallResult<bsl_vm::FsStat> {
        self.stat(ex)?.ok_or_else(|| {
            fs_error(FsError {
                kind: FsErrorKind::NotFound,
                path: self.full.clone(),
            })
            .into()
        })
    }
}

impl HostObject for FileObj {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(file_type())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = file_type();
        let (path, name) = self.split();
        let text = match property_name(t, m) {
            Some("Name") => name,
            Some("BaseName") => &name[..name.len() - Self::extension(name).len()],
            Some("Extension") => Self::extension(name),
            Some("FullName") => &self.full,
            Some("Path") => path,
            _ => return Err(not_implemented(t, m).into()),
        };
        Ok(Value::String(Str::from(text)))
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = file_type();
        match property_name(t, m) {
            Some(_) => Err(HostError::key(bmsg::PROP_NOT_WRITABLE)
                .with_arg(property_ru(t, m))
                .raw()
                .into()),
            None => Err(not_implemented(t, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        let t = file_type();
        match method_name(t, m) {
            Some("Exists") => Ok(Value::Bool(self.stat(ex)?.is_some())),
            Some("IsFile") => Ok(Value::Bool(self.stat(ex)?.is_some_and(|s| !s.is_dir))),
            Some("IsDirectory") => Ok(Value::Bool(self.stat(ex)?.is_some_and(|s| s.is_dir))),
            Some("Size") => Ok(Value::from(
                i64::try_from(self.existing(ex)?.size).unwrap_or(i64::MAX),
            )),
            Some("GetModificationUniversalTime") => {
                Ok(Value::Date(self.existing(ex)?.modified_utc))
            }
            Some("GetModificationTime") => {
                let utc = self.existing(ex)?.modified_utc;
                Ok(Value::Date(to_local_in(server_zone(ex).as_ref(), utc)))
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.len() != 1 {
        return Err(ctor_not_implemented(file_type()).into());
    }
    let path = arg_str(ex, args.first())?.to_string_lossy();
    fs(ex, &path)?;
    Ok(new_file(&path))
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        file_type(),
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
}
