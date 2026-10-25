use super::{Default, encoding_arg, not_open, read_file, text_arg, text_types};
use crate::binary::{SharedStream, read_stream, stream_of};
use crate::bind::{arg_i64, arg_value, is_given, type_mismatch, value_mismatch};
use crate::collections::{ctor_not_implemented, method_name, not_implemented, property_ru};
use crate::fs::{FileLock, file_not_found, file_sharing, fs, fs_error};
use crate::globals::reserve_units;
use bsl_lib::codec::{self, Encoding};
use bsl_lib::textio::reader_all;
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
enum Source {
    File(String),
    Stream(SharedStream),
}

struct Cfg {
    src: Source,
    enc: Encoding,
    sep: Option<Vec<u16>>,
    conv: Option<Vec<u16>>,
    _lock: Option<FileLock>,
}

struct TextReader {
    cfg: RefCell<Option<Rc<Cfg>>>,
    text: RefCell<Option<Rc<Vec<u16>>>>,
    pos: Cell<usize>,
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let r = TextReader {
        cfg: RefCell::new(None),
        text: RefCell::new(None),
        pos: Cell::new(0),
    };
    if !args.is_empty() {
        r.open(ex, args)?;
    }
    Ok(Value::Object(Rc::new(r)))
}

fn setup(ex: &mut Execution, args: &[Arg]) -> CallResult<Cfg> {
    if args.len() > 4 {
        return Err(ctor_not_implemented(text_types().reader).into());
    }
    let first = arg_value(ex, args.first())?;
    let enc_value = arg_value(ex, args.get(1))?;
    let (enc, _) = encoding_arg(ex, &enc_value, 2, Default::Ansi)?;
    let given = |ex: &mut Execution, i: usize| -> CallResult<Option<Vec<u16>>> {
        if is_given(args.get(i)) {
            Ok(Some(text_arg(ex, args.get(i), &[])?).filter(|t| !t.is_empty()))
        } else {
            Ok(None)
        }
    };
    let sep = given(ex, 2)?;
    let conv = given(ex, 3)?;
    let mut lock = None;
    let src = match &first {
        Value::String(p) => {
            let path = p.to_string_lossy();
            let f = fs(ex, &path)?;
            if f.get().stat(&path).map_err(fs_error)?.is_none() {
                match f.get().read(&path) {
                    Err(e) if e.kind != bsl_vm::FsErrorKind::NotFound => {
                        return Err(fs_error(e).into());
                    }
                    _ => return Err(file_not_found(&path).into()),
                }
            }
            lock = Some(FileLock::acquire(f.app(), &path).ok_or_else(|| file_sharing(&path))?);
            Source::File(path)
        }
        v => match stream_of(v) {
            Some(s) => Source::Stream(s),
            None => return Err(type_mismatch(1).into()),
        },
    };
    Ok(Cfg {
        src,
        enc,
        sep,
        conv,
        _lock: lock,
    })
}

impl TextReader {
    fn open(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        *self.cfg.borrow_mut() = None;
        let c = setup(ex, args)?;
        *self.cfg.borrow_mut() = Some(Rc::new(c));
        *self.text.borrow_mut() = None;
        self.pos.set(0);
        Ok(Value::Undefined)
    }

    fn cfg(&self) -> CallResult<Rc<Cfg>> {
        self.cfg.borrow().clone().ok_or_else(|| not_open().into())
    }

    fn text(&self, ex: &mut Execution, cfg: &Cfg) -> CallResult<Rc<Vec<u16>>> {
        if let Some(t) = self.text.borrow().clone() {
            return Ok(t);
        }
        let bytes = match &cfg.src {
            Source::File(path) => read_file(ex, path)?,
            Source::Stream(s) => read_stream(ex, s, u64::MAX)?,
        };
        reserve_units(ex, bytes.len(), 1)?;
        let t = Rc::new(codec::bytes_to_string(&bytes, cfg.enc));
        drop(bytes);
        *self.text.borrow_mut() = Some(t.clone());
        Ok(t)
    }

