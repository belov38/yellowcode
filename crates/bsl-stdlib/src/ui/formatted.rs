use super::font::font_data;
use super::{NS_UI, SharedOf, data_text, types};
use crate::bind::arg_value;
use crate::collections::{ctor_not_implemented, not_implemented};
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use crate::serial::internal::text::{decode_str, encode_str};
use crate::xdto::{XdtoCodec, XdtoCtx, XdtoData};
use brace::Node;
use bsl_lib::xdto::QName;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostObject, MemberId, ObjRef, SharedValue, TypeId, Value,
};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) struct Formatted {
    markup: String,
}

pub(super) fn presentation_input_len(value: &Value) -> Option<u64> {
    let formatted = value.as_object()?.as_any().downcast_ref::<Formatted>()?;
    Some(formatted.markup.len() as u64)
}

fn new_formatted(markup: String) -> Value {
    Value::Object(Rc::new(Formatted { markup }))
}

pub(super) fn ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let unsupported =
        || -> CallResult<Value> { Err(ctor_not_implemented(types().formatted).into()) };
    if args.len() != 2 {
        return unsupported();
    }
    let (Value::String(text), font) = (arg_value(ex, args.first())?, arg_value(ex, args.get(1))?)
    else {
        return unsupported();
    };
    let text = text.to_string_lossy();
    if text.contains(['<', '>', '&']) {
        return unsupported();
    }
    let Some(id) = font_data(&font).and_then(|f| f.normal_bold_only()) else {
        return unsupported();
    };
    Ok(new_formatted(format!("<fontstyle {id}><b>{text}</></>")))
}

impl HostObject for Formatted {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().formatted)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(types().formatted, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(not_implemented(types().formatted, m).into())
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().formatted, m).into())
    }

    fn presentation(&self, _ex: &mut Execution) -> CallResult<Option<Str>> {
        let mut out = String::new();
        let mut in_tag = false;
        for c in self.markup.chars() {
            match c {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        Ok(Some(Str::from(out.as_str())))
    }

    fn equals(&self, other: &dyn HostObject) -> Option<bool> {
        let o = other.as_any().downcast_ref::<Formatted>()?;
        Some(o.markup == self.markup)
    }

    fn boost_hash(&self) -> Option<i32> {
        let u: Vec<u16> = self.markup.encode_utf16().collect();
        Some(bsl_prim::boost_hash::string(&u))
    }

    fn to_shared(&self) -> Option<SharedValue> {
        Some(SharedValue::Object(Arc::new(SharedOf::new(
            self.markup.clone(),
            |m| new_formatted(m.clone()),
        ))))
    }
}

pub(super) struct FormattedCodec;

impl ValueCodec for FormattedCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        let Some(f) = v.as_any().downcast_ref::<Formatted>() else {
            return Ok(None);
        };
        w.account(ex, f.markup.len() as u64 * 3)?;
        let u: Vec<u16> = f.markup.encode_utf16().collect();
        Ok(Some(vec![Node::int(1), encode_str(&u)]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        if let Some(text) = items.get(1).and_then(Node::as_str) {
            ex.reserve((text.len() as u64).saturating_mul(4))?;
        }
        match items.get(1).and_then(decode_str) {
            Some(u) => Ok(new_formatted(String::from_utf16_lossy(&u))),
            None => Err(r.format_error(ex)),
        }
    }
}

fn ui_type() -> QName {
    QName::new(NS_UI, "FormattedString")
}

impl XdtoCodec for FormattedCodec {
    fn to_xdto(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        s: &mut XdtoCtx,
    ) -> Result<XdtoData, Exception> {
        let Some(f) = v.as_any().downcast_ref::<Formatted>() else {
            return Err(s.mismatch(ex));
        };
        s.account(ex, f.markup.len() as u64)?;
        Ok(data_text(ui_type(), &f.markup))
    }

    fn from_xdto(
        &self,
        ex: &mut Execution,
        d: &XdtoData,
        s: &mut XdtoCtx,
    ) -> Result<Value, Exception> {
        match d {
            XdtoData::Value { lexical, .. } => {
                ex.reserve(lexical.len() as u64 * 2)?;
                Ok(new_formatted(lexical.clone()))
            }
            _ => Err(s.lexical_error(ex, "", &ui_type())),
        }
    }

    fn xdto_type(&self) -> QName {
        ui_type()
    }
}
