use super::guard_bytes;
use crate::bind::{arg_str, arg_value, msg, type_mismatch};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::enums::{as_enum_value, type_by_id};
use crate::fs::{fs, fs_error};
use crate::values::{binary_data_bytes, new_binary_data};
use bsl_lib::hash::{HashFn, HashSum, Hasher};
use bsl_prim::Number;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use platform_catalog::TypeIdx;
use platform_catalog::messages::{Lang, text_key};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

struct HashTypes {
    hashing: TypeIdx,
    function: TypeIdx,
}

#[allow(
    clippy::panic,
    reason = "the embedded catalog has every type listed here"
)]
fn hash_types() -> &'static HashTypes {
    static T: OnceLock<HashTypes> = OnceLock::new();
    T.get_or_init(|| {
        let t = |id: &str| type_by_id(id).unwrap_or_else(|| panic!("catalog type {id}"));
        HashTypes {
            hashing: t("DataHashing"),
            function: t("HashFunction"),
        }
    })
}

pub(crate) fn function_presentation(t: TypeIdx, name_en: &str, lang: Lang) -> Option<Str> {
    if t != hash_types().function {
        return None;
    }
    let key = match name_en {
        "CRC32" => msg::HASH_CRC32,
        "MD5" => msg::HASH_MD5,
        "SHA1" => msg::HASH_SHA1,
        "SHA256" => msg::HASH_SHA256,
        "SHA512" => msg::HASH_SHA512,
        "PBKDF2SHA256" => msg::HASH_PBKDF2,
        _ => return None,
    };
    text_key(key, lang).map(Str::from)
}

struct DataHashing {
    h: RefCell<Hasher>,
    function: Value,
}

fn hash_fn(v: &Value) -> Option<HashFn> {
    let e = as_enum_value(v)?;
    if e.type_idx() != hash_types().function {
        return None;
    }
    match e.name_en() {
        "MD5" => Some(HashFn::Md5),
        "SHA1" => Some(HashFn::Sha1),
        "SHA256" => Some(HashFn::Sha256),
        "SHA512" => Some(HashFn::Sha512),
        "CRC32" => Some(HashFn::Crc32),
        _ => None,
    }
}

fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.len() != 1 {
        return Err(ctor_not_implemented(hash_types().hashing).into());
    }
    let function = arg_value(ex, args.first())?;
    let Some(f) = hash_fn(&function) else {
        return Err(type_mismatch(1).into());
    };
    Ok(Value::Object(Rc::new(DataHashing {
        h: RefCell::new(Hasher::new(f)),
        function,
    })))
}

impl DataHashing {
    fn feed(&self, ex: &mut Execution, bytes: &[u8]) -> CallResult<()> {
        ex.charge(bytes.len() as u64 / 64)?;
        self.h.borrow_mut().update(bytes);
        Ok(())
    }

    fn append(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args.first())?;
        match &v {
            Value::String(s) => {
                ex.charge(s.units().len() as u64 * 3 / 64)?;
                self.h.borrow_mut().update_str(s.units());
            }
            _ => match binary_data_bytes(&v) {
                Some(b) => self.feed(ex, b)?,
                None => return Err(type_mismatch(1).into()),
            },
        }
        Ok(Value::Undefined)
    }

    fn append_file(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let path = arg_str(ex, args.first())?.to_string_lossy();
        let f = fs(ex, &path)?;
        if let Some(st) = f.get().stat(&path).map_err(fs_error)? {
            guard_bytes(ex, st.size)?;
        }
        let data = f.get().read(&path).map_err(fs_error)?;
        self.h.borrow_mut().update(&data);
        Ok(Value::Undefined)
    }

    fn sum(&self) -> Value {
        match self.h.borrow().sum() {
            HashSum::Crc32(n) => Value::Number(Number::from_i64(i64::from(n))),
            HashSum::Bytes(b) => new_binary_data(b),
        }
    }
}

impl HostObject for DataHashing {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(hash_types().hashing)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = hash_types().hashing;
        match property_name(t, m) {
            Some("HashSum") => Ok(self.sum()),
            Some("HashFunction") => Ok(self.function.clone()),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = hash_types().hashing;
        match property_name(t, m) {
            Some(_) => Err(HostError::key(msg::PROP_NOT_WRITABLE)
                .with_arg(property_ru(t, m))
                .raw()
                .into()),
            None => Err(not_implemented(t, m).into()),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = hash_types().hashing;
        let args: &[Arg] = args;
        match method_name(t, m) {
            Some("Append") => self.append(ex, args),
            Some("AppendFile") => self.append_file(ex, args),
            _ => Err(not_implemented(t, m).into()),
        }
    }
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        hash_types().hashing,
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
}