    fn shown(ex: &mut Execution, conv: Option<&[u16]>, text: Vec<u16>) -> CallResult<Value> {
        let Some(conv) = conv else {
            return Ok(Value::String(Str::from_vec(text)));
        };
        reserve_units(ex, text.len(), 1)?;
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        while i < text.len() {
            if text[i..].starts_with(conv) {
                out.push(0x0A);
                i += conv.len();
            } else {
                out.push(text[i]);
                i += 1;
            }
        }
        Ok(Value::String(Str::from_vec(out)))
    }

    fn line(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let cfg = self.cfg()?;
        let call = if is_given(args.first()) {
            Some(text_arg(ex, args.first(), &[])?).filter(|t| !t.is_empty())
        } else {
            None
        };
        let sep = call.or_else(|| cfg.sep.clone());
        let t = self.text(ex, &cfg)?;
        let start = self.pos.get();
        if start >= t.len() {
            return Ok(Value::Undefined);
        }
        let rest = &t[start..];
        let (end, next) = match &sep {
            Some(sep) => match (0..rest.len()).find(|&i| rest[i..].starts_with(sep)) {
                Some(i) => (i, i + sep.len()),
                None => (rest.len(), rest.len()),
            },
            None => match rest.iter().position(|&c| c == 0x0A) {
                Some(i) if i > 0 && rest[i - 1] == 0x0D => (i - 1, i + 1),
                Some(i) => (i, i + 1),
                None => (rest.len(), rest.len()),
            },
        };
        reserve_units(ex, end, 1)?;
        let line = rest[..end].to_vec();
        let conv = match &sep {
            Some(_) => Some(cfg.conv.as_deref().unwrap_or(&super::CRLF)),
            None => cfg.conv.as_deref(),
        };
        let v = Self::shown(ex, conv, line)?;
        self.pos.set(start + next);
        Ok(v)
    }

    fn read(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let limit = if is_given(args.first()) {
            let n = arg_i64(ex, args.first(), 1)?;
            Some(usize::try_from(n).map_err(|_| value_mismatch(1))?)
        } else {
            None
        };
        let cfg = self.cfg()?;
        let t = self.text(ex, &cfg)?;
        let start = self.pos.get();
        if start >= t.len() {
            return Ok(Value::Undefined);
        }
        let rest = &t[start..];
        reserve_units(ex, limit.map_or(rest.len(), |n| n.min(rest.len())), 1)?;
        let (taken, used) = match limit {
            None => (reader_all(rest), rest.len()),
            Some(n) => {
                let (mut out, mut i, mut k) = (Vec::new(), 0, 0);
                while k < n && i < rest.len() {
                    let c = rest[i];
                    if c == 0x0D && rest.get(i + 1) == Some(&0x0A) {
                        i += 1;
                    } else if (0xD800..0xDC00).contains(&c)
                        && rest
                            .get(i + 1)
                            .is_some_and(|d| (0xDC00..0xE000).contains(d))
                    {
                        out.push(c);
                        i += 1;
                    }
                    out.push(rest[i]);
                    i += 1;
                    k += 1;
                }
                (out, i)
            }
        };
        let v = Self::shown(ex, cfg.conv.as_deref(), taken)?;
        self.pos.set(start + used);
        Ok(v)
    }
}

impl HostObject for TextReader {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(text_types().reader)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(crate::collections::type_name(ex, text_types().reader)))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(text_types().reader, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = text_types().reader;
        let name = property_ru(t, m);
        if name.is_empty() {
            return Err(not_implemented(t, m).into());
        }
        Err(HostError::key(crate::bind::msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = text_types().reader;
        let args: &[Arg] = args;
        match method_name(t, m) {
            Some("Open") => self.open(ex, args),
            Some("Close") => {
                *self.cfg.borrow_mut() = None;
                *self.text.borrow_mut() = None;
                Ok(Value::Undefined)
            }
            Some("ReadLine") => self.line(ex, args),
            Some("Read") => self.read(ex, args),
            _ => Err(not_implemented(t, m).into()),
        }
    }
}
