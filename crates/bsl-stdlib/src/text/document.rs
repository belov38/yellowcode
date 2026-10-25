use super::{CRLF, Default, encoding_arg, read_file, text_arg, text_types};
use crate::bind::{arg_i64, arg_str, arg_value, is_given, msg, with_other};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::fs::{fs, fs_error};
use crate::globals::reserve_units;
use bsl_lib::codec;
use bsl_lib::textio::{document_lines, reader_all, writer_text};
use bsl_prim::Str;
use bsl_vm::{Arg, CallResult, Execution, HostError, HostObject, MemberId, TypeId, Value};
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Doc {
    lines: Vec<Vec<u16>>,
    raw: Option<Vec<u16>>,
}

struct TextDocument {
    d: RefCell<Doc>,
    sep: RefCell<Vec<u16>>,
}

pub(super) fn ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(text_types().document).into());
    }
    Ok(Value::Object(Rc::new(TextDocument {
        d: RefCell::new(Doc::default()),
        sep: RefCell::new(CRLF.to_vec()),
    })))
}

impl Doc {
    fn set_text(&mut self, text: Vec<u16>) {
        self.lines = document_lines(&text)
            .into_iter()
            .map(|r| text[r].to_vec())
            .collect();
        self.raw = Some(text);
    }

    fn text(&self) -> Vec<u16> {
        if let Some(r) = &self.raw {
            return r.clone();
        }
        let mut out = Vec::new();
        for l in &self.lines {
            out.extend_from_slice(l);
            out.push(0x0A);
        }
        out
    }

    fn touch(&mut self) {
        self.raw = None;
    }
}

fn string(units: Vec<u16>) -> Value {
    Value::String(Str::from_vec(units))
}

impl TextDocument {
    fn number(ex: &mut Execution, args: &[Arg]) -> CallResult<i64> {
        arg_i64(ex, args.first(), 1)
    }

