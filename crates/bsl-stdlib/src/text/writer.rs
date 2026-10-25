use super::{CRLF, Default, LF, encoding_arg, not_open, text_arg, text_types};
use crate::binary::{SharedStream, open_file, stream_of, write_stream};
use crate::bind::{arg_bool, arg_str, arg_value, is_given, type_mismatch};
use crate::collections::{ctor_not_implemented, method_name, not_implemented, property_ru};
use crate::fs::{FileLock, file_sharing, fs};
use crate::globals::reserve_units;
use bsl_lib::codec::{self, Encoding};
use bsl_lib::textio::writer_text;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

struct Open {
    s: SharedStream,
    owns: bool,
    enc: Encoding,
    sep: Vec<u16>,
    conv: Vec<u16>,
    _lock: Option<FileLock>,
}

struct TextWriter {
    st: RefCell<Option<Rc<Open>>>,
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let w = TextWriter {
        st: RefCell::new(None),
    };
    if !args.is_empty() {
        let o = open(ex, args)?;
        *w.st.borrow_mut() = Some(Rc::new(o));
    }
    Ok(Value::Object(Rc::new(w)))
}

fn open(ex: &mut Execution, args: &[Arg]) -> CallResult<Open> {
    let first = arg_value(ex, args.first())?;
    let by_name = matches!(first, Value::String(_));
    let (conv_at, max) = if by_name { (4, 5) } else { (3, 4) };
    if args.len() > max {
        return Err(ctor_not_implemented(text_types().writer).into());
    }
    let enc_value = arg_value(ex, args.get(1))?;
    let (enc, bom) = encoding_arg(ex, &enc_value, 2, Default::Utf8)?;
    let sep = text_arg(ex, args.get(2), &LF)?;
    let append = by_name && is_given(args.get(3)) && arg_bool(ex, args.get(3))?;
    let conv = text_arg(ex, args.get(conv_at), &CRLF)?;
    let mut lock = None;
    let (target, owns) = match &first {
        Value::String(path) => {
            let p = path.to_string_lossy();
            let f = fs(ex, &p)?;
            lock = Some(FileLock::acquire(f.app(), &p).ok_or_else(|| file_sharing(&p))?);
            let mode = if append {
                "Дописать"
            } else {
                "Создать"
            };
            let v = open_file(ex, &path.to_string_lossy(), mode, Some("Запись"))?;
            (v, true)
        }
        v if stream_of(v).is_some() => (v.clone(), false),
        _ => return Err(type_mismatch(1).into()),
    };
    let Some(s) = stream_of(&target) else {
        return Err(type_mismatch(1).into());
    };
    if owns && bom && s.borrow().len().map_err(|e| e.into_call(ex))? == 0 {
        write_stream(ex, &s, codec::bom(enc))?;
    }
    Ok(Open {
        s,
        owns,
        enc,
        sep,
        conv,
        _lock: lock,
    })
}

impl Open {
    fn put(&self, ex: &mut Execution, text: &[u16]) -> CallResult<Value> {
        reserve_units(ex, text.len(), 4)?;
        let t = writer_text(text, &self.conv);
        write_stream(ex, &self.s, &codec::encode(&t, self.enc))?;
        Ok(Value::Undefined)
    }

    fn close(&self, ex: &Execution) -> CallResult<()> {
        let mut b = self.s.borrow_mut();
        let r = if self.owns { b.close() } else { b.flush() };
        r.map_err(|e| e.into_call(ex))
    }
}

impl TextWriter {
    fn opened(&self) -> CallResult<Rc<Open>> {
        self.st.borrow().clone().ok_or_else(|| not_open().into())
    }

    fn reopen(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        self.close(ex)?;
        let o = open(ex, args)?;
        *self.st.borrow_mut() = Some(Rc::new(o));
        Ok(Value::Undefined)
    }

    fn close(&self, ex: &mut Execution) -> CallResult<Value> {
        let old = self.st.borrow_mut().take();
        if let Some(o) = old {
            o.close(ex)?;
        }
        Ok(Value::Undefined)
    }
}

impl HostObject for TextWriter {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(text_types().writer)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(crate::collections::type_name(ex, text_types().writer)))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(text_types().writer, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        let t = text_types().writer;
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
        let t = text_types().writer;
        let args: &[Arg] = args;
        match method_name(t, m) {
            Some("Close") => self.close(ex),
            Some("Open") => self.reopen(ex, args),
            Some(n @ ("Write" | "WriteLine")) => {
                let o = self.opened()?;
                let s = arg_str(ex, args.first())?;
                if n == "Write" {
                    return o.put(ex, s.units());
                }
                let mut text = s.units().to_vec();
                let sep = text_arg(ex, args.get(1), &o.sep)?;
                text.extend_from_slice(&sep);
                o.put(ex, &text)
            }
            _ => Err(not_implemented(t, m).into()),
        }
    }
}