    fn line_text(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Vec<Vec<u16>>> {
        let s = arg_str(ex, a)?;
        reserve_units(ex, s.len(), 2)?;
        let units = s.units();
        let lines: Vec<Vec<u16>> = document_lines(units)
            .into_iter()
            .map(|r| units[r].to_vec())
            .collect();
        Ok(if lines.is_empty() {
            vec![Vec::new()]
        } else {
            lines
        })
    }

    fn one_text(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Vec<u16>> {
        let s = arg_str(ex, a)?;
        reserve_units(ex, s.len(), 1)?;
        Ok(s.units().to_vec())
    }

    fn get_text(&self, ex: &mut Execution) -> CallResult<Value> {
        let n: usize = {
            let d = self.d.borrow();
            d.raw
                .as_ref()
                .map_or_else(|| d.lines.iter().map(|l| l.len() + 1).sum(), Vec::len)
        };
        reserve_units(ex, n, 2)?;
        Ok(string(self.d.borrow().text()))
    }

    fn set_text(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let s = arg_str(ex, args.first())?;
        reserve_units(ex, s.len(), 3)?;
        self.d.borrow_mut().set_text(s.units().to_vec());
        Ok(Value::Undefined)
    }

    fn get_line(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = Self::number(ex, args)?;
        let d = self.d.borrow();
        let line = usize::try_from(n)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| d.lines.get(i));
        Ok(string(line.cloned().unwrap_or_default()))
    }

    fn add_line(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let t = Self::line_text(ex, args.first())?;
        let mut d = self.d.borrow_mut();
        d.lines.extend(t);
        d.touch();
        Ok(Value::Undefined)
    }

    fn insert_line(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = Self::number(ex, args)?;
        let t = Self::line_text(ex, args.get(1))?;
        let mut d = self.d.borrow_mut();
        let at = usize::try_from(n.saturating_sub(1))
            .unwrap_or(0)
            .min(d.lines.len());
        d.lines.splice(at..at, t);
        d.touch();
        Ok(Value::Undefined)
    }

    fn replace_line(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = Self::number(ex, args)?;
        let t = Self::one_text(ex, args.get(1))?;
        let mut d = self.d.borrow_mut();
        if let Some(l) = index(n, d.lines.len()).and_then(|i| d.lines.get_mut(i)) {
            *l = t;
            d.touch();
        }
        Ok(Value::Undefined)
    }

    fn delete_line(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let n = Self::number(ex, args)?;
        let mut d = self.d.borrow_mut();
        if let Some(i) = index(n, d.lines.len()) {
            d.lines.remove(i);
            d.touch();
        }
        Ok(Value::Undefined)
    }

    fn read(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let path = arg_str(ex, args.first())?.to_string_lossy();
        let v = arg_value(ex, args.get(1))?;
        let (enc, _) = encoding_arg(ex, &v, 2, Default::System)?;
        let bytes = read_file(ex, &path)?;
        reserve_units(ex, bytes.len(), 3)?;
        let text = codec::bytes_to_string(&bytes, enc);
        self.d.borrow_mut().set_text(text);
        Ok(Value::Undefined)
    }

    fn write(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let path = arg_str(ex, args.first())?.to_string_lossy();
        let v = arg_value(ex, args.get(1))?;
        let (enc, bom) = encoding_arg(ex, &v, 2, Default::Utf8)?;
        let sep = if is_given(args.get(2)) {
            text_arg(ex, args.get(2), &CRLF)?
        } else {
            self.sep.borrow().clone()
        };
        let joined = {
            let d = self.d.borrow();
            let total = d
                .raw
                .as_ref()
                .map_or_else(|| d.lines.iter().map(|l| l.len() + 1).sum(), Vec::len);
            reserve_units(ex, total, 6)?;
            reader_all(&d.text())
        };
        let text = writer_text(&joined, &sep);
        reserve_units(ex, text.len(), 2)?;
        let bytes = codec::string_to_bytes(&text, enc, bom);
        let f = fs(ex, &path)?;
        if crate::fs::is_held(f.get(), &path) {
            return Err(crate::fs::file_sharing(&path).into());
        }
        f.get().write(&path, &bytes, false).map_err(fs_error)?;
        Ok(Value::Undefined)
    }
}

fn index(n: i64, len: usize) -> Option<usize> {
    let n = if n == 0 { 1 } else { n };
    let i = usize::try_from(n.checked_sub(1)?).ok()?;
    (i < len).then_some(i)
}

impl HostObject for TextDocument {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(text_types().document)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(Some(crate::collections::type_name(
            ex,
            text_types().document,
        )))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = text_types().document;
        match property_name(t, m) {
            Some("LineSeparator") => Ok(string(self.sep.borrow().clone())),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let t = text_types().document;
        if property_name(t, m) == Some("LineSeparator") {
            let s = arg_str(ex, Some(&Arg::Val(v)))?;
            let units = s.units();
            if units != [0x0A] && units != [0x0D, 0x0A] && units != [0x0D] {
                return Err(with_other(HostError::key(msg::PROPERTY_VALUE_MISMATCH), false).into());
            }
            *self.sep.borrow_mut() = units.to_vec();
            return Ok(());
        }
        let name = property_ru(t, m);
        if name.is_empty() {
            return Err(not_implemented(t, m).into());
        }
        Err(HostError::key(msg::PROP_NOT_WRITABLE)
            .with_arg(name)
            .raw()
            .into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = text_types().document;
        let args: &[Arg] = args;
        match method_name(t, m) {
            Some("GetText") => self.get_text(ex),
            Some("SetText") => self.set_text(ex, args),
            Some("LineCount") => Ok(Value::Number(bsl_prim::Number::from_u64(
                self.d.borrow().lines.len() as u64,
            ))),
            Some("GetLine") => self.get_line(ex, args),
            Some("AddLine") => self.add_line(ex, args),
            Some("InsertLine") => self.insert_line(ex, args),
            Some("ReplaceLine") => self.replace_line(ex, args),
            Some("DeleteLine") => self.delete_line(ex, args),
            Some("Clear") => {
                *self.d.borrow_mut() = Doc::default();
                Ok(Value::Undefined)
            }
            Some("Read") => self.read(ex, args),
            Some("Write") => self.write(ex, args),
            _ => Err(not_implemented(t, m).into()),
        }
    }
}
